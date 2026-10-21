//! Client-side cursor (spec phase 3): the cursor is left out of the video and
//! sent as position updates plus occasional images, so it moves at once on
//! the PC regardless of video latency.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use annex_core::identity::fnv1a64;
use annex_proto::control::CursorShape;
use annex_sender::CursorSink;
use objc2::rc::autoreleasepool;
use objc2::runtime::AnyObject;
use objc2::msg_send;
use objc2_app_kit::NSCursor;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGDisplayBounds, CGEvent};

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
    fn CGColorSpaceRelease(cs: *mut c_void);
    fn CGBitmapContextCreate(data: *mut c_void, w: usize, h: usize, bpc: usize, bpr: usize, cs: *mut c_void, info: u32) -> *mut c_void;
    fn CGContextDrawImage(ctx: *mut c_void, rect: CGRect, image: *mut c_void);
    fn CGContextRelease(ctx: *mut c_void);
}

/// kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big → RGBA bytes.
const RGBA_PREMULTIPLIED: u32 = 1 | (4 << 12);

#[derive(Clone, Copy, PartialEq)]
pub struct Target {
    pub display_id: u32,
    pub width: u32,
    pub height: u32,
}

pub struct CursorTracker {
    stop: Arc<AtomicBool>,
    targets: Arc<Mutex<HashMap<u8, Target>>>,
}

impl CursorTracker {
    pub fn start(sink: CursorSink, targets: HashMap<u8, Target>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let targets = Arc::new(Mutex::new(targets));
        let (s, t) = (stop.clone(), targets.clone());
        std::thread::Builder::new()
            .name("annex-cursor".into())
            .spawn(move || run(sink, s, t))
            .expect("cursor thread");
        CursorTracker { stop, targets }
    }

    pub fn set_targets(&self, targets: HashMap<u8, Target>) {
        *self.targets.lock().unwrap() = targets;
    }
}

impl Drop for CursorTracker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn run(sink: CursorSink, stop: Arc<AtomicBool>, targets: Arc<Mutex<HashMap<u8, Target>>>) {
    let mut last: HashMap<u8, (i32, i32, bool, u32)> = HashMap::new();
    let mut shape_id = 0u32;
    let mut tick = 0u32;
    while !stop.load(Ordering::Relaxed) {
        if tick % 4 == 0
            && let Some(shape) = current_shape()
            && shape.shape_id != shape_id
        {
            shape_id = shape.shape_id;
            sink.shape(shape);
        }
        tick = tick.wrapping_add(1);
        let p = CGEvent::location(CGEvent::new(None).as_deref());
        let targets = targets.lock().unwrap().clone();
        last.retain(|id, _| targets.contains_key(id));
        for (id, t) in &targets {
            let b = CGDisplayBounds(t.display_id);
            let inside = p.x >= b.origin.x
                && p.y >= b.origin.y
                && p.x < b.origin.x + b.size.width
                && p.y < b.origin.y + b.size.height;
            let x = ((p.x - b.origin.x) * t.width as f64 / b.size.width.max(1.0)) as i32;
            let y = ((p.y - b.origin.y) * t.height as f64 / b.size.height.max(1.0)) as i32;
            let now = (x, y, inside, shape_id);
            let prev = last.get(id).copied();
            // Only send changes; hidden only once when leaving the display.
            if prev != Some(now) && (inside || prev.map(|p| p.2).unwrap_or(true)) {
                sink.position(*id, x, y, inside, shape_id);
                last.insert(*id, now);
            }
        }
        std::thread::sleep(Duration::from_millis(8));
    }
}

/// The system cursor as straight RGBA at its point size.
fn current_shape() -> Option<CursorShape> {
    autoreleasepool(|_| unsafe {
        // Deprecated but still the only way to see another app's cursor;
        // fall back to the arrow so the PC always has something to draw.
        #[allow(deprecated)]
        let cursor = NSCursor::currentSystemCursor().unwrap_or_else(NSCursor::arrowCursor);
        let image = cursor.image();
        let size = image.size();
        let hot = cursor.hotSpot();
        let (w, h) = (size.width.round() as usize, size.height.round() as usize);
        if w == 0 || h == 0 || w > 256 || h > 256 {
            return None;
        }
        let mut rect = CGRect { origin: CGPoint { x: 0.0, y: 0.0 }, size: CGSize { width: w as f64, height: h as f64 } };
        let img: *mut c_void = msg_send![&*image, CGImageForProposedRect: &mut rect, context: std::ptr::null::<AnyObject>(), hints: std::ptr::null::<AnyObject>()];
        if img.is_null() {
            return None;
        }
        let mut rgba = vec![0u8; w * h * 4];
        let cs = CGColorSpaceCreateDeviceRGB();
        let ctx = CGBitmapContextCreate(rgba.as_mut_ptr().cast(), w, h, 8, w * 4, cs, RGBA_PREMULTIPLIED);
        CGColorSpaceRelease(cs);
        if ctx.is_null() {
            return None;
        }
        CGContextDrawImage(ctx, rect, img);
        CGContextRelease(ctx);
        for px in rgba.chunks_exact_mut(4) {
            let a = px[3] as u32;
            if a > 0 && a < 255 {
                for c in &mut px[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        let (hx, hy) = (hot.x.max(0.0) as u16, hot.y.max(0.0) as u16);
        let id = fnv1a64(&[&rgba, &hx.to_be_bytes(), &hy.to_be_bytes()]) as u32 | 1;
        Some(CursorShape { shape_id: id, width: w as u16, height: h as u16, hot_x: hx, hot_y: hy, rgba })
    })
}
