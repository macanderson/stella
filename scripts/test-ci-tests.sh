#!/usr/bin/env bash
#
# Tests for check-ci-tests.sh.
#
#   ./scripts/test-ci-tests.sh
#
# Hermetic. Every case drives the script through its own fixtures. Nothing
# here needs `gh`, a network, or real run history. test-main-verified.sh uses
# this same style for the sibling question this script leaves alone. See
# check-ci-tests.sh's header for why.
#
# The shape this exists for.
#
# `main` failed the same test three times in two days. Nothing filed a
# `main-red` issue. Nothing blocked a merge. Nothing in the chain ever asked
# `ci.yml` what it found. The cases below are the three non-answers that must
# stay silent: a cancelled run, a run that never began, and a run nothing has
# finished yet. Beside them sits the one answer that must not stay silent.
#
# bash 3.2 compatible, matching test-main-verified.sh.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-ci-tests.sh"

pass=0
fail=0

# A commit line as `git log --format='%H %h %s'` prints one.
commit() { printf '%s %s %s\n' "$1" "${1:0:8}" "$2"; }
# A run line in this script's own, shorter fixture shape. No `createdAt`.
# Nothing here ever asks a run's age — see the header for why.
run() { printf '%s %s %s\n' "$1" "$2" "$3"; }

# `want <name> <expect-red|expect-green> <commits> <runs> [substring]`
want() {
  local name="$1" expect="$2" commits="$3" runs="$4" sub="${5:-}" out rc
  out="$("$SCRIPT" --fixture-commits "$commits" --fixture-runs "$runs" 2>&1)"
  rc=$?
  if [ "$expect" = "expect-red" ]; then
    if [ "$rc" -eq 1 ]; then
      pass=$((pass + 1)); echo "ok   $name"
    else
      fail=$((fail + 1)); echo "FAIL $name — expected exit 1, got $rc:"; echo "$out"
      return
    fi
  else
    if [ "$rc" -eq 0 ]; then
      pass=$((pass + 1)); echo "ok   $name"
    else
      fail=$((fail + 1)); echo "FAIL $name — expected exit 0, got $rc:"; echo "$out"
      return
    fi
  fi
  if [ -n "$sub" ]; then
    case "$out" in
      *"$sub"*) ;;
      *)
        fail=$((fail + 1)); echo "FAIL $name — output did not say '$sub':"; echo "$out" ;;
    esac
  fi
}

A=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
B=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb

# The one answer that must file.
want "a completed FAILING run is reported red" expect-red \
  "$(commit $A 'a merge that broke a test')" \
  "$(run $A completed failure)" \
  "${A:0:8}"

# The three non-answers that must stay silent.
want "a cancelled run is not a suite failure" expect-green \
  "$(commit $A 'a merge whose run was cancelled')" \
  "$(run $A completed cancelled)" \
  "not a definite suite failure"

want "a startup_failure is not a suite failure" expect-green \
  "$(commit $A 'a merge whose workflow never began')" \
  "$(run $A completed startup_failure)" \
  "not a definite suite failure"

want "no completed run yet is not a suite failure" expect-green \
  "$(commit $A 'a merge nothing has finished checking')" \
  "$(run $A in_progress none)" \
  "nothing conclusive"

want "a timed-out run is not a suite failure" expect-green \
  "$(commit $A 'a merge whose run timed out')" \
  "$(run $A completed timed_out)" \
  "not a definite suite failure"

# The ordinary green case.
want "a completed passing run is green" expect-green \
  "$(commit $A 'a good merge')" \
  "$(run $A completed success)"

# The run for this exact commit has not finished; an older one has.
# The last VERIFIED state, not the newest commit's own still-open question.
multi="$(commit $A 'newer, still running')
$(commit $B 'older, and it failed')"
runs="$(run $A queued none)
$(run $B completed failure)"
want "the walk finds the older failing commit behind an in-flight newer one" \
  expect-red "$multi" "$runs" "${B:0:8}"

# Several commits, the newest already green.
multi2="$(commit $A 'the fix landed')
$(commit $B 'what it fixed')"
runs2="$(run $A completed success)
$(run $B completed failure)"
want "a newer green run outranks an older red one" expect-green "$multi2" "$runs2"

# An explicitly EMPTY fixture pair, the `--manifest-dir` shape.
# main-canary.sh exports both variables, even empty, whenever it is hermetic
# for every other row. That keeps this row off the live repository's run
# history, so an otherwise-green fixture case stays green. Both empty must
# reach the plain "no commits" path, not the pairing error. That error is
# for a MISMATCHED pair, not an empty matched one.
out="$(CHECK_CI_TESTS_FIXTURE_COMMITS="" CHECK_CI_TESTS_FIXTURE_RUNS="" "$SCRIPT" 2>&1)"; rc=$?
if [ "$rc" -eq 0 ] && [ -z "${out##*nothing conclusive*}" ]; then
  pass=$((pass + 1)); echo "ok   an explicitly empty fixture pair is 'nothing conclusive', not an error"
else
  fail=$((fail + 1)); echo "FAIL an empty fixture pair was not treated as a legitimate empty answer: $out"
fi

# Argument handling.
out="$("$SCRIPT" --fixture-commits "$A" 2>&1)"; rc=$?
if [ "$rc" -eq 2 ] && [ -z "${out##*must be supplied together*}" ]; then
  pass=$((pass + 1)); echo "ok   an unpaired fixture flag is refused"
else
  fail=$((fail + 1)); echo "FAIL an unpaired fixture flag was not refused: $out"
fi

out="$("$SCRIPT" --nope 2>&1)"; rc=$?
if [ "$rc" -eq 2 ] && [ -z "${out##*unknown argument*}" ]; then
  pass=$((pass + 1)); echo "ok   an unknown argument exits 2"
else
  fail=$((fail + 1)); echo "FAIL an unknown argument was not refused: $out"
fi

# The env-var fixture path the `ci-tests` row relies on.
# main-canary.sh cannot safely put a multi-line fixture into the `eval`'d
# string its `checks` array runs. See check-ci-tests.sh's header. So it
# exports these instead. Prove the script actually reads them.
out="$(CHECK_CI_TESTS_FIXTURE_COMMITS="$(commit $A 'via env')" \
  CHECK_CI_TESTS_FIXTURE_RUNS="$(run $A completed failure)" "$SCRIPT" 2>&1)"
rc=$?
if [ "$rc" -eq 1 ] && [ -z "${out##*FAIL*}" ]; then
  pass=$((pass + 1)); echo "ok   the environment-variable fixtures are honoured"
else
  fail=$((fail + 1)); echo "FAIL the environment-variable fixtures were ignored: $out"
fi

# A flag must still win over an inherited variable. A caller that passes
# one by hand is never overridden by the other.
out="$(CHECK_CI_TESTS_FIXTURE_COMMITS="$(commit $A 'env says red')" \
  CHECK_CI_TESTS_FIXTURE_RUNS="$(run $A completed failure)" \
  "$SCRIPT" --fixture-commits "$(commit $B 'flag says green')" \
  --fixture-runs "$(run $B completed success)" 2>&1)"
rc=$?
if [ "$rc" -eq 0 ]; then
  pass=$((pass + 1)); echo "ok   an explicit flag overrides an inherited fixture variable"
else
  fail=$((fail + 1)); echo "FAIL a CLI flag did not override the environment fixture: $out"
fi

echo
echo "ci-tests: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
