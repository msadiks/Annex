//! Per-monitor render thread: decode → NV12→RGB (+Sharp recombine) → upscale
//! → present, all on one thread so a frame goes to the screen the moment it
//! is decoded (spec 3.2, 5.3). Cursor-only updates re-present the last frame.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use annex_core::resolution::letterbox;
use annex_core::stats::{Mean, unwrap_ts, us_since};
use annex_proto::control::{ControlMsg, CursorShape, StreamConfig, VideoMode};
use annex_proto::cursor::CursorPos;
use annex_proto::reassembler::Delivery;
use anyhow::{Context, Result, anyhow};
use crossbeam_channel::{Receiver, Sender, bounded, select, unbounded};
use windows::Win32::Foundation::{HANDLE, HWND, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D::{D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Threading::WaitForSingleObjectEx;
use windows::core::Interface;

use crate::ffmpeg::{GpuFrame, HevcDecoder};
use crate::gpu::{Gpu, Params};
use crate::overlay;
use crate::shared::Shared;

/// Show black this long after the sender is gone (spec 3.4).
const NO_SIGNAL: Duration = Duration::from_secs(2);

pub struct FrameMsg {
    pub stream_id: u8,
    pub au: Vec<u8>,
    pub del: Delivery,
}

pub enum PipeMsg {
    Attach(StreamConfig),
    Mode(VideoMode),
    Cursor(CursorPos),
    CursorShape(Arc<CursorShape>),
    ShowStats(bool),
    Quit,
}

#[derive(Clone, Copy)]
struct SendHwnd(isize);
unsafe impl Send for SendHwnd {}

pub struct PipelineHandle {
    pub ctl: Sender<PipeMsg>,
    pub frames: Sender<FrameMsg>,
    join: Option<JoinHandle<()>>,
}

impl PipelineHandle {
    pub fn send(&self, m: PipeMsg) {
        let _ = self.ctl.send(m);
    }
}

impl Drop for PipelineHandle {
    fn drop(&mut self) {
        let _ = self.ctl.send(PipeMsg::Quit);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

pub fn spawn(
    hwnd: HWND,
    panel: (u32, u32),
    gpu: Arc<Gpu>,
    shared: Arc<Shared>,
    recycle: Sender<Vec<u8>>,
) -> PipelineHandle {
    let (ctl_tx, ctl_rx) = unbounded();
    let (f_tx, f_rx) = bounded(4);
    let h = SendHwnd(hwnd.0 as isize);
    let join = std::thread::Builder::new()
        .name("annex-render".into())
        .spawn(move || {
            let hwnd = HWND(h.0 as *mut _);
            match Pipeline::new(hwnd, panel, gpu, shared, recycle) {
                Ok(mut p) => p.run(ctl_rx, f_rx),
                Err(e) => tracing::error!("render thread failed to start: {e:#}"),
            }
        })
        .expect("spawn render thread");
    PipelineHandle { ctl: ctl_tx, frames: f_tx, join: Some(join) }
}

struct Swap {
    chain: IDXGISwapChain2,
    rtv: ID3D11RenderTargetView,
    waitable: HANDLE,
}

struct Stream {
    cfg: StreamConfig,
    decoder: HevcDecoder,
    waiting_idr: bool,
    last_idr_req: Option<Instant>,
    last_good_seq: u32,
}

#[derive(Default)]
struct Counters {
    presented: u32,
    decode: Mean,
    render: Mean,
    e2e: Mean,
    network: Mean,
}

struct Pipeline {
    gpu: Arc<Gpu>,
    shared: Arc<Shared>,
    recycle: Sender<Vec<u8>>,
    panel: (u32, u32),
    swap: Swap,
    stream: Option<Stream>,
    last_frame: Option<GpuFrame>,
    views: Vec<(usize, u32, ID3D11ShaderResourceView, ID3D11ShaderResourceView)>,
    inter: Option<((u32, u32), ID3D11Texture2D, ID3D11RenderTargetView, ID3D11ShaderResourceView)>,
    cursor: Option<CursorPos>,
    shapes: HashMap<u32, (u32, u32, u32, u32, ID3D11ShaderResourceView)>,
    show_stats: bool,
    overlay: Option<(u32, u32, ID3D11ShaderResourceView)>,
    last_present: Instant,
    black: bool,
    counters: Counters,
    last_report: Instant,
}

impl Pipeline {
    fn new(hwnd: HWND, panel: (u32, u32), gpu: Arc<Gpu>, shared: Arc<Shared>, recycle: Sender<Vec<u8>>) -> Result<Self> {
        let swap = create_swapchain(&gpu, hwnd, panel)?;
        let show_stats = shared.show_stats();
        let mut p = Pipeline {
            gpu,
            shared,
            recycle,
            panel,
            swap,
            stream: None,
            last_frame: None,
            views: Vec::new(),
            inter: None,
            cursor: None,
            shapes: HashMap::new(),
            show_stats,
            overlay: None,
            last_present: Instant::now(),
            black: false,
            counters: Counters::default(),
            last_report: Instant::now(),
        };
        p.present_black();
        Ok(p)
    }

    fn run(&mut self, ctl: Receiver<PipeMsg>, frames: Receiver<FrameMsg>) {
        let tick = crossbeam_channel::tick(Duration::from_millis(250));
        loop {
            select! {
                recv(frames) -> f => match f {
                    Ok(f) => self.on_frame(f),
                    Err(_) => return,
                },
                recv(ctl) -> m => match m {
                    Ok(PipeMsg::Quit) | Err(_) => return,
                    Ok(m) => self.on_ctl(m),
                },
                recv(tick) -> _ => self.on_tick(),
            }
        }
    }

    fn on_ctl(&mut self, m: PipeMsg) {
        match m {
            PipeMsg::Attach(cfg) => {
                let id = cfg.stream_id;
                let reuse = self.stream.as_ref().map(|s| s.cfg.stream_id == id).unwrap_or(false);
                if reuse {
                    let s = self.stream.as_mut().unwrap();
                    s.cfg = cfg;
                    s.waiting_idr = true;
                } else {
                    match HevcDecoder::new(&self.gpu.device, self.gpu.decoder_lock()) {
                        Ok(decoder) => {
                            self.stream = Some(Stream { cfg, decoder, waiting_idr: true, last_idr_req: None, last_good_seq: 0 });
                        }
                        Err(e) => {
                            tracing::error!("decoder init failed: {e}");
                            self.shared.send(ControlMsg::Warning { stream_id: Some(id), text: format!("PC can't decode: {e}") });
                            return;
                        }
                    }
                }
                self.last_present = Instant::now();
                self.shared.send(ControlMsg::StreamReady { stream_id: id });
            }
            PipeMsg::Mode(mode) => {
                if let Some(s) = &mut self.stream {
                    s.cfg.mode = mode;
                    s.waiting_idr = true;
                }
            }
            PipeMsg::Cursor(c) => {
                let changed = self.cursor.map(|o| (o.x, o.y, o.visible, o.shape_id) != (c.x, c.y, c.visible, c.shape_id)).unwrap_or(true);
                if self.cursor.map(|o| annex_proto::packet::seq_after(c.seq, o.seq)).unwrap_or(true) {
                    self.cursor = Some(c);
                    if changed && !self.black && self.last_frame.is_some() {
                        self.redraw();
                    }
                }
            }
            PipeMsg::CursorShape(s) => self.add_shape(&s),
            PipeMsg::ShowStats(on) => {
                self.show_stats = on;
                self.overlay = None;
                if !self.black && self.last_frame.is_some() {
                    self.redraw();
                }
            }
            PipeMsg::Quit => {}
        }
    }

    fn on_frame(&mut self, f: FrameMsg) {
        let FrameMsg { stream_id, au, del } = f;
        let Some(s) = &mut self.stream else {
            let _ = self.recycle.try_send(au);
            return;
        };
        if s.cfg.stream_id != stream_id {
            let _ = self.recycle.try_send(au);
            return;
        }
        if s.waiting_idr && !del.idr {
            request_idr(&self.shared, s);
            let _ = self.recycle.try_send(au);
            return;
        }
        let t0 = Instant::now();
        let result = s.decoder.decode(&au);
        let _ = self.recycle.try_send(au);
        match result {
            Ok(Some(frame)) => {
                s.waiting_idr = false;
                s.last_good_seq = del.frame_seq;
                let t1 = Instant::now();
                self.counters.decode.add((t1 - t0).as_secs_f64() * 1e3);
                self.counters.network.add((del.completed_at - del.first_packet_at).as_secs_f64() * 1e3);
                self.last_frame = Some(frame);
                if let Err(e) = self.draw_and_present() {
                    tracing::warn!("render failed: {e:#}");
                    return;
                }
                let t2 = Instant::now();
                self.counters.render.add((t2 - t1).as_secs_f64() * 1e3);
                self.counters.presented += 1;
                if let Some(offset) = self.shared.clock_offset_us() {
                    let now_us = us_since(self.shared.epoch, t2) as i64;
                    let cap_sender = unwrap_ts(del.capture_ts, (now_us + offset).max(0) as u64) as i64;
                    let e2e = (now_us - (cap_sender - offset)) as f64 / 1e3;
                    if (0.0..2000.0).contains(&e2e) {
                        self.counters.e2e.add(e2e);
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::debug!("decode error on stream {stream_id}: {e}");
                s.decoder.flush();
                s.waiting_idr = true;
                request_idr(&self.shared, s);
            }
        }
    }

    fn on_tick(&mut self) {
        let now = Instant::now();
        // A static Mac screen sends no frames; only a lost sender means no signal.
        if !self.black && now.duration_since(self.last_present) > NO_SIGNAL && !self.shared.has_session() {
            self.last_frame = None;
            self.present_black();
        }
        if now.duration_since(self.last_report) >= Duration::from_secs(1) {
            let secs = now.duration_since(self.last_report).as_secs_f64();
            self.last_report = now;
            let mut c = std::mem::take(&mut self.counters);
            if let Some(s) = &self.stream {
                self.shared.report_render(
                    s.cfg.stream_id,
                    (c.presented as f64 / secs) as f32,
                    c.presented,
                    c.decode.take(),
                    c.render.take(),
                    c.e2e.take(),
                    c.network.take(),
                    self.panel,
                );
            }
            if self.show_stats {
                self.overlay = None;
                if !self.black && self.last_frame.is_some() {
                    self.redraw();
                }
            }
        }
    }

    fn add_shape(&mut self, s: &CursorShape) {
        if s.width == 0 || s.height == 0 || s.rgba.len() != s.width as usize * s.height as usize * 4 {
            return;
        }
        // Straight RGBA → premultiplied BGRA.
        let mut bgra = vec![0u8; s.rgba.len()];
        for (o, i) in bgra.chunks_exact_mut(4).zip(s.rgba.chunks_exact(4)) {
            let a = i[3] as u32;
            o[0] = (i[2] as u32 * a / 255) as u8;
            o[1] = (i[1] as u32 * a / 255) as u8;
            o[2] = (i[0] as u32 * a / 255) as u8;
            o[3] = i[3];
        }
        let _g = self.gpu.lock.guard();
        if let Ok((_, srv)) = self.gpu.upload_texture(s.width as u32, s.height as u32, &bgra) {
            if self.shapes.len() > 64 {
                self.shapes.clear();
            }
            self.shapes.insert(s.shape_id, (s.width as u32, s.height as u32, s.hot_x as u32, s.hot_y as u32, srv));
        }
    }

    fn redraw(&mut self) {
        if let Err(e) = self.draw_and_present() {
            tracing::warn!("redraw failed: {e:#}");
        }
    }

    fn views_for(&mut self, frame: &GpuFrame) -> Result<(ID3D11ShaderResourceView, ID3D11ShaderResourceView)> {
        let key = frame.texture().as_raw() as usize;
        let slice = frame.slice();
        if let Some(v) = self.views.iter().find(|v| v.0 == key && v.1 == slice) {
            return Ok((v.2.clone(), v.3.clone()));
        }
        if self.views.len() >= 48 {
            self.views.clear();
        }
        let (y, uv) = self.gpu.nv12_views(frame.texture(), slice)?;
        self.views.push((key, slice, y.clone(), uv.clone()));
        Ok((y, uv))
    }

    fn draw_and_present(&mut self) -> Result<()> {
        let Some(frame) = self.last_frame.take() else { return Ok(()) };
        let r = self.draw(&frame);
        self.last_frame = Some(frame);
        r?;
        self.present()
    }

    fn draw(&mut self, frame: &GpuFrame) -> Result<()> {
        let Some(cfg) = self.stream.as_ref().map(|s| s.cfg.clone()) else { return Ok(()) };
        let (sw, sh) = (cfg.stream_w, cfg.stream_h);
        let sharp = cfg.mode == VideoMode::Sharp && frame.width() == sw * 2;
        let (pw, ph) = self.panel;
        unsafe { WaitForSingleObjectEx(self.swap.waitable, 50, true) };

        let (vy, vuv) = self.views_for(frame)?;
        let mut tex_desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { frame.texture().GetDesc(&mut tex_desc) };
        let uv_scale = [
            frame.width() as f32 / tex_desc.Width as f32,
            frame.height() as f32 / tex_desc.Height as f32,
        ];
        let direct = (sw, sh) == (pw, ph);
        if !direct && self.inter.as_ref().map(|i| i.0 != (sw, sh)).unwrap_or(true) {
            let _g = self.gpu.lock.guard();
            let (t, rtv, srv) = self.gpu.render_texture(sw, sh)?;
            self.inter = Some(((sw, sh), t, rtv, srv));
        }
        let (cx, cy, cw, ch) = letterbox(sw, sh, pw, ph);

        let g = &self.gpu;
        let _guard = g.lock.guard();
        let ctx = &g.context;
        unsafe {
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.OMSetBlendState(None, None, 0xffff_ffff);
            ctx.VSSetShader(&g.vs_full, None);
            ctx.PSSetSamplers(0, Some(&[Some(g.linear.clone()), Some(g.point.clone())]));

            // Pass 1: NV12 → RGB at stream size.
            let target = if direct { self.swap.rtv.clone() } else { self.inter.as_ref().unwrap().2.clone() };
            ctx.OMSetRenderTargets(Some(&[Some(target)]), None);
            ctx.RSSetViewports(Some(&[viewport(0, 0, sw, sh)]));
            ctx.PSSetShader(if sharp { &g.ps_sharp } else { &g.ps_fast }, None);
            ctx.PSSetShaderResources(0, Some(&[Some(vy), Some(vuv)]));
            g.set_params(&Params { uv_scale, ..Default::default() });
            ctx.Draw(3, 0);
            ctx.PSSetShaderResources(0, Some(&[None, None]));

            // Pass 2: bicubic upscale into the letterbox rect.
            if !direct {
                ctx.OMSetRenderTargets(Some(&[Some(self.swap.rtv.clone())]), None);
                if (cw, ch) != (pw, ph) {
                    ctx.ClearRenderTargetView(&self.swap.rtv, &[0.0, 0.0, 0.0, 1.0]);
                }
                ctx.RSSetViewports(Some(&[viewport(cx, cy, cw, ch)]));
                ctx.PSSetShader(&g.ps_bicubic, None);
                ctx.PSSetShaderResources(2, Some(&[Some(self.inter.as_ref().unwrap().3.clone())]));
                g.set_params(&Params { src_size: [sw as f32, sh as f32], ..Default::default() });
                ctx.Draw(3, 0);
                ctx.PSSetShaderResources(2, Some(&[None]));
            }

            // Client-side cursor.
            if let Some(c) = self.cursor.filter(|c| c.visible) {
                if let Some((w, h, hx, hy, srv)) = self.shapes.get(&c.shape_id) {
                    let scale = cw as f32 / sw as f32;
                    let x0 = cx as f32 + (c.x as f32 - *hx as f32) * scale;
                    let y0 = cy as f32 + (c.y as f32 - *hy as f32) * scale;
                    self.quad(srv, x0, y0, *w as f32 * scale, *h as f32 * scale);
                }
            }

            // Stats overlay.
            if self.show_stats {
                if self.overlay.is_none() {
                    let text = self.shared.overlay_text(cfg.stream_id, &cfg, self.panel);
                    let (w, h, px) = overlay::render(&text, 2);
                    if let Ok((_, srv)) = g.upload_texture(w, h, &px) {
                        self.overlay = Some((w, h, srv));
                    }
                }
                if let Some((w, h, srv)) = &self.overlay {
                    self.quad(srv, 16.0, 16.0, *w as f32, *h as f32);
                }
            }
        }
        Ok(())
    }

    /// Draws a premultiplied texture at panel pixel rect. Call under the lock.
    fn quad(&self, srv: &ID3D11ShaderResourceView, x: f32, y: f32, w: f32, h: f32) {
        let g = &self.gpu;
        let (pw, ph) = (self.panel.0 as f32, self.panel.1 as f32);
        let rect = [x / pw * 2.0 - 1.0, 1.0 - y / ph * 2.0, (x + w) / pw * 2.0 - 1.0, 1.0 - (y + h) / ph * 2.0];
        unsafe {
            let ctx = &g.context;
            ctx.OMSetRenderTargets(Some(&[Some(self.swap.rtv.clone())]), None);
            ctx.RSSetViewports(Some(&[viewport(0, 0, self.panel.0, self.panel.1)]));
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            ctx.VSSetShader(&g.vs_quad, None);
            ctx.PSSetShader(&g.ps_tex, None);
            ctx.OMSetBlendState(&g.blend_premul, None, 0xffff_ffff);
            ctx.PSSetShaderResources(2, Some(&[Some(srv.clone())]));
            g.set_params(&Params { rect, ..Default::default() });
            ctx.Draw(4, 0);
            ctx.PSSetShaderResources(2, Some(&[None]));
            ctx.OMSetBlendState(None, None, 0xffff_ffff);
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&g.vs_full, None);
        }
    }

    fn present(&mut self) -> Result<()> {
        let _g = self.gpu.lock.guard();
        let r = unsafe {
            if self.gpu.tearing {
                self.swap.chain.Present(0, DXGI_PRESENT_ALLOW_TEARING)
            } else {
                self.swap.chain.Present(1, DXGI_PRESENT(0))
            }
        };
        r.ok().context("Present")?;
        self.last_present = Instant::now();
        self.black = false;
        Ok(())
    }

    fn present_black(&mut self) {
        {
            let _g = self.gpu.lock.guard();
            unsafe { self.gpu.context.ClearRenderTargetView(&self.swap.rtv, &[0.0, 0.0, 0.0, 1.0]) };
        }
        let _ = self.present();
        self.black = true;
    }
}

fn request_idr(shared: &Shared, s: &mut Stream) {
    let now = Instant::now();
    if s.last_idr_req.map(|t| now.duration_since(t) < Duration::from_millis(250)).unwrap_or(false) {
        return;
    }
    s.last_idr_req = Some(now);
    shared.send(ControlMsg::IdrRequest { stream_id: s.cfg.stream_id, last_good_seq: s.last_good_seq });
}

fn viewport(x: u32, y: u32, w: u32, h: u32) -> D3D11_VIEWPORT {
    D3D11_VIEWPORT { TopLeftX: x as f32, TopLeftY: y as f32, Width: w as f32, Height: h as f32, MinDepth: 0.0, MaxDepth: 1.0 }
}

fn create_swapchain(gpu: &Gpu, hwnd: HWND, (w, h): (u32, u32)) -> Result<Swap> {
    unsafe {
        let mut flags = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32;
        if gpu.tearing {
            flags |= DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32;
        }
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: w,
            Height: h,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_NONE,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: flags,
        };
        let _g = gpu.lock.guard();
        let chain1 = gpu
            .factory
            .CreateSwapChainForHwnd(&gpu.device, hwnd, &desc, None, None)
            .context("CreateSwapChainForHwnd")?;
        let _ = gpu.factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER | DXGI_MWA_NO_WINDOW_CHANGES);
        let chain: IDXGISwapChain2 = chain1.cast()?;
        chain.SetMaximumFrameLatency(1)?;
        let waitable = chain.GetFrameLatencyWaitableObject();
        let back: ID3D11Texture2D = chain.GetBuffer(0)?;
        let mut rtv = None;
        gpu.device.CreateRenderTargetView(&back, None, Some(&mut rtv))?;
        let rtv = rtv.ok_or_else(|| anyhow!("no back buffer view"))?;
        // The first wait is required before the first frame.
        if WaitForSingleObjectEx(waitable, 100, true) != WAIT_OBJECT_0 {
            tracing::debug!("initial frame-latency wait timed out");
        }
        Ok(Swap { chain, rtv, waitable })
    }
}
