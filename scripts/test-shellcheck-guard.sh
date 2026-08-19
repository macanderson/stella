#!/usr/bin/env bash
#
# Tests for the `shellcheck` gate step's PRESENCE GUARD (#3615).
#
#   ./scripts/test-shellcheck-guard.sh
#
# Run it after touching that Makefile recipe. Not part of `make gate`: it
# shells out to `make` against throwaway PATHs, the same posture as
# `make typed-errors-test` thirty lines up the same Makefile.
#
# ── Why this suite exists at all ─────────────────────────────────────────────
#
# Every rung of the gate ladder includes `shellcheck` (GATE_STEPS), so on a
# machine without the binary the ladder could not be climbed at all. What made
# that worse than an inconvenience was the shape of the failure:
#
#     /bin/sh: 1: shellcheck: not found
#     make: *** [Makefile:313: shellcheck] Error 127
#
# which reads exactly like a lint finding in a script somebody had just
# edited. The guard replaces it with a notice that says plainly that the step
# DID NOT RUN, and still fails — a gate step that can no-op is a gate step
# that will.
#
# The two halves are tested separately because they fail independently. A
# guard that prints the notice and exits 0 would satisfy P1 alone; a guard
# that fails without explaining itself would satisfy P2 alone. #3615 is
# specifically about the pair.
#
# A1 is the half that a careless "just make it not crash" fix would break, and
# is the reason this file exists rather than a one-line assertion: the lint
# invocation must stay byte-identical when the binary IS present, so a machine
# that has shellcheck sees no behaviour change whatsoever.
#
# ── Why PATH surgery instead of a fixture tree ───────────────────────────────
#
# The subject is "is this binary reachable", so the fixture has to be the PATH
# itself. Each case builds a directory of symlinks to the real PATH's contents
# and then adds or removes exactly one entry named `shellcheck`. The stub is a
# script rather than the real binary so that A1 proves the recipe RAN the
# thing on PATH, not that this repository's shell scripts happen to be clean.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

# `make` itself must stay reachable, so a case cannot simply empty the PATH.
# Mirror every real PATH entry as symlinks into one directory we control, then
# add or drop the single name under test.
mirror_path() {
  local dir="$TMP/$1/bin"
  mkdir -p "$dir"
  local d f
  local IFS=:
  for d in $PATH; do
    [ -d "$d" ] || continue
    for f in "$d"/*; do
      [ -e "$f" ] || continue
      # First occurrence wins, matching how PATH lookup itself resolves.
      [ -e "$dir/$(basename "$f")" ] || ln -s "$f" "$dir/$(basename "$f")" 2>/dev/null
    done
  done
  rm -f "$dir/shellcheck"
  echo "$dir"
}

# A stub that records that it was invoked, and with what. $1 = bin dir,
# $2 = exit code it should report.
stub_shellcheck() {
  local dir="$1" rc="$2"
  cat >"$dir/shellcheck" <<EOF
#!/bin/sh
printf '%s\n' "STUB-RAN \$*" >"$TMP/stub.args"
exit $rc
EOF
  chmod +x "$dir/shellcheck"
}

# want <name> <expect-pass|expect-fail> <bindir> [substring]
#
# The substring is checked on both verdicts: a guard that failed for some
# unrelated reason must not be able to satisfy an expect-fail case.
want() {
  local name="$1" expect="$2" bindir="$3" sub="${4:-}" out rc
  out="$(env PATH="$bindir" make -C "$repo_root" shellcheck 2>&1)"
  rc=$?
  if [ "$expect" = "expect-pass" ]; then
    if [ "$rc" -ne 0 ]; then
      fail=$((fail + 1)); echo "FAIL $name — expected OK, got:"; echo "$out"
      return
    fi
  elif [ "$rc" -eq 0 ]; then
    fail=$((fail + 1)); echo "FAIL $name — the step reported success without running:"; echo "$out"
    return
  fi
  case "$out" in
    *"$sub"*) pass=$((pass + 1)); echo "ok   $name" ;;
    *) fail=$((fail + 1)); echo "FAIL $name — wanted '$sub' in the output:"; echo "$out" ;;
  esac
}

# ── A: the binary is present — the recipe must be unchanged ──────────────────

A="$(mirror_path present)"
stub_shellcheck "$A" 0
rm -f "$TMP/stub.args"
want "A1 a present shellcheck is invoked and the step passes" \
  expect-pass "$A" "shellcheck install.sh"

if [ -f "$TMP/stub.args" ] && grep -q 'STUB-RAN .*install.sh' "$TMP/stub.args"; then
  pass=$((pass + 1)); echo "ok   A2 the real lint invocation reached the binary, argv intact"
else
  fail=$((fail + 1))
  echo "FAIL A2 the binary on PATH was never invoked (guard swallowed the lint):"
  cat "$TMP/stub.args" 2>/dev/null || echo "(stub never ran)"
fi

# A finding must still be a finding: the guard sits in front of the lint, so it
# must not convert a real non-zero lint result into a pass.
B="$(mirror_path finding)"
stub_shellcheck "$B" 1
want "A3 a real lint finding still fails the step" \
  expect-fail "$B" "Error 1"

# ── B: the binary is absent — fail, and say why ──────────────────────────────

C="$(mirror_path absent)"
want "B1 a missing shellcheck fails rather than skipping" \
  expect-fail "$C" "THIS STEP DID NOT RUN"

want "B2 the notice says the tool is unavailable, not that a script is dirty" \
  expect-fail "$C" "shellcheck: UNAVAILABLE"

# Deliberately NOT the bare word "install": the lint invocation itself names
# `install.sh`, so a substring that loose passes against the pre-fix recipe's
# own echoed command line. Pin the actual remedy instead.
want "B3 the notice names how to install it" \
  expect-fail "$C" "brew install shellcheck"

# The failure it replaces was `Error 127` from the shell, whose text a reader
# cannot tell from a lint failure. Pin that it is gone.
out="$(env PATH="$C" make -C "$repo_root" shellcheck 2>&1)"
case "$out" in
  *"not found"*)
    fail=$((fail + 1))
    echo "FAIL B4 the raw 'not found' shell error still leaks through:"; echo "$out" ;;
  *)
    pass=$((pass + 1)); echo "ok   B4 the bare 'shellcheck: not found' shell error is gone" ;;
esac

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]
