//! ScreenCaptureKit display capture (spec 6.2).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use block2::RcBlock;
use crossbeam_channel::bounded;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send, sel};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::kCGColorSpaceSRGB;
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{CVPixelBuffer, kCVPixelFormatType_32BGRA};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSNumber, NSObject, NSString};
use objc2_screen_capture_kit::{
    SCCaptureResolutionType, SCContentFilter, SCDisplay, SCFrameStatus, SCShareableContent, SCStream,
    SCStreamConfiguration, SCStreamDelegate, SCStreamFrameInfoDisplayTime, SCStreamFrameInfoStatus, SCStreamOutput,
    SCStreamOutputType,
};

use crate::sys::{SendBox, instant_from_mach};

/// Called on the capture queue with each new frame and its capture time.
pub type FrameFn = Box<dyn Fn(&CVPixelBuffer, Instant) + Send + Sync>;

pub struct OutputIvars {
    on_frame: FrameFn,
    display_id: u32,
    rate: Mutex<Rate>,
}

/// What ScreenCaptureKit delivers, logged every 10 s while the display is
/// changing.
#[derive(Default)]
struct Rate {
    since: Option<Instant>,
    complete: u32,
    other: u32,
    last_shown: Option<Instant>,
    /// Spacing of the display times of complete frames, in ms.
    min_gap: f64,
    max_gap: f64,
}

impl Rate {
    fn note(&mut self, display_id: u32, complete: bool, shown: Option<Instant>) {
        let now = Instant::now();
        let since = *self.since.get_or_insert(now);
        if complete {
            self.complete += 1;
            if let (Some(a), Some(b)) = (self.last_shown, shown) {
                let gap = b.saturating_duration_since(a).as_secs_f64() * 1e3;
                self.min_gap = if self.complete == 2 { gap } else { self.min_gap.min(gap) };
                self.max_gap = self.max_gap.max(gap);
            }
            self.last_shown = shown;
        } else {
            self.other += 1;
        }
        if now.duration_since(since) >= Duration::from_secs(10) {
            if self.complete >= 10 {
                tracing::info!(
                    display_id,
                    complete = self.complete,
                    other = self.other,
                    min_gap_ms = format_args!("{:.1}", self.min_gap),
                    max_gap_ms = format_args!("{:.1}", self.max_gap),
                    "capture rate (10 s)"
                );
            }
            *self = Rate::default();
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "AnnexCaptureOutput"]
    #[ivars = OutputIvars]
    struct CaptureOutput;

    unsafe impl NSObjectProtocol for CaptureOutput {}

    unsafe impl SCStreamOutput for CaptureOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(&self, _stream: &SCStream, sample: &CMSampleBuffer, ty: SCStreamOutputType) {
            if ty == SCStreamOutputType::Screen {
                self.handle(sample);
            }
        }
    }

    unsafe impl SCStreamDelegate for CaptureOutput {
        #[unsafe(method(stream:didStopWithError:))]
        fn did_stop(&self, _stream: &SCStream, error: &NSError) {
            tracing::warn!("screen capture stopped: {}", error.localizedDescription());
        }
    }
);

impl CaptureOutput {
    fn new(display_id: u32, on_frame: FrameFn) -> Retained<Self> {
        let this = Self::alloc().set_ivars(OutputIvars { on_frame, display_id, rate: Mutex::default() });
        unsafe { msg_send![super(this), init] }
    }

    fn handle(&self, sample: &CMSampleBuffer) {
        unsafe {
            let Some(arr) = sample.sample_attachments_array(false) else { return };
            // CFArray of CFDictionary, toll-free bridged.
            let arr: &NSArray<NSDictionary<NSString, AnyObject>> = &*(CFRetained::as_ptr(&arr).as_ptr() as *const _);
            let Some(info) = arr.firstObject() else { return };
            let number = |key: &NSString| info.objectForKey(key).and_then(|v| v.downcast::<NSNumber>().ok());
            let shown = number(SCStreamFrameInfoDisplayTime).map(|n| instant_from_mach(n.unsignedLongLongValue()));
            let complete = number(SCStreamFrameInfoStatus).map(|n| n.integerValue()) == Some(SCFrameStatus::Complete.0);
            let ivars = self.ivars();
            ivars.rate.lock().unwrap().note(ivars.display_id, complete, shown);
            // Idle/blank frames carry no new pixels (spec 6.2: don't synthesize).
            if !complete {
                return;
            }
            let captured = shown.unwrap_or_else(Instant::now);
            let Some(img) = sample.image_buffer() else { return };
            (self.ivars().on_frame)(&img, captured);
        }
    }
}

fn shareable_content() -> anyhow::Result<Retained<SCShareableContent>> {
    let (tx, rx) = bounded(1);
    let block = RcBlock::new(move |content: *mut SCShareableContent, err: *mut NSError| {
        let r = match unsafe { Retained::retain(content) } {
            Some(c) => Ok(SendBox(c)),
            None => Err(unsafe { err.as_ref() }.map(|e| e.localizedDescription().to_string()).unwrap_or_default()),
        };
        let _ = tx.send(r);
    });
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&block) };
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(c)) => Ok(c.0),
        Ok(Err(e)) => bail!("Screen Recording permission is needed ({e}). Allow Annex in System Settings › Privacy & Security › Screen Recording."),
        Err(_) => bail!("ScreenCaptureKit did not answer"),
    }
}

fn find_display(display_id: u32) -> anyhow::Result<Retained<SCDisplay>> {
    // A new virtual display takes a moment to show up.
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let content = shareable_content()?;
        if let Some(d) = unsafe { content.displays() }.iter().find(|d| unsafe { d.displayID() } == display_id) {
            return Ok(d);
        }
        if Instant::now() > deadline {
            bail!("display {display_id} is not available for capture");
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

pub struct Capture {
    stream: Retained<SCStream>,
    cfg: Retained<SCStreamConfiguration>,
    _output: Retained<CaptureOutput>,
    _queue: DispatchRetained<DispatchQueue>,
}

unsafe impl Send for Capture {}

impl Capture {
    /// Captures `display_id` scaled to `width × height` BGRA at up to 120 fps.
    pub fn start(display_id: u32, width: u32, height: u32, shows_cursor: bool, on_frame: FrameFn) -> anyhow::Result<Self> {
        let display = find_display(display_id)?;
        unsafe {
            let filter = SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &NSArray::new());
            let cfg = SCStreamConfiguration::new();
            cfg.setWidth(width as usize);
            cfg.setHeight(height as usize);
            cfg.setPixelFormat(kCVPixelFormatType_32BGRA);
            // Asking for exactly 1/60 makes SCK skip every other frame when
            // they arrive a little early, which halves a 60 Hz display to
            // ~30 fps. Ask for more and let the backend pace to 60.
            cfg.setMinimumFrameInterval(CMTime::new(1, 120));
            // Room for the frame kept for keyframe resends, one waiting and two
            // being packed, without starving ScreenCaptureKit.
            cfg.setQueueDepth(6);
            cfg.setShowsCursor(shows_cursor);
            cfg.setColorSpaceName(kCGColorSpaceSRGB);
            if cfg.respondsToSelector(sel!(setCaptureResolution:)) {
                cfg.setCaptureResolution(SCCaptureResolutionType::Best);
            }
            if cfg.respondsToSelector(sel!(setPreservesAspectRatio:)) {
                cfg.setPreservesAspectRatio(true);
            }
            let output = CaptureOutput::new(display_id, on_frame);
            let stream = SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &cfg,
                Some(ProtocolObject::from_ref(&*output)),
            );
            let queue = DispatchQueue::new("annex.capture", None);
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    ProtocolObject::from_ref(&*output),
                    SCStreamOutputType::Screen,
                    Some(&queue),
                )
                .map_err(|e| anyhow::anyhow!("adding capture output: {}", e.localizedDescription()))?;
            let (tx, rx) = bounded(1);
            let done = RcBlock::new(move |err: *mut NSError| {
                let _ = tx.send(err.as_ref().map(|e| e.localizedDescription().to_string()));
            });
            stream.startCaptureWithCompletionHandler(Some(&done));
            match rx.recv_timeout(Duration::from_secs(5)).context("screen capture did not start")? {
                None => {}
                Some(e) => bail!("screen capture failed to start: {e}"),
            }
            tracing::info!(display_id, width, height, "capture started");
            Ok(Capture { stream, cfg, _output: output, _queue: queue })
        }
    }
}

impl Capture {
    /// Cursor in the video (off while the PC draws it, spec phase 3).
    pub fn set_shows_cursor(&self, on: bool) {
        unsafe {
            self.cfg.setShowsCursor(on);
            self.stream.updateConfiguration_completionHandler(&self.cfg, None);
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        unsafe { self.stream.stopCaptureWithCompletionHandler(None) };
    }
}
