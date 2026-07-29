# Homebrew formula for the Stella CLI.
#
# Build-from-source formula: it compiles `stella` from the tagged source with
# cargo, so it stays correct across releases without per-release/per-platform
# bottle sha256 placeholders to maintain. Bump `tag:` (and, ideally, add a
# matching `revision:`) on each release.
#
# The stable `url` uses Homebrew's git download strategy (url ending in `.git`
# with a `tag:`), which needs no `sha256`. To pin exactly, add
# `revision: "<full-commit-sha>"` alongside the tag.
#
# To distribute prebuilt bottles later (faster installs, no Rust toolchain
# needed), publish tarballs from .github/workflows/release.yml and add a
# `bottle do ... end` block with `sha256` lines per platform, or move this
# formula into a `homebrew-tap` repo that CI updates automatically.
class Stella < Formula
  desc "Fast, BYOK, model-agnostic terminal coding agent"
  homepage "https://github.com/macanderson/stella"
  url "https://github.com/macanderson/stella.git", tag: "v0.5.77"
  version "0.5.77"
  license "AGPL-3.0-only"
  head "https://github.com/macanderson/stella.git", branch: "main"
  depends_on "rust" => :build

  def install
    # The tagged tree carries its own version since #786 (auto-tag stamps the
    # manifests in the release commit the tag points at), so no version
    # rewrite is needed before building — and `--locked` holds, because the
    # tagged lockfile is synced to the same version.
    system "cargo", "install", "--locked", "--root", prefix, "--path", "stella-cli"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/stella --version")
  end
end
