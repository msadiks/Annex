# Releasing

For maintainers. A release is one draft GitHub release holding `Annex.exe`
and `Annex-mac-arm64.dmg`.

## Once, on the Mac that signs releases

Install a *Developer ID Application* certificate in the login keychain, then
store a notarization profile (it asks for an app-specific password and keeps
it in the keychain):

```sh
xcrun notarytool store-credentials annex --apple-id <apple id> --team-id <team id>
```

## Each release

1. Bump the version in `Cargo.toml` (`[workspace.package]`) and
   `apps/annex-mac-app/tauri.conf.json`, and merge that to `main`.
2. Push a tag such as `v1.0.0` (or run the *Release* workflow by hand with the
   tag name). The workflow builds `Annex.exe` into a draft GitHub release.
3. On the Mac, from a clean checkout of the tag:

   ```sh
   tools/mac/release.sh        # writes dist/Annex-mac-arm64.dmg, notarized and stapled
   ```

4. Attach `dist/Annex-mac-arm64.dmg` to the draft release, write the notes and
   publish it.

macOS ties the Screen Recording permission to the signing identity, so builds
signed with the same certificate keep the permission across updates. Always
sign with the same certificate.
