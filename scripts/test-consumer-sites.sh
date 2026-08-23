#!/usr/bin/env bash
#
# Tests for check-consumer-sites.sh (#4459).
#
#   ./scripts/test-consumer-sites.sh
#
# Not part of `make gate` — it builds throwaway fixture trees, the same
# posture as test-module-reachability.sh. Run it after touching the guard.
#
# The guard's own tree is clean by construction (it is a gate step), so
# proving it can FAIL needs a fixture with a site pointing nowhere — there is
# no such row in the real tree to fail on. Each case below builds the shape
# it is about and points the guard at it via its two overrides ($1 for the
# tags-shaped file, CRATES_ROOT for where site paths resolve).
#
# bash 3.2 compatible.
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-consumer-sites.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

# want <name> <expect-pass|expect-fail> <tags-file> <crates-root> [substring]
want() {
  local name="$1" expect="$2" tags="$3" root="$4" sub="${5:-}" out rc
  out="$(CRATES_ROOT="$root" "$SCRIPT" "$tags" 2>&1)"
  rc=$?
  if [ "$expect" = "expect-pass" ]; then
    if [ "$rc" -eq 0 ]; then
      pass=$((pass + 1)); echo "ok   $name"
    else
      fail=$((fail + 1)); echo "FAIL $name — expected OK, got:"; echo "$out"
    fi
    return
  fi
  if [ "$rc" -eq 0 ]; then
    fail=$((fail + 1)); echo "FAIL $name — the guard passed a dead site:"; echo "$out"
    return
  fi
  case "$out" in
  *"$sub"*) pass=$((pass + 1)); echo "ok   $name" ;;
  *) fail=$((fail + 1)); echo "FAIL $name — failed for the wrong reason (wanted '$sub'):"; echo "$out" ;;
  esac
}

# A minimal fixture: one crate, one file with one function, and a tags-shaped
# file elsewhere carrying the `site: "..."` rows under test.
new_case() { # <name>
  local dir="$TMP/$1"
  mkdir -p "$dir/crates/mycrate/src"
  printf 'pub fn consume_it(x: u32) -> u32 {\n    x + 1\n}\n' >"$dir/crates/mycrate/src/handler.rs"
  echo "$dir"
}

# ── N: the shapes that must pass ─────────────────────────────────────────────
d="$(new_case plain)"
printf 'site: "mycrate/src/handler.rs::consume_it",\n' >"$d/tags.rs"
want "N1 a site naming a symbol that exists passes" \
  expect-pass "$d/tags.rs" "$d/crates"

d="$(new_case qualified)"
mkdir -p "$d/crates/mycrate/src"
printf 'impl Thing {\n    pub fn per_goal_round(&self) {}\n}\n' >"$d/crates/mycrate/src/handler.rs"
printf 'site: "mycrate/src/handler.rs::Thing::per_goal_round",\n' >"$d/tags.rs"
want "N2 a ::-qualified symbol checks the trailing segment" \
  expect-pass "$d/tags.rs" "$d/crates"

d="$(new_case parenthetical)"
printf 'site: "mycrate/src/handler.rs::consume_it (run terminator, latches Outcome::Done)",\n' >"$d/tags.rs"
want "N3 a parenthetical human aside is dropped before the symbol check" \
  expect-pass "$d/tags.rs" "$d/crates"

d="$(new_case multiple_rows)"
printf 'site: "mycrate/src/handler.rs::consume_it",\nsite: "mycrate/src/handler.rs::consume_it",\n' >"$d/tags.rs"
want "N4 several rows pointing at the same live symbol all pass" \
  expect-pass "$d/tags.rs" "$d/crates"

# ── O: the defect — a site pointing nowhere ──────────────────────────────────
d="$(new_case dead_symbol)"
printf 'site: "mycrate/src/handler.rs::renamed_away",\n' >"$d/tags.rs"
want "O1 a site naming a symbol absent from the file fails, naming it" \
  expect-fail "$d/tags.rs" "$d/crates" "renamed_away"

d="$(new_case dead_file)"
printf 'site: "mycrate/src/ghost.rs::consume_it",\n' >"$d/tags.rs"
want "O2 a site naming a file that does not exist fails, naming it" \
  expect-fail "$d/tags.rs" "$d/crates" "mycrate/src/ghost.rs"

d="$(new_case malformed)"
printf 'site: "not-a-file-and-symbol-shape",\n' >"$d/tags.rs"
want "O3 a site with no '.rs::' separator fails loudly rather than guessing" \
  expect-fail "$d/tags.rs" "$d/crates" "shaped"

# ── E: edges ─────────────────────────────────────────────────────────────────
d="$(new_case empty)"
: >"$d/tags.rs"
want "E1 a tags file with no Behavioral rows fails (nothing to check is a bug, not a pass)" \
  expect-fail "$d/tags.rs" "$d/crates" "no 'site:"

# ── R: the real workspace ────────────────────────────────────────────────────
want "R1 this repository's own tags.rs is clean" \
  expect-pass "crates/stella-protocol/src/event/tags.rs" "crates"

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
