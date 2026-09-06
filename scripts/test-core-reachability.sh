#!/usr/bin/env bash
#
# Tests for check-core-reachability.py (#5115).
#
#   ./scripts/test-core-reachability.sh
#
# Run it after touching that script. Not part of `make gate`: it builds
# throwaway crate fixtures, the same posture as `module-reachability-test`.
#
# ── Why a fixture instead of the real tree ───────────────────────────────────
#
# Asserting against `crates/stella-core` alone would only prove the baseline
# matches today's residents, which is what a green run says anyway. What needs
# proving is the three directions the guard can be wrong in:
#
#   misses      a module the engine never reaches passes unrecorded — the
#               #5113 defect, a subsystem landing behind a `pub mod` line.
#   fabricates  a module the engine DOES reach is reported unreachable. Worse
#               in practice: it reddens a healthy tree and the reader's only
#               recourse is to distrust the guard.
#   drifts      a baseline entry outlives the module it recorded, so the
#               ratchet stops meaning anything.
#
# The test-only case is its own kind of fabrication and has its own case: a
# `#[cfg(test)]` reference must NOT count, or the guard would call every module
# a driver test touches "engine code".
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-core-reachability.py"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

# A throwaway tree shaped like the real one: `crates/stella-core/src` with a
# step path, so the guard's roots resolve.
new_core() { # <case>
  local dir="$TMP/$1/crates/stella-core/src"
  mkdir -p "$dir"
  printf 'pub mod driver;\npub mod step;\npub mod ports;\n' >"$dir/lib.rs"
  printf 'pub fn drive() {}\n' >"$dir/driver.rs"
  printf 'pub fn step() {}\n' >"$dir/step.rs"
  printf 'pub trait Port {}\n' >"$dir/ports.rs"
  echo "$dir"
}

seed_baseline() { # <case> <entries…>
  local case="$1"
  shift
  mkdir -p "$TMP/$case/scripts"
  : >"$TMP/$case/scripts/core-reachability-baseline.txt"
  for name in "$@"; do
    echo "$name" >>"$TMP/$case/scripts/core-reachability-baseline.txt"
  done
}

# want <name> <expect-pass|expect-fail> <case> [substring]
want() {
  local name="$1" expect="$2" case="$3" sub="${4:-}" out rc
  out="$(python3 "$SCRIPT" "$TMP/$case" 2>&1)"
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
    fail=$((fail + 1)); echo "FAIL $name — the guard passed it:"; echo "$out"
    return
  fi
  case "$out" in
    *"$sub"*) pass=$((pass + 1)); echo "ok   $name" ;;
    *) fail=$((fail + 1)); echo "FAIL $name — failed for the wrong reason (wanted '$sub'):"; echo "$out" ;;
  esac
}

# ── U: the defect — a subsystem the engine never reaches ─────────────────────
# Reachable from `lib.rs`, which is exactly what `module-reachability` checks
# and passes. This guard exists because that is not the same question.
src="$(new_core unreached)"
printf 'pub mod records;\n' >>"$src/lib.rs"
mkdir -p "$src/records"
printf 'pub fn promote() {}\n' >"$src/records/mod.rs"
seed_baseline unreached
want "a module the engine never reaches is reported" expect-fail unreached "records"

# ...and is silent once recorded, which is what lets this land green.
seed_baseline unreached records
want "and is silent once the baseline records it" expect-pass unreached

# ── R: a module the engine DOES reach is never reported ──────────────────────
src="$(new_core reached)"
printf 'pub mod budget;\n' >>"$src/lib.rs"
printf 'pub fn spend() {}\n' >"$src/budget.rs"
printf 'pub fn drive() { crate::budget::spend(); }\n' >"$src/driver.rs"
seed_baseline reached
want "a module the step path uses is not reported" expect-pass reached

# The grouped form resolves too, so `use crate::{a, b}` is not a blind spot.
src="$(new_core grouped)"
printf 'pub mod budget;\npub mod loopdetect;\n' >>"$src/lib.rs"
printf 'pub fn spend() {}\n' >"$src/budget.rs"
printf 'pub fn detect() {}\n' >"$src/loopdetect.rs"
printf 'use crate::{budget, loopdetect};\npub fn drive() { budget::spend(); loopdetect::detect(); }\n' >"$src/driver.rs"
seed_baseline grouped
want "a grouped use reaches every module it names" expect-pass grouped

# ── T: a test-only reference is not reachability ─────────────────────────────
# The false positive #5115 names: `driver/restore.rs`'s test module references
# `crate::skills`, which would otherwise make the whole skill plane look like
# engine code.
src="$(new_core testonly)"
printf 'pub mod skills;\n' >>"$src/lib.rs"
printf 'pub fn select() {}\n' >"$src/skills.rs"
cat >"$src/driver.rs" <<'RS'
pub fn drive() {}

#[cfg(test)]
mod tests {
    #[test]
    fn t() {
        crate::skills::select();
    }
}
RS
seed_baseline testonly
want "a #[cfg(test)] reference does not make a module engine code" expect-fail testonly "skills"

# ── C: a comment is not a reference ──────────────────────────────────────────
# The sibling guard's own history: prose describing the rule must not satisfy
# it.
src="$(new_core commented)"
printf 'pub mod skills;\n' >>"$src/lib.rs"
printf 'pub fn select() {}\n' >"$src/skills.rs"
printf '// The engine does not call crate::skills — that is the point.\npub fn drive() {}\n' >"$src/driver.rs"
seed_baseline commented
want "a module named only in a comment is still unreached" expect-fail commented "skills"

# ── S: a stale baseline entry is reported ────────────────────────────────────
# Without this the ratchet rots: an entry outliving its module records debt
# that no longer exists, and the count stops meaning anything.
src="$(new_core stale)"
seed_baseline stale records
want "a baseline entry whose module is gone is reported" expect-fail stale "STALE"

# ── G: --update refuses to grow ──────────────────────────────────────────────
# The whole ratchet contract. Without this the remedy for a red run is one
# command, and the guard governs nothing.
src="$(new_core grow)"
printf 'pub mod records;\n' >>"$src/lib.rs"
printf 'pub fn promote() {}\n' >"$src/records.rs"
seed_baseline grow
out="$(python3 "$SCRIPT" --update "$TMP/grow" 2>&1)"
rc=$?
if [ "$rc" -ne 0 ] && [ -z "${out##*REFUSING*}" ]; then
  pass=$((pass + 1)); echo "ok   --update refuses to record new debt"
else
  fail=$((fail + 1)); echo "FAIL --update recorded new debt (rc=$rc):"; echo "$out"
fi
case "$(cat "$TMP/grow/scripts/core-reachability-baseline.txt")" in
  *records*) fail=$((fail + 1)); echo "FAIL --update wrote the entry anyway" ;;
  *) pass=$((pass + 1)); echo "ok   and left the baseline untouched" ;;
esac

# ── D: --update shrinks ──────────────────────────────────────────────────────
src="$(new_core shrink)"
seed_baseline shrink records goal
python3 "$SCRIPT" --update "$TMP/shrink" >/dev/null 2>&1
if [ ! -s "$TMP/shrink/scripts/core-reachability-baseline.txt" ] ||
   [ -z "$(grep -v '^#' "$TMP/shrink/scripts/core-reachability-baseline.txt" | tr -d '[:space:]')" ]; then
  pass=$((pass + 1)); echo "ok   --update retires entries whose modules are gone"
else
  fail=$((fail + 1)); echo "FAIL --update kept dead entries:"
  cat "$TMP/shrink/scripts/core-reachability-baseline.txt"
fi

# ── W: the two ways a baseline entry dies read differently ───────────────────
# Both leave the unreached set. Stating the reachable one as fact in both cases
# makes every eviction PR read "the engine reaches it now" about a module that
# is gone.
src="$(new_core evicted)"
seed_baseline evicted records
out="$(python3 "$SCRIPT" --update "$TMP/evicted" 2>&1)"
case "$out" in
  *"retired records — evicted, gone from stella-core"*)
    pass=$((pass + 1)); echo "ok   --update names an evicted module as evicted" ;;
  *)
    fail=$((fail + 1)); echo "FAIL --update mis-named an evicted module:"; echo "$out" ;;
esac

src="$(new_core nowreached)"
printf 'pub mod budget;\n' >>"$src/lib.rs"
printf 'pub fn spend() {}\n' >"$src/budget.rs"
printf 'pub fn drive() { crate::budget::spend(); }\n' >"$src/driver.rs"
seed_baseline nowreached budget
out="$(python3 "$SCRIPT" --update "$TMP/nowreached" 2>&1)"
case "$out" in
  *"retired budget — the engine reaches it now"*)
    pass=$((pass + 1)); echo "ok   --update still names a reached module as reached" ;;
  *)
    fail=$((fail + 1)); echo "FAIL --update mis-named a reached module:"; echo "$out" ;;
esac

# The plain run's STALE block splits the same way, and it is the one an author
# reads first — the update branch only runs once they believe the diagnosis.
want "the STALE block names an eviction as one" expect-fail stale "evicted, gone from stella-core"

src="$(new_core stalereached)"
printf 'pub mod budget;\n' >>"$src/lib.rs"
printf 'pub fn spend() {}\n' >"$src/budget.rs"
printf 'pub fn drive() { crate::budget::spend(); }\n' >"$src/driver.rs"
seed_baseline stalereached budget
want "the STALE block names a reached module as reached" expect-fail stalereached "budget — the engine reaches it now"

# ── the real tree ────────────────────────────────────────────────────────────
# It must pass with its seeded baseline; a guard that cannot run green on the
# repository it ships in is not adoptable.
if out="$(python3 "$SCRIPT" "$repo_root" 2>&1)"; then
  pass=$((pass + 1)); echo "ok   the real stella-core passes with its seeded baseline"
else
  fail=$((fail + 1)); echo "FAIL the real tree is red:"; echo "$out"
fi

echo
echo "core-reachability: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
