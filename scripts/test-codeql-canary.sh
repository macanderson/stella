#!/usr/bin/env bash
#
# Hermetic tests for scripts/codeql-canary.sh.
#
#   ./scripts/test-codeql-canary.sh    (or: make codeql-canary-test)
#
# No network and no `gh`. Every announcing case runs under `--dry-run`, and
# the issue lookup is pinned with `--fixture-open-issue` instead of a live
# tracker read. That keeps the suite the same on a bare runner as on a dev
# box. A monitor that files and closes issues is exactly the kind of script
# you cannot test by running it for real.
#
# CodeQL itself went silently red for weeks with nothing to say so. Every
# case below fails on the old commit. The script it runs did not exist yet.
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
canary="$repo_root/scripts/codeql-canary.sh"

pass=0
fail=0

ok() {
  printf '  \033[32m✓\033[0m %s\n' "$*"
  pass=$((pass + 1))
}
bad() {
  printf '  \033[31m✗\033[0m %s\n' "$*"
  fail=$((fail + 1))
}

# A run that must hit one exit code and print one substring.
expect() {
  local name="$1" want_code="$2" needle="$3"
  shift 3
  local out code
  out="$("$canary" "$@" 2>&1)"
  code=$?
  if [ "$code" -ne "$want_code" ]; then
    bad "$name — exit $code, wanted $want_code: $out"
    return
  fi
  case "$out" in
  *"$needle"*) ok "$name" ;;
  *) bad "$name — output did not contain '$needle': $out" ;;
  esac
}

# A run that must NOT print one substring.
refute() {
  local name="$1" needle="$2"
  shift 2
  local out code
  out="$("$canary" "$@" 2>&1)"
  code=$?
  # The canary always exits 0, 1 or 2. Any other code means the shell
  # crashed, not the canary. A refute against a run that never happened
  # would pass by accident, so this catches that case first.
  case "$code" in
  0 | 1 | 2) ;;
  *)
    bad "$name — the canary did not run (exit $code): $out"
    return
    ;;
  esac
  case "$out" in
  *"$needle"*) bad "$name — should NOT have contained '$needle': $out" ;;
  *) ok "$name" ;;
  esac
}

echo "codeql-canary:"

# Argument validation.

expect "a missing --conclusion is rejected" 2 "must be 'success' or 'failure'"
expect "an unrecognized --conclusion is rejected" 2 "must be 'success' or 'failure'" \
  --conclusion maybe
expect "--dry-run without --announce is rejected" 2 "only means something with --announce" \
  --conclusion failure --dry-run
expect "an unknown flag is rejected" 2 "unknown argument" \
  --conclusion failure --nonsense

# The bare check, with no --announce: exit status only, no gh calls.

expect "success exits 0" 0 "OK — CodeQL is green" \
  --conclusion success
expect "failure exits 1" 1 "FAIL — CodeQL is red" \
  --conclusion failure
refute "a bare check never touches gh" "gh " \
  --conclusion failure

# Red, no issue open yet: opens one.

expect "a red run with nothing open creates an issue" 1 "gh issue create" \
  --conclusion failure --announce --dry-run
expect "the created issue is labelled so only one stays open" 1 "codeql-red" \
  --conclusion failure --announce --dry-run
expect "the issue carries the run URL" 1 "https://example.invalid/run/42" \
  --conclusion failure --announce --dry-run --run-url "https://example.invalid/run/42"
expect "the issue carries the tested commit" 1 "deadbee" \
  --conclusion failure --announce --dry-run --sha deadbee
expect "the issue explains why this exists" 1 "five and a half weeks" \
  --conclusion failure --announce --dry-run
expect "the issue carries a Definition of done section" 1 "**Definition of done**" \
  --conclusion failure --announce --dry-run
expect "...with an unchecked box, since CodeQL is still red" 1 "- [ ] CodeQL analysis completes successfully" \
  --conclusion failure --announce --dry-run

# Red, an issue is already open: comments, does not create a second one.

expect "a red run with one already open comments instead" 1 "gh issue comment 99" \
  --conclusion failure --announce --dry-run --fixture-open-issue 99
refute "...and does not open a second issue" "issue create" \
  --conclusion failure --announce --dry-run --fixture-open-issue 99

# Green, an issue is open: closes it, ticking the box.

expect "a green run with one open closes it" 0 "gh issue close 99" \
  --conclusion success --announce --dry-run --fixture-open-issue 99
expect "...and ticks the Definition of done box first" 0 "- [x] CodeQL analysis completes successfully" \
  --conclusion success --announce --dry-run --fixture-open-issue 99
expect "...naming the recovery commit" 0 "recovered" \
  --conclusion success --announce --dry-run --fixture-open-issue 99

# Green, nothing open: does nothing. It never leaves a stale issue open.

expect "a green run with nothing open does nothing" 0 "nothing open to close" \
  --conclusion success --announce --dry-run
refute "...and never calls gh" "gh " \
  --conclusion success --announce --dry-run

echo
echo "codeql-canary: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
