# Measurements

Numbers that decide the design (spec section 10). Fill these in from real
hardware; leave a cell empty rather than guessing.

## How to measure

- **Estimated capture→present:** the `e2e` figure in the PC stats overlay
  (`Ctrl+Shift+F12`) or the Mac window. It uses the clock offset from
  `ClockPing`/`ClockPong`, so treat it as ±1 ms.
- **Glass-to-glass:** mirror a Mac display that shows a millisecond counter
  (for example a stopwatch web page), photograph both screens in one frame at
  1/1000 s shutter, subtract. Take at least five photos and report the median.
- **Per-stage timings:** run either side with `ANNEX_LOG=debug` and read the
  per-frame timings from the logs.

Budget (Ethernet, 1080p60, target ≤ 35 ms): capture ≤ 16.7 · pack < 1 ·
encode 3–8 · packetize+send < 1 · network 1–3 · reassemble+decode 2–6 ·
present 0–8.

## Setup

| | |
| --- | --- |
| Mac | |
| macOS | |
| PC GPU | |
| Windows | |
| Network | |
| Annex version | |

## Latency (1080p60)

| Link | Mode | e2e estimate (ms) | Glass-to-glass (ms) | Encode (ms) | Decode (ms) | Loss % | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Ethernet | Sharp | | | | | | |
| Ethernet | Fast | | | | | | |
| Wi-Fi 5 GHz | Sharp | | | | | | |
| Wi-Fi 5 GHz | Fast | | | | | | |

## Encoder budget

How many streams the Mac's encoder sustains at 60 fps. The thresholds in
`annex-core` (`assign_modes`) start conservative and should be adjusted to
these results.

| Streams | Modes | Sustained fps per stream | Encode ms (worst) | Notes |
| --- | --- | --- | --- | --- |
| 1 | Sharp | | | |
| 2 | Sharp + Fast | | | |
| 3 | Sharp + Fast + Fast | | | |
| 3 | Fast ×3 | | | |
