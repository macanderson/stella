#!/usr/bin/env bash
#
# Tests for check-dead-code-allows.py — the justification FLOOR, the ratchet
# DIRECTION, and the exclusions that keep it from crying wolf (#3949).
#
#   ./scripts/test-dead-code-allows.sh
#
# Run it after touching that script. Not part of `make gate`: it builds a
# handful of throwaway workspace trees, the same posture as
# `make typed-errors-test` and `make module-reachability-test`.
#
# ── Why a fixture instead of the real workspace ──────────────────────────────
#
# The same two reasons `scripts/test-typed-errors.sh` gives, and the second is
# still the hard one. Asserting against this repository would only ever prove
# the tree currently matches its baseline — there is no first-time violation in
# it to fail on. And `--update`/`--seed` WRITE: pointing them at the real root
# would rewrite scripts/dead-code-allows-baseline.txt as a side effect of
# running the tests. Every case below builds its own root under $TMP.
#
# ── What has to be true, and why each half needs its own cases ───────────────
#
# The guard answers two questions that fail for different reasons:
#
#   * the FLOOR — every suppression says why the lint is wrong here. Absolute:
#     violated at any count, satisfied only by writing the sentence.
#   * the RATCHET — the per-crate total never grows. Relative to a baseline.
#
# A suite testing only one would accept a guard that dropped the other, and a
# guard with only one is materially weaker: the floor alone lets a tree accrue
# unbounded well-argued suppressions (which is how this one got to twenty), and
# the ratchet alone lets an author buy a slot back by deleting someone else's
# suppression and spending it on an unexplained one. J* covers the floor, U*/D*
# the ratchet, and J5 is the case that pins they are independent — a justified
# suppression still counts.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-dead-code-allows.py"
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
  printf '# test baseline\n' >"$dir/scripts/dead-code-allows-baseline.txt"
  echo "$dir"
}

# Write a source file. $1 = root, $2 = crate, $3 = path under src/, then the
# file body on stdin.
src_file() {
  local path="$1/crates/$2/src/$3"
  mkdir -p "$(dirname "$path")"
  cat >"$path"
}

# Write a file under a crate's `tests/` directory — test code, which the guard
# must not scan at all. Same argument shape as src_file.
test_file() {
  local path="$1/crates/$2/tests/$3"
  mkdir -p "$(dirname "$path")"
  cat >"$path"
}

# Overwrite the baseline. $1 = root, then one "<crate> <count>" per entry.
set_baseline() {
  local dir="$1"
  shift
  {
    printf '# test baseline\n'
    for entry in "$@"; do printf '%s\n' "$entry"; done
  } >"$dir/scripts/dead-code-allows-baseline.txt"
}

# want <name> <expect-pass|expect-fail> <root> [substring] [flag]
#
# The substring is checked on BOTH verdicts. On expect-fail it pins what was
# flagged and in what words; on expect-pass it pins what a passing run still
# reported — a guard that passed by forgetting to look would satisfy a bare
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
# The verdict cannot answer this. A writer reports AFTER it writes, so "did it
# refuse" and "did it write the entry anyway" are two separate questions —
# #3750 got both wrong at once in the sibling guard.
entry_is() {
  local name="$1" file="$2/scripts/dead-code-allows-baseline.txt" crate="$3" expect="$4" got
  got="$(awk -v c="$crate" '$1 == c { print $2; exit }' "$file")"
  [ -n "$got" ] || got=absent
  if [ "$got" = "$expect" ]; then
    pass=$((pass + 1)); echo "ok   $name"
  else
    fail=$((fail + 1)); echo "FAIL $name — baseline says '$got', wanted '$expect':"; cat "$file"
  fi
}

# ── J: the justification floor ───────────────────────────────────────────────

r="$(new_root bare)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
#[allow(dead_code)]
fn orphan() {}
EOF
set_baseline "$r" "stella-fixture 9"
want "J1 a bare suppression is flagged even with ratchet room to spare" \
  expect-fail "$r" "do not say why the lint is wrong here"

r="$(new_root reasoned)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
#[allow(dead_code, reason = "the retirement half; see the module docs")]
fn orphan() {}
EOF
set_baseline "$r" "stella-fixture 1"
want "J2 an in-band reason = \"...\" satisfies the floor" expect-pass "$r" "each justified"

r="$(new_root commented)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
/// No production caller since the pipeline removal; kept for its own tests.
#[allow(dead_code)]
fn orphan() {}
EOF
set_baseline "$r" "stella-fixture 1"
want "J3 a comment line directly above satisfies the floor" expect-pass "$r" "each justified"

# A blank line ends the association. Without this the floor is satisfiable by
# any comment anywhere earlier in the file, which is not a justification of
# THIS attribute — it is a coincidence of layout.
r="$(new_root detached)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
/// A comment about something else entirely.

#[allow(dead_code)]
fn orphan() {}
EOF
set_baseline "$r" "stella-fixture 1"
want "J4 a comment separated by a blank line does NOT justify" \
  expect-fail "$r" "do not say why the lint is wrong here"

# The independence case. A justified suppression is still COUNTED — a comment
# makes a suppression reviewable, not free. Without this the ratchet could be
# bought off with prose, which is the failure mode the tree already lived
# through: twenty well-argued suppressions.
r="$(new_root justified_still_counts)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
/// Justified, at length, very persuasively.
#[allow(dead_code)]
fn orphan() {}
EOF
want "J5 a justified suppression still counts against the ratchet" \
  expect-fail "$r" "stella-fixture: 1 dead-code/unused-import suppression(s), ratchet allows 0"

# An empty comment is not a justification. `//` costs one keystroke and would
# otherwise turn the floor into a formality.
r="$(new_root empty_comment)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
//
#[allow(dead_code)]
fn orphan() {}
EOF
set_baseline "$r" "stella-fixture 1"
want "J6 an empty comment does not justify anything" \
  expect-fail "$r" "do not say why the lint is wrong here"

# Intervening attributes do not break the association: `#[must_use]` sits
# between the doc comment and the allow in the real tree (reward.rs).
r="$(new_root through_attrs)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
/// Why this lint is wrong here.
#[must_use]
#[allow(dead_code)]
fn orphan() -> bool { true }
EOF
set_baseline "$r" "stella-fixture 1"
want "J7 an intervening attribute does not detach the comment" expect-pass "$r" "each justified"

# ── X: the exclusions, by construction ───────────────────────────────────────

# Test code is not scanned. The canonical `tests/common/mod.rs` helper carrying
# a module-level `#![allow(dead_code)]` — correct Rust, since each test binary
# uses a subset — is excluded by living where it lives, not by an allowlist.
r="$(new_root test_paths)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
EOF
test_file "$r" stella-fixture common/mod.rs <<'EOF'
#![allow(dead_code)]
pub fn helper() {}
EOF
want "X1 a tests/ helper's module-level allow is not counted" \
  expect-pass "$r" "no dead-code suppressions"

# The in-crate `tests.rs` submodule convention, likewise.
r="$(new_root tests_rs)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
EOF
src_file "$r" stella-fixture tests.rs <<'EOF'
#[allow(dead_code)]
fn helper() {}
EOF
want "X2 an in-crate tests.rs is not scanned" expect-pass "$r" "no dead-code suppressions"

# A `#[cfg(test)]` module inside a production file is test code too.
r="$(new_root cfg_test_mod)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
#[cfg(test)]
mod tests {
    #[allow(dead_code)]
    fn helper() {}
}
EOF
want "X3 an allow inside a #[cfg(test)] module is not counted" \
  expect-pass "$r" "no dead-code suppressions"

# Platform-conditional suppression is genuinely correct — the item is live on
# the other target — and takes the cfg_attr form, which is not the attribute
# form this guard counts.
r="$(new_root cfg_attr)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn platform_specific() {}
EOF
want "X4 a platform-conditional cfg_attr allow is not counted" \
  expect-pass "$r" "no dead-code suppressions"

# The hole the tests/ exclusion must not open: an inner `#![allow(dead_code)]`
# in PRODUCTION src is the same suppression with a wider blast radius.
r="$(new_root inner_in_src)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
#![allow(dead_code)]
fn orphan() {}
EOF
want "X5 an inner allow in production src IS counted" \
  expect-fail "$r" "stella-fixture: 1 dead-code/unused-import suppression(s)"

# An unrelated lint is not this guard's business.
r="$(new_root other_lint)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
#[allow(clippy::too_many_arguments)]
fn wide() {}
EOF
want "X6 an unrelated lint's allow is ignored" expect-pass "$r" "no dead-code suppressions"

# ── U: the ratchet refuses to grow (the #3750 shape) ─────────────────────────

r="$(new_root new_crate)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
/// Justified.
#[allow(dead_code)]
fn orphan() {}
EOF
want "U1 a crate absent from the baseline is flagged on its first suppression" \
  expect-fail "$r" "stella-fixture: 1 dead-code/unused-import suppression(s)"
want "U2 --update refuses it rather than grandfathering it" \
  expect-fail "$r" "refusing to add stella-fixture at 1" "--update"
entry_is "U3 the refused --update wrote no entry for it" "$r" stella-fixture absent

r="$(new_root raise_existing)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
/// Justified.
#[allow(dead_code)]
fn a() {}
/// Justified.
#[allow(dead_code)]
fn b() {}
/// Justified.
#[allow(dead_code)]
fn c() {}
EOF
set_baseline "$r" "stella-fixture 1"
want "U4 raising an existing entry is still refused" \
  expect-fail "$r" "refusing to raise stella-fixture from 1 to 3" "--update"
entry_is "U5 the refused --update left the existing entry alone" "$r" stella-fixture 1

# ── S: seeding is one-time, and cannot launder the floor ─────────────────────

r="$(new_root seed_existing)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
EOF
want "S1 --seed refuses when a baseline already exists" \
  expect-fail "$r" "already exists" "--seed"

# The floor is a rule, not a debt. If --seed recorded unjustified suppressions
# the way it records justified ones, the first run of a new guard would launder
# exactly the suppressions it exists to surface.
r="$(new_root seed_unjustified)"
mkdir -p "$r/scripts"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
#[allow(dead_code)]
fn orphan() {}
EOF
rm -f "$r/scripts/dead-code-allows-baseline.txt"
want "S2 --seed refuses while any suppression is unjustified" \
  expect-fail "$r" "the floor is not a debt to be recorded" "--seed"

# ── D: the down-only direction, which must still work ────────────────────────

r="$(new_root down_only)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
/// Justified.
#[allow(dead_code)]
fn orphan() {}
EOF
set_baseline "$r" "stella-fixture 3"
want "D1 the check notes a crate that dropped below its ceiling" \
  expect-pass "$r" "note: stella-fixture is down to 1 (ratchet says 3)"
want "D2 --update locks the win in" expect-pass "$r" "(1 suppression(s) remaining)" "--update"
entry_is "D3 the entry was lowered to what is really there" "$r" stella-fixture 1

# A crate that reached zero leaves the file entirely — the ratchet is meant to
# reach empty, and an entry saying `0` would be a standing permission slip.
r="$(new_root cleared)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
EOF
set_baseline "$r" "stella-fixture 2"
want "D4 --update drops a crate that reached zero" \
  expect-pass "$r" "(0 suppression(s) remaining)" "--update"
entry_is "D5 the cleared crate is gone from the baseline" "$r" stella-fixture absent

# ── N: the negative direction ────────────────────────────────────────────────
# Without these the suite is satisfiable by a guard that fails on everything.

r="$(new_root at_ceiling)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
/// Justified.
#[allow(dead_code)]
fn a() {}
/// Justified.
#[allow(dead_code)]
fn b() {}
EOF
set_baseline "$r" "stella-fixture 2"
want "N1 a crate exactly at its ceiling passes" expect-pass "$r" "each justified"

r="$(new_root clean_tree)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
pub fn live() {}
EOF
want "N2 a tree with no suppressions at all passes" expect-pass "$r" "no dead-code suppressions"

echo
if [ "$fail" -eq 0 ]; then
  echo "test-dead-code-allows: OK — $pass checks passed"
  exit 0
fi
echo "test-dead-code-allows: FAILED — $fail of $((pass + fail)) checks"
exit 1
