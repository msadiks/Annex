//! Metal colour packing (spec 4.1, 4.2, 6.3): BGRA capture → NV12 encoder
//! input, entirely on the GPU. Kernels are compiled at startup from source.

use std::ptr::NonNull;
use std::sync::Mutex;

use anyhow::{Context, anyhow, bail};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::CFRetained;
use objc2_core_video::{CVMetalTexture, CVMetalTextureCache, CVMetalTextureGetTexture, CVPixelBuffer};
use objc2_foundation::NSString;
use objc2_metal::{
    MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLComputeCommandEncoder, MTLComputePipelineState,
    MTLCreateSystemDefaultDevice, MTLDevice, MTLLibrary, MTLPixelFormat, MTLSize, MTLTexture,
};

use annex_proto::control::VideoMode;

const SHADERS: &str = r#"
#include <metal_stdlib>
using namespace metal;

// BT.709, full range (spec 3.6 / 6.3).
static float3 ycc(float3 c) {
    float y = dot(c, float3(0.2126, 0.7152, 0.0722));
    return float3(y, (c.b - y) / 1.8556 + 0.5, (c.r - y) / 1.5748 + 0.5);
}

// One thread per 2x2 block: four lumas, chroma = mean of the block.
kernel void pack_fast(texture2d<float, access::read> src [[texture(0)]],
                      texture2d<float, access::write> luma [[texture(1)]],
                      texture2d<float, access::write> chroma [[texture(2)]],
                      uint2 gid [[thread_position_in_grid]]) {
    if (gid.x >= chroma.get_width() || gid.y >= chroma.get_height()) return;
    float2 cbcr = float2(0.0);
    for (uint dy = 0; dy < 2; dy++) {
        for (uint dx = 0; dx < 2; dx++) {
            uint2 p = gid * 2 + uint2(dx, dy);
            float3 c = ycc(src.read(p).rgb);
            luma.write(float4(c.x), p);
            cbcr += c.yz;
        }
    }
    chroma.write(float4(cbcr * 0.25, 0.0, 0.0), gid);
}

// One thread per source pixel: its luma fills a 2x2 block of the doubled
// frame, its own chroma goes to the matching chroma sample.
kernel void pack_sharp(texture2d<float, access::read> src [[texture(0)]],
                       texture2d<float, access::write> luma [[texture(1)]],
                       texture2d<float, access::write> chroma [[texture(2)]],
                       uint2 gid [[thread_position_in_grid]]) {
    if (gid.x >= src.get_width() || gid.y >= src.get_height()) return;
    float3 c = ycc(src.read(gid).rgb);
    uint2 b = gid * 2;
    luma.write(float4(c.x), b);
    luma.write(float4(c.x), b + uint2(1, 0));
    luma.write(float4(c.x), b + uint2(0, 1));
    luma.write(float4(c.x), b + uint2(1, 1));
    chroma.write(float4(c.y, c.z, 0.0, 0.0), gid);
}
"#;

type Texture = Retained<ProtocolObject<dyn MTLTexture>>;

pub struct Packer {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    sharp: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    fast: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    cache: CFRetained<CVMetalTextureCache>,
}

// Metal devices, queues, pipeline states and texture caches are thread-safe.
unsafe impl Send for Packer {}
unsafe impl Sync for Packer {}

impl Packer {
    pub fn new() -> anyhow::Result<Self> {
        let device = MTLCreateSystemDefaultDevice().context("no Metal device")?;
        let lib = device
            .newLibraryWithSource_options_error(&NSString::from_str(SHADERS), None)
            .map_err(|e| anyhow!("Metal shader compile: {}", e.localizedDescription()))?;
        let pipeline = |name: &str| -> anyhow::Result<_> {
            let f = lib.newFunctionWithName(&NSString::from_str(name)).with_context(|| format!("kernel {name}"))?;
            device
                .newComputePipelineStateWithFunction_error(&f)
                .map_err(|e| anyhow!("Metal pipeline {name}: {}", e.localizedDescription()))
        };
        let sharp = pipeline("pack_sharp")?;
        let fast = pipeline("pack_fast")?;
        let queue = device.newCommandQueue().context("Metal command queue")?;
        let mut cache: *mut CVMetalTextureCache = std::ptr::null_mut();
        let rc = unsafe { CVMetalTextureCache::create(None, None, &device, None, NonNull::from(&mut cache)) };
        if rc != 0 || cache.is_null() {
            bail!("CVMetalTextureCacheCreate failed ({rc})");
        }
        let cache = unsafe { CFRetained::from_raw(NonNull::new_unchecked(cache)) };
        Ok(Packer { device, queue, sharp, fast, cache })
    }

    fn texture(
        &self,
        buf: &CVPixelBuffer,
        format: MTLPixelFormat,
        w: usize,
        h: usize,
        plane: usize,
    ) -> anyhow::Result<(CFRetained<CVMetalTexture>, Texture)> {
        let mut out: *mut CVMetalTexture = std::ptr::null_mut();
        let rc = unsafe {
            CVMetalTextureCache::create_texture_from_image(None, &self.cache, buf, None, format, w, h, plane, NonNull::from(&mut out))
        };
        if rc != 0 || out.is_null() {
            bail!("CVMetalTextureCacheCreateTextureFromImage failed ({rc})");
        }
        let cv = unsafe { CFRetained::from_raw(NonNull::new_unchecked(out)) };
        let tex = CVMetalTextureGetTexture(&cv).context("no Metal texture for pixel buffer")?;
        Ok((cv, tex))
    }

    /// Packs `src` (BGRA, `w × h`) into `dst` (NV12, `w × h` for Fast or
    /// `2w × 2h` for Sharp). `done` runs on a Metal thread when the GPU is
    /// finished with both buffers.
    pub fn pack(
        &self,
        mode: VideoMode,
        src: &CVPixelBuffer,
        w: usize,
        h: usize,
        dst: &CVPixelBuffer,
        done: impl FnOnce() + Send + 'static,
    ) -> anyhow::Result<()> {
        let (dw, dh) = match mode {
            VideoMode::Sharp => (w * 2, h * 2),
            VideoMode::Fast => (w, h),
        };
        let s = self.texture(src, MTLPixelFormat::BGRA8Unorm, w, h, 0)?;
        let y = self.texture(dst, MTLPixelFormat::R8Unorm, dw, dh, 0)?;
        let c = self.texture(dst, MTLPixelFormat::RG8Unorm, dw / 2, dh / 2, 1)?;
        let cb = self.queue.commandBuffer().context("Metal command buffer")?;
        let enc = cb.computeCommandEncoder().context("Metal compute encoder")?;
        let (pso, grid) = match mode {
            VideoMode::Sharp => (&self.sharp, (w, h)),
            VideoMode::Fast => (&self.fast, (dw / 2, dh / 2)),
        };
        enc.setComputePipelineState(pso);
        unsafe {
            enc.setTexture_atIndex(Some(&s.1), 0);
            enc.setTexture_atIndex(Some(&y.1), 1);
            enc.setTexture_atIndex(Some(&c.1), 2);
        }
        let tg = MTLSize { width: 16, height: 16, depth: 1 };
        let groups = MTLSize { width: grid.0.div_ceil(16), height: grid.1.div_ceil(16), depth: 1 };
        enc.dispatchThreadgroups_threadsPerThreadgroup(groups, tg);
        enc.endEncoding();
        // The CoreVideo textures must live until the GPU is done.
        let keep = Mutex::new(Some((s, y, c, done)));
        let handler = RcBlock::new(move |_cb: NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
            if let Some((_s, _y, _c, done)) = keep.lock().unwrap().take() {
                done();
            }
        });
        unsafe { cb.addCompletedHandler(RcBlock::as_ptr(&handler)) };
        cb.commit();
        let _ = &self.device;
        Ok(())
    }
}
