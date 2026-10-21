# Annex

Annex turns a Windows PC's monitors into extra displays for a Mac, over your
local network. It behaves like a cable would, minus the cable: turn a PC
monitor on in Annex and the Mac sees a new display; turn it off and the
display is unplugged.

- **Sharp** mode sends exact colour (4:4:4) for text and UI, **Fast** mode is
  plain 4:2:0 for video and motion. The first display gets Sharp, others Fast,
  and you can pin either per monitor.
- Up to three PC monitors at once, each as its own Mac display, or mirroring a
  Mac display.
- Hardware HEVC on both ends (VideoToolbox on the Mac, D3D11VA on the PC),
  forward error correction, and encryption for everything on the wire.
- LAN only. Nothing leaves your network and there is no account or relay.

The design is in [docs/spec.md](docs/spec.md).

## Requirements

- **Mac:** Apple silicon, macOS 13 or later.
- **PC:** Windows 10 or 11, 64-bit, with a GPU that decodes HEVC (NVIDIA GTX
  950 / AMD RX 400 / Intel 6th gen or newer).
- Both on the same network. Wired Ethernet on at least one side gives the
  lowest latency; 5 GHz Wi-Fi works.

## Install and connect

1. Download `Annex.exe` and `Annex-mac-arm64.dmg` from the
   [latest release](https://github.com/msadiks/annex-app/releases).
2. **PC:** run `Annex.exe`. It lives in the system tray (the monitor icon).
   - Windows may show a SmartScreen warning for a new app: choose
     *More info → Run anyway*.
   - When Windows Firewall asks, allow Annex on **private** networks. If you
     missed the prompt, use *Allow through Windows Firewall…* in the tray menu.
     Annex uses TCP 48800 and UDP 48801.
3. **Mac:** open the .dmg and drag Annex to Applications, then open it. It
   lives in the menu bar.
   - Allow **Screen Recording** when macOS asks (System Settings › Privacy &
     Security › Screen Recording), then quit and reopen Annex.
   - Allow **Local Network** access when asked.
4. **Pair once:** in the PC tray menu choose *Pair with a Mac…*. A PIN appears
   on the PC. Type it into the Annex window on the Mac (menu bar icon ›
   *Open Annex…*; it opens by itself until you've paired).
5. **Plug in a monitor:** tick a monitor in either menu (PC tray or the Mac
   settings window). Within a second the Mac shows a new display. Arrange it
   in System Settings › Displays like any other monitor.

From then on both apps reconnect by themselves whenever they're on the same
network. Turn on *Start at login* on both sides to make it permanent.

## Everyday use

- **PC tray menu:** tick monitors on or off, pick *Mode* (Auto / Sharp / Fast)
  and *Source* (*Extend* as a new Mac display, or *Mirror* one of the Mac's
  displays), and toggle the stats overlay (also `Ctrl+Shift+F12`).
- **Mac window:** the same per-monitor settings, live stats (frame rate,
  bitrate, loss, estimated latency), pointer drawing and login options.
- If the network drops, the Mac keeps the display for 30 seconds so your
  windows don't move. After that it's unplugged until the PC is back.

## Troubleshooting

- **The Mac can't find the PC:** both must be on the same subnet, and the
  firewall must allow Annex on private networks. Check that the PC's network
  is set to *Private* in Windows settings.
- **Black screen on the PC:** the PC shows black when the Mac is gone for 2 s
  (no signal). Make sure the Mac app is running and Screen Recording is
  allowed.
- **macOS asks again about screen recording every month:** that's macOS's
  periodic re-confirmation for apps that capture the screen. Approve it. Known
  workarounds exist (for example the *Amnesia* app, or editing the `replayd`
  plist), but they are unsupported.
- **Logs:** PC: tray menu › *Open log folder* (`%LOCALAPPDATA%\Annex\logs`).
  Mac: `~/Library/Logs/Annex/annex.log`.
- **Reset:** delete `%APPDATA%\Annex\config.toml` on the PC or
  `~/Library/Application Support/Annex/config.toml` on the Mac and pair again.

## Building from source

Everything is Rust (edition 2024). The protocol, core logic and sender engine
are platform independent:

```sh
cargo test -p annex-proto -p annex-core -p annex-sender
```

**Windows receiver** (`crates/annex-win`), cross-compiled from Linux (or
built in MSYS2) with mingw-w64 against a trimmed, LGPL-only static FFmpeg:

```sh
sudo apt install mingw-w64 nasm
rustup target add x86_64-pc-windows-gnu
tools/ffmpeg/build-mingw.sh            # HEVC decoder + D3D11VA only
cargo build --release --target x86_64-pc-windows-gnu -p annex-win
```

The result is a single `annex.exe` that needs only system DLLs.

**Mac sender** (`crates/annex-mac`) and app (`apps/annex-mac-app`, Tauri 2),
on a Mac:

```sh
cargo build --release -p annex-mac          # includes annex-mac-cli for testing
npm install -g @tauri-apps/cli@2
cd apps/annex-mac-app && tauri build --bundles dmg
```

**Test sender** (`crates/annex-testsrc`) plays generated HEVC test patterns
to a receiver, so the PC side can be checked without a Mac:

```sh
cargo run --release -p annex-testsrc -- gen --size 1920x1080 --mode sharp --out media
cargo run --release -p annex-testsrc -- run --addr <pc-ip> --media media --pin <PIN> --enable 0
```

### Signing and releases

Push a tag such as `v1.0.0` and the *Release* workflow builds `Annex.exe` and
`Annex-mac-arm64.dmg` and attaches them to a draft GitHub release.

To sign and notarize the Mac app with a Developer ID, add these repository
secrets (Settings › Secrets and variables › Actions):

| Secret | Value |
| --- | --- |
| `APPLE_CERTIFICATE` | Your *Developer ID Application* certificate exported as .p12, base64-encoded (`base64 -i cert.p12`) |
| `APPLE_CERTIFICATE_PASSWORD` | The .p12 export password |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | The Apple ID used for notarization |
| `APPLE_PASSWORD` | An app-specific password for that Apple ID |
| `APPLE_TEAM_ID` | Your 10-character team ID |

Without them the workflow still produces an unsigned .dmg. macOS ties the
Screen Recording permission to the signing identity, so builds signed with
the same identity keep the permission across updates; unsigned builds lose
it on every rebuild. For self-use without a Developer ID, a self-signed
code-signing certificate made in Keychain Access works too: sign with
`codesign --force --deep -s "Annex Dev" Annex.app`.

## Licence

Annex is licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option.

The Windows receiver statically links [FFmpeg](https://ffmpeg.org) (tag
`n8.1.3`, configured LGPL-only with just the HEVC decoder and D3D11VA; see
`tools/ffmpeg/build-mingw.sh`), which is licensed under the LGPL 2.1 or later.
