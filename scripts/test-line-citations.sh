#!/usr/bin/env bash
#
# Tests for check-line-citations.py's ratchet direction and its scope.
#
#   ./scripts/test-line-citations.sh
#
# Run it after touching that script. Not part of `make gate`: it builds
# throwaway git trees, the same posture as `make prose-test`.
#
# The guard's promise is one sentence: a file may shrink its count of
# line-pinned citations, never grow it, and a file absent from the baseline
# must be at zero. L2 is the witness -- the case that fails before the guard
# exists and passes after.
#
# L5 and L6 are the scope. A line number inside running code is data (a parser
# fixture, a test's expected output) and must not be read as a citation, or
# every crate with a diagnostic-rendering test fails a prose guard.
#
# Fixtures are their own git roots because the guard enumerates with
# `git ls-files`, and because `--update` WRITES -- pointing it at this
# repository would rewrite scripts/line-citations-baseline.txt as a side
# effect of running the tests.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-line-citations.py"
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

new_root() {
  dir="$TMP/$1"
  mkdir -p "$dir/scripts"
  (cd "$dir" && git init -q .) >/dev/null 2>&1
  echo "$dir"
}

# $1 = root, $2 = relative path, body on stdin.
doc() {
  mkdir -p "$(dirname "$1/$2")"
  cat >"$1/$2"
  (cd "$1" && git add -A) >/dev/null 2>&1
}

# $1 = root, then `path count` pairs.
baseline() {
  root="$1"
  shift
  printf '# test baseline\n' >"$root/scripts/line-citations-baseline.txt"
  while [ $# -gt 1 ]; do
    printf '%s %s\n' "$1" "$2" >>"$root/scripts/line-citations-baseline.txt"
    shift 2
  done
}

expect_pass() {
  if python3 "$SCRIPT" "$2" >/dev/null 2>&1; then ok "$1"; else no "$1" "exited non-zero"; fi
}

expect_fail() {
  if python3 "$SCRIPT" "$2" >/dev/null 2>&1; then no "$1" "exited 0"; else ok "$1"; fi
}

expect_absent() {
  if grep -q "^$3 " "$2/scripts/line-citations-baseline.txt" 2>/dev/null; then
    no "$1" "$3 is listed in the baseline"
  else
    ok "$1"
  fi
}

# ── L1: a citation by name passes ────────────────────────────────────────────
r="$(new_root l1)"
doc "$r" docs/a.md <<'EOF'
What `dispatch` does, in order (`fleet.rs`).
EOF
baseline "$r"
expect_pass "L1 a citation by name passes" "$r"

# ── L2: a first-time line-pinned citation fails (the witness) ────────────────
r="$(new_root l2)"
doc "$r" docs/a.md <<'EOF'
What `dispatch` does, in order (`fleet.rs:463`).
EOF
baseline "$r"
expect_fail "L2 first-time line citation fails" "$r"

# ── L3: a grandfathered count passes ─────────────────────────────────────────
r="$(new_root l3)"
doc "$r" docs/a.md <<'EOF'
See `fleet.rs:463`.
EOF
baseline "$r" docs/a.md 1
expect_pass "L3 grandfathered count passes" "$r"

# ── L4: growing past the baseline fails ──────────────────────────────────────
r="$(new_root l4)"
doc "$r" docs/a.md <<'EOF'
See `fleet.rs:463` and `ledger.rs:178`.
EOF
baseline "$r" docs/a.md 1
expect_fail "L4 growth past baseline fails" "$r"

# ── L5: a line number in running code is data, not a citation ───────────────
r="$(new_root l5)"
doc "$r" src/render.rs <<'EOF'
fn render() -> String {
    format!("{}:{}", "src/a.rs", 42)
}

#[test]
fn it_renders() {
    assert_eq!(render(), "src/a.rs:42");
}
EOF
baseline "$r"
expect_pass "L5 a line number in code is not a citation" "$r"

# ── L6: the same number in a doc comment IS a citation ──────────────────────
r="$(new_root l6)"
doc "$r" src/render.rs <<'EOF'
/// Mirrors the shape `src/a.rs:42` renders.
pub fn render() {}
EOF
baseline "$r"
expect_fail "L6 a line citation in a comment fails" "$r"

# ── L7: --update refuses to grandfather a first-time offender ───────────────
r="$(new_root l7)"
doc "$r" docs/a.md <<'EOF'
See `fleet.rs:463`.
EOF
baseline "$r"
if python3 "$SCRIPT" --update "$r" >/dev/null 2>&1; then
  no "L7 --update refuses to raise" "--update exited 0"
else
  ok "L7 --update refuses to raise"
fi
expect_absent "L7 --update wrote no entry" "$r" "docs/a.md"

# ── L8: --update retightens when a citation is rewritten ────────────────────
r="$(new_root l8)"
doc "$r" docs/a.md <<'EOF'
What `dispatch` does, in order (`fleet.rs`).
EOF
baseline "$r" docs/a.md 3
if python3 "$SCRIPT" --update "$r" >/dev/null 2>&1; then
  ok "L8 --update accepts a shrink"
else
  no "L8 --update accepts a shrink" "--update exited non-zero"
fi
expect_absent "L8 --update drops a file that reached zero" "$r" "docs/a.md"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
