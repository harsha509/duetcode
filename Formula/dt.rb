class Dt < Formula
  desc "AI pair programming CLI — one model writes code, another reviews it"
  homepage "https://github.com/harsha509/duetcode"
  version "0.1.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/harsha509/duetcode/releases/download/v0.1.0/dt-macos-arm64.tar.gz"
    else
      url "https://github.com/harsha509/duetcode/releases/download/v0.1.0/dt-macos-amd64.tar.gz"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/harsha509/duetcode/releases/download/v0.1.0/dt-linux-amd64.tar.gz"
    end
  end

  def install
    bin.install "dt"
  end

  test do
    system "#{bin}/dt", "--version"
  end
end