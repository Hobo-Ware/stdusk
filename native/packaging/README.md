# Packaging stdusk

## Release flow (automated)

1. Bump `version` in `native/Cargo.toml`.
2. Tag and push: `git tag stdusk-v0.1.0 && git push origin stdusk-v0.1.0`.
3. `.github/workflows/native-release.yml` builds a **universal** macOS binary (arm64 + x86_64
   lipo'd together), wraps it in a `stdusk.app` bundle (icns + Info.plist so the Dock shows the
   brand icon), zips it, and creates the GitHub Release with two assets:
   - `stdusk-<version>-universal.app.zip` - the app bundle
   - `stdusk.rb` - the Homebrew formula, with the correct `sha256` already filled in

   The formula installs the bundle and symlinks `stdusk` onto the PATH, so you get both the CLI
   and a proper Dock icon.

## Homebrew tap (one-time)

The formula lives in a tap so users get `brew install hobo-ware/tap/stdusk`:

```sh
gh repo create Hobo-Ware/homebrew-tap --public -d "Homebrew tap for Hobo-Ware tools"
git clone https://github.com/Hobo-Ware/homebrew-tap && cd homebrew-tap
mkdir -p Formula
# after each release, drop the generated formula in:
gh release download stdusk-v0.1.0 -R Hobo-Ware/stdusk -p stdusk.rb -O Formula/stdusk.rb
git add Formula/stdusk.rb && git commit -m "stdusk 0.1.0" && git push
```

Then anyone can:

```sh
brew install hobo-ware/tap/stdusk
```

`stdusk` is a plain binary (not a `.app`), so a Homebrew **formula** - not a cask - is correct,
and formula-installed binaries aren't Gatekeeper-quarantined: it runs unsigned without a
notarization prompt. A signed `.app` bundle is a later polish item.

`native/packaging/stdusk.rb` is a reference copy with placeholder `version`/`sha256`; the real
values come from the release build.
