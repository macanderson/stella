#!/usr/bin/env bash
#
# Tests for check-module-siblings.py's ratchet direction.
#
#   ./scripts/test-module-siblings.sh
#
# Run it after touching that script. Not part of `make gate`: it builds
# throwaway git trees, the same posture as `make prose-test`.
#
# The guard's promise is one sentence: a `foo.rs` beside a `foo/` fails unless
# the baseline already records it, and the baseline may never gain a line. S2 is
# the witness — the case that fails before the guard exists and passes after,
# and the one that would have caught the pair a merged pull request added.
#
# S5 and S6 are the half a ratchet usually gets wrong, which is the writer
# rather than the checker: an `--update` that records a first-time pair turns
# the gate green without moving a file, and a `--bootstrap` that runs twice
# re-grandfathers whatever landed in between.
#
# Fixtures are their own git roots because the guard enumerates with
# `git ls-files`, and because `--update` and `--bootstrap` WRITE — pointing them
# at this repository would rewrite the real baseline as a side effect.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-module-siblings.py"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

ok() {
  pass=$((pass + 1))
  printf 'ok   %s\n' "$1"
}

no() {
  fail=$((fail + 1))
  printf 'FAIL %s -- %s\n' "$1" "$2"
}

# A throwaway repository root with a scripts/ directory for the baseline.
# $1 = case name. Echoes the root path.
new_root() {
  dir="$TMP/$1"
  mkdir -p "$dir/scripts"
  (cd "$dir" && git init -q .) >/dev/null 2>&1
  echo "$dir"
}

# $1 = root, $2 = crate, $3 = module stem. Writes `<stem>.rs` AND `<stem>/`,
# which is the forbidden pair.
pair() {
  mkdir -p "$1/crates/$2/src/$3"
  printf '// a module\n' >"$1/crates/$2/src/$3.rs"
  printf '// a submodule\n' >"$1/crates/$2/src/$3/inner.rs"
  (cd "$1" && git add -A) >/dev/null 2>&1
}

# $1 = root, $2 = crate, $3 = module stem. The permitted shape: no sibling file.
folder_only() {
  mkdir -p "$1/crates/$2/src/$3"
  printf '// a module\n' >"$1/crates/$2/src/$3/mod.rs"
  printf '// a submodule\n' >"$1/crates/$2/src/$3/inner.rs"
  (cd "$1" && git add -A) >/dev/null 2>&1
}

# $1 = root, then the paths to record.
baseline() {
  root="$1"
  shift
  printf '# test baseline\n' >"$root/scripts/module-siblings-baseline.txt"
  for path in "$@"; do
    printf '%s\n' "$path" >>"$root/scripts/module-siblings-baseline.txt"
  done
}

expect_pass() {
  if python3 "$SCRIPT" "$2" >/dev/null 2>&1; then ok "$1"; else no "$1" "exited non-zero"; fi
}

expect_fail() {
  if python3 "$SCRIPT" "$2" >/dev/null 2>&1; then no "$1" "exited 0"; else ok "$1"; fi
}

# ── S1: a folder with no sibling file passes ─────────────────────────────────
r="$(new_root s1)"
folder_only "$r" alpha engine
baseline "$r"
expect_pass "S1 a folder with only mod.rs passes" "$r"

# ── S2: a first-time pair fails (the witness) ────────────────────────────────
r="$(new_root s2)"
pair "$r" alpha engine
baseline "$r"
expect_fail "S2 a new foo.rs beside foo/ fails" "$r"

# ── S3: a recorded pair passes ───────────────────────────────────────────────
r="$(new_root s3)"
pair "$r" alpha engine
baseline "$r" crates/alpha/src/engine.rs
expect_pass "S3 a recorded pair passes" "$r"

# ── S4: a recorded pair plus a new one still fails ───────────────────────────
r="$(new_root s4)"
pair "$r" alpha engine
pair "$r" alpha router
baseline "$r" crates/alpha/src/engine.rs
expect_fail "S4 recording one pair does not forgive another" "$r"

# ── S5: --update refuses to record a first-time pair ─────────────────────────
# Two assertions, because this defect fails both ways: a writer can refuse
# loudly and still write the line.
r="$(new_root s5)"
pair "$r" alpha engine
baseline "$r"
if python3 "$SCRIPT" --update "$r" >/dev/null 2>&1; then
  no "S5 --update refuses to grow" "it exited 0"
else
  ok "S5 --update refuses to grow"
fi
if grep -q 'engine.rs' "$r/scripts/module-siblings-baseline.txt"; then
  no "S5 --update wrote no entry" "the pair was recorded anyway"
else
  ok "S5 --update wrote no entry"
fi

# ── S6: --bootstrap closes behind itself ─────────────────────────────────────
# A second run would re-grandfather whatever landed after the first, inside a
# diff whose stated purpose was something else.
r="$(new_root s6)"
pair "$r" alpha engine
baseline "$r"
if python3 "$SCRIPT" --bootstrap "$r" >/dev/null 2>&1; then
  no "S6 --bootstrap refuses to overwrite" "it exited 0"
else
  ok "S6 --bootstrap refuses to overwrite"
fi

# ── S7: --update retires a pair that was resolved ────────────────────────────
r="$(new_root s7)"
folder_only "$r" alpha engine
baseline "$r" crates/alpha/src/engine.rs
expect_fail "S7 a stale entry is reported, not ignored" "$r"
if python3 "$SCRIPT" --update "$r" >/dev/null 2>&1; then
  ok "S7 --update accepts the shrink"
else
  no "S7 --update accepts the shrink" "it exited non-zero"
fi
if grep -q 'engine.rs' "$r/scripts/module-siblings-baseline.txt"; then
  no "S7 --update dropped the dead entry" "it is still listed"
else
  ok "S7 --update dropped the dead entry"
fi

# ── S8: an integration-test pair is out of scope ─────────────────────────────
# `tests/foo.rs` beside `tests/foo/` is the layout cargo REQUIRES of a test
# binary with helper modules — the entry point cannot be `tests/foo/mod.rs`.
r="$(new_root s8)"
mkdir -p "$r/crates/alpha/tests/wire_contract"
printf '// test entry point\n' >"$r/crates/alpha/tests/wire_contract.rs"
printf '// helper\n' >"$r/crates/alpha/tests/wire_contract/samples.rs"
(cd "$r" && git add -A) >/dev/null 2>&1
baseline "$r"
expect_pass "S8 a tests/ pair is not the rule's subject" "$r"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
