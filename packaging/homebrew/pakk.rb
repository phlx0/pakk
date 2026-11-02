class Pakk < Formula
  desc "Create and extract zip, tar.gz, tar.zst, tar.xz and tar.bz2 archives on every core"
  homepage "https://github.com/phlx0/pakk"
  url "https://github.com/phlx0/pakk/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "REPLACE_WITH_SHA256_OF_THE_TAG_TARBALL"
  license "GPL-2.0-or-later"
  head "https://github.com/phlx0/pakk.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
    generate_completions_from_executable(bin/"pakk", "completions")
    (man1/"pakk.1").write Utils.safe_popen_read(bin/"pakk", "manpage")
  end

  test do
    (testpath/"in/hello.txt").write "hello"
    system bin/"pakk", "-q", "pack", "in", "-o", "in.tar.zst"
    system bin/"pakk", "-q", "unpack", "in.tar.zst", "-C", "out"
    assert_equal "hello", (testpath/"out/in/hello.txt").read
  end
end
