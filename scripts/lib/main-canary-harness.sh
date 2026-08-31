# shellcheck shell=bash
#
# Sourced helper: the fixtures and assertion vocabulary
# scripts/test-main-canary.sh drives scripts/main-canary.sh with.
#
# One suite, not two. Every row of the canary reads `--manifest-dir` when it
# is given one, so a fixture's verdict — GREEN included — is a fact about the
# canary rather than about whatever this repository happens to be doing, and
# a second suite holding only the GREEN cases would have nothing to justify
# it.
#
# Everything the suite needs lives here, so a fixture builder or an assertion
# helper has exactly one copy. Source it before any `cd`, so `$0` still
# resolves:
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
#
# `git init` and the empty baselines are what let `file-size` and `prose`
# judge THIS fixture instead of the real repository: both rows require being
# inside a git repository, `check-file-size.sh` refuses to run at all against
# a manifest directory with no baseline file, and `check-prose.py` refuses the
# same way when its density or reading-grade baseline is missing — a real
# checkout always carries them, so a fixture needs its own, empty ones, to
# read as a clean tree rather than as a misconfigured one.
make_workspace() {
  local dir="$1"
  mkdir -p "$dir/crates/demo/src" "$dir/scripts"
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
  echo "# Empty — the fixture starts under every ratchet." >"$dir/scripts/file-size-baseline.txt"
  echo "# Empty — the fixture's one crate starts under the density ceiling." \
    >"$dir/scripts/prose-density-baseline.txt"
  echo "# Empty — the fixture's prose starts under the reading-grade ceiling." \
    >"$dir/scripts/prose-grade-baseline.txt"
  (cd "$dir" && git init -q && cargo generate-lockfile --offline >/dev/null 2>&1)
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
