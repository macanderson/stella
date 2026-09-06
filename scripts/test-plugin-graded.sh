#!/usr/bin/env bash
#
# Tests for `check-plugin-graded.sh`. It still fails on a plugin no test runs.
#
#   ./scripts/test-plugin-graded.sh
#
# Not a `make gate` step. It builds throwaway fixture trees, the same posture
# as `test-consumer-sites.sh`. Run it when you touch the guard.
#
# The guard's own tree is green by design, since it is a gate step. So a fixture
# is what shows it can still fail: a plugin that no test names. Each case below
# builds the shape it is about. It then points the guard at that tree with the
# two overrides, `PLUGIN_ROOT` and `CRATES_ROOT`.
#
# Runs on bash 3.2.
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-plugin-graded.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

# `want <name> <expect-pass|expect-fail> <plugin-root> <crates-root> [sub]`
want() {
  local name="$1" expect="$2" plugins="$3" crates="$4" sub="${5:-}" out rc
  out="$(PLUGIN_ROOT="$plugins" CRATES_ROOT="$crates" "$SCRIPT" 2>&1)"
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
    fail=$((fail + 1)); echo "FAIL $name — the guard passed an ungraded plugin:"; echo "$out"
    return
  fi
  case "$out" in
  *"$sub"*) pass=$((pass + 1)); echo "ok   $name" ;;
  *) fail=$((fail + 1)); echo "FAIL $name — failed for the wrong reason (wanted '$sub'):"; echo "$out" ;;
  esac
}

# One fixture. It holds one plugin with a manifest, and one crate with tests.
new_case() { # <name>
  local dir="$TMP/$1"
  mkdir -p "$dir/plugins/demo-plugin" "$dir/crates/mycrate/tests"
  printf '[plugin]\nid = "demo-plugin"\n' >"$dir/plugins/demo-plugin/plugin.toml"
  echo "$dir"
}

# ── N: the shapes that must pass ─────────────────────────────────────────────
d="$(new_case graded)"
printf 'let dir = root.join("plugins/demo-plugin");\n' >"$d/crates/mycrate/tests/spawn.rs"
want "N1 a plugin named in a test's code passes" \
  expect-pass "$d/plugins" "$d/crates"

d="$(new_case shared_helper)"
mkdir -p "$d/crates/mycrate/tests/common"
printf 'pub fn dir() -> &str { "plugins/demo-plugin" }\n' \
  >"$d/crates/mycrate/tests/common/mod.rs"
want "N2 a shared helper one level under tests/ counts" \
  expect-pass "$d/plugins" "$d/crates"

d="$(new_case no_manifest)"
mkdir -p "$d/plugins/notes"
printf 'let dir = root.join("plugins/demo-plugin");\n' >"$d/crates/mycrate/tests/spawn.rs"
want "N3 a directory with no plugin.toml is not a plugin and is skipped" \
  expect-pass "$d/plugins" "$d/crates"

# ── O: the defect — a plugin nothing runs ────────────────────────────────────
d="$(new_case ungraded)"
printf 'let dir = root.join("plugins/other-plugin");\n' >"$d/crates/mycrate/tests/spawn.rs"
want "O1 a plugin no test names fails, naming it" \
  expect-fail "$d/plugins" "$d/crates" "demo-plugin is graded by no test"

d="$(new_case comment_only)"
printf '//! Drives plugins/demo-plugin one day.\n// plugins/demo-plugin\n' \
  >"$d/crates/mycrate/tests/spawn.rs"
want "O2 a plugin named only in a comment fails — a mention is not a run" \
  expect-fail "$d/plugins" "$d/crates" "demo-plugin is graded by no test"

d="$(new_case src_only)"
mkdir -p "$d/crates/mycrate/src"
printf 'const DIR: &str = "plugins/demo-plugin";\n' >"$d/crates/mycrate/src/lib.rs"
printf 'let dir = root.join("plugins/other-plugin");\n' >"$d/crates/mycrate/tests/spawn.rs"
want "O3 a plugin named in src/ but by no test fails" \
  expect-fail "$d/plugins" "$d/crates" "demo-plugin is graded by no test"

# ── E: edges ─────────────────────────────────────────────────────────────────
d="$TMP/empty"
mkdir -p "$d/plugins" "$d/crates"
want "E1 a plugin root with no manifests fails (nothing to check is a bug)" \
  expect-fail "$d/plugins" "$d/crates" "no"

d="$(new_case no_crates)"
want "E2 a tree with no test sources at all fails rather than passing vacuously" \
  expect-fail "$d/plugins" "$d/nowhere" "demo-plugin is graded by no test"

want "E3 a plugin root that does not exist fails" \
  expect-fail "$TMP/absent" "$TMP" "no"

# ── R: the real workspace ────────────────────────────────────────────────────
want "R1 this repository's own plugins are each graded" \
  expect-pass "plugins" "crates"

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
