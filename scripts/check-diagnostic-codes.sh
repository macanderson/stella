#!/usr/bin/env bash
#
# Guard: every diagnostic code the tree emits is documented, once, in
# docs/reference/diagnostics.md — and nothing is documented that no longer
# emits. See docs/spec/diagnostics.md §11 and #2507.
#
# A record's `code` is public surface the way rustc's E0308 is: operators
# alert on it and docs link it. Public surface that nothing enumerates rots in
# both directions at once — a new code ships undocumented, a deleted emit site
# leaves its section behind reading as authority. This guard holds both
# directions, plus the two failure modes in between: two crates claiming one
# code, and a section whose generated facts have gone stale against the tree.
#
# It also holds the website's curated table
# (website/content/docs/diagnostics.mdx) to the same scan (#3045). That table
# picks which codes to surface and writes its own prose for them, and neither
# is generated; its `Code` and `Level` cells are facts about the tree, were
# transcribed by hand, and were read by no gate at all. The Makefile target
# runs here and in ci.yml; docs-guards.yml runs it too, because a PR that edits
# only the `.mdx` starts no Rust job.
#
# The real work lives in scripts/diagnostic-codes.py (`check` mode), which
# scans source rather than running a binary — so this stays cheap and works in
# `make guards-fast`, which compiles nothing. `make diag-reference` regenerates
# the reference's generated parts and preserves its hand-written prose.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

exec python3 ./scripts/diagnostic-codes.py check
