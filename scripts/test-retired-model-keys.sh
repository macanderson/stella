#!/usr/bin/env bash
#
# Tests for check-retired-model-keys.py. It must flag a retired key in code.
# It must leave alone the three places one may still appear.
#
#   ./scripts/test-retired-model-keys.sh
#
# Run it after you touch that script. It is not part of `make gate`: it
# builds throwaway trees under $TMP, as `make typed-errors-test` does.
#
# ── Why a fixture, not the real workspace ────────────────────────────────────
#
# Run the guard on this tree and it can only pass. No fresh violation is in
# it. A guard nobody has watched fail is a guard nobody knows still works.
# So each case below builds its own root.
#
# ── What has to be true ──────────────────────────────────────────────────────
#
# The guard says "no retired key in code". It has to hold both ends of that.
# Flag too little and the flaw comes back. Flag too much and it dies: three
# files here cite a retired key to explain the change, and a guard that fails
# on those gets turned off. F* holds the first end. Q* holds the second.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-retired-model-keys.py"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

# A throwaway workspace root with a `crates/` tree for the guard to walk.
# $1 = case name. Echoes the root path.
new_root() {
  local dir="$TMP/$1"
  mkdir -p "$dir/crates"
  echo "$dir"
}

# Write a source file. $1 = root, $2 = crate, $3 = path under src/, then the
# file body on stdin.
src_file() {
  local path="$1/crates/$2/src/$3"
  mkdir -p "$(dirname "$path")"
  cat >"$path"
}

# want <name> <expect-pass|expect-fail> <root> <substring>
#
# The substring is checked on both verdicts. On expect-fail it pins what was
# flagged, and in what words. On expect-pass it pins what a passing run said.
# A guard that passed by never looking cannot meet that.
want() {
  local name="$1" expect="$2" root="$3" sub="${4:-}" out rc
  out="$(python3 "$SCRIPT" "$root" 2>&1)"
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

# ── F: a retired key in code is flagged ──────────────────────────────────────

r="$(new_root flat_key)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
pub fn source() -> &'static str {
    "pipeline_verifier_model"
}
EOF
want "F1 a flat pipeline key in a string literal is flagged" \
  expect-fail "$r" "crates/stella-fixture/src/lib.rs"

r="$(new_root persona_key)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
pub const KNOB: &str = "agent_engine_config.agents.triage.effort";
EOF
want "F2 a retired persona block in a string literal is flagged" \
  expect-fail "$r" "1 retired key(s) in code"

r="$(new_root every_persona)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
pub const KEYS: [&str; 5] = [
    "pipeline_worker_model",
    "pipeline_verifier_model",
    "pipeline_triage_model",
    "pipeline_research_model",
    "pipeline_plan_model",
];
EOF
want "F3 all five retired personas are known" expect-fail "$r" "5 retired key(s)"

r="$(new_root remedy)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
pub const KEY: &str = "pipeline_plan_model";
EOF
want "F4 the failure names the seat assignment that replaces the key" \
  expect-fail "$r" "seats"

# ── Q: the three places a retired key may still appear ───────────────────────

r="$(new_root doc_comment)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
//! `pipeline_verifier_model` is retired; assign a seat instead.

/// Retired: `pipeline_triage_model` steers nothing.
pub fn live() {}
EOF
want "Q1 a doc comment citing a retired key passes" expect-pass "$r" "none new"

r="$(new_root cfg_test)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
pub fn live() {}

#[cfg(test)]
mod tests {
    #[test]
    fn the_retired_key_is_reported() {
        assert_eq!("pipeline_verifier_model", "pipeline_verifier_model");
    }
}
EOF
want "Q2 a #[cfg(test)] block naming a retired key passes" expect-pass "$r" "none new"

r="$(new_root test_module)"
src_file "$r" stella-fixture tests.rs <<'EOF'
//! Test module.
pub const SEEDED: &str = "pipeline_verifier_model";
EOF
src_file "$r" stella-fixture settings/tests/retired.rs <<'EOF'
//! A file under a `tests/` directory.
pub const SEEDED: &str = "pipeline_triage_model";
EOF
want "Q3 tests.rs and a tests/ directory are not scanned" expect-pass "$r" "none new"

r="$(new_root clean_tree)"
src_file "$r" stella-fixture lib.rs <<'EOF'
//! Fixture crate.
pub const SEATS: &str = "seat_models";
pub const DEFAULT: &str = "default_model";
EOF
want "Q4 the live keys pass" expect-pass "$r" "0 recorded"

echo
if [ "$fail" -eq 0 ]; then
  echo "test-retired-model-keys: OK — $pass checks passed"
  exit 0
fi
echo "test-retired-model-keys: FAILED — $fail of $((pass + fail)) checks"
exit 1
