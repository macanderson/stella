#!/usr/bin/env bash
#
# Tests for scripts/check-lockfile-sync.sh (#3332).
#
# Hermetic: every case builds a throwaway workspace under mktemp with path-only
# members and no registry dependencies, so nothing here needs a network or the
# repository's own lock. `--manifest-dir` exists for exactly this.
#
# The cases that matter are the failure branches. A guard that cannot be shown
# to fail is indistinguishable from one that always passes — which is how the
# empty-diff check sat believed-green for a week (#1714), and it is not
# hypothetical here: this guard's first draft exited 1 correctly but printed an
# empty "cargo said:", because a `printf | while` loop appends to a $report that
# dies with its subshell. `the reason survives` below is that regression.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
guard="$repo_root/scripts/check-lockfile-sync.sh"

if ! command -v cargo >/dev/null 2>&1; then
  echo "skip test-lockfile-sync — no cargo toolchain on PATH"
  exit 0
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

pass=0
fail=0

# Build a minimal, network-free workspace at $1 whose single member is at
# version $2, and write a lock that matches it.
make_workspace() {
  dir="$1"
  version="$2"
  mkdir -p "$dir/crates/demo/src"
  cat >"$dir/Cargo.toml" <<TOML
[workspace]
members = ["crates/demo"]
resolver = "2"

[workspace.package]
version = "$version"
edition = "2021"
TOML
  cat >"$dir/crates/demo/Cargo.toml" <<'TOML'
[package]
name = "demo"
version.workspace = true
edition.workspace = true
TOML
  echo 'pub fn demo() {}' >"$dir/crates/demo/src/lib.rs"
  (cd "$dir" && cargo generate-lockfile --offline >/dev/null 2>&1)
}

# Run the guard over a fixture directory, asserting the exit code and that
# stdout+stderr contains `needle`.
expect() {
  name="$1"
  want_code="$2"
  needle="$3"
  shift 3
  set +e
  out="$("$guard" "$@" 2>&1)"
  code=$?
  set -e
  if [ "$code" -ne "$want_code" ]; then
    echo "FAIL  $name — exit $code, wanted $want_code"
    printf '      %s\n' "$out"
    fail=$((fail + 1))
    return
  fi
  case "$out" in
  *"$needle"*) ;;
  *)
    echo "FAIL  $name — output did not contain: $needle"
    printf '      %s\n' "$out"
    fail=$((fail + 1))
    return
    ;;
  esac
  echo "ok    $name"
  pass=$((pass + 1))
}

# ── a workspace whose lock matches passes ─────────────────────────────────
make_workspace "$tmp/clean" "0.1.0"
expect "a synced lock passes" 0 "OK — Cargo.lock resolves" --manifest-dir "$tmp/clean"

# ── the 2026-08-16 incident, reproduced ───────────────────────────────────
# The release sync bumped [workspace.package].version while the lock kept the
# old number for one member. This is the exact shape that reddened `main`
# twice in a day (#3311 then #3323 x #3311); it is the reason the guard exists,
# so it is the first thing proven.
make_workspace "$tmp/skewed" "0.1.0"
# sed-to-temp rather than `sed -i` (BSD and GNU disagree on -i's argument) or
# perl (an extra toolchain dependency for one substitution).
sed 's/^version = "0\.1\.0"$/version = "0.2.0"/' "$tmp/skewed/Cargo.toml" \
  >"$tmp/skewed/Cargo.toml.new"
mv "$tmp/skewed/Cargo.toml.new" "$tmp/skewed/Cargo.toml"
expect "a version bump without a relock fails" 1 "out of sync with the manifests" \
  --manifest-dir "$tmp/skewed"

# ── the reason must reach the reader ──────────────────────────────────────
# Regression for the subshell bug in the first draft. Exiting 1 with an empty
# "cargo said:" tells the author a lock is stale but not that it is the version
# — which is the entire content of the diagnosis.
expect "the reason survives the report buffer" 1 "cannot update the lock file" \
  --manifest-dir "$tmp/skewed"

# ── a missing lock is stale, not absent ───────────────────────────────────
# `--locked` refuses to author a lock at all, so "there is no lock" and "the
# lock is wrong" are the same verdict here. Worth pinning: a guard that treated
# a missing file as nothing-to-check would pass the worst case.
make_workspace "$tmp/nolock" "0.1.0"
rm -f "$tmp/nolock/Cargo.lock"
expect "a missing lock fails too" 1 "out of sync with the manifests" \
  --manifest-dir "$tmp/nolock"

# ── argument handling is a usage error, never a silent pass ───────────────
expect "an unknown argument exits 2" 2 "unknown argument" --nope
expect "--manifest-dir with no value exits 2" 2 "needs a directory" --manifest-dir
expect "a nonexistent directory exits 2" 2 "no such directory" \
  --manifest-dir "$tmp/does-not-exist"

# ── the report survives a reader that closes the pipe (#1815) ─────────────
# The guard buffers its whole verdict and emits once; `| head -1` must not turn
# a decided failure into some partial exit status.
set +e
"$guard" --manifest-dir "$tmp/skewed" 2>&1 | head -1 >/dev/null
code=${PIPESTATUS[0]}
set -e
if [ "$code" -eq 1 ]; then
  echo "ok    a truncated reader still gets exit 1"
  pass=$((pass + 1))
else
  echo "FAIL  a truncated reader changed the exit status to $code"
  fail=$((fail + 1))
fi

echo
echo "test-lockfile-sync: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
