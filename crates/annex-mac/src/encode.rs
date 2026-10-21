//! VideoToolbox HEVC encoder (spec 4.4). Output is converted from hvcC
//! (length-prefixed NAL units) to Annex B, with VPS/SPS/PPS in front of every
//! IDR, and handed straight to the stream's [`FrameSink`].

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use annex_sender::FrameSink;
use anyhow::bail;
use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMFormatDescription, CMSampleBuffer, CMTime, CMVideoFormatDescriptionGetHEVCParameterSetAtIndex,
    kCMSampleAttachmentKey_NotSync, kCMTimeInvalid, kCMVideoCodecType_HEVC,
};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferPool, kCVImageBufferColorPrimaries_ITU_R_709_2, kCVImageBufferTransferFunction_ITU_R_709_2,
    kCVImageBufferYCbCrMatrix_ITU_R_709_2, kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferMetalCompatibilityKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
    kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
};
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTSessionSetProperty, kVTCompressionPropertyKey_AllowFrameReordering,
    kVTCompressionPropertyKey_AllowTemporalCompression, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ColorPrimaries, kVTCompressionPropertyKey_ConstantBitRate,
    kVTCompressionPropertyKey_DataRateLimits, kVTCompressionPropertyKey_ExpectedFrameRate,
    kVTCompressionPropertyKey_MaxFrameDelayCount, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality, kVTCompressionPropertyKey_ProfileLevel,
    kVTCompressionPropertyKey_RealTime, kVTCompressionPropertyKey_TransferFunction,
    kVTCompressionPropertyKey_YCbCrMatrix, kVTEncodeFrameOptionKey_ForceKeyFrame, kVTProfileLevel_HEVC_Main_AutoLevel,
    kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
};

unsafe extern "C" {
    fn CFArrayGetCount(a: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(a: *const c_void, i: isize) -> *const c_void;
    fn CFDictionaryGetValue(d: *const c_void, key: *const c_void) -> *const c_void;
    fn CFBooleanGetValue(b: *const c_void) -> u8;
}

struct FrameMeta {
    captured_at: Instant,
    pack_ms: f64,
    submitted: Instant,
}

struct Ctx {
    sink: Mutex<FrameSink>,
    out: Mutex<(Vec<u8>, Vec<u8>)>,
}

pub struct Encoder {
    session: CFRetained<VTCompressionSession>,
    ctx: *const Ctx,
    force_key: AtomicBool,
    /// Serializes submissions and keeps timestamps increasing.
    submit: Mutex<i64>,
    epoch: Instant,
    cbr: AtomicBool,
}

unsafe impl Send for Encoder {}
unsafe impl Sync for Encoder {}

fn num(v: i64) -> CFRetained<CFNumber> {
    CFNumber::new_i64(v)
}

fn dict(pairs: &[(&CFString, &CFType)]) -> CFRetained<CFDictionary<CFString, CFType>> {
    let keys: Vec<&CFString> = pairs.iter().map(|p| p.0).collect();
    let values: Vec<&CFType> = pairs.iter().map(|p| p.1).collect();
    CFDictionary::from_slices(&keys, &values)
}

impl Encoder {
    pub fn new(width: u32, height: u32, kbps: u32, sink: FrameSink) -> anyhow::Result<Self> {
        let ctx = Box::into_raw(Box::new(Ctx {
            sink: Mutex::new(sink),
            out: Mutex::new((Vec::with_capacity(1 << 20), Vec::with_capacity(1 << 20))),
        }));
        let yes: &CFType = CFBoolean::new(true);
        let fmt = num(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange as i64);
        let (w, h) = (num(width as i64), num(height as i64));
        let empty: CFRetained<CFDictionary<CFString, CFType>> = CFDictionary::from_slices(&[], &[]);
        let src = unsafe {
            dict(&[
                (kCVPixelBufferPixelFormatTypeKey, &fmt),
                (kCVPixelBufferWidthKey, &w),
                (kCVPixelBufferHeightKey, &h),
                (kCVPixelBufferIOSurfacePropertiesKey, &empty),
                (kCVPixelBufferMetalCompatibilityKey, yes),
            ])
        };
        let mut session = None;
        for low_latency in [true, false] {
            let spec = unsafe {
                if low_latency {
                    dict(&[
                        (kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder, yes),
                        (kVTVideoEncoderSpecification_EnableLowLatencyRateControl, yes),
                    ])
                } else {
                    dict(&[(kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder, yes)])
                }
            };
            let mut out: *mut VTCompressionSession = std::ptr::null_mut();
            let status = unsafe {
                VTCompressionSession::create(
                    None,
                    width as i32,
                    height as i32,
                    kCMVideoCodecType_HEVC,
                    Some(spec.as_ref()),
                    Some(src.as_ref()),
                    None,
                    Some(output_callback),
                    ctx as *mut c_void,
                    NonNull::from(&mut out),
                )
            };
            if status == 0 && !out.is_null() {
                tracing::info!(width, height, low_latency, "HEVC encoder ready");
                session = Some(unsafe { CFRetained::from_raw(NonNull::new_unchecked(out)) });
                break;
            }
            tracing::warn!(status, low_latency, "HEVC encoder creation failed");
        }
        let Some(session) = session else {
            unsafe { drop(Box::from_raw(ctx)) };
            bail!("this Mac has no hardware HEVC encoder for {width}x{height}");
        };
        let enc = Encoder {
            session,
            ctx,
            force_key: AtomicBool::new(true),
            submit: Mutex::new(0),
            epoch: Instant::now(),
            cbr: AtomicBool::new(true),
        };
        unsafe {
            let no: &CFType = CFBoolean::new(false);
            enc.set(kVTCompressionPropertyKey_RealTime, yes);
            enc.set(kVTCompressionPropertyKey_AllowFrameReordering, no);
            enc.set(kVTCompressionPropertyKey_MaxFrameDelayCount, &num(0));
            enc.set(kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality, yes);
            enc.set(kVTCompressionPropertyKey_ExpectedFrameRate, &num(60));
            enc.set(kVTCompressionPropertyKey_AllowTemporalCompression, yes);
            enc.set(kVTCompressionPropertyKey_MaxKeyFrameInterval, &num(36000));
            enc.set(kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_HEVC_Main_AutoLevel);
            enc.set(kVTCompressionPropertyKey_ColorPrimaries, kCVImageBufferColorPrimaries_ITU_R_709_2);
            enc.set(kVTCompressionPropertyKey_TransferFunction, kCVImageBufferTransferFunction_ITU_R_709_2);
            enc.set(kVTCompressionPropertyKey_YCbCrMatrix, kCVImageBufferYCbCrMatrix_ITU_R_709_2);
        }
        enc.set_bitrate(kbps);
        unsafe { enc.session.prepare_to_encode_frames() };
        Ok(enc)
    }

    unsafe fn set(&self, key: &CFString, value: &CFType) -> bool {
        let status = unsafe { VTSessionSetProperty(&self.session, key, Some(value)) };
        if status != 0 {
            tracing::debug!(status, key = %key, "encoder property not supported");
        }
        status == 0
    }

    /// Target bitrate. Constant bitrate when the encoder supports it,
    /// otherwise average bitrate with a 1-second data rate limit.
    pub fn set_bitrate(&self, kbps: u32) {
        let bps = kbps as i64 * 1000;
        unsafe {
            if self.cbr.load(Ordering::Relaxed) && self.set(kVTCompressionPropertyKey_ConstantBitRate, &num(bps)) {
                return;
            }
            self.cbr.store(false, Ordering::Relaxed);
            self.set(kVTCompressionPropertyKey_AverageBitRate, &num(bps));
            let bytes = CFNumber::new_f64(bps as f64 / 8.0 * 1.5);
            let secs = CFNumber::new_f64(1.0);
            let limits: CFRetained<CFArray<CFNumber>> = CFArray::from_objects(&[&*bytes, &*secs]);
            let limits: &CFType = &limits;
            self.set(kVTCompressionPropertyKey_DataRateLimits, limits);
        }
    }

    pub fn set_sink(&self, sink: FrameSink) {
        *unsafe { &*self.ctx }.sink.lock().unwrap() = sink;
        self.force_key.store(true, Ordering::Relaxed);
    }

    /// Counts a captured frame for the stats (even if it isn't encoded).
    pub fn note_captured(&self) {
        unsafe { &*self.ctx }.sink.lock().unwrap().note_captured();
    }

    pub fn request_key(&self) {
        self.force_key.store(true, Ordering::Relaxed);
    }

    /// The pool VideoToolbox allocates matching input buffers from.
    pub fn pool(&self) -> Option<CFRetained<CVPixelBufferPool>> {
        unsafe { self.session.pixel_buffer_pool() }
    }

    /// Submits one packed NV12 frame. Asynchronous: the output callback
    /// delivers the access unit.
    pub fn encode(&self, buf: &CVPixelBuffer, captured_at: Instant, pack_ms: f64) {
        let mut last = self.submit.lock().unwrap();
        let pts = (self.epoch.elapsed().as_micros() as i64).max(*last + 1);
        *last = pts;
        let force = self.force_key.swap(false, Ordering::Relaxed);
        let props = force.then(|| unsafe {
            let yes: &CFType = CFBoolean::new(true);
            dict(&[(kVTEncodeFrameOptionKey_ForceKeyFrame, yes)])
        });
        let meta = Box::into_raw(Box::new(FrameMeta { captured_at, pack_ms, submitted: Instant::now() }));
        let status = unsafe {
            self.session.encode_frame(
                buf,
                CMTime::new(pts, 1_000_000),
                kCMTimeInvalid,
                props.as_ref().map(|d| d.as_ref()),
                meta.cast(),
                std::ptr::null_mut(),
            )
        };
        if status != 0 {
            unsafe { drop(Box::from_raw(meta)) };
            if force {
                self.force_key.store(true, Ordering::Relaxed);
            }
            tracing::debug!(status, "encode_frame failed");
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe {
            self.session.complete_frames(kCMTimeInvalid);
            self.session.invalidate();
            drop(Box::from_raw(self.ctx as *mut Ctx));
        }
    }
}

unsafe fn is_keyframe(sample: &CMSampleBuffer) -> bool {
    unsafe {
        let Some(arr) = sample.sample_attachments_array(false) else { return true };
        let arr_ptr = CFRetained::as_ptr(&arr).as_ptr() as *const c_void;
        if CFArrayGetCount(arr_ptr) < 1 {
            return true;
        }
        let d = CFArrayGetValueAtIndex(arr_ptr, 0);
        let key = kCMSampleAttachmentKey_NotSync as *const CFString as *const c_void;
        let v = CFDictionaryGetValue(d, key);
        v.is_null() || CFBooleanGetValue(v) == 0
    }
}

unsafe fn append_parameter_sets(desc: &CMFormatDescription, out: &mut Vec<u8>) {
    let mut count = 0usize;
    unsafe {
        let mut p: *const u8 = std::ptr::null();
        let mut len = 0usize;
        if CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(desc, 0, &mut p, &mut len, &mut count, std::ptr::null_mut()) != 0 {
            return;
        }
        for i in 0..count {
            if CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(desc, i, &mut p, &mut len, std::ptr::null_mut(), std::ptr::null_mut()) == 0
                && !p.is_null()
            {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.extend_from_slice(std::slice::from_raw_parts(p, len));
            }
        }
    }
}

unsafe extern "C-unwind" fn output_callback(
    refcon: *mut c_void,
    frame_refcon: *mut c_void,
    status: i32,
    _flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    let ctx = unsafe { &*(refcon as *const Ctx) };
    let meta = unsafe { Box::from_raw(frame_refcon as *mut FrameMeta) };
    if status != 0 || sample.is_null() {
        return;
    }
    let sample = unsafe { &*sample };
    let encode_ms = meta.submitted.elapsed().as_secs_f64() * 1e3;
    let key = unsafe { is_keyframe(sample) };
    let mut bufs = ctx.out.lock().unwrap();
    let (out, raw) = &mut *bufs;
    out.clear();
    unsafe {
        if key && let Some(desc) = sample.format_description() {
            append_parameter_sets(&desc, out);
        }
        let Some(bb) = sample.data_buffer() else { return };
        let total = bb.data_length();
        raw.resize(total, 0);
        if bb.copy_data_bytes(0, total, NonNull::new_unchecked(raw.as_mut_ptr().cast())) != 0 {
            return;
        }
    }
    // hvcC: 4-byte big-endian lengths → Annex B start codes.
    let mut i = 0;
    while i + 4 <= raw.len() {
        let n = u32::from_be_bytes([raw[i], raw[i + 1], raw[i + 2], raw[i + 3]]) as usize;
        i += 4;
        if i + n > raw.len() {
            break;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&raw[i..i + n]);
        i += n;
    }
    ctx.sink.lock().unwrap().push(out, key, meta.captured_at, meta.pack_ms, encode_ms);
}
