# Releasing

For maintainers. A release is one draft GitHub release holding
`Annex-windows-x64-setup.exe`, `Annex-windows-x64-portable.zip` and
`Annex-mac-arm64.dmg`, all built by the
[Release workflow](../.github/workflows/release.yml), plus `SHA256SUMS.txt`
with their checksums. The workflow also signs a build provenance for each
file, so anyone can check with `gh attestation verify <file> --repo
msadiks/annex-app` that a download came from this repository's workflow. The Windows job
cross-builds `annex.exe` and packages it with
[`tools/windows/package.sh`](../tools/windows/package.sh) (NSIS installer and
portable zip). The Mac job signs the
app with the project's Developer ID certificate, has Apple notarize the .dmg
and staples the ticket to it, so it opens without Gatekeeper warnings.

## Each release

1. Bump the version in `Cargo.toml` (`[workspace.package]`; the Windows
   installer takes its version from there),
   `apps/annex-mac-app/tauri.conf.json` and
   `crates/annex-win/resources/annex.rc`, and merge that to `main`.
2. Push a tag such as `v1.1.0` (or run the *Release* workflow by hand from the
   Actions tab with the tag name).
3. The **mac** job waits for approval, because it is the only job that can read
   the Apple secrets. Open the workflow run, choose *Review deployments*, tick
   `release` and *Approve and deploy*.
4. When both jobs are green, open the draft release, write the notes, check
   that all four files are attached (the three downloads and
   `SHA256SUMS.txt`) and publish it. If you ran the workflow
   by hand, publishing creates the tag and so starts the workflow again; that
   run sees the release is already published and builds nothing. (Running it
   by hand with the tag of a published release fails for the same reason, so
   the files people download are never swapped.)

Expect the Mac job to take roughly 10 to 20 minutes, mostly the build and
Apple's notarization queue. If it fails or you reject it, the draft is still
made with the Windows installer and portable zip, and you can add a .dmg
signed by hand (see the end of
this page). CI on every pull request also uploads an unsigned
.dmg (`annex-mac-unsigned-dmg`), which is handy for checking the installer
window before a release.

## One-time setup

Everything Apple-related lives in a GitHub *environment* named `release`, not
in plain repository secrets. Environment secrets are only handed to a job that
names the environment, after a reviewer approves it, and only for runs from
`main` or a `v*` tag. Pull requests, including ones from forks, never see them.

### 1. Create the `release` environment

Repository *Settings › Environments › New environment*, name it `release`:

- **Required reviewers:** add yourself (and any co-maintainer who may approve
  releases). Leave *Prevent self-review* off, or you can't approve your own
  release.
- **Deployment branches and tags:** choose *Selected branches and tags* and add
  the branch rule `main` and the tag rule `v*`.

Create it before the first release: if a workflow names an environment that
doesn't exist, GitHub creates one with no protection.

### 2. Export the signing certificate

On the Mac that has the *Developer ID Application* certificate:

1. Open *Keychain Access* (Spotlight finds it; on recent macOS it lives in
   `/System/Library/CoreServices/Applications`).
2. Under *login › My Certificates*, find
   `Developer ID Application: <name> (<team id>)` and expand it to check its
   private key is there.
3. Right-click the certificate › *Export…*, choose *Personal Information
   Exchange (.p12)*, save it as `annex-signing.p12` and give it a strong
   password.

### 3. Create an App Store Connect API key for notarization

1. In [App Store Connect](https://appstoreconnect.apple.com) go to *Users and
   Access › Integrations › App Store Connect API › Team Keys* and click *+*
   (the first time, the account holder may need to click *Request Access*).
2. Name it `Annex notarization` and give it the **Developer** role, the lowest
   one that can notarize.
3. Download `AuthKey_<KEY ID>.p8` (Apple offers the download only once) and
   note the **Key ID** and the **Issuer ID** shown above the list of keys.

An API key is used instead of an Apple ID because it can do much less: it
can't sign in to your Apple Account or iCloud, it is limited to App Store
Connect's Developer role, and you can revoke it on its own at any time.

### 4. Add the secrets

In *Settings › Environments › release › Environment secrets*, add:

| Secret | Value | What it allows |
| --- | --- | --- |
| `APPLE_CERTIFICATE` | `annex-signing.p12`, base64 encoded | Signing code as the project's Developer ID |
| `APPLE_CERTIFICATE_PASSWORD` | The password you set when exporting | Opening the .p12 above |
| `APPLE_API_KEY_P8` | The whole text of `AuthKey_<KEY ID>.p8` | Notarizing (App Store Connect Developer role) |
| `APPLE_API_KEY_ID` | The Key ID, e.g. `2X9R4HXF34` | Nothing alone; it names the key |
| `APPLE_API_ISSUER` | The Issuer ID (a UUID) | Nothing alone; it names your team |

With the [GitHub CLI](https://cli.github.com), from a clone of the repository:

```sh
base64 -i annex-signing.p12 | gh secret set APPLE_CERTIFICATE --env release
gh secret set APPLE_CERTIFICATE_PASSWORD --env release        # prompts for the value
gh secret set APPLE_API_KEY_P8 --env release < AuthKey_XXXXXXXXXX.p8
gh secret set APPLE_API_KEY_ID --env release --body XXXXXXXXXX
gh secret set APPLE_API_ISSUER --env release --body 00000000-0000-0000-0000-000000000000
```

Then delete the local `annex-signing.p12`, and keep the `.p8` only in a
password manager if at all. The team ID is not needed: the workflow reads it
from the certificate.

**Rather use an app-specific password?** It works too, but it signs in as your
Apple ID and also opens iCloud Mail, Contacts and Calendars, so the API key is
the safer choice. Create one at [account.apple.com](https://account.apple.com)
› *Sign-In and Security › App-Specific Passwords* and add `APPLE_ID` (your
Apple ID email) and `APPLE_APP_PASSWORD` instead of the three `APPLE_API_*`
secrets. Never use your Apple ID's real password.

### If a secret leaks

Revoke the API key in App Store Connect (or the app-specific password at
account.apple.com) and add a new one; builds already shipped are unaffected.
Revoking the Developer ID certificate also affects copies of Annex that are
already installed, so read Apple's guidance or ask Apple Developer Support
before doing that.

### 5. Lock down the repository

These live in the repository settings, not in its files, so check them once:

- *Settings › Actions › General*: set *Workflow permissions* to read-only
  (the workflows ask for more where they need it), and turn on the policy that
  requires actions to be pinned to a full-length commit SHA if your plan
  offers it.
- *Settings › Rules › Rulesets*: a **tag** ruleset for `v*` that restricts
  creating, updating and deleting those tags to you (add yourself to the
  bypass list). A tag push starts a release, so nobody else should be able to
  make one. A **branch** ruleset for `main` that blocks force pushes and
  deletion and requires a pull request with CI passing.
- *Settings › Code security*: Dependabot alerts (they only notify you; keep
  Dependabot's version and security updates off, as pins are changed by hand),
  secret scanning with push protection, and private vulnerability reporting
  ([SECURITY.md](../SECURITY.md) sends reporters there).
- *Settings › General › Releases*: turn on immutable releases if it is
  offered, so a published release's files and tag can't be changed.
- Two-factor authentication on every account that can push or approve the
  `release` environment.

## What the build trusts, and how it stays pinned

The Mac job builds and signs in one place, so everything it runs is fixed to
an exact version that can't change underneath it:

- **GitHub Actions** are pinned to full commit hashes in
  [ci.yml](../.github/workflows/ci.yml),
  [release.yml](../.github/workflows/release.yml) and
  [audit.yml](../.github/workflows/audit.yml), with the release name in a
  comment. Rust comes from the runner's own `rustup`.
- **Crates** are built with `--locked`, exactly as in `Cargo.lock`.
- **The Tauri CLI** version is `TAURI_CLI_VERSION` in
  [tools/mac/release.sh](../tools/mac/release.sh) and in `ci.yml`; change
  both together.
- **FFmpeg** is a tag plus the commit it must point to in
  [tools/ffmpeg/build-mingw.sh](../tools/ffmpeg/build-mingw.sh); the build
  refuses any other source. Releases always build it from source, never from
  CI's cache.

Nothing updates these on its own. To move one to a newer version, change it
by hand and read what changed first, preferring a release that has been out
for a week or so (hijacked releases are usually caught within days):

- **An action:** find the commit of the new release with
  `git ls-remote https://github.com/<owner>/<action> "refs/tags/<version>*"`
  (take the `^{}` line if there is one) and update the hash and the comment
  everywhere the action appears.
- **Crates:** `cargo update` (or `cargo update -p <crate>`), then commit
  `Cargo.lock`.

The [Audit workflow](../.github/workflows/audit.yml) checks `Cargo.lock` against
the RustSec advisory database on every dependency change and every Monday, so
a crate with a known vulnerability shows up as a failed run even when nothing
else changed.

The Windows files are not code-signed (Windows shows a SmartScreen warning).
Signing them needs a code signing certificate; free options for open source
projects exist, such as the SignPath Foundation. Until then, the checksums and
build provenance are how people can check a download.

## Signing by hand on a Mac

The workflow runs [`tools/mac/release.sh`](../tools/mac/release.sh), which
also works on a Mac with the certificate in the login keychain. Store a
notarization profile once (with the same API key, or
`--apple-id <apple id> --team-id <team id>` to be asked for an app-specific
password):

```sh
xcrun notarytool store-credentials annex --key AuthKey_XXXXXXXXXX.p8 --key-id XXXXXXXXXX --issuer <issuer id>
```

Then, from a clean checkout of the tag:

```sh
tools/mac/release.sh        # writes dist/Annex-mac-arm64.dmg, notarized and stapled
gh release upload v1.1.0 dist/Annex-mac-arm64.dmg
# Add its checksum next to the Windows files' ones.
gh release download v1.1.0 --pattern SHA256SUMS.txt --dir dist --clobber
(cd dist && shasum -a 256 Annex-mac-arm64.dmg >> SHA256SUMS.txt)
gh release upload v1.1.0 dist/SHA256SUMS.txt --clobber
```

A .dmg built by hand has no build provenance, only its checksum and Apple's
notarization.

macOS ties the Screen Recording permission to the app's code signature, so
builds signed with the same Developer ID keep the permission across updates.
Always sign with the same certificate.
