//! HEVC decoding through FFmpeg's D3D11VA hwaccel on our own device (spec 4.5).
//!
//! Decoded frames stay on the GPU: `data[0]` is the decoder's texture array
//! and `data[1]` the slice index. Nothing is ever copied to the CPU.

#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals, dead_code, clippy::all)]

#[rustfmt::skip]
#[allow(unsafe_op_in_unsafe_fn)]
mod sys;

use std::ffi::{CStr, c_int, c_void};
use std::ptr;

use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_DECODER, D3D11_BIND_SHADER_RESOURCE, ID3D11Device, ID3D11Texture2D,
};
use windows::core::Interface;

pub use sys::AVFrame;
use sys::*;

/// `AVD3D11VADeviceContext` from libavutil/hwcontext_d3d11va.h (FFmpeg 8.1).
#[repr(C)]
struct AVD3D11VADeviceContext {
    device: *mut c_void,
    device_context: *mut c_void,
    video_device: *mut c_void,
    video_context: *mut c_void,
    lock: Option<unsafe extern "C" fn(*mut c_void)>,
    unlock: Option<unsafe extern "C" fn(*mut c_void)>,
    lock_ctx: *mut c_void,
    bind_flags: u32,
    misc_flags: u32,
}

/// `AVD3D11VAFramesContext` from libavutil/hwcontext_d3d11va.h (FFmpeg 8.1).
#[repr(C)]
struct AVD3D11VAFramesContext {
    texture: *mut c_void,
    bind_flags: u32,
    misc_flags: u32,
    texture_infos: *mut c_void,
}

const EAGAIN: c_int = 11;

fn averror_str(err: c_int) -> String {
    let mut buf = [0i8; 128];
    unsafe {
        av_strerror(err, buf.as_mut_ptr(), buf.len());
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    }
}

/// Lock shared by FFmpeg and our renderer: both use the device's immediate
/// context. Must outlive the decoder.
pub trait DeviceLock: Send + Sync {
    fn lock(&self);
    /// # Safety
    /// Only after a matching `lock`.
    unsafe fn unlock(&self);
}

unsafe extern "C" fn lock_cb(ctx: *mut c_void) {
    let l = unsafe { &*(ctx as *const Box<dyn DeviceLock>) };
    l.lock();
}

unsafe extern "C" fn unlock_cb(ctx: *mut c_void) {
    let l = unsafe { &*(ctx as *const Box<dyn DeviceLock>) };
    unsafe { l.unlock() };
}

unsafe extern "C" fn get_format(ctx: *mut AVCodecContext, fmts: *const AVPixelFormat) -> AVPixelFormat {
    unsafe {
        let mut p = fmts;
        while *p != AVPixelFormat::AV_PIX_FMT_NONE {
            if *p == AVPixelFormat::AV_PIX_FMT_D3D11 {
                // Make our own frame pool so the textures can be bound as
                // shader resources (the default pool is decoder-only).
                let mut frames: *mut AVBufferRef = ptr::null_mut();
                if avcodec_get_hw_frames_parameters(ctx, (*ctx).hw_device_ctx, AVPixelFormat::AV_PIX_FMT_D3D11, &mut frames) < 0 {
                    return AVPixelFormat::AV_PIX_FMT_NONE;
                }
                let fc = (*frames).data as *mut AVHWFramesContext;
                let d3d = (*fc).hwctx as *mut AVD3D11VAFramesContext;
                (*d3d).bind_flags |= (D3D11_BIND_DECODER.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
                (*fc).initial_pool_size += 4;
                if av_hwframe_ctx_init(frames) < 0 {
                    av_buffer_unref(&mut frames);
                    return AVPixelFormat::AV_PIX_FMT_NONE;
                }
                av_buffer_unref(&mut (*ctx).hw_frames_ctx);
                (*ctx).hw_frames_ctx = frames;
                return AVPixelFormat::AV_PIX_FMT_D3D11;
            }
            p = p.add(1);
        }
        tracing::error!("decoder offered no D3D11 output format; hardware HEVC decode unavailable");
        AVPixelFormat::AV_PIX_FMT_NONE
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("FFmpeg setup: {0}")]
    Setup(String),
    #[error("decode: {0}")]
    Decode(String),
}

pub struct HevcDecoder {
    ctx: *mut AVCodecContext,
    pkt: *mut AVPacket,
    frame: *mut AVFrame,
    hw_device: *mut AVBufferRef,
    _lock: Box<Box<dyn DeviceLock>>,
}

unsafe impl Send for HevcDecoder {}

/// A decoded picture on the GPU. Holds an FFmpeg reference to the surface.
pub struct GpuFrame {
    frame: *mut AVFrame,
}

unsafe impl Send for GpuFrame {}

impl GpuFrame {
    /// The decoder's texture array (borrowed; valid while this frame lives).
    pub fn texture(&self) -> &ID3D11Texture2D {
        unsafe {
            let raw = &(*self.frame).data[0];
            ID3D11Texture2D::from_raw_borrowed(&*(raw as *const *mut u8 as *const *mut c_void))
                .expect("D3D11 frame has a texture")
        }
    }

    pub fn slice(&self) -> u32 {
        unsafe { (*self.frame).data[1] as usize as u32 }
    }

    pub fn width(&self) -> u32 {
        unsafe { (*self.frame).width as u32 }
    }

    pub fn height(&self) -> u32 {
        unsafe { (*self.frame).height as u32 }
    }
}

impl Drop for GpuFrame {
    fn drop(&mut self) {
        unsafe { av_frame_free(&mut self.frame) };
    }
}

pub fn quiet_logs() {
    unsafe { av_log_set_level(AV_LOG_ERROR as c_int) };
}

impl HevcDecoder {
    pub fn new(device: &ID3D11Device, lock: Box<dyn DeviceLock>) -> Result<Self, DecodeError> {
        unsafe {
            let codec = avcodec_find_decoder(AVCodecID::AV_CODEC_ID_HEVC);
            if codec.is_null() {
                return Err(DecodeError::Setup("HEVC decoder not built in".into()));
            }
            let lock = Box::new(lock);
            let mut hw = av_hwdevice_ctx_alloc(AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA);
            if hw.is_null() {
                return Err(DecodeError::Setup("av_hwdevice_ctx_alloc failed".into()));
            }
            let devctx = (*hw).data as *mut AVHWDeviceContext;
            let d3d = (*devctx).hwctx as *mut AVD3D11VADeviceContext;
            // FFmpeg releases the device when the context is freed: hand it a reference.
            (*d3d).device = device.clone().into_raw();
            (*d3d).lock = Some(lock_cb);
            (*d3d).unlock = Some(unlock_cb);
            (*d3d).lock_ctx = &*lock as *const Box<dyn DeviceLock> as *mut c_void;
            let r = av_hwdevice_ctx_init(hw);
            if r < 0 {
                av_buffer_unref(&mut hw);
                return Err(DecodeError::Setup(format!("D3D11VA device init: {}", averror_str(r))));
            }
            let ctx = avcodec_alloc_context3(codec);
            (*ctx).hw_device_ctx = av_buffer_ref(hw);
            (*ctx).get_format = Some(get_format);
            (*ctx).flags |= AV_CODEC_FLAG_LOW_DELAY as c_int;
            (*ctx).thread_count = 1;
            (*ctx).thread_type = 0;
            (*ctx).extra_hw_frames = 4;
            let r = avcodec_open2(ctx, codec, ptr::null_mut());
            if r < 0 {
                let mut c = ctx;
                avcodec_free_context(&mut c);
                av_buffer_unref(&mut hw);
                return Err(DecodeError::Setup(format!("avcodec_open2: {}", averror_str(r))));
            }
            Ok(HevcDecoder {
                ctx,
                pkt: av_packet_alloc(),
                frame: av_frame_alloc(),
                hw_device: hw,
                _lock: lock,
            })
        }
    }

    /// Decodes one Annex B access unit. Returns the picture if one came out.
    pub fn decode(&mut self, au: &[u8]) -> Result<Option<GpuFrame>, DecodeError> {
        unsafe {
            (*self.pkt).data = au.as_ptr() as *mut u8;
            (*self.pkt).size = au.len() as c_int;
            let r = avcodec_send_packet(self.ctx, self.pkt);
            (*self.pkt).data = ptr::null_mut();
            (*self.pkt).size = 0;
            if r < 0 && r != -EAGAIN {
                return Err(DecodeError::Decode(averror_str(r)));
            }
            let r = avcodec_receive_frame(self.ctx, self.frame);
            if r == -EAGAIN {
                return Ok(None);
            }
            if r < 0 {
                return Err(DecodeError::Decode(averror_str(r)));
            }
            if (*self.frame).format != AVPixelFormat::AV_PIX_FMT_D3D11 as c_int {
                av_frame_unref(self.frame);
                return Err(DecodeError::Decode("decoder fell back to software output".into()));
            }
            // Move the reference into a fresh AVFrame the caller owns.
            let out = av_frame_alloc();
            av_frame_move_ref(out, self.frame);
            Ok(Some(GpuFrame { frame: out }))
        }
    }

    pub fn flush(&mut self) {
        unsafe { avcodec_flush_buffers(self.ctx) };
    }
}

impl Drop for HevcDecoder {
    fn drop(&mut self) {
        unsafe {
            av_frame_free(&mut self.frame);
            av_packet_free(&mut self.pkt);
            avcodec_free_context(&mut self.ctx);
            av_buffer_unref(&mut self.hw_device);
        }
    }
}
