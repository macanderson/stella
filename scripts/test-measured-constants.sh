#!/usr/bin/env bash
#
# Tests for check-measured-constants.sh — that it still FAILS on the shape it
# exists for, and does not cry wolf on the shapes it must accept (#2495).
#
#   ./scripts/test-measured-constants.sh
#
# Run it after touching that script. Not a `make gate` step: it builds
# throwaway source trees under $TMP, the same posture as
# `make dead-code-allows-test` and `make typed-errors-test`.
#
# ── Why a fixture instead of the real workspace ──────────────────────────────
#
# Asserting against this repository would only ever prove the tree currently
# satisfies the guard. There is no violation in it to fail on — and there is
# not meant to be, so a suite pointed at the real root can never show that the
# guard can still fail. That is the exact failure #3750 is about: a ratchet
# that had quietly become incapable of failing reported green forever.
#
# Every case below builds its own root and runs the guard against it, which is
# what the script's optional directory argument is for.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-measured-constants.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

# A throwaway source root. $1 = case name. Echoes the root path.
new_root() {
  local dir="$TMP/$1"
  mkdir -p "$dir/src"
  echo "$dir"
}

# Write a file under the root. $1 = root, $2 = path, body on stdin.
write() {
  local path="$1/$2"
  mkdir -p "$(dirname "$path")"
  cat >"$path"
}

# want <name> <expect-pass|expect-fail> <root> <substring>
#
# The substring is checked on BOTH verdicts. On expect-fail it pins what was
# flagged and in what words; on expect-pass it pins what a passing run still
# reported, so a guard that passed by forgetting to look would not satisfy it.
want() {
  local name="$1" expect="$2" root="$3" sub="$4" out rc
  out="$("$SCRIPT" "$root" 2>&1)"
  rc=$?
  if [ "$expect" = "expect-pass" ]; then
    if [ "$rc" -ne 0 ]; then
      fail=$((fail + 1))
      echo "FAIL $name — expected OK, got:"
      echo "$out"
      return
    fi
  elif [ "$rc" -eq 0 ]; then
    fail=$((fail + 1))
    echo "FAIL $name — the guard passed a violation it should have flagged:"
    echo "$out"
    return
  fi
  case "$out" in
  *"$sub"*)
    pass=$((pass + 1))
    echo "ok   $name"
    ;;
  *)
    fail=$((fail + 1))
    echo "FAIL $name — wanted '$sub' in:"
    echo "$out"
    ;;
  esac
}

# The marker, assembled rather than written out, so this suite is not itself a
# file full of the pattern the guard scans for.
MARK="MEASURED:"

# ── The case the guard exists for ────────────────────────────────────────────

r="$(new_root unasserted)"
write "$r" src/pipeline.rs <<EOF
/// The triage ceiling.
/// $MARK 34 triage calls on 2026-08-09; 27 burned the full 10s.
const TRIAGE_LATENCY_CEILING: u64 = 30;
EOF
want "M1 a marked constant no test names is flagged" \
  expect-fail "$r" "TRIAGE_LATENCY_CEILING"

r="$(new_root asserted_sibling)"
write "$r" src/pipeline.rs <<EOF
/// $MARK 34 triage calls on 2026-08-09; 27 burned the full 10s.
const TRIAGE_LATENCY_CEILING: u64 = 30;

#[cfg(test)]
mod tests {
    #[test]
    fn the_ceiling_stays_where_the_measurement_put_it() {
        assert_eq!(super::TRIAGE_LATENCY_CEILING, 30);
    }
}
EOF
want "M2 a test in the same file's tests module satisfies it" \
  expect-pass "$r" "1 measured constant"

# The assertion may live anywhere in the workspace, and usually does — a
# `pub(crate)` constant is often pinned from a crate-level tests file.
r="$(new_root asserted_elsewhere)"
write "$r" src/pipeline.rs <<EOF
/// $MARK 34 triage calls on 2026-08-09.
pub const TRIAGE_LATENCY_CEILING: u64 = 30;
EOF
write "$r" tests/ceilings.rs <<'EOF'
#[test]
fn the_ceiling_stays_where_the_measurement_put_it() {
    assert_eq!(fixture::TRIAGE_LATENCY_CEILING, 30);
}
EOF
want "M3 a test under tests/ satisfies it" expect-pass "$r" "1 measured constant"

# The near-miss that would make the guard useless: the constant is named in
# PRODUCTION code elsewhere, which proves nothing about a revert.
r="$(new_root production_mention)"
write "$r" src/pipeline.rs <<EOF
/// $MARK 34 triage calls on 2026-08-09.
const TRIAGE_LATENCY_CEILING: u64 = 30;
EOF
write "$r" src/driver.rs <<'EOF'
fn deadline() -> u64 {
    crate::pipeline::TRIAGE_LATENCY_CEILING
}
EOF
want "M4 a mention in production code does NOT satisfy it" \
  expect-fail "$r" "no test names it"

# ── The two shapes that look like coverage and are not ───────────────────────

r="$(new_root empty_marker)"
write "$r" src/pipeline.rs <<EOF
/// $MARK
const TRIAGE_LATENCY_CEILING: u64 = 30;

#[cfg(test)]
mod tests {
    #[test]
    fn pinned() {
        assert_eq!(super::TRIAGE_LATENCY_CEILING, 30);
    }
}
EOF
want "M5 a marker recording no measurement is flagged" \
  expect-fail "$r" "records no measurement"

r="$(new_root unattached)"
write "$r" src/pipeline.rs <<EOF
/// $MARK 34 triage calls on 2026-08-09.
fn triage_deadline() -> u64 {
    30
}
EOF
want "M6 a marker not on a const or static is flagged" \
  expect-fail "$r" "names nothing this guard can pin"

# ── The shapes it must not cry wolf on ───────────────────────────────────────

r="$(new_root unmarked)"
write "$r" src/pipeline.rs <<'EOF'
/// A judgement, not a measurement, and deliberately unmarked.
const RETRY_LIMIT: u64 = 3;
EOF
want "M7 an unmarked constant is not the guard's business" \
  expect-pass "$r" "0 measured constant"

r="$(new_root statics_and_visibility)"
write "$r" src/pipeline.rs <<EOF
/// $MARK 158 files censused; the lowest cosine anywhere was 0.418.
pub(crate) static ADMISSION_FLOOR: f64 = 0.25;

#[cfg(test)]
mod tests {
    #[test]
    fn the_floor_stays_below_the_measured_minimum() {
        assert!(super::ADMISSION_FLOOR < 0.418);
    }
}
EOF
want "M8 a pub(crate) static is read the same as a private const" \
  expect-pass "$r" "1 measured constant"

# ── Verdict ──────────────────────────────────────────────────────────────────

echo
echo "measured-constants guard: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
