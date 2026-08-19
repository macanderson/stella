#!/usr/bin/env bash
#
# Tests for check-typed-errors.py's RATCHET DIRECTION (#3750).
#
#   ./scripts/test-typed-errors.sh
#
# Run it after touching that script. Not part of `make gate`: it builds a
# handful of throwaway workspace trees, the same posture as
# `make module-reachability-test`.
#
# ── Why this suite exists at all ─────────────────────────────────────────────
#
# The guard is a down-only ratchet, and its whole promise is the one sentence
# its own baseline header prints: "a crate may shrink its count; it may never
# grow it, and a crate absent from this file must be at zero." The writer broke
# the second half of that for as long as it existed:
#
#     merged = {c: min(n, baseline.get(c, n)) for c, n in counts.items()}
#     raised = {c: (baseline[c], n) for c, n in counts.items() if n > baseline.get(c, n)}
#
# `baseline.get(c, n)` defaults an ABSENT crate to its own new count, so the
# test is `n > n` — False for every n. A crate introducing its FIRST
# `Result<_, String>` was therefore the one case `make typed-errors-update`
# grandfathered silently: it wrote the entry, printed a success line, and
# exited 0. That is precisely the expedient CLAUDE.md forbids, performed by the
# guard on the author's behalf.
#
# The check path never had the bug (it spells the default `0`), so U1 alone
# proves nothing about the fix. U2 and U3 are the witnesses, and they are two
# assertions rather than one because the defect failed both independently: the
# old code exited 0 *and* wrote the entry, so a suite checking only the verdict
# would still pass a writer that refused loudly and grandfathered anyway.
#
# ── Why a fixture instead of the real workspace ──────────────────────────────
#
# Two reasons, and the second is the hard one. Asserting against this
# repository would only ever prove the tree currently matches its baseline —
# there is no first-time violation in it to fail on. And `--update` WRITES:
# pointing it at the real root would rewrite scripts/typed-errors-baseline.txt
# as a side effect of running the tests. Every case below therefore builds its
# own root, with its own baseline, under $TMP. R1 is the single exception and
# is read-only (check mode, no --update).
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-typed-errors.py"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

# A throwaway workspace root: `crates/` for the guard to walk, and the
# `scripts/` directory it reads its baseline from and writes it back to.
# $1 = case name. Echoes the root path.
new_root() {
  local dir="$TMP/$1"
  mkdir -p "$dir/scripts"
  printf '# test baseline\n' >"$dir/scripts/typed-errors-baseline.txt"
  echo "$dir"
}

# A library crate (the guard keys off src/lib.rs) with $3 public fns returning
# `Result<_, String>`, plus one typed signature so a case is never satisfied by
# a guard that simply counts every fn. $1 = root, $2 = crate, $3 = violations.
crate() {
  local src="$1/crates/$2/src"
  mkdir -p "$src"
  {
    printf '//! Fixture crate.\n'
    awk -v n="$3" 'BEGIN {
      for (i = 0; i < n; i++)
        printf "pub fn v%d() -> Result<(), String> { Ok(()) }\n", i
    }'
    printf 'pub fn typed() -> Result<(), MyError> { Ok(()) }\n'
  } >"$src/lib.rs"
}

# Overwrite the baseline. $1 = root, then one "<crate> <count>" per entry.
set_baseline() {
  local dir="$1"
  shift
  {
    printf '# test baseline\n'
    for entry in "$@"; do printf '%s\n' "$entry"; done
  } >"$dir/scripts/typed-errors-baseline.txt"
}

# want <name> <expect-pass|expect-fail> <root> [substring] [flag]
#
# `flag` is passed through to the guard — `--update` is the only one today.
#
# The substring is checked on BOTH verdicts. On expect-fail it pins which crate
# was flagged and in what words; on expect-pass it pins what a passing run
# still reported — a "down to N" note is a real finding that happens not to be
# fatal, and a guard that passed by forgetting the crate would satisfy a bare
# expect-pass.
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
#
# The verdict cannot answer this. `--update` writes the baseline and then
# reports, so "did it refuse" and "did it write the entry anyway" are two
# separate questions — and #3750 got both wrong at once.
entry_is() {
  local name="$1" file="$2/scripts/typed-errors-baseline.txt" crate="$3" expect="$4" got
  got="$(awk -v c="$crate" '$1 == c { print $2; exit }' "$file")"
  [ -n "$got" ] || got=absent
  if [ "$got" = "$expect" ]; then
    pass=$((pass + 1)); echo "ok   $name"
  else
    fail=$((fail + 1)); echo "FAIL $name — baseline says '$got', wanted '$expect':"; cat "$file"
  fi
}

# ── U: the defect — a crate with NO baseline entry (#3750) ───────────────────
r="$(new_root new_crate)"
crate "$r" stella-fixture 1

want "U1 a crate absent from the baseline is flagged on its first violation" \
  expect-fail "$r" "stella-fixture: 1 public fn(s) return"

want "U2 --update refuses it rather than grandfathering it" \
  expect-fail "$r" "refusing to add stella-fixture at 1" "--update"

entry_is "U3 the refused --update wrote no entry for it" "$r" stella-fixture absent

# The pre-existing half of the rule, which the fix must not have traded away:
# a crate that HAS an entry and grew past it is still refused, in its own
# words, and its recorded number is left where it was.
r="$(new_root raise_existing)"
crate "$r" stella-fixture 3
set_baseline "$r" "stella-fixture 1"
want "U4 raising an existing entry is still refused" \
  expect-fail "$r" "refusing to raise stella-fixture from 1 to 3" "--update"
entry_is "U5 the refused --update left the existing entry alone" "$r" stella-fixture 1

# Both shapes at once, so neither message is produced by a branch that only
# ever sees one crate at a time.
r="$(new_root both_shapes)"
crate "$r" stella-old 2
crate "$r" stella-new 1
set_baseline "$r" "stella-old 1"
want "U6 an added crate and a raised crate are both named in one run" \
  expect-fail "$r" "refusing to add stella-new at 1" "--update"
entry_is "U7 neither the added crate..." "$r" stella-new absent
entry_is "U8 ...nor the raised one was written" "$r" stella-old 1

# ── D: the down-only direction, which must still work ────────────────────────
r="$(new_root down_only)"
crate "$r" stella-fixture 1
set_baseline "$r" "stella-fixture 3"
want "D1 the check notes a crate that dropped below its ceiling" \
  expect-pass "$r" "note: stella-fixture is down to 1 (ratchet says 3)"
want "D2 --update locks the win in" \
  expect-pass "$r" "(1 remaining)" "--update"
entry_is "D3 the entry was lowered to what is really there" "$r" stella-fixture 1

# A crate that reached zero leaves the file entirely — the ratchet is meant to
# reach empty, and an entry saying `0` would be a standing permission slip.
r="$(new_root cleared)"
crate "$r" stella-fixture 0
set_baseline "$r" "stella-fixture 2"
want "D4 --update drops a crate that reached zero" \
  expect-pass "$r" "(0 remaining)" "--update"
entry_is "D5 the cleared crate is gone from the baseline" "$r" stella-fixture absent

# ── N: the negative direction ────────────────────────────────────────────────
# Without these the suite is satisfiable by a guard that fails on everything.
r="$(new_root at_ceiling)"
crate "$r" stella-fixture 2
set_baseline "$r" "stella-fixture 2"
want "N1 a crate exactly at its ceiling passes" expect-pass "$r"

r="$(new_root clean)"
crate "$r" stella-fixture 0
want "N2 a crate with only typed signatures passes with no entry" expect-pass "$r"

# The two false positives the guard's docstring says the obvious regex causes,
# both live during the audit that prompted it: `, String>` INSIDE a generic,
# and a crate `Result<T>` alias whose error is defaulted. A guard that cries
# wolf gets `#[allow]`-ed, so these belong in the suite, not just the prose.
r="$(new_root not_violations)"
mkdir -p "$r/crates/stella-fixture/src"
{
  printf '//! Fixture crate.\n'
  printf 'pub fn nested() -> Result<HashMap<String, String>, MyError> { todo!() }\n'
  printf 'pub fn aliased() -> Result<String> { todo!() }\n'
} >"$r/crates/stella-fixture/src/lib.rs"
want "N3 a nested String generic and a defaulted-error alias are not violations" \
  expect-pass "$r"

# `pub(crate)` is deliberately out of scope: the hazard is a caller outside the
# crate that cannot branch, and an internal helper has none (#2392).
r="$(new_root pub_crate)"
mkdir -p "$r/crates/stella-fixture/src"
{
  printf '//! Fixture crate.\n'
  printf 'pub(crate) fn internal() -> Result<(), String> { Ok(()) }\n'
} >"$r/crates/stella-fixture/src/lib.rs"
want "N4 pub(crate) is out of scope" expect-pass "$r"

# A crate with no src/lib.rs is a binary — a `String` there is the finished
# product, not an unnamed error. Counting them reached a number five times the
# real one.
r="$(new_root binary_crate)"
mkdir -p "$r/crates/stella-bin/src"
{
  printf 'pub fn run() -> Result<(), String> { Ok(()) }\n'
  printf 'fn main() {}\n'
} >"$r/crates/stella-bin/src/main.rs"
want "N5 a binary crate is not counted" expect-pass "$r"

# ── R: the real workspace ────────────────────────────────────────────────────
# Check mode only. Never `--update` here: that would rewrite this repository's
# baseline as a side effect of running the tests.
want "R1 this repository still matches its baseline" expect-pass "$repo_root"

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
