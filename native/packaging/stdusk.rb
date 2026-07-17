# Reference Homebrew CASK for stdusk (a GUI .app -> a cask, not a formula, so it lands in
# /Applications and Spotlight/Launchpad find it). The release workflow (native-release.yml)
# regenerates this with the real version + sha256 per build and attaches it to the GitHub Release
# as `stdusk.rb`; ship it by copying that into the tap at Hobo-Ware/homebrew-tap:Casks/stdusk.rb.
# The version/sha below are placeholders.
cask "stdusk" do
  version "0.1.0"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  url "https://github.com/Hobo-Ware/stdusk/releases/download/stdusk-v#{version}/stdusk-#{version}-universal.app.zip"
  name "stdusk"
  desc "Native Rust quake terminal with a real GUI tab bar and ambient AI-CLI awareness"
  homepage "https://github.com/Hobo-Ware/stdusk"

  app "stdusk.app"
  binary "#{appdir}/stdusk.app/Contents/MacOS/stdusk"

  postflight do
    # Ad-hoc signed, not notarized: strip quarantine so Gatekeeper doesn't hard-block the GUI
    # launch. Proper fix is Developer ID signing + notarization (see packaging/README.md).
    system_command "/usr/bin/xattr",
                   args: ["-dr", "com.apple.quarantine", "#{appdir}/stdusk.app"]
  end

  zap trash: "~/.config/stdusk"
end
