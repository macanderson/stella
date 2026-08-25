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

# The prose row RUNS, rather than merely being present in the checks array
# (#4828). It cannot be exercised the way `compile` and `lockfile-sync` are:
# those two take `--manifest-dir` and can be pointed at a broken fixture,
# while `check-prose` reads the live tree and has no such switch — the same
# reason `file-size` has no red fixture case here either. So the assertion
# available is that the row reports its verdict at all, which is exactly what
# was missing when the canary called a prose-red `main` green.
expect "the prose row runs and reports" 0 "ok   prose" --manifest-dir "$tmp/clean"

# ── red: the 2026-08-16 incident ──────────────────────────────────────────
# The shared-cell shape: the version moved and the lock did not. Both PRs that
# composed into this were green on their own.
make_workspace "$tmp/red"
perl -0pi -e 's/^version = "0\.1\.0"$/version = "0.2.0"/m' "$tmp/red/Cargo.toml"
expect "a broken composition fails" 1 "FAIL — main is red" --manifest-dir "$tmp/red"
expect "and it names the failing check" 1 "lockfile-sync" --manifest-dir "$tmp/red"

# ── red: the 2026-08-18 incident — a tree that does not COMPILE ───────────
# The witness for #3660. The shared cell here is not a file but a seam: two
# PRs each redesigned how one function talks to its surface, both green, and
# git reported no conflict because neither side contained the other's lines.
# The composed tree was not parseable Rust (#3659). Before the `compile` row,
# THIS CASE PASSED — the canary read files and compiled nothing, so it ran on
# the broken commit and reported ok. A monitor blind to "does it build" is not
# a monitor.
make_workspace "$tmp/nocompile"
echo 'pub fn demo() { let x: u32 = "not a u32"; }' >"$tmp/nocompile/crates/demo/src/lib.rs"
expect "a tree that does not compile fails" 1 "FAIL — main is red" \
  --manifest-dir "$tmp/nocompile"
expect "and the compile row is named as the cause" 1 "compile" \
  --manifest-dir "$tmp/nocompile"
# The lock and the ceilings are fine here — only the code is broken. Without
# this the case above could pass for the wrong reason.
expect "while the file-reading checks still pass" 1 "ok   lockfile-sync" \
  --manifest-dir "$tmp/nocompile"
# The compiler's own diagnostic has to reach the issue: "compile failed" with
# no error in it sends the reader back to the Actions tab this canary exists
# to keep them out of.
expect "the issue carries the compiler's diagnostic" 1 "mismatched types" \
  --announce --dry-run --manifest-dir "$tmp/nocompile"

# A break confined to a test target is the one no other post-merge check sees,
# which is what `--all-targets` is for. `cargo check` alone would pass here.
make_workspace "$tmp/badtest"
cat >>"$tmp/badtest/crates/demo/src/lib.rs" <<'RS'

#[cfg(test)]
mod tests {
    #[test]
    fn t() {
        let _: u32 = "not a u32";
    }
}
RS
expect "a break confined to a test target is caught" 1 "compile" \
  --manifest-dir "$tmp/badtest"

# ── red + announce ────────────────────────────────────────────────────────
expect "a red run opens an issue" 1 "gh issue create" \
  --announce --dry-run --manifest-dir "$tmp/red"
expect "the issue carries the remediation" 1 "cargo metadata --format-version 1" \
  --announce --dry-run --manifest-dir "$tmp/red"

# The remedy must match the check that failed. It used to be one hardcoded
# lockfile recipe printed whatever had broken, so #3655 — a file-size failure —
# told its reader to regenerate `Cargo.lock`, which fixes nothing. An issue
# that misdirects costs more than one that only names the check.
expect "a compile failure gets the compile remedy" 1 "cargo check --workspace --all-targets" \
  --announce --dry-run --manifest-dir "$tmp/nocompile"
refute "and not the lockfile recipe that fixes nothing here" "git add Cargo.lock" \
  --announce --dry-run --manifest-dir "$tmp/nocompile"
expect "the issue explains the pre-merge blind spot" 1 "shared cell" \
  --announce --dry-run --manifest-dir "$tmp/red"
expect "the issue is labelled so only one stays open" 1 "main-red" \
  --announce --dry-run --manifest-dir "$tmp/red"

# The body must not assert that the break STARTED at the commit it tested
# (#4815). It cannot know that without checking the parent, which this run
# does not do. #4810 made exactly that claim about `ce6b1cf3c` — an innocent
# merge whose whole diff to the broken file was three of its own lines — and
# the first instinct of everyone reading it was to revert that commit. The
# real cause was many merges older.
expect "the issue says what it tested, not what broke" 1 "tested \`main\` at" \
  --announce --dry-run --manifest-dir "$tmp/red"
expect "and warns the cause may be older" 1 "not the commit that broke the tree" \
  --announce --dry-run --manifest-dir "$tmp/red"
refute "and never claims the break started there" "canary found \`main\` broken at" \
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
