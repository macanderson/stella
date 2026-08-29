# shellcheck shell=bash
#
# Sourced helper: the fixtures and assertion vocabulary both main-canary test
# suites drive scripts/main-canary.sh with (#5356).
#
# The suites are split by what a case NEEDS, not by what it tests:
#
#   scripts/test-main-canary.sh       every case whose verdict is a fact about
#                                     the canary. A fixture tree under
#                                     `--manifest-dir` decides it, so it holds
#                                     whatever `main` is red on, and CI runs it
#                                     (.github/workflows/guard-self-tests.yml).
#   scripts/test-main-canary-live.sh  the cases that expect the canary to report
#                                     GREEN. It can only do that when the live
#                                     tree is green — the `file-size` and
#                                     `prose` rows read the repository itself
#                                     and take no `--manifest-dir` — so those
#                                     cases report what `main` is doing rather
#                                     than what the canary can do, which is
#                                     main-canary.yml's job after the merge.
#
# Everything both suites need lives here, so the split costs no second copy of
# the fixture builder or the assertion vocabulary. Source it before any `cd`,
# so `$0` still resolves:
#
#   # shellcheck source=scripts/lib/main-canary-harness.sh
#   . "$(dirname "$0")/lib/main-canary-harness.sh"

canary="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/main-canary.sh"

pass=0
fail=0

# Both suites build cargo workspaces and the canary's `compile` row runs
# `cargo check`, so a toolchain-free machine can only report a false green.
# Skipping says so; it does not pass silently.
require_cargo() {
  if ! command -v cargo >/dev/null 2>&1; then
    echo "skip $1 — no cargo toolchain on PATH"
    exit 0
  fi
}

# Set `$tmp` to a throwaway directory, removed when the suite exits. It assigns
# rather than prints because a `$(...)` reader would run the `trap` in a
# subshell, leaving the real one uncleaned.
canary_scratch() {
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
}

# A minimal, network-free workspace at $1 whose lock matches its manifests.
make_workspace() {
  local dir="$1"
  mkdir -p "$dir/crates/demo/src"
  cat >"$dir/Cargo.toml" <<'TOML'
[workspace]
members = ["crates/demo"]
resolver = "2"

[workspace.package]
version = "0.1.0"
edition = "2021"
TOML
  cat >"$dir/crates/demo/Cargo.toml" <<'TOML'
[package]
name = "demo"
version.workspace = true
edition.workspace = true
TOML
  echo 'pub fn demo() {}' >"$dir/crates/demo/src/lib.rs"
  (cd "$dir" && cargo generate-lockfile --offline >/dev/null 2>&1)
}

expect() {
  local name="$1" want_code="$2" needle="$3"
  shift 3
  local out code
  set +e
  out="$("$canary" "$@" 2>&1)"
  code=$?
  set -e
  if [ "$code" -ne "$want_code" ]; then
    echo "FAIL  $name — exit $code, wanted $want_code"
    printf '      %s\n' "$out"
    fail=$((fail + 1))
    return
  fi
  case "$out" in
  *"$needle"*) ;;
  *)
    echo "FAIL  $name — output did not contain: $needle"
    printf '      %s\n' "$out"
    fail=$((fail + 1))
    return
    ;;
  esac
  echo "ok    $name"
  pass=$((pass + 1))
}

refute() {
  local name="$1" needle="$2"
  shift 2
  local out code
  set +e
  out="$("$canary" "$@" 2>&1)"
  code=$?
  set -e
  # An absent needle proves nothing if the canary never ran: a refute against a
  # script that could not start passes for free, which is how a broken harness
  # reports green. The canary decides every run with 0, 1 or 2 — anything else
  # is the shell answering instead of it.
  case "$code" in
  0 | 1 | 2) ;;
  *)
    echo "FAIL  $name — the canary did not run (exit $code)"
    printf '      %s\n' "$out"
    fail=$((fail + 1))
    return
    ;;
  esac
  case "$out" in
  *"$needle"*)
    echo "FAIL  $name — output should NOT have contained: $needle"
    printf '      %s\n' "$out"
    fail=$((fail + 1))
    ;;
  *)
    echo "ok    $name"
    pass=$((pass + 1))
    ;;
  esac
}

# The tally, and the suite's exit status.
canary_tally() {
  echo
  echo "$1: $pass passed, $fail failed"
  [ "$fail" -eq 0 ]
}
