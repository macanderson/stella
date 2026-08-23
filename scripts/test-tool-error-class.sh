#!/usr/bin/env bash
#
# Tests for check-tool-error-class.py's ratchet direction (#3167), mirroring
# scripts/test-typed-errors.sh -- see that script's header for why the
# defect it guards against (#3750, the same writer-default bug repeated in a
# second ratchet) is worth two assertions per case rather than one.
#
#   ./scripts/test-tool-error-class.sh
#
# Run it after touching that script. Not part of `make gate`: it builds
# throwaway workspace trees under $TMP, the same posture as
# `make typed-errors-test`.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-tool-error-class.py"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

# A throwaway workspace root: `crates/<name>/src` for the guard to walk, and
# the `scripts/` directory it reads its baseline from and writes it back to.
# $1 = case name. Echoes the root path.
new_root() {
  local dir="$TMP/$1"
  mkdir -p "$dir/scripts"
  printf '# test baseline\n' >"$dir/scripts/tool-error-class-baseline.txt"
  echo "$dir"
}

# A crate's src/lib.rs with $3 unclassified sites, plus one classified site
# so a case is never satisfied by a guard that counts every ToolOutput
# construction. $1 = root, $2 = crate, $3 = unclassified count.
crate() {
  local src="$1/crates/$2/src"
  mkdir -p "$src"
  {
    printf '//! Fixture crate.\n'
    awk -v n="$3" 'BEGIN {
      for (i = 0; i < n; i++)
        printf "fn v%d() -> ToolOutput { ToolOutput::error(\"boom\") }\n", i
    }'
    printf 'fn typed() -> ToolOutput { ToolOutput::classified_error(ErrorClass::Internal, "boom") }\n'
  } >"$src/lib.rs"
}

# Overwrite the baseline. $1 = root, then one "<crate> <count>" per entry.
set_baseline() {
  local dir="$1"
  shift
  {
    printf '# test baseline\n'
    for entry in "$@"; do printf '%s\n' "$entry"; done
  } >"$dir/scripts/tool-error-class-baseline.txt"
}

# want <name> <expect-pass|expect-fail> <root> [substring] [flag]
want() {
  local name="$1" expect="$2" root="$3" sub="${4:-}" flag="${5:-}" out rc
  out="$(python3 "$SCRIPT" ${flag:+"$flag"} "$root" 2>&1)"
  rc=$?
  if [ "$expect" = "expect-pass" ]; then
    if [ "$rc" -ne 0 ]; then
      fail=$((fail + 1)); echo "FAIL $name — expected OK, got:"; echo "$out"
      return
    fi
    case "$out" in
      *"$sub"*) pass=$((pass + 1)); echo "ok   $name" ;;
      *) fail=$((fail + 1)); echo "FAIL $name — passed, but did not report '$sub':"; echo "$out" ;;
    esac
    return
  fi
  if [ "$rc" -eq 0 ]; then
    fail=$((fail + 1)); echo "FAIL $name — the guard passed a violation it should have flagged:"; echo "$out"
    return
  fi
  case "$out" in
    *"$sub"*) pass=$((pass + 1)); echo "ok   $name" ;;
    *) fail=$((fail + 1)); echo "FAIL $name — flagged the wrong thing (wanted '$sub'):"; echo "$out" ;;
  esac
}

# entry_is <name> <root> <crate> <count|absent>
entry_is() {
  local name="$1" file="$2/scripts/tool-error-class-baseline.txt" crate="$3" expect="$4" got
  got="$(awk -v c="$crate" '$1 == c { print $2; exit }' "$file")"
  [ -n "$got" ] || got=absent
  if [ "$got" = "$expect" ]; then
    pass=$((pass + 1)); echo "ok   $name"
  else
    fail=$((fail + 1)); echo "FAIL $name — baseline says '$got', wanted '$expect':"; cat "$file"
  fi
}

# ── U: the #3750-shaped defect — a crate with NO baseline entry ──────────────
r="$(new_root new_crate)"
crate "$r" stella-fixture 1

want "U1 a crate absent from the baseline is flagged on its first unclassified site" \
  expect-fail "$r" "stella-fixture: 1 unclassified"

want "U2 --update refuses it rather than grandfathering it" \
  expect-fail "$r" "refusing to add stella-fixture at 1" "--update"

entry_is "U3 the refused --update wrote no entry for it" "$r" stella-fixture absent

r="$(new_root raise_existing)"
crate "$r" stella-fixture 3
set_baseline "$r" "stella-fixture 1"
want "U4 raising an existing entry is still refused" \
  expect-fail "$r" "refusing to raise stella-fixture from 1 to 3" "--update"
entry_is "U5 the refused --update left the existing entry alone" "$r" stella-fixture 1

# ── D: the down-only direction ────────────────────────────────────────────────
r="$(new_root down_only)"
crate "$r" stella-fixture 1
set_baseline "$r" "stella-fixture 3"
want "D1 the check notes a crate that dropped below its ceiling" \
  expect-pass "$r" "note: stella-fixture is down to 1 (ratchet says 3)"
want "D2 --update locks the win in" \
  expect-pass "$r" "(1 remaining)" "--update"
entry_is "D3 the entry was lowered to what is really there" "$r" stella-fixture 1

r="$(new_root cleared)"
crate "$r" stella-fixture 0
set_baseline "$r" "stella-fixture 2"
want "D4 --update drops a crate that reached zero" \
  expect-pass "$r" "(0 remaining)" "--update"
entry_is "D5 the cleared crate is gone from the baseline" "$r" stella-fixture absent

# ── N: the negative direction ─────────────────────────────────────────────────
r="$(new_root at_ceiling)"
crate "$r" stella-fixture 2
set_baseline "$r" "stella-fixture 2"
want "N1 a crate exactly at its ceiling passes" expect-pass "$r"

r="$(new_root clean)"
crate "$r" stella-fixture 0
want "N2 a crate with only classified sites passes with no entry" expect-pass "$r"

# A classified site must never be counted, however many there are — the
# whole point of the migration-friendly `error()` vs. `classified_error()`
# split (#3145).
r="$(new_root classified_only)"
mkdir -p "$r/crates/stella-fixture/src"
{
  printf '//! Fixture crate.\n'
  for i in 1 2 3; do
    printf 'fn v%d() -> ToolOutput { ToolOutput::classified_error(ErrorClass::NotFound, "boom") }\n' "$i"
  done
} >"$r/crates/stella-fixture/src/lib.rs"
want "N3 classified_error sites are never counted" expect-pass "$r"

# Test code is out of scope, the same way check-typed-errors.py excludes it:
# a fixture must not inflate the audit backlog.
r="$(new_root test_code)"
mkdir -p "$r/crates/stella-fixture/src"
{
  printf '//! Fixture crate.\n'
  printf 'fn prod() -> ToolOutput { ToolOutput::classified_error(ErrorClass::NotFound, "boom") }\n'
  printf '#[cfg(test)]\n'
  printf 'mod tests {\n'
  printf '    fn t() -> ToolOutput { ToolOutput::error("boom") }\n'
  printf '}\n'
} >"$r/crates/stella-fixture/src/lib.rs"
want "N4 a #[cfg(test)] block is out of scope" expect-pass "$r"

mkdir -p "$r/crates/stella-fixture/src/tests"
printf 'fn t() -> ToolOutput { ToolOutput::error("boom") }\n' \
  >"$r/crates/stella-fixture/src/tests/helpers.rs"
want "N5 a tests/ directory is out of scope" expect-pass "$r"

printf 'fn t() -> ToolOutput { ToolOutput::error("boom") }\n' \
  >"$r/crates/stella-fixture/src/tests.rs"
want "N6 a tests.rs file is out of scope" expect-pass "$r"

# Unlike check-typed-errors.py this guard is NOT library-only: a binary
# crate's unclassified site is exactly as real a gap as a library one.
r="$(new_root binary_crate)"
mkdir -p "$r/crates/stella-bin/src"
{
  printf 'fn run() -> ToolOutput { ToolOutput::error("boom") }\n'
  printf 'fn main() {}\n'
} >"$r/crates/stella-bin/src/main.rs"
want "N7 a binary crate IS counted (unlike the typed-errors guard)" \
  expect-fail "$r" "stella-bin: 1 unclassified"

# ── R: the real workspace ─────────────────────────────────────────────────────
# Check mode only. Never `--update` here: that would rewrite this
# repository's baseline as a side effect of running the tests.
want "R1 this repository still matches its baseline" expect-pass "$repo_root"

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
