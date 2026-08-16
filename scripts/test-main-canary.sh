#!/usr/bin/env bash
#
# Tests for scripts/main-canary.sh (#3332).
#
# Hermetic: the red cases drive the canary at a throwaway workspace via
# --manifest-dir, and every announcing case runs under --dry-run, so nothing
# here needs a network, a GH_TOKEN, or the repository's real issues. A monitor
# that files issues is precisely the kind of script you cannot test by running
# it for real.
#
# The cases that matter are the announcing branches. A canary that cannot be
# shown to open an issue when main is red, and to CLOSE it when main recovers,
# is indistinguishable from one that always says "green" — and a monitor
# trusted on that basis is worse than no monitor, because it is also an excuse
# not to look.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
canary="$repo_root/scripts/main-canary.sh"

if ! command -v cargo >/dev/null 2>&1; then
  echo "skip test-main-canary — no cargo toolchain on PATH"
  exit 0
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

pass=0
fail=0

# A minimal, network-free workspace at $1 whose lock matches its manifests.
make_workspace() {
  dir="$1"
  mkdir -p "$dir/crates/demo/src"
  cat >"$dir/Cargo.toml" <<'TOML'
[workspace]
members = ["crates/demo"]
resolver = "2"

[workspace.package]
version = "0.1.0"
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

expect() {
  name="$1"
  want_code="$2"
  needle="$3"
  shift 3
  set +e
  out="$("$canary" "$@" 2>&1)"
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

refute() {
  name="$1"
  needle="$2"
  shift 2
  set +e
  out="$("$canary" "$@" 2>&1)"
  set -e
  case "$out" in
  *"$needle"*)
    echo "FAIL  $name — output should NOT have contained: $needle"
    printf '      %s\n' "$out"
    fail=$((fail + 1))
    ;;
  *)
    echo "ok    $name"
    pass=$((pass + 1))
    ;;
  esac
}

# ── green ─────────────────────────────────────────────────────────────────
make_workspace "$tmp/clean"
expect "a composing tree passes" 0 "OK — main composes green" --manifest-dir "$tmp/clean"

# A green run must not file anything. This is the case that keeps the canary
# worth reading: a monitor that comments on healthy days gets muted.
refute "a green run announces nothing" "gh issue create" \
  --announce --dry-run --manifest-dir "$tmp/clean"

# ── red: the 2026-08-16 incident ──────────────────────────────────────────
# The shared-cell shape: the version moved and the lock did not. Both PRs that
# composed into this were green on their own.
make_workspace "$tmp/red"
perl -0pi -e 's/^version = "0\.1\.0"$/version = "0.2.0"/m' "$tmp/red/Cargo.toml"
expect "a broken composition fails" 1 "FAIL — main is red" --manifest-dir "$tmp/red"
expect "and it names the failing check" 1 "lockfile-sync" --manifest-dir "$tmp/red"

# ── red + announce ────────────────────────────────────────────────────────
expect "a red run opens an issue" 1 "gh issue create" \
  --announce --dry-run --manifest-dir "$tmp/red"
expect "the issue carries the remediation" 1 "cargo metadata --format-version 1" \
  --announce --dry-run --manifest-dir "$tmp/red"
expect "the issue explains the pre-merge blind spot" 1 "shared cell" \
  --announce --dry-run --manifest-dir "$tmp/red"
expect "the issue is labelled so only one stays open" 1 "main-red" \
  --announce --dry-run --manifest-dir "$tmp/red"

# The label does not exist in this repository yet, and `gh issue create
# --label` fails outright on an unknown label — the canary would have broken at
# the exact moment it first had something to say. Creating it is idempotent.
expect "the label is provisioned before the issue" 1 "gh label create main-red" \
  --announce --dry-run --manifest-dir "$tmp/red"

# ── recovery: the branch that keeps this worth reading ────────────────────
# A canary that only ever opens issues becomes a stale-issue generator and gets
# muted, at which point it is worse than nothing. Recovery only runs when an
# issue is already open, hence --fixture-open-issue.
expect "main going green closes the open issue" 0 "gh issue close 42" \
  --announce --dry-run --fixture-open-issue 42 --manifest-dir "$tmp/clean"
expect "and says so on the way out" 0 "main recovered — closed #42" \
  --announce --dry-run --fixture-open-issue 42 --manifest-dir "$tmp/clean"

# ── a red main that stays red is ONE issue, not one per merge ─────────────
# Ten merges against a broken tree must produce ten comments on one issue. The
# opposite — an issue per run — is how a monitor earns a mute filter.
expect "a still-red main comments instead of filing again" 1 "gh issue comment 42" \
  --announce --dry-run --fixture-open-issue 42 --manifest-dir "$tmp/red"
refute "and does not open a second issue" "gh issue create" \
  --announce --dry-run --fixture-open-issue 42 --manifest-dir "$tmp/red"

# ── argument handling ─────────────────────────────────────────────────────
# --dry-run alone reads as a configured monitor while reporting to nobody.
expect "--dry-run without --announce is refused" 2 "only means something with --announce" \
  --dry-run --manifest-dir "$tmp/clean"
expect "an unknown argument exits 2" 2 "unknown argument" --nope
expect "--label with no value exits 2" 2 "--label needs a value" --label

# ── the verdict survives a reader that closes the pipe (#1815) ────────────
# The canary prints per check rather than buffering one report, so a `| head -1`
# reader can SIGPIPE it mid-write. Under `set -euo pipefail` that would turn a
# decided "main is red" into whatever partial status the write died with — a
# monitor that reports green because someone truncated its output is the worst
# failure this repository has a name for. Ten runs, all of which must agree
# with the unpiped verdict.
sig_fail=0
for _ in 1 2 3 4 5 6 7 8 9 10; do
  set +e
  "$canary" --manifest-dir "$tmp/red" 2>&1 | head -1 >/dev/null
  code=${PIPESTATUS[0]}
  set -e
  [ "$code" -eq 1 ] || sig_fail=1
done
if [ "$sig_fail" -eq 0 ]; then
  echo "ok    a truncated reader still gets exit 1"
  pass=$((pass + 1))
else
  echo "FAIL  a truncated reader changed the red verdict"
  fail=$((fail + 1))
fi

echo
echo "test-main-canary: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
