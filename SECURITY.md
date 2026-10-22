# Security policy

## Supported versions

Only the latest release gets security fixes. Please check that a problem
still happens on it before reporting.

## Reporting a vulnerability

Please **don't open a public issue**. Report it privately through GitHub
instead: go to the repository's **Security** tab and choose **Report a
vulnerability**
([direct link](https://github.com/msadiks/annex-app/security/advisories/new)).

Include the Annex version, which side is affected (Mac, PC or both), what an
attacker can do, and the steps to reproduce it. The maintainer aims to reply
within a week. Once a fix is released, the advisory is published and you're
credited unless you'd rather not be.

## What counts

Annex streams your screen, so anything that undermines these promises is in
scope:

- Only a Mac and PC that were paired with the PIN can connect to each other.
- Nobody else on the network can see the picture or inject frames or control
  messages (Noise handshake, ChaCha20-Poly1305 on every packet).
- The PIN can't be brute-forced offline from captured traffic (SPAKE2).
- Annex only talks to addresses on the local network.
- A device on the network can't crash or take over either app with crafted
  packets.

Out of scope: attacks that already need admin rights or physical access on
the Mac or PC, macOS's own Screen Recording prompts, and SmartScreen or
Gatekeeper warnings.
