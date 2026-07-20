# Packaging stdusk

stdusk ships as a Homebrew **cask** (it's a GUI `.app`, so a cask - not a formula - puts it in
`/Applications` where Spotlight/Launchpad find it, and the `binary` stanza still links `stdusk`
onto your PATH).

## Release flow (automated)

1. Bump `version` in `Cargo.toml`.
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

The release workflow signs + notarizes **automatically when the five GitHub secrets below are
configured**, and skips both (with a log line) when they aren't. Unsigned builds ship ad-hoc
signed (Rust's linker default) and the generated cask carries a `postflight` that strips
`com.apple.quarantine` so Gatekeeper doesn't hard-block the GUI launch; signed + notarized
builds get a cask **without** that postflight (the workflow bakes the difference in via the
`sign` step's `signed` output). A manually-downloaded unsigned `.app` (not via brew) may still
need System Settings -> Privacy & Security -> "Open Anyway".

### One-time setup (needs an Apple Developer account, $99/yr)

Sign under **Hobo-Ware's own Apple Developer team** - it matches the app's
`dev.hoboware.stdusk` bundle id (Info.plist). The cert's Team ID becomes the app's stable
identity that macOS anchors permission grants to.

1. **Developer ID Application certificate**
   - <https://developer.apple.com/account/resources/certificates/add> -> type
     "Developer ID Application". Create the CSR with Keychain Access (Certificate Assistant ->
     Request a Certificate from a Certificate Authority -> Saved to disk), upload it, download
     the issued `.cer`, and double-click to add it to your login keychain.
   - Export it: Keychain Access -> My Certificates -> right-click the
     "Developer ID Application: …" entry (expand it - the private key must be included) ->
     Export -> `.p12`, and set an export password.
   - Base64 the file for the secret: `base64 -i DeveloperID.p12 | pbcopy`.
2. **App Store Connect API key** (for `notarytool`; app-specific passwords work too, but the
   API key doesn't depend on your Apple ID password)
   - <https://appstoreconnect.apple.com/access/integrations/api> -> Team Keys -> Generate. Role
     "Developer" suffices for notarization.
   - Note the **Key ID** and the page's **Issuer ID**, and download the `AuthKey_<ID>.p8`
     (single download - keep it safe).
3. **GitHub secrets** (repo Settings -> Secrets and variables -> Actions):

   | Secret | Value |
   |---|---|
   | `MACOS_CERT_P12` | the `.p12`, base64-encoded |
   | `MACOS_CERT_PASSWORD` | the `.p12` export password |
   | `NOTARY_KEY_ID` | the API key's Key ID (e.g. `2X9R4HXF34`) |
   | `NOTARY_ISSUER` | the Issuer ID (a UUID) |
   | `NOTARY_KEY` | the full contents of `AuthKey_<ID>.p8` |

With the secrets in place the next `stdusk-v*` tag produces a signed, notarized, stapled
`.app` (`codesign --deep --options runtime`, `notarytool submit --wait`, `stapler staple`,
verified with `spctl --assess`) and a cask with no quarantine postflight. Nothing else changes:
same tag flow, same assets, same tap copy step.
