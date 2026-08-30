#!/usr/bin/env bash
#
# Tests for check-module-layout.py: a new pair fails, a listed pair passes,
# and --update removes entries but refuses to add one.
#
#   ./scripts/test-module-layout.sh
#
# Fixtures are their own git roots because the guard enumerates with
# `git ls-files`, and because `--update` writes -- pointing it at this
# repository would rewrite scripts/module-layout-baseline.txt as a side
# effect of running the tests.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-module-layout.py"
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

# $1 = root, $2 = relative path.
code() {
  mkdir -p "$(dirname "$1/$2")"
  printf 'pub fn f() {}\n' >"$1/$2"
  (cd "$1" && git add -A) >/dev/null 2>&1
}

baseline() {
  root="$1"
  shift
  printf '# test baseline\n' >"$root/scripts/module-layout-baseline.txt"
  while [ $# -gt 0 ]; do
    printf '%s\n' "$1" >>"$root/scripts/module-layout-baseline.txt"
    shift
  done
}

# ── M1: a tree with no pair passes ───────────────────────────────────────────
r="$(new_root m1)"
code "$r" crates/a/src/lib.rs
code "$r" crates/a/src/store/mod.rs
baseline "$r"
if python3 "$SCRIPT" "$r" >/dev/null 2>&1; then
  ok "M1 a clean tree passes"
else
  no "M1 a clean tree passes" "the guard exited non-zero"
fi

# ── M2: a new pair fails (the witness) ───────────────────────────────────────
r="$(new_root m2)"
code "$r" crates/a/src/store.rs
code "$r" crates/a/src/store/edge.rs
baseline "$r"
if python3 "$SCRIPT" "$r" >/dev/null 2>&1; then
  no "M2 a new pair fails" "the guard exited 0"
else
  ok "M2 a new pair fails"
fi

# ── M3: a listed pair passes ─────────────────────────────────────────────────
r="$(new_root m3)"
code "$r" crates/a/src/store.rs
code "$r" crates/a/src/store/edge.rs
baseline "$r" crates/a/src/store.rs
if python3 "$SCRIPT" "$r" >/dev/null 2>&1; then
  ok "M3 a listed pair passes"
else
  no "M3 a listed pair passes" "the guard exited non-zero"
fi

# ── M4: --update refuses to add a new pair ───────────────────────────────────
r="$(new_root m4)"
code "$r" crates/a/src/store.rs
code "$r" crates/a/src/store/edge.rs
baseline "$r"
if python3 "$SCRIPT" --update "$r" >/dev/null 2>&1; then
  no "M4 --update refuses a new pair" "--update exited 0"
else
  ok "M4 --update refuses a new pair"
fi
if grep -q "crates/a/src/store.rs" "$r/scripts/module-layout-baseline.txt"; then
  no "M4 --update wrote nothing" "the refused pair landed in the baseline"
else
  ok "M4 --update wrote nothing"
fi

# ── M5: --update retires an entry whose pair is gone ─────────────────────────
r="$(new_root m5)"
code "$r" crates/a/src/store/mod.rs
code "$r" crates/a/src/store/edge.rs
baseline "$r" crates/a/src/store.rs
if python3 "$SCRIPT" --update "$r" >/dev/null 2>&1; then
  if grep -q "crates/a/src/store.rs" "$r/scripts/module-layout-baseline.txt"; then
    no "M5 --update retires a split pair" "the entry survived"
  else
    ok "M5 --update retires a split pair"
  fi
else
  no "M5 --update retires a split pair" "--update exited non-zero"
fi

# ── M6: a stale entry does not fail the plain check ──────────────────────────
# Two branches each split one pair and merge; neither branch swept the other's
# entry. The plain check tolerates that, so the merge composes green.
r="$(new_root m6)"
code "$r" crates/a/src/store/mod.rs
baseline "$r" crates/a/src/store.rs
if python3 "$SCRIPT" "$r" >/dev/null 2>&1; then
  ok "M6 a stale entry does not fail the plain check"
else
  no "M6 a stale entry does not fail the plain check" "the guard exited non-zero"
fi

# ── M7: --bootstrap refuses to overwrite ─────────────────────────────────────
r="$(new_root m7)"
code "$r" crates/a/src/lib.rs
baseline "$r"
if python3 "$SCRIPT" --bootstrap "$r" >/dev/null 2>&1; then
  no "M7 --bootstrap refuses to overwrite" "it exited 0"
else
  ok "M7 --bootstrap refuses to overwrite"
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
