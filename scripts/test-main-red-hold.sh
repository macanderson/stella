#!/usr/bin/env bash
#
# Hermetic tests for scripts/check-main-red-hold.sh.
#
#   ./scripts/test-main-red-hold.sh     (or: make main-red-hold-test)
#
# No network and no `gh`: every case drives the fixture seams, so the suite is
# the same on a bare runner as on a dev box with a live tracker.
#
# The case that matters most is the negative control. A hold that cannot be
# shown to *block* is a claim rather than a guard — and this one spends its
# life passing, because main is usually green, so nothing else would ever
# exercise the branch it exists for.
#
# Deliberately not a `make gate` step, matching `main-canary-test`: the thing
# under test is a CI-only guard that asks the issue tracker a question, and
# the gate is hermetic and offline by contract.
#
# bash 3.2 compatible.
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-main-red-hold.sh"

pass=0
fail=0

ok()  { printf '  \033[32m✓\033[0m %s\n' "$*"; pass=$((pass + 1)); }
bad() { printf '  \033[31m✗\033[0m %s\n' "$*"; fail=$((fail + 1)); }

# One assertion shape: run with both fixture seams pinned and compare the exit
# code. An expected failure must also name its reason, because "exit 1" is
# satisfied by a typo in the script just as well as by the defect the case is
# about — the same discipline test-install-zsh-completions.sh applies.
want() { # want <name> <expect-pass|expect-block> <want-substring> <issues> <labels>
  local name="$1" expect="$2" want_text="$3" issues="$4" labels="$5"
  local out rc
  out="$("$SCRIPT" --fixture-open-issues "$issues" --fixture-labels "$labels" 2>&1)"
  rc=$?
  if [ "$expect" = "expect-pass" ] && [ "$rc" -ne 0 ]; then
    bad "$name — expected pass, got exit $rc: $out"
    return
  fi
  if [ "$expect" = "expect-block" ] && [ "$rc" -eq 0 ]; then
    bad "$name — expected the hold to BLOCK, but it passed: $out"
    return
  fi
  case "$out" in
  *"$want_text"*) ok "$name" ;;
  *) bad "$name — wrong reason (wanted '$want_text'): $out" ;;
  esac
}

printf '\033[1mmain-red-hold — the canary'"'"'s signal reaches the merge decision\033[0m\n'

# The ordinary day: nothing open, nothing to say.
want "a green main lets every PR through" \
  expect-pass "main is not known-broken" "" ""

# The negative control. Without this the suite would be green on a script that
# never blocks anything at all.
want "an open main-red issue holds an ordinary PR" \
  expect-block "merging onto it is how one break becomes four" "3904" ""

want "the block names the issue so the reader can go read it" \
  expect-block "3904" "3904" ""

# The escape hatch: the repair must be able to land, or the hold is a deadlock.
want "the labelled repair PR is let through" \
  expect-pass "labelled \`unblocks-main\`" "3904" "unblocks-main"

# A near-miss label must NOT open the gate — the check is exact, not a
# substring, or `unblocks-main-later` would silently be a bypass.
want "a label that merely contains the escape name does not bypass" \
  expect-block "merging onto it" "3904" "unblocks-main-later"

want "an unrelated label does not bypass" \
  expect-block "merging onto it" "3904" "bug,area:ci"

# Several open issues are one state, not several: main is red, once.
want "multiple open issues still name them all" \
  expect-block "3904 3905" "3904
3905" ""

want "the escape hatch works with several issues open too" \
  expect-pass "labelled" "3904
3905" "area:ci,unblocks-main"

# Argument handling: a caller mistake must be a loud exit 2, never a silent
# pass that looks like a green check.
out="$("$SCRIPT" --pr 2>&1)"
if [ $? -eq 2 ]; then ok "a flag missing its value exits 2, not 0"; else bad "a flag missing its value did not exit 2"; fi

out="$("$SCRIPT" --nonsense 2>&1)"
if [ $? -eq 2 ]; then ok "an unknown flag exits 2, not 0"; else bad "an unknown flag did not exit 2"; fi

printf '\n'
if [ "$fail" -eq 0 ]; then
  printf '\033[32mmain-red-hold-test: OK\033[0m — %d checks passed\n' "$pass"
  exit 0
fi
printf '\033[31mmain-red-hold-test: FAILED\033[0m — %d passed, %d failed\n' "$pass" "$fail"
exit 1
