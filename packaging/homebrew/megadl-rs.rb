# Generated from packaging/homebrew/megadl-rs.rb in apiplant/megadl-rs by the
# release workflow, which fills in the version and checksums and commits the
# result to apiplant/homebrew-tap as Formula/megadl-rs.rb. Changes belong in
# the source repository: the next release overwrites this file.
class MegadlRs < Formula
  desc "Command-line downloader and terminal download manager for mega.nz links"
  homepage "https://github.com/apiplant/megadl-rs"
  version "@VERSION@"
  license "MIT"

  # No bottles: the release archives *are* the binaries, so the formula only
  # unpacks what the tagged workflow already built for each platform.
  on_macos do
    on_arm do
      url "https://github.com/apiplant/megadl-rs/releases/download/v@VERSION@/megadl-rs-v@VERSION@-aarch64-apple-darwin.tar.gz"
      sha256 "@SHA_MACOS_ARM64@"
    end
  end
  on_linux do
    on_intel do
      url "https://github.com/apiplant/megadl-rs/releases/download/v@VERSION@/megadl-rs-v@VERSION@-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "@SHA_LINUX_X86_64@"
    end
    on_arm do
      url "https://github.com/apiplant/megadl-rs/releases/download/v@VERSION@/megadl-rs-v@VERSION@-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "@SHA_LINUX_ARM64@"
    end
  end

  def install
    bin.install "megadl"
    bin.install "megadl-tui"
    doc.install "README.md"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/megadl --version").split(" ").last
    assert_match version.to_s, shell_output("#{bin}/megadl-tui --version").split(" ").last
  end
end
