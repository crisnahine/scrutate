class Scrutate < Formula
  desc "Instant code search and a persistent code-intelligence index"
  homepage "https://github.com/scrutate/scrutate"
  url "https://github.com/scrutate/scrutate/archive/refs/tags/v0.1.0.tar.gz"
  # Filled in at release time: shasum -a 256 of the tarball at the URL above.
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license any_of: ["MIT", "Apache-2.0"]
  head "https://github.com/scrutate/scrutate.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/scrutate")
    generate_completions_from_executable(bin/"scrutate", "completions")
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/scrutate --version")
    (testpath/"hello.py").write "def greet():\n    return 'hello'\n"
    assert_match "hello.py:1:def greet", shell_output("#{bin}/scrutate grep --no-color greet #{testpath}")
  end
end
