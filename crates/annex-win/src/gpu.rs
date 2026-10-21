//! One D3D11 device per adapter, shared by decode and render of every monitor
//! on that adapter (spec 7: decode and render on the adapter that owns the
//! output, so frames never cross adapters).

use std::ffi::c_void;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use parking_lot::lock_api::RawMutex as _;
use windows::Win32::Foundation::{HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1, ID3DBlob,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::{BOOL, GUID, Interface, PCSTR, s};

use crate::ffmpeg::DeviceLock;

/// Serializes use of the immediate context between our render threads and
/// FFmpeg's decode submissions.
pub struct GpuLock(parking_lot::RawMutex);

impl GpuLock {
    pub fn guard(&self) -> GpuGuard<'_> {
        self.0.lock();
        GpuGuard(self)
    }
}

pub struct GpuGuard<'a>(&'a GpuLock);

impl Drop for GpuGuard<'_> {
    fn drop(&mut self) {
        unsafe { self.0.0.unlock() };
    }
}

struct SharedLock(Arc<GpuLock>);

impl DeviceLock for SharedLock {
    fn lock(&self) {
        self.0.0.lock();
    }
    unsafe fn unlock(&self) {
        unsafe { self.0.0.unlock() };
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Params {
    pub uv_scale: [f32; 2],
    pub src_size: [f32; 2],
    pub rect: [f32; 4],
}

pub struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub factory: IDXGIFactory2,
    pub lock: Arc<GpuLock>,
    pub tearing: bool,
    pub name: String,
    pub luid: LUID,
    pub hevc_ok: bool,
    pub vs_full: ID3D11VertexShader,
    pub vs_quad: ID3D11VertexShader,
    pub ps_fast: ID3D11PixelShader,
    pub ps_sharp: ID3D11PixelShader,
    pub ps_bicubic: ID3D11PixelShader,
    pub ps_tex: ID3D11PixelShader,
    pub linear: ID3D11SamplerState,
    pub point: ID3D11SamplerState,
    pub blend_premul: ID3D11BlendState,
    pub cbuf: ID3D11Buffer,
}

// D3D11 devices are free-threaded; the immediate context is only touched under `lock`.
unsafe impl Send for Gpu {}
unsafe impl Sync for Gpu {}

const HLSL: &str = include_str!("shaders.hlsl");

/// HEVC Main decode profile GUID (D3D11_DECODER_PROFILE_HEVC_VLD_MAIN).
const HEVC_MAIN: GUID = GUID::from_u128(0x5b11d51b_2f4c_4452_bcc3_09f2a1160cc0);

fn compile(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob> {
    let mut code = None;
    let mut errors = None;
    let r = unsafe {
        D3DCompile(
            HLSL.as_ptr() as *const c_void,
            HLSL.len(),
            s!("shaders.hlsl"),
            None,
            None,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = r {
        let msg = errors
            .map(|b| unsafe {
                String::from_utf8_lossy(std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize())).into_owned()
            })
            .unwrap_or_default();
        return Err(anyhow!("shader compile failed: {e} {msg}"));
    }
    code.ok_or_else(|| anyhow!("shader compiler returned no code"))
}

fn blob_bytes(b: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()) }
}

fn adapter_name(desc: &DXGI_ADAPTER_DESC1) -> String {
    let n = desc.Description.iter().position(|c| *c == 0).unwrap_or(desc.Description.len());
    String::from_utf16_lossy(&desc.Description[..n])
}

impl Gpu {
    /// Creates the device for the adapter with `luid`, or the default adapter.
    pub fn new(luid: Option<LUID>) -> Result<Arc<Gpu>> {
        unsafe {
            let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
            let adapter: IDXGIAdapter1 = match luid {
                Some(l) => factory.EnumAdapterByLuid(l)?,
                None => factory.EnumAdapters1(0)?,
            };
            let desc = adapter.GetDesc1()?;
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .context("D3D11CreateDevice")?;
            let device: ID3D11Device = device.ok_or_else(|| anyhow!("no device"))?;
            let context: ID3D11DeviceContext = context.ok_or_else(|| anyhow!("no context"))?;
            if let Ok(mt) = context.cast::<ID3D11Multithread>() {
                let _ = mt.SetMultithreadProtected(true);
            }

            let dxgi_dev: IDXGIDevice = device.cast()?;
            let factory2: IDXGIFactory2 = dxgi_dev.GetAdapter()?.GetParent()?;
            let mut tearing = BOOL(0);
            let tearing = factory2
                .cast::<IDXGIFactory5>()
                .and_then(|f5| {
                    f5.CheckFeatureSupport(
                        DXGI_FEATURE_PRESENT_ALLOW_TEARING,
                        &mut tearing as *mut BOOL as *mut c_void,
                        std::mem::size_of::<BOOL>() as u32,
                    )
                })
                .is_ok()
                && tearing.as_bool();

            let hevc_ok = check_hevc(&device);

            let vs_full_b = compile(s!("vs_full"), s!("vs_5_0"))?;
            let vs_quad_b = compile(s!("vs_quad"), s!("vs_5_0"))?;
            let mut vs_full = None;
            device.CreateVertexShader(blob_bytes(&vs_full_b), None, Some(&mut vs_full))?;
            let mut vs_quad = None;
            device.CreateVertexShader(blob_bytes(&vs_quad_b), None, Some(&mut vs_quad))?;
            let ps = |entry: PCSTR| -> Result<ID3D11PixelShader> {
                let b = compile(entry, s!("ps_5_0"))?;
                let mut p = None;
                device.CreatePixelShader(blob_bytes(&b), None, Some(&mut p))?;
                p.ok_or_else(|| anyhow!("no pixel shader"))
            };
            let ps_fast = ps(s!("ps_fast"))?;
            let ps_sharp = ps(s!("ps_sharp"))?;
            let ps_bicubic = ps(s!("ps_bicubic"))?;
            let ps_tex = ps(s!("ps_tex"))?;

            let sampler = |filter: D3D11_FILTER| -> Result<ID3D11SamplerState> {
                let d = D3D11_SAMPLER_DESC {
                    Filter: filter,
                    AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                    MaxLOD: f32::MAX,
                    ComparisonFunc: D3D11_COMPARISON_NEVER,
                    ..Default::default()
                };
                let mut s = None;
                device.CreateSamplerState(&d, Some(&mut s))?;
                s.ok_or_else(|| anyhow!("no sampler"))
            };
            let linear = sampler(D3D11_FILTER_MIN_MAG_MIP_LINEAR)?;
            let point = sampler(D3D11_FILTER_MIN_MAG_MIP_POINT)?;

            let mut bd = D3D11_BLEND_DESC::default();
            bd.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: true.into(),
                SrcBlend: D3D11_BLEND_ONE,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            let mut blend = None;
            device.CreateBlendState(&bd, Some(&mut blend))?;

            let cbd = D3D11_BUFFER_DESC {
                ByteWidth: std::mem::size_of::<Params>().next_multiple_of(16) as u32,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            let mut cbuf = None;
            device.CreateBuffer(&cbd, None, Some(&mut cbuf))?;

            let gpu = Gpu {
                device,
                context,
                factory: factory2,
                lock: Arc::new(GpuLock(parking_lot::RawMutex::INIT)),
                tearing,
                name: adapter_name(&desc),
                luid: desc.AdapterLuid,
                hevc_ok,
                vs_full: vs_full.unwrap(),
                vs_quad: vs_quad.unwrap(),
                ps_fast,
                ps_sharp,
                ps_bicubic,
                ps_tex,
                linear,
                point,
                blend_premul: blend.unwrap(),
                cbuf: cbuf.unwrap(),
            };
            tracing::info!(adapter = %gpu.name, tearing = gpu.tearing, hevc = gpu.hevc_ok, "GPU ready");
            Ok(Arc::new(gpu))
        }
    }

    pub fn decoder_lock(&self) -> Box<dyn DeviceLock> {
        Box::new(SharedLock(self.lock.clone()))
    }

    /// Sets constants; call under the lock.
    pub fn set_params(&self, p: &Params) {
        unsafe {
            self.context.UpdateSubresource(&self.cbuf, 0, None, p as *const Params as *const c_void, 0, 0);
            self.context.VSSetConstantBuffers(0, Some(&[Some(self.cbuf.clone())]));
            self.context.PSSetConstantBuffers(0, Some(&[Some(self.cbuf.clone())]));
        }
    }

    /// An RGBA texture usable as render target and shader input.
    pub fn render_texture(&self, w: u32, h: u32) -> Result<(ID3D11Texture2D, ID3D11RenderTargetView, ID3D11ShaderResourceView)> {
        unsafe {
            let d = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                ..Default::default()
            };
            let mut t = None;
            self.device.CreateTexture2D(&d, None, Some(&mut t))?;
            let t: ID3D11Texture2D = t.unwrap();
            let mut rtv = None;
            self.device.CreateRenderTargetView(&t, None, Some(&mut rtv))?;
            let mut srv = None;
            self.device.CreateShaderResourceView(&t, None, Some(&mut srv))?;
            Ok((t, rtv.unwrap(), srv.unwrap()))
        }
    }

    /// A premultiplied BGRA texture filled from CPU memory (overlay, cursor).
    pub fn upload_texture(&self, w: u32, h: u32, bgra: &[u8]) -> Result<(ID3D11Texture2D, ID3D11ShaderResourceView)> {
        unsafe {
            let d = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                ..Default::default()
            };
            let init = D3D11_SUBRESOURCE_DATA {
                pSysMem: bgra.as_ptr() as *const c_void,
                SysMemPitch: w * 4,
                SysMemSlicePitch: 0,
            };
            let mut t = None;
            self.device.CreateTexture2D(&d, Some(&init), Some(&mut t))?;
            let t: ID3D11Texture2D = t.unwrap();
            let mut srv = None;
            self.device.CreateShaderResourceView(&t, None, Some(&mut srv))?;
            Ok((t, srv.unwrap()))
        }
    }

    /// Luma (R8) and chroma (R8G8) views of one slice of a decoder texture array.
    pub fn nv12_views(&self, tex: &ID3D11Texture2D, slice: u32) -> Result<(ID3D11ShaderResourceView, ID3D11ShaderResourceView)> {
        unsafe {
            let mk = |fmt: DXGI_FORMAT| -> Result<ID3D11ShaderResourceView> {
                let d = D3D11_SHADER_RESOURCE_VIEW_DESC {
                    Format: fmt,
                    ViewDimension: windows::Win32::Graphics::Direct3D::D3D11_SRV_DIMENSION_TEXTURE2DARRAY,
                    Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                        Texture2DArray: D3D11_TEX2D_ARRAY_SRV {
                            MostDetailedMip: 0,
                            MipLevels: 1,
                            FirstArraySlice: slice,
                            ArraySize: 1,
                        },
                    },
                };
                let mut v = None;
                self.device.CreateShaderResourceView(tex, Some(&d), Some(&mut v))?;
                v.ok_or_else(|| anyhow!("no view"))
            };
            Ok((mk(DXGI_FORMAT_R8_UNORM)?, mk(DXGI_FORMAT_R8G8_UNORM)?))
        }
    }
}

fn check_hevc(device: &ID3D11Device) -> bool {
    unsafe {
        let Ok(vd) = device.cast::<ID3D11VideoDevice>() else { return false };
        let n = vd.GetVideoDecoderProfileCount();
        for i in 0..n {
            if let Ok(p) = vd.GetVideoDecoderProfile(i) {
                if p == HEVC_MAIN {
                    return vd.CheckVideoDecoderFormat(&p, DXGI_FORMAT_NV12).map(|b| b.as_bool()).unwrap_or(false);
                }
            }
        }
        false
    }
}
