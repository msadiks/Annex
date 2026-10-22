# Annex — Handoff Spec for the Coding Agent

**Annex** turns a Windows PC's monitors into wireless external displays for a Mac. The guiding rule for every design decision: **behave exactly like a cable would, minus the cable.**

This document is the single source of truth for the project. Read it fully before writing code. When something here conflicts with a library's README or your intuition, follow this document and raise the conflict in your notes.

---

## 0. How to work on this project

- Work **phase by phase** (section 12). Do not start a phase before the previous one's acceptance criteria are met or explicitly waived by the human.
- You cannot run the GPU/capture pipeline yourself. For each phase, produce a short **manual test checklist** the human runs on real hardware (Apple Silicon Mac + Windows 11 PC) and ask for the numbers. Unit-test everything that is platform-independent (`annex-proto`, `annex-core`) so it runs in CI.
- **Measure before optimizing.** Every pipeline stage logs its own timing (section 10). Latency numbers decide the design; opinions don't.
- Keep PRs/commits small and single-purpose. Commit messages explain *why*.
- Do **not** copy code from GPL projects (Sunshine, Moonlight, moonlight-common-c). Use them as design references only. Our license is MIT OR Apache-2.0.
- Use the versions of crates on crates.io at the time of writing code; verify API names against current docs. Names in this document were correct at spec time but bindings crates move fast.

---

## 1. Goals and non-goals

### Goals
- Mac (Apple Silicon, macOS 26 and 27) streams to **one** Windows 11 x64 PC.
- That PC can have multiple monitors; Annex drives **up to 3** of them as independent Mac displays.
- Two source modes per monitor: **Extend** (a new virtual display) and **Mirror** (an existing Mac display).
- Video quality target: **1:1 with a cable** for still content, visually lossless in motion. Height capped at **1080**, any width (ultrawide supported).
- **60 fps**, lowest achievable glass-to-glass latency (target **≤ 35 ms** on Ethernet, **≤ 50 ms** on good 5/6 GHz Wi-Fi).
- Works over **Wi-Fi or Ethernet**; both machines on the same LAN is the only requirement.
- Works on **NVIDIA and AMD** GPUs (Intel should work via the same path but is untested).
- Very light: idle CPU near zero, no runtime dependencies beyond GPU drivers.
- Self-use + open source. **No App Store** constraints; private APIs and workarounds are allowed when they work.

### Non-goals (v1)
- Audio. Input from the Windows side (keyboard/mouse). Internet/relay streaming. HDR / 10-bit. Intel Macs. Windows on ARM. More than one receiver PC. Touch.

---

## 2. Platform & language decisions (final)

| | Mac (sender) | Windows (receiver) |
|---|---|---|
| Language | Rust | Rust |
| App shell | Tauri 2, **tray-only** (no dock icon), settings window only. Built to `.dmg`. | Pure Rust single `.exe`, tray icon + menu. **No WebView2.** |
| Virtual display | Private CoreGraphics `CGVirtualDisplay*` classes via `objc2` | n/a |
| Capture | ScreenCaptureKit (`SCStream`). **Never** `CGDisplayStream`. | n/a |
| Color pack | Metal compute kernel (BGRA → NV12, zero-copy from capture IOSurface) | Pixel shader (NV12 → RGB, Sharp recombine, upscale) |
| Codec | VideoToolbox HEVC Main (8-bit 4:2:0), hardware, real-time | FFmpeg `libavcodec` HEVC decoder + `d3d11va` hwaccel, statically linked, trimmed build |
| Present | n/a | D3D11 flip-model swapchain, tearing allowed, waitable, max latency 1 |
| Transport | Custom UDP (video) + TCP (control), shared crate | same crate |
| Discovery | mDNS (`mdns-sd`) | mDNS (`mdns-sd`) |
| Crypto | Noise IK handshake (`snow`), ChaCha20-Poly1305 per packet | same |
| Async | **No tokio on the hot path.** Dedicated OS threads. | same |

---

## 3. The cable rules

Every behavior derives from "what would happen with a cable?"

### 3.1 Resolution rule
The receiver reports each monitor's **current desktop mode** (width, height, refresh) as if it were EDID.

```
stream_h = min(native_h, 1080)
stream_w = round_to_even(native_w * stream_h / native_h)
stream_h = round_to_even(stream_h)
```

Examples: 1920×1080 → 1920×1080 · 1366×768 → 1366×768 · 2560×1440 → 1920×1080 · 3440×1440 → 2580×1080 · 5120×1440 → 3840×1080 · 1920×1200 → 1728×1080.

- Extend mode: the virtual display is created at exactly `stream_w × stream_h`, **60 Hz, one mode only, hiDPI off**. (See 6.2 for why only one mode.)
- The receiver scales the stream to fill the panel (aspect is preserved by construction). Use a good upscale filter (bicubic or Lanczos) when sizes differ; blit directly when equal.
- Mirror mode: capture the chosen Mac display, scale to fit the rule above (ScreenCaptureKit can scale during capture). If aspect ratios differ, the receiver letterboxes with black.

### 3.2 Refresh
Virtual displays are 60 Hz. The receiver presents each frame **the moment it is decoded** (sync interval 0, tearing allowed). A 120/144 Hz panel therefore reduces latency; it never causes judder handling logic on our side.

### 3.3 Plug / unplug
- Enabling a monitor in the Windows tray menu = plugging in a cable: within ~1 s the Mac creates the virtual display (or starts mirroring).
- Disabling it, or quitting the receiver, = unplugging: the Mac removes the display.
- Network loss: the Mac keeps the display for a **30 s grace period** (so a Wi-Fi blip doesn't rearrange windows), then removes it.
- Each of the 3 virtual displays has a **fixed, distinct identity** (vendor ID, product ID, serial derived from receiver ID + monitor index) so macOS remembers arrangement/position across sessions, like real monitors. The display name is `"Annex: <Windows monitor name>"`.

### 3.4 Signal loss / sleep
- Receiver shows **black** after 2 s without frames (a monitor with no signal). On resume it just continues.
- Mac sleep → stream stops → black. Mac wake → resumes within the grace period, same display.
- Receiver keeps the Windows display awake while streaming (`SetThreadExecutionState(ES_DISPLAY_REQUIRED | ES_CONTINUOUS)`); releases it when idle.

### 3.5 Identity and security
- Exactly one Mac ↔ one PC. Pairing once with a PIN; afterwards auto-connect whenever both are on the LAN and the receiver has at least one monitor enabled.
- Only private (RFC 1918) and link-local addresses are accepted. No relay. Nothing leaves the LAN.
- Everything on the wire is encrypted and authenticated, even on the LAN (it's cheap).

### 3.6 Color
sRGB content, BT.709 matrix, **full range**, 8-bit. No HDR. Virtual display gets the sRGB color space.

---

## 4. Video modes: Sharp and Fast

Why not HEVC 4:4:4: AMD has no HEVC 4:4:4 hardware decode in any generation, NVIDIA exposes it only through its own vendor API (not D3D11VA), and VideoToolbox has no HEVC 4:4:4 encode profile. So we never rely on it.

Both modes are **plain HEVC Main 8-bit 4:2:0**, decodable through standard D3D11VA on every Win 11-capable GPU.

### 4.1 Sharp (exact 4:4:4 via "doubled 4:2:0")
Pack the `W×H` RGB frame into a `2W×2H` NV12 frame:
- **Luma plane (2W×2H):** `Y'(x, y) = Y(x/2, y/2)` — each source pixel's luma repeated in a 2×2 block.
- **Chroma plane (W×H interleaved CbCr):** `CbCr'(x, y) = CbCr(x, y)` — full-resolution chroma, one sample per source pixel.

Encode as a normal `2W×2H` HEVC Main stream. The decoder sees an ordinary 4:2:0 picture, so chroma motion-vector derivation stays correct.

Receiver recombine shader, per output pixel `p`:
- `Y = bilinear sample of luma at texel (2p.x + 1, 2p.y + 1)` (= average of the 2×2 block; this also averages out codec quantization noise)
- `Cb, Cr = point sample of chroma at p`
- RGB = BT.709 full-range inverse.

Result: pixel-exact 4:4:4 at `W×H`, limited only by codec quantization at high bitrate.

**Eligibility:** `stream_w ≤ 1920` (doubled frame ≤ 3840×2160, within HEVC Level 5.1 picture limits that all target decoders run at 60 fps). Wider displays run Fast in v1 (see 13 for the split-stream follow-up).

Cost: encoder processes a 4K-sized frame; bitrate ≈ 1.5–2× Fast for the same quality.

### 4.2 Fast (native 4:2:0)
Standard BGRA → NV12 at `W×H` (chroma = average of the 2×2 block). Luma is full-res, so text shapes are crisp; only colored edges are slightly soft. This is the mode for secondary monitors and for wide displays.

### 4.3 Mode selection
- Default: display #1 Sharp (if eligible), others Fast.
- The sender tracks encoder time per frame per stream. If the sum can't sustain 60 fps for all active streams, it downgrades the lowest-priority Sharp stream to Fast and tells the receiver (which swaps shaders; no reconnect).
- The user can pin Sharp/Fast per display in the Mac menu; a pinned Sharp that can't be sustained shows a warning instead of silently dropping frames.
- Thresholds are **measured in phase 1** on a base M1; do not guess them.

### 4.4 Encoder settings (VideoToolbox)
- Codec `kCMVideoCodecType_HEVC`, profile `kVTProfileLevel_HEVC_Main_AutoLevel`.
- `RealTime = true`, `AllowFrameReordering = false` (no B-frames), `MaxFrameDelayCount = 0`, `PrioritizeEncodingSpeedOverQuality = true`, `ExpectedFrameRate = 60`, `AllowTemporalCompression = true`.
- Keyframes: `MaxKeyFrameInterval` very large (e.g. 36000). IDR **only on demand** (`kVTEncodeFrameOptionKey_ForceKeyFrame`) when the receiver reports a lost frame, plus one at stream start.
- Rate control: try `ConstantBitRate` (macOS 13+); fall back to `AverageBitRate` + `DataRateLimits` (1-second window). Defaults: Sharp 80 Mbps, Fast 40 Mbps. Adaptive range 15–150 Mbps.
- Color: `ColorPrimaries/TransferFunction/YCbCrMatrix` = BT.709; input pixel format `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange`.
- Require the hardware encoder (`kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder`); fail loudly if unavailable.
- Output arrives as hvcC (length-prefixed NALs) with parameter sets in the format description. Convert to **Annex B** and prepend VPS/SPS/PPS to every IDR access unit. One access unit = one frame = one packetized unit on the wire.

### 4.5 Decoder settings (FFmpeg)
- `AV_CODEC_ID_HEVC`, `hw_device_ctx` = D3D11VA built on **our** `ID3D11Device` (created with `D3D11_CREATE_DEVICE_VIDEO_SUPPORT`; enable `ID3D10Multithread::SetMultithreadProtected(TRUE)` since we own the device).
- `get_format` returns `AV_PIX_FMT_D3D11`. Output `data[0]` = `ID3D11Texture2D` (texture array), `data[1]` = slice index. Create R8 (Y) and RG8 (CbCr) SRVs on that slice; never copy to CPU.
- `flags |= AV_CODEC_FLAG_LOW_DELAY`, `thread_count = 1`, `thread_type = 0` (no frame threading: it adds latency).
- `extra_hw_frames` ≥ 4.
- Feed one Annex B access unit per `avcodec_send_packet`, then `avcodec_receive_frame` immediately.
- Trimmed static FFmpeg: `--disable-everything --enable-decoder=hevc --enable-parser=hevc --enable-hwaccel=hevc_d3d11va --enable-d3d11va --disable-programs --disable-doc --disable-network`, LGPL only (no `--enable-gpl`, no `--enable-nonfree`). Build in CI (vcpkg `x64-windows-static-md` with a custom port, or a direct configure under MSYS2); commit the recipe, not binaries. Rust binding: `rsmpeg`/`rusty_ffmpeg` or `ffmpeg-sys-next` with static linking — pick whichever links the trimmed build cleanly and document it.

---

## 5. Architecture

### 5.1 Repository layout
```
annex/
  Cargo.toml                 # workspace
  crates/
    annex-proto/             # wire format, packetizer/reassembler, FEC, AEAD, control messages, discovery; cross-platform, fully unit-tested, includes a lossy-channel simulator
    annex-core/              # resolution rules, mode negotiation, stats, config file I/O, latency estimation; cross-platform, unit-tested
    annex-mac/               # sender engine: virtual display, SCK capture, Metal pack, VT encode, send threads (lib + `annex-mac-spike` bin for phase 1)
    annex-win/               # receiver: monitor enumeration, windows, D3D11, FFmpeg decode, render, tray (bin)
  apps/
    annex-mac-app/           # Tauri 2 tray shell around annex-mac (phase 4)
  docs/
  .github/workflows/         # macos-latest (arm64) and windows-latest builds; proto/core tests on both
```

### 5.2 Sender data flow (per stream)
```
SCStream callback (SCK dispatch queue)
  → IOSurface-backed CVPixelBuffer (BGRA, W×H)
  → Metal compute: pack_sharp or pack_fast into NV12 CVPixelBuffer from a Metal-compatible pool
  → VTCompressionSessionEncodeFrame (async; encode on completion handler of the Metal command buffer, never block the SCK queue)
  → VT output callback: hvcC → Annex B access unit
  → Packetizer thread: split, FEC, encrypt
  → UDP send thread (one socket, all streams)
```
Threads: SCK delivers on its own queue; Metal/VT are async; **one** packetizer thread and **one** send thread serve all streams. Pre-allocate all buffers; zero allocations per frame on the hot path.

### 5.3 Receiver data flow (per stream)
```
UDP recv thread (one socket, SO_RCVBUF ≥ 8 MB, all streams)
  → demux by stream_id → per-stream reassembler (FEC repair, AEAD verify)
  → per-stream decode thread: avcodec_send_packet / receive_frame
  → per-monitor render thread: NV12 → RGB (+recombine for Sharp) → upscale → Present(0, ALLOW_TEARING)
```
Frames must be decoded **in order** (every frame references the previous). If a frame is incomplete when its successor is complete, drop it and request an IDR over the control channel; the decoder discards until the IDR arrives. **No jitter buffer.**

### 5.4 Control channel
TCP, length-prefixed messages (serde + a compact binary format such as `postcard`), encrypted inside the Noise transport. Carries: handshake, capability exchange, display list and stream configs, heartbeats (1 s), stats reports (1 s), IDR requests, mode changes, cursor data (phase 3), clock sync pings.

---

## 6. macOS specifics

### 6.1 Virtual display (private API)
Classes in CoreGraphics: `CGVirtualDisplayDescriptor`, `CGVirtualDisplaySettings`, `CGVirtualDisplayMode`, `CGVirtualDisplay`. Call them from Rust with `objc2` (`AnyClass::get`, `msg_send!`); link CoreGraphics. Reference implementations: **DeskPad** (Swift, MIT), `go-macos/virtualdisplay` (Go, documents many behaviors), FreeDisplay. Known shape (verify at runtime; the API is undocumented):

```
desc = CGVirtualDisplayDescriptor.new
desc.name, .maxPixelsWide, .maxPixelsHigh, .sizeInMillimeters, .productID, .vendorID, .serialNum,
desc.queue (dispatch queue), .terminationHandler, .whitePoint, .redPrimary, .greenPrimary, .bluePrimary
display = CGVirtualDisplay.alloc.initWithDescriptor(desc)
mode = CGVirtualDisplayMode.alloc.initWithWidth(w).height(h).refreshRate(60.0)
settings = CGVirtualDisplaySettings.new; settings.hiDPI = 0; settings.modes = [mode]
display.applySettings(settings)
display.displayID  → CGDirectDisplayID used to find the SCDisplay
```

Behaviors established on macOS 26.x that we must design around:
- **Releasing the `CGVirtualDisplay` object destroys the display — unless its mode was ever changed.** Therefore: one mode only, never change it. (Scaled "looks like" modes are a later experiment, see 13.)
- macOS remembers mode and arrangement **per monitor identity** (vendor/product/serial). Keep identities stable.
- Destruction is asynchronous: the display may remain listed for 0.7–2 s after release. Wait for `CGDisplayReconfigurationCallBack` / poll before reusing an identity.
- A process that queries CoreGraphics display modes **before** creating its virtual display may never get a `CGDisplayMode` for it. We don't need that call, but order operations defensively: create virtual displays before any other CG display queries in the sender process where possible.
- Wrap the whole thing in `annex-mac::vdisplay` with a `probe()` that checks the classes and selectors exist, so a macOS update that breaks the API fails with a clear message instead of a crash.

### 6.2 ScreenCaptureKit
- Find the `SCDisplay` whose `displayID` matches; `SCContentFilter(display:excludingWindows: [])`.
- `SCStreamConfiguration`: `width/height` = stream size in pixels, `pixelFormat = kCVPixelFormatType_32BGRA`, `minimumFrameInterval = 1/60`, `queueDepth = 3`, `showsCursor = true` in v1 (false once client-side cursor lands in phase 3), `colorSpaceName = kCGColorSpaceSRGB`, `captureResolution = best`.
- SCK may stop delivering frames when nothing changes (status `idle`). That's fine: don't synthesize frames; the receiver keeps the last one. Make sure rate control and stats handle variable input rate.
- Use the `displayTime` attachment as the capture timestamp (section 10).
- Permission: Screen Recording. macOS ties the TCC grant to the **code-signing identity**; sign every build with the same identity (a self-signed "Annex Dev" code-signing cert in Keychain is enough for self-use; Developer ID + notarization is optional). Ad-hoc/unsigned builds lose the permission on every rebuild.
- Monthly re-approval nag: macOS re-prompts apps using SCK without the system picker roughly monthly. Accept it for v1; document the known workarounds (Amnesia app, the replayd plist script) in the README. Do not spend engineering time on it.
- Local Network permission (macOS 15+): `Info.plist` needs `NSLocalNetworkUsageDescription` and `NSBonjourServices` = `_annex._tcp`, `_annex._udp`.

### 6.3 Metal packing
- `MTLDevice` system default. Use `CVMetalTextureCache` to wrap both the capture buffer (BGRA, read) and the NV12 output buffer (plane 0 as `R8Unorm`, plane 1 as `RG8Unorm`, write).
- Output buffers come from a `CVPixelBufferPool` with `kCVPixelBufferIOSurfacePropertiesKey` and `kCVPixelBufferMetalCompatibilityKey` set, format `420YpCbCr8BiPlanarFullRange`, size `2W×2H` (Sharp) or `W×H` (Fast). Pool size 4.
- Two kernels: `pack_sharp` (section 4.1) and `pack_fast` (chroma = mean of the 2×2 block). BT.709 full range: `Y = 0.2126 R + 0.7152 G + 0.0722 B`, `Cb = (B − Y)/1.8556 + 0.5`, `Cr = (R − Y)/1.5748 + 0.5`, all in [0, 1] then ×255.
- Encode is triggered from the command buffer's completion handler. Target < 1 ms GPU time.

### 6.4 Rust crates (verify current names/versions)
`objc2`, `objc2-foundation`, `objc2-core-graphics`, `objc2-screen-capture-kit`, `objc2-core-media`, `objc2-core-video`, `objc2-metal`, `block2`, `dispatch2`; VideoToolbox via `objc2-video-toolbox` if published, otherwise the standalone `videotoolbox` crate (supports IOSurface input) or hand-written `extern "C"` declarations — VT's surface we need is small (`VTCompressionSessionCreate`, `…Prepare`, `…EncodeFrame`, `VTSessionSetProperty`, `…Invalidate`).

### 6.5 Tauri shell (phase 4)
- `LSUIElement` / accessory activation policy: tray icon only. Settings window opens on demand.
- The engine is a plain Rust library (`annex-mac`) driven by Tauri commands; the webview is never on the hot path.
- Bundle: `.dmg` for `aarch64-apple-darwin`. Auto-start via `tauri-plugin-autostart`.

---

## 7. Windows specifics

- **DPI:** declare Per-Monitor V2 awareness so all coordinates are physical pixels.
- **Monitors:** enumerate via DXGI outputs (`DXGI_OUTPUT_DESC1`, `GetDisplayModeList`) plus `QueryDisplayConfig` for friendly names; current desktop mode via `EnumDisplaySettingsEx` for the output's device name. Re-enumerate on `WM_DISPLAYCHANGE`. Stable per-monitor id = hash of (friendly name, EDID manufacturer/product/serial when available, else device path).
- **Window per monitor:** `WS_POPUP`, exact output rect, `WS_EX_NOREDIRECTIONBITMAP`, black background, no cursor (`WM_SETCURSOR` → `SetCursor(NULL)`), `WM_ERASEBKGND` suppressed. Not exclusive fullscreen.
- **Swapchain:** `CreateSwapChainForHwnd`, `DXGI_FORMAT_B8G8R8A8_UNORM`, `FLIP_DISCARD`, 2 buffers, `DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING | FRAME_LATENCY_WAITABLE_OBJECT`, `SetMaximumFrameLatency(1)`, `Present(0, DXGI_PRESENT_ALLOW_TEARING)`. Check `DXGI_FEATURE_PRESENT_ALLOW_TEARING`; without it fall back to `Present(1, 0)`.
- **GPU selection:** use the adapter that owns the output (`IDXGIOutput::GetParent`); decode and render on the same device to avoid cross-adapter copies. Laptops with hybrid graphics: prefer the adapter attached to the display.
- **Shaders:** `nv12_to_rgb_fast.hlsl`, `nv12_to_rgb_sharp.hlsl` (recombine), `upscale_bicubic.hlsl`. Compile at build time (`fxc`/`dxc` in build.rs) or ship precompiled bytecode in the repo. Render directly into the back buffer when stream size == panel size.
- **Tray:** `tray-icon` + `muda` on a raw Win32 message loop (no winit needed, but allowed if simpler). Menu: list of monitors with checkboxes (enable = plug in), per-monitor Mode (Auto/Sharp/Fast) and Source (Extend/Mirror…) submenus, Pair…, Stats overlay toggle (also hotkey `Ctrl+Shift+F12`), Start at login, Quit. Left-click opens the **Annex window**: a WebView2 page (`crates/annex-win/ui/`) that gets the whole state pushed as JSON and posts commands back over IPC; it shows the same settings plus pairing, live stats and firewall status. It's destroyed on close, and it moves off (or stays above) monitors that show a Mac display.
- **Config:** TOML in `%APPDATA%\Annex\config.toml` (enabled monitors, modes, paired Mac identity + keys). Portable mode (a `portable.txt` next to the exe) keeps config, logs and the WebView2 profile in `data\` beside it instead.
- **Autostart:** `HKCU\...\Run` entry. Single instance via a named mutex.
- **Firewall:** the receiver listens on TCP 48800 (control) and UDP 48801 (video). The installer (per user, in `%LOCALAPPDATA%\Programs\Annex`) runs `Annex.exe --allow-firewall` elevated after the user clicks Install, which adds an inbound allow rule for the exe on private/domain networks and one on public networks limited to `remoteip=localsubnet`; uninstall runs `--remove-firewall`. The app reads the rules through `INetFwPolicy2` for the active profiles and, when the Mac would be blocked, shows a banner and tray item that rerun `--allow-firewall` elevated.
- **Crates:** `windows` (Direct3D11, Dxgi, Gdi, WindowsAndMessaging, Power, Threading features), `tray-icon`, `muda`, `mdns-sd`, `snow`, `chacha20poly1305`, `reed-solomon-simd` (or `reed-solomon-erasure`), `socket2`, `serde`, `postcard`, `toml`, `tracing`.

---

## 8. Wire protocol (`annex-proto`)

### 8.1 Discovery
Receiver advertises `_annex._tcp.local.` with TXT `v=1 id=<receiver-uuid> name=<hostname> cp=48800 vp=48801`. Sender browses continuously while it has a paired receiver and no active session. Addresses outside private/link-local ranges are ignored.

### 8.2 Pairing (once)
1. User chooses "Pair…" on the receiver; it shows a 6-digit PIN full-screen on the primary monitor for 2 minutes and accepts pairing connections.
2. Sender menu: "Pair with <name>" → user types PIN.
3. Both run SPAKE2 (`spake2` crate) over the PIN → shared key; under it they exchange and store each other's **static X25519 public keys** plus ids. Store on Mac in `~/Library/Application Support/Annex/` (0600) and on Windows in `%APPDATA%\Annex\`.
4. Either side may "Forget" the pairing.

### 8.3 Session handshake
TCP connect from sender → receiver, `Noise_IK_25519_ChaChaPoly_BLAKE2s` (sender knows the receiver's static key). Afterwards the TCP channel uses the Noise transport. From the handshake hash, derive with HKDF: `k_video_tx` (sender→receiver UDP), `k_cursor_tx` (phase 3). The receiver learns the sender's UDP source address from the first video packet that authenticates.

### 8.4 Capability / configuration messages (control channel)
- `Hello { proto_version, app_version, os, gpu_name, hevc_decode_ok, max_streams: 3 }`
- `Monitors { [ { id, name, native_w, native_h, refresh_mhz, enabled, priority, pinned_mode, source } ] }` (receiver → sender, on connect and on any change)
- `StreamConfig { stream_id, monitor_id, source: Extend | Mirror(display_id), stream_w, stream_h, mode: Sharp | Fast, bitrate_kbps, fec_percent }` (sender → receiver; receiver acks with `StreamReady`)
- `ModeChange { stream_id, mode }`, `IdrRequest { stream_id, last_good_seq }`, `Stats { … }`, `Heartbeat`, `ClockPing/ClockPong { t1, t2, t3 }`.

### 8.5 Video packet (UDP)
```
magic      u8   = 0xA7
version    u8   = 1
stream_id  u8
flags      u8   bit0 = contains IDR, bit1 = FEC parity shard
frame_seq  u32  per-stream, increments per frame
shard_idx  u16
data_shards u16 (k)
parity_shards u16 (m)
capture_ts u32  µs since session start (sender clock), low 32 bits
payload_len u16
payload    [u8; payload_len]   AEAD-encrypted; nonce = stream_id ‖ frame_seq ‖ shard_idx (96-bit), AAD = header
```
- Payload ≤ **1200 bytes** (no IP fragmentation on 1500 MTU; don't assume jumbo frames).
- Reed-Solomon per frame: `k` data shards, `m = ceil(k × fec_percent / 100)` parity. Default 10 %; raise toward 30 % as reported loss grows, drop back when clean. Keyframes always get ≥ 20 %.
- Reassembler per stream: a small ring (8 frames) keyed by `frame_seq`; a frame completes when any `k` of `k+m` shards arrive. Deliver in order; on gap → `IdrRequest`.
- Send pacing: spread a frame's packets over ~2 ms rather than bursting (helps Wi-Fi).

### 8.6 Adaptive bitrate
Receiver reports per second: packets received/lost/repaired, decode time, queue depth. Sender: on loss > 1 % for 2 consecutive seconds → bitrate ×0.8 and FEC up; on 10 clean seconds → bitrate ×1.1 up to the cap. Always apply changes at a frame boundary.

### 8.7 Simulator
`annex-proto::sim`: packetize → lossy/reordering/jittery channel → reassemble, used in unit tests to prove FEC recovery rates, nonce uniqueness, and in-order delivery. Must run in CI on all OSes.

---

## 9. Mode negotiation (`annex-core`)
Pure functions, fully unit-tested:
- `stream_size(native_w, native_h) -> (w, h)` per 3.1.
- `sharp_eligible(w) -> bool` (`w ≤ 1920`).
- `assign_modes(monitors, encoder_budget) -> [StreamConfig]` implementing 4.3 with priorities.
- `virtual_display_identity(receiver_id, monitor_index) -> (vendor, product, serial)`.

---

## 10. Latency measurement and stats

Both sides log per-stage timings with `tracing` at debug level and roll them into the 1-second `Stats` message.

Sender per frame: `capture_ts` (SCK `displayTime`), pack done, encode done, last packet sent. Receiver per frame: first packet, frame complete, decode done, present called. Clock offset from `ClockPing/Pong` (NTP-style, averaged); estimated **capture→present** latency = `present_time − capture_ts − offset`.

Receiver stats overlay (toggle via tray/hotkey) shows: fps, bitrate, loss/repaired %, FEC %, decode ms, render ms, estimated e2e ms, mode, stream size → panel size.

Glass-to-glass (the real number): display a millisecond counter on the Mac display being mirrored, photograph both screens in one frame at 1/1000 s shutter, subtract. Do this at the end of phase 1 on Ethernet and on Wi-Fi, for Sharp and Fast, and record the numbers in `docs/measurements.md`.

Budget (Ethernet, 1080p60, target ≤ 35 ms): capture ≤ 16.7 · pack < 1 · encode 3–8 (Sharp on M1 maybe 8–12) · packetize+send < 1 · network 1–3 · reassemble+decode 2–6 · present 0–8.

---

## 11. Non-negotiables on the hot path
- No per-frame or per-packet heap allocations after startup (pools, fixed rings).
- No B-frames, no frame reordering, no decoder frame-threading, no jitter buffer.
- No blocking on the ScreenCaptureKit callback queue.
- No CPU readback of decoded frames; decode texture → shader → swapchain only.
- No tokio/async on capture, encode, packetize, receive, decode or render threads.
- Logging on the hot path only through `tracing` with cheap fields; never format strings per packet.

---

## 12. Phases and acceptance criteria

### Phase 0 — Scaffold
Workspace, crates with stub APIs, CI (build mac arm64 + win x64, test proto/core), README, LICENSE (MIT OR Apache-2.0), `docs/measurements.md`, this file in `docs/`.
**Done when:** CI is green on both OSes and `annex-proto::sim` tests pass.

### Phase 1 — Latency spike (mirror only, hardcoded IP, no crypto, no discovery)
- `annex-mac-spike --mirror <displayID> --to <ip> --mode sharp|fast --bitrate N`
- `annex-win --spike --monitor <n>`; stats overlay on.
- Full pipeline: SCK → Metal pack → VT → UDP (+FEC) → FFmpeg d3d11va → shader → present.
**Done when:** the human reports, on a base M1 (or the lowest chip available) and on both an NVIDIA and an AMD PC: Sharp and Fast run at 60 fps; glass-to-glass ≤ 35 ms Ethernet / ≤ 50 ms Wi-Fi; Sharp screenshot on Windows is pixel-identical to the Mac for static text (compare crops); encoder ms/frame for Sharp and Fast recorded. These numbers set the mode-assignment thresholds.

### Phase 2 — Cable behavior
Virtual displays (3.1, 6.1), Extend + Mirror per monitor, up to 3 streams, mode assignment (4.3), plug/unplug/grace/black rules (3.3–3.4), monitor hot-plug on Windows, in-order decode + IDR recovery.
**Done when:** enabling/disabling monitors behaves like plugging cables; three streams run concurrently (modes as budget allows); killing Wi-Fi for 10 s and restoring it resumes without rearranging Mac windows; arrangement persists across restarts.

### Phase 3 — Productization
mDNS discovery, PIN pairing, Noise + per-packet AEAD, auto-connect/reconnect, adaptive bitrate/FEC, client-side cursor (separate low-rate channel with position + cursor image; `showsCursor = false`), private-address enforcement, config files, logs.
**Done when:** a cold start on both machines ends in a working session with no clicks; cursor feels instant; pulling Ethernet and switching to Wi-Fi recovers automatically.

### Phase 4 — Packaging
Tauri 2 tray app for Mac (`.dmg`, settings window: paired PC, per-display mode, stats), Windows single `.exe` (optional NSIS installer), autostart both sides, signing docs (self-signed cert recipe for TCC stability), GitHub Releases workflow.
**Done when:** a fresh Mac + fresh PC go from download to streaming following only the README.

### Phase 5 — Later
See section 13.

---

## 13. Backlog (after v1)
- **Sharp for wide displays:** split a `stream_w > 1920` display into two side-by-side streams (each ≤ 1920 wide), decode separately, composite in one window. Gate on encoder headroom.
- **Lossless static refinement:** use SCK dirty rects; after a region has been static for N frames, send its exact pixels (LZ4/zstd) so still content becomes bit-exact even in Fast mode.
- Scaled "looks like" modes on the virtual display (needs an answer to the un-removable-after-mode-change behavior; possibly recreate the display instead of changing mode).
- 10-bit (Main10) toggle to reduce banding. Intel GPU validation. Multiple receivers. Audio.

---

## 14. Reference material (design only; do not copy GPL code)
- DeskPad (Stengo/DeskPad, MIT) — `CGVirtualDisplay` usage in Swift.
- go-macos/virtualdisplay — documented runtime behaviors of the private API on macOS 26.
- FreeDisplay / BetterDisplay — virtual displays on macOS 26/27 (BetterDisplay 5 confirms macOS 27 support).
- LizardByte discussion #220 and Sunshine/Moonlight — cross-vendor 4:4:4 situation and the "doubled 4:2:0" recombine idea.
- Moonlight / moonlight-common-c (GPL) — FEC + packetization design patterns, IDR-on-loss strategy.
- Apple: ScreenCaptureKit, VideoToolbox, Metal docs; the TCC/code-signing identity note; monthly SCK prompt and known workarounds (Amnesia, replayd plist script).
- FFmpeg `libavcodec` D3D11VA hwaccel docs; `dxva2_hevc.c` for what the decoder needs from the bitstream.
