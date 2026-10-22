# Contributing to Annex

Thanks for helping. Annex is a small project maintained in spare time, so
these rules are here to keep reviews quick and the app dependable. By taking
part you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Ways to help

- **Report bugs** with the bug report form. Include your hardware, OS
  versions and logs from both sides.
- **Test on new hardware.** Annex has only been tried on a few machines.
  Numbers from other Macs and GPUs (especially NVIDIA and Intel) are very
  useful; add them to [docs/measurements.md](docs/measurements.md) in a pull
  request, or post them in an issue.
- **Fix bugs and improve the docs.**
- **Build features**, after agreeing on them in an issue first (see below).

## Before you write code

- **Bug fixes, docs and small changes:** open a pull request directly.
- **Features, new dependencies, protocol changes, or anything large:** open
  an issue first and wait for a maintainer to agree on the approach. This
  saves you from writing code that can't be merged.
- **Check the scope.** Some things are deliberately left out for now (audio,
  input from the PC, streaming over the internet, Intel Macs, Windows on ARM;
  see [Goals and non-goals](docs/spec.md#1-goals-and-non-goals)). Discuss
  these before working on them.
- **Claim the issue** with a comment so two people don't do the same work.

## Development setup

The build commands are in the README under
[Building from source](README.md#building-from-source). In short:

- `annex-proto`, `annex-core` and `annex-sender` build and test on any OS:
  `cargo test -p annex-proto -p annex-core -p annex-sender`.
- The Windows receiver cross-compiles from Linux (or builds in MSYS2) once
  `tools/ffmpeg/build-mingw.sh` has built FFmpeg.
- The Mac sender and app need an Apple silicon Mac.
- No Mac? `annex-testsrc` streams test patterns to a PC receiver.
- `ANNEX_LOG=debug` on either side logs per-stage timings.

Read [docs/spec.md](docs/spec.md) before changing the video pipeline or the
protocol. It explains why things are the way they are.

## Pull request rules

1. **One change per pull request.** Keep it focused. Don't mix features,
   refactors and formatting.
2. **CI must pass.** It runs the platform-independent tests, cross-builds the
   Windows receiver and builds the Mac app.
3. **Test what CI can't.** Capture, encoding, decoding and display only run on
   real hardware. For changes to `annex-mac`, `annex-win` or the Mac app, say
   in the pull request what you ran it on (Mac model, macOS version, PC GPU,
   Windows version, Wi-Fi or Ethernet) and what you checked.
4. **Measure performance changes.** Include before and after numbers (frame
   rate, `e2e` latency, encode time) from the stats overlay
   (`Ctrl+Shift+F12`) or the debug logs. Numbers decide, not opinions.
5. **Add tests for platform-independent logic.** New behaviour in
   `annex-proto`, `annex-core` and `annex-sender` comes with unit tests.
6. **Respect the hot-path rules** in
   [spec section 11](docs/spec.md#11-non-negotiables-on-the-hot-path): no
   per-frame allocations, no blocking the capture queue, no async on the video
   threads, no CPU readback of decoded frames.
7. **Change the protocol in one place.** Wire format changes go in
   `annex-proto` and update both the Mac and the PC side in the same pull
   request. Bump `PROTO_VERSION` for any incompatible change, so mismatched
   versions refuse each other cleanly.
8. **Keep the licence clean.**
   - Don't copy code from GPL projects (Sunshine, Moonlight,
     moonlight-common-c). Reading them for ideas is fine.
   - New dependencies must use a licence compatible with MIT and Apache-2.0
     (MIT, Apache-2.0, BSD, ISC, Zlib and similar). Keep them few.
   - The FFmpeg build stays LGPL-only: don't enable GPL, non-free or extra
     components in `tools/ffmpeg/build-mingw.sh`.
9. **Nothing private in the repo.** No certificates, Apple IDs, keys or
   tokens, and no logs or screenshots that show personal paths, names or
   network details. Don't commit generated test media.
10. **Update the README** in the same pull request when something users see
    changes.

## Style

- Match the code around you. The codebase isn't formatted with `rustfmt` yet,
  so don't reformat lines you aren't changing; it buries the real change.
- Don't add new `cargo clippy` warnings.
- Comments explain *why*, not *what*.
- Commit messages: a short imperative subject that says what changes, ideally
  from the user's point of view (`Keep the builder's home folder out of the
  Mac release binary`), and a body that explains why. Squash "fix typo"
  commits before the pull request is merged.

## AI-assisted contributions

Welcome, with one rule: you are responsible for every line. Understand it,
build it and test it as described above before you open the pull request.

## Review

A maintainer reviews every pull request. Reviews happen in spare time; if
yours has had no reply for two weeks, a friendly ping is welcome. A pull
request may be closed if it goes against the rules above or the project's
direction, or if it stalls for a month without replies.

## Security issues

Don't open a public issue. Follow [SECURITY.md](SECURITY.md).

## Licence

Annex is licensed under MIT or Apache-2.0, at the user's option. Unless you
explicitly state otherwise, any contribution you intentionally submit for
inclusion in Annex, as defined in the Apache-2.0 licence, is dual licensed
under the same terms, without any additional terms or conditions. There is no
CLA to sign.
