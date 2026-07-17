# Reference Homebrew formula for stdusk. The release workflow (native-release.yml) regenerates
# this with the real url + sha256 for each build and attaches it to the GitHub Release as
# `stdusk.rb`. To ship it, copy that generated file into the tap repo (Hobo-Ware/homebrew-tap)
# at Formula/stdusk.rb. The version/sha below are placeholders.
class Stdusk < Formula
  desc "Native Rust quake terminal with a real GUI tab bar and ambient AI-CLI awareness"
  homepage "https://github.com/Hobo-Ware/stdusk"
  version "0.1.0"
  license "MIT"
  url "https://github.com/Hobo-Ware/stdusk/releases/download/stdusk-v0.1.0/stdusk-0.1.0-universal.app.zip"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  def install
    # Homebrew strips the single top-level dir, landing us inside stdusk.app/.
    (prefix/"stdusk.app").install "Contents"
    bin.install_symlink prefix/"stdusk.app/Contents/MacOS/stdusk"
  end

  test do
    assert_match "stdusk #{version}", shell_output("#{bin}/stdusk --version")
  end
end
