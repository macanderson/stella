#!/usr/bin/env bash
#
# Guard: Cargo.lock resolves against the manifests as committed (#3332).
#
# `Cargo.lock` is tracked, and the paths that ship pin it: install.sh's
# `cargo install --locked --git --tag`, the Homebrew formula, release.yml's
# `cargo build --release --locked`, docker-serve.yml and msrv.yml. None of the
# everyday commands pass `--locked`, so a stale lock is invisible while you work
# and fails at release time — or, worse, on a user's machine installing from
# source.
#
# ci.yml has run exactly this check since it was added, in the `fmt + clippy +
# test` job. This guard is the same oracle moved to where it can act *earlier*:
# `make gate`, and therefore the pre-push hook. Two incidents on 2026-08-16 are
# the argument:
#
#   * #3311 added the stella-plugin crate, then merged a `main` that had bumped
#     the workspace version (0.9.47 -> 0.9.49) and thiserror (2.0.19 -> 2.0.20).
#     The merge took the new dependency versions but not a matching lock entry.
#     Both `--locked` CI jobs went red after review, not before the push.
#   * The release sync to 0.9.50 (#3323) then regenerated the lock while #3311
#     was still in flight. Both PRs were green; the composition left 23 crates
#     at 0.9.50 and stella-plugin alone at 0.9.49, and `main` was red for every
#     `--locked` build until #3333.
#
# What this guard does and does not buy, stated plainly because the difference
# is the whole design: it catches the FIRST shape — a branch whose lock is stale
# against its own merge result — on the author's machine, before the push. It
# cannot catch the SECOND: two branches that are each individually correct and
# collide only when both land. Nothing that runs pre-merge can, because neither
# author's tree is wrong. That half needs a post-merge canary on `main`, which
# #3332 tracks separately and this guard deliberately does not pretend to cover.
#
# `cargo metadata` rather than `--locked` on a build step, matching ci.yml's
# reasoning: it is a sub-second resolve that reports the drift precisely,
# instead of making a later failure ambiguous between "the lock is stale" and
# "the code is broken". It compiles nothing, which is what lets it sit in the
# guards-fast tier next to `cargo fmt --check`.
#
# Usage:
#   scripts/check-lockfile-sync.sh                 # this repository
#   scripts/check-lockfile-sync.sh --manifest-dir DIR
#
# `--manifest-dir` exists for scripts/test-lockfile-sync.sh, which builds
# throwaway path-only workspaces so the failure branch can be proven rather
# than assumed. A guard that cannot be shown to fail is indistinguishable from
# one that always passes.
set -euo pipefail

target_dir=""

while [ $# -gt 0 ]; do
  case "$1" in
  --manifest-dir)
    [ $# -ge 2 ] || {
      echo "check-lockfile-sync: --manifest-dir needs a directory" >&2
      exit 2
    }
    target_dir="$2"
    shift 2
    ;;
  -h | --help)
    # The whole comment block after the shebang, bounded by its first
    # non-comment line — hardcoded line numbers truncate silently the first
    # time the header grows.
    awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0"
    exit 0
    ;;
  *)
    echo "check-lockfile-sync: unknown argument '$1'" >&2
    exit 2
    ;;
  esac
done

if [ -z "$target_dir" ]; then
  target_dir="$(cd "$(dirname "$0")/.." && pwd)"
fi

if [ ! -d "$target_dir" ]; then
  echo "check-lockfile-sync: no such directory: $target_dir" >&2
  exit 2
fi

cd "$target_dir"

# The verdict is decided before anything is written (#1815): a guard that
# prints as it scans dies mid-report when its reader exits early (`| head -1`),
# and under `set -euo pipefail` that partial state becomes the exit status.
# scripts/check-gate-parity.sh is the shape being copied.
report=""
note() { report="${report}check-lockfile-sync: $1"$'\n'; }

emit() {
  trap '' PIPE
  printf '%s' "$report" >&2 || true
}

if ! command -v cargo >/dev/null 2>&1; then
  note "FAIL — cargo is not on PATH, so the lock cannot be resolved."
  note "     This guard is not skippable on a machine that runs the gate:"
  note "     a silent skip reads exactly like a pass, which is the failure"
  note "     mode the whole gate exists to prevent."
  emit
  exit 2
fi

# Deliberately NOT --offline. With a cold registry cache, --offline fails on a
# perfectly correct lock, and a guard that cries wolf gets bypassed. The cost of
# being right is an index fetch on the rare run where the lock really is stale.
# `--color never` so the reason below is the same bytes on a developer's
# terminal and in a CI log. cargo styles its diagnostics whenever the caller
# exports CLICOLOR_FORCE, and ANSI escapes in a guard's report are noise at
# best — and, in the one guard that parsed its own output, invalid input.
if err="$(cargo metadata --locked --color never --format-version 1 2>&1 >/dev/null)"; then
  emit
  printf 'check-lockfile-sync: OK — Cargo.lock resolves against the manifests.\n' || true
  exit 0
fi

note "FAIL — Cargo.lock is out of sync with the manifests."
note ""
note "cargo said:"
# A here-string, NOT `printf | while`: a piped loop runs in a subshell, so every
# `note` inside it appends to a copy of $report that dies with the subshell and
# the reason never reaches the reader. That is exactly how this guard's first
# draft printed an empty "cargo said:" while still exiting 1 — a failure that
# told you nothing is barely better than no guard.
while IFS= read -r line; do
  note "     $line"
done <<<"$err"
note ""
note "Fix it by regenerating the lock and committing it:"
note ""
note "       cargo metadata --format-version 1 >/dev/null   # or: cargo check --workspace"
note "       git add Cargo.lock"
note ""
note "Cargo.lock is tracked and install.sh builds --locked, so this reaches"
note "people installing from source, not only CI. Commit the regenerated lock"
note "in the same PR as the manifest change that moved it (#3332)."
emit
exit 1
