# Packaging stdusk

stdusk ships as a Homebrew **cask** (it's a GUI `.app`, so a cask - not a formula - puts it in
`/Applications` where Spotlight/Launchpad find it, and the `binary` stanza still links `stdusk`
onto your PATH).

## Release flow (automated)

1. Bump `version` in `native/Cargo.toml`.
2. Tag and push: `git tag stdusk-v0.1.0 && git push origin stdusk-v0.1.0`.
3. `.github/workflows/native-release.yml` builds a **universal** macOS binary (arm64 + x86_64
   lipo'd), wraps it in `stdusk.app` (icns + Info.plist), zips it with `ditto`, cuts the GitHub
   Release, and generates the cask with the real `sha256`. Two assets:
   - `stdusk-<version>-universal.app.zip` - the app bundle
   - `stdusk.rb` - the cask, ready for the tap

## Homebrew tap (one-time)

```sh
gh repo create Hobo-Ware/homebrew-tap --public -d "Homebrew tap for Hobo-Ware tools"
git clone https://github.com/Hobo-Ware/homebrew-tap && cd homebrew-tap
mkdir -p Casks
gh release download stdusk-v0.1.0 -R Hobo-Ware/stdusk -p stdusk.rb -O Casks/stdusk.rb
git add Casks/stdusk.rb && git commit -m "stdusk 0.1.0" && git push
```

Then:

```sh
brew install hobo-ware/tap/stdusk   # installs to /Applications + links the `stdusk` CLI
```

## Signing / notarization

The `.app` is **ad-hoc signed** (Rust's linker default), not Developer ID signed or notarized.
On macOS the cask's `postflight` strips the `com.apple.quarantine` flag so Gatekeeper doesn't
hard-block the GUI launch. That's a pragmatic stopgap, not the real fix.

**Proper fix (later):** sign with a Developer ID cert + notarize with `notarytool` in CI. That
needs an Apple Developer account ($99/yr) and these GitHub secrets: the signing cert (`.p12` +
password) and an app-specific password / API key for notarization. Once notarized, drop the
`postflight` quarantine-strip. Until then, a first Finder launch of a manually-downloaded `.app`
(not via brew) may still need System Settings -> Privacy & Security -> "Open Anyway".
