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
  url "https://github.com/macanderson/stella.git", tag: "v0.5.61"
  version "0.5.61"
  license "AGPL-3.0-only"
  head "https://github.com/macanderson/stella.git", branch: "main"
  depends_on "rust" => :build

  def install
    # The tagged tree still carries the PREVIOUS release's version: auto-tag
    # tags the merge commit first and lands the version-sync manifest bump
    # afterwards (see RELEASING.md), so building the tag verbatim ships a
    # binary whose `--version` is one release behind — and fails the test
    # block below. Mirror release.yml's "Stamp release version" step: rewrite
    # the single [workspace.package] version line before building. HEAD builds
    # skip the stamp; main's manifest is already synced.
    inreplace "Cargo.toml", /^version = ".*"$/, "version = \"#{version}\"" if build.stable?

    # Builds the `stella` binary from the `stella-cli` package and installs it
    # into the formula prefix. `--locked` (which std_cargo_args would add) is
    # deliberately absent: the stamp makes the lockfile's workspace-member
    # entries stale. Registry dependencies stay pinned by the existing
    # Cargo.lock — cargo rewrites only the workspace entries, the same trade
    # release.yml makes.
    system "cargo", "install", "--root", prefix, "--path", "stella-cli"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/stella --version")
  end
end
