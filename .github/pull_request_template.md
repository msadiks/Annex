## What and why

<!-- What does this change, and why? Link the issue, e.g. "Fixes #12". -->

## How it was tested

<!--
CI covers the protocol, core and sender crates. For changes to the Mac or
Windows apps, say what you ran it on (Mac model, macOS, PC GPU, Windows,
Wi-Fi or Ethernet) and what you checked. For performance changes, add before
and after numbers from the stats overlay or debug logs.
-->

## Checklist

- [ ] One focused change, with no unrelated reformatting
- [ ] Tests for new platform-independent logic (`annex-proto`, `annex-core`, `annex-sender`)
- [ ] README updated if something users see changed
- [ ] `PROTO_VERSION` bumped if the wire format changed incompatibly
- [ ] No code copied from GPL projects; any new dependency has an MIT/Apache-compatible licence
