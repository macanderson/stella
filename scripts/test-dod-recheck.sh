#!/usr/bin/env bash
#
# Hermetic tests for the DoD re-check (`#6079`).
#
#   ./scripts/test-dod-recheck.sh     (or: make dod-recheck-test)
#
# No network and no `gh`: every case drives the fixture seams, so the suite is
# the same on a bare runner as on a dev box with a live tracker.
#
# The cases that matter most are the two negative controls. A sweep that
# re-runs everything looks the same as a correct one from the outside, and a
# sweep that re-runs nothing looks the same as a quiet day. So the suite pins
# both edges: the pull request that names the issue and is already green is
# left alone, and the pull request that names no such issue is never touched
# at all.
#
# Not a `make gate` step, matching `main-red-hold-test`: the thing under test
# asks GitHub a question, and the gate is hermetic and offline by contract.
#
# bash 3.2 compatible.
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/dod-recheck.sh"

pass=0
fail=0

ok() { printf '  \033[32m✓\033[0m %s\n' "$*"; pass=$((pass + 1)); }
bad() { printf '  \033[31m✗\033[0m %s\n' "$*"; fail=$((fail + 1)); }

if [ ! -x "$SCRIPT" ]; then
  printf '\033[31mtest-dod-recheck: FAILED\033[0m — %s is missing or not executable.\n' \
    "$SCRIPT"
  exit 1
fi

# The state of 2026-09-05, as fixtures. Four pull requests are open. Three
# name the issue under test. Two of those carry a failed dod check on the head
# they have now.
#
# The third field is the body, with its line breaks already flattened — the
# same shape the live `gh pr list` gives the matcher.
open_prs="6046 aaaaaaa Closes #6079 — the re-check trigger.
6062 bbbbbbb Refs #6079 and #5913, already green.
6073 ccccccc Closes #60791, a different issue.
6074 ddddddd Closes #6079 as well."

failed_runs="aaaaaaa 33951700124
ccccccc 33950666389
ddddddd 33952811235"

# Every case checks the exit code too. This script runs on an issue edit, and
# a red job there would read as a verdict about the checklist. It has no such
# verdict to give.
says() { # says <name> <want-substring> <issue> <prs> <runs>
  local name="$1" want_text="$2" issue="$3" prs="$4" runs="$5"
  local out rc
  out="$("$SCRIPT" --fixture-open-prs "$prs" --fixture-failed-runs "$runs" \
    "$issue" 2>&1)"
  rc=$?
  if [ "$rc" -ne 0 ]; then
    bad "$name — expected exit 0 (fail-open), got $rc: $out"
    return
  fi
  case "$out" in
  *"$want_text"*) ok "$name" ;;
  *) bad "$name — wanted '$want_text': $out" ;;
  esac
}

lacks() { # lacks <name> <unwanted-substring> <issue> <prs> <runs>
  local name="$1" unwanted="$2" issue="$3" prs="$4" runs="$5"
  local out
  out="$("$SCRIPT" --fixture-open-prs "$prs" --fixture-failed-runs "$runs" \
    "$issue" 2>&1)"
  case "$out" in
  *"$unwanted"*) bad "$name — said '$unwanted' when it should not: $out" ;;
  *) ok "$name" ;;
  esac
}

exits_two() { # exits_two <name> <args...>
  local name="$1"
  shift
  "$SCRIPT" "$@" >/dev/null 2>&1
  if [ $? -eq 2 ]; then ok "$name"; else bad "$name — did not exit 2"; fi
}

printf '\033[1mdod-recheck — a ticked box reaches the pull request\033[0m\n'

# The witness. Before this fix nothing ran the check again. The failure from
# the unticked checklist stayed the last word on that commit, and the pull
# request could not merge until someone edited it.
#
# Each case names the run as well as the pull request. Running the wrong run
# would still look like a sweep, and would clear nothing.
says "a ticked box re-runs the check on the first pull request that names it" \
  "6046 (head aaaaaaa, run 33951700124)" 6079 "$open_prs" "$failed_runs"

says "...and on every other pull request naming the same issue" \
  "6074 (head ddddddd, run 33952811235)" 6079 "$open_prs" "$failed_runs"

# First negative control: the fan-out bound. A green check is already the
# right answer, and running it again would spend a job to change nothing.
lacks "a pull request whose dod check already passes is not re-run" \
  "6062 (head bbbbbbb, run" 6079 "$open_prs" "$failed_runs"

says "...and it says so, so a quiet sweep is not mistaken for a broken one" \
  "6062 names the issue and its dod check is not failing" 6079 \
  "$open_prs" "$failed_runs"

# Second negative control: the match. `#60791` is a different issue, and its
# check is failing, so a loose matcher would re-run it here.
lacks "a longer number that starts with the issue number is not a match" \
  "33950666389" 6079 "$open_prs" "$failed_runs"

says "the summary counts what it swept" \
  "named by 3 open pull request(s); ran the dod check again on 2" 6079 \
  "$open_prs" "$failed_runs"

# Third negative control: the unanswered lookup. A 403, a rate limit or a
# renamed workflow file leaves the run unread, and spelling that the way a
# green check is spelled makes the sweep announce a verdict it never read. So
# the seam gives it an answer of its own, `?`, and this pins that the sweep
# neither claims the check passed nor counts it as swept.
unread_runs="aaaaaaa ?
ddddddd 33952811235"

lacks "a lookup that did not answer is not reported as a passing check" \
  "6046 names the issue and its dod check is not failing" 6079 \
  "$open_prs" "$unread_runs"

says "...it says the check went unread, so the gap is visible" \
  "(head aaaaaaa), so nothing was" 6079 \
  "$open_prs" "$unread_runs"

says "...and it is not counted as swept, while its siblings still are" \
  "named by 3 open pull request(s); ran the dod check again on 1" 6079 \
  "$open_prs" "$unread_runs"

# An issue no open pull request names is a state, not an error.
says "an issue nothing references says so and stops" \
  "named by 0 open pull request(s); ran the dod check again on 0" 4242 \
  "$open_prs" "$failed_runs"

says "no open pull request at all is a state too" \
  "named by 0 open pull request(s)" 6079 "" ""

# A caller that passes no number, or a number that is not one, has a wiring
# bug. That is the one thing here that must be loud.
exits_two "no issue number exits 2, not 0" --fixture-open-prs "$open_prs"
exits_two "a non-numeric issue exits 2, not 0" --fixture-open-prs "$open_prs" main
exits_two "two issue numbers exit 2, not 0" --fixture-open-prs "$open_prs" 1 2
exits_two "an unknown flag exits 2, not 0" --nonsense 6079
exits_two "a flag missing its value exits 2, not 0" --limit

printf '\n\033[1mwiring — a sweep nothing calls sweeps nothing\033[0m\n'

# The three cases below read the workflow file. On a tree where an issue edit
# does not call the sweep, they fail.
holds_text() { # holds_text <name> <file> <pattern>
  local name="$1" file="$repo_root/.github/workflows/$2" pattern="$3"
  if [ -f "$file" ] && grep -q -- "$pattern" "$file"; then
    ok "$name"
  else
    bad "$name — $2 does not carry '$pattern'"
  fi
}

holds_text "an edit to an issue starts a sweep" \
  dod-recheck.yml "types: \[edited\]"

holds_text "...which runs this script" \
  dod-recheck.yml "dod-recheck.sh"

holds_text "...and holds the write scope a re-run needs" \
  dod-recheck.yml "actions: write"

printf '\n'
if [ "$fail" -eq 0 ]; then
  printf '\033[32mtest-dod-recheck: OK\033[0m — %d checks passed\n' "$pass"
  exit 0
fi
printf '\033[31mtest-dod-recheck: FAILED\033[0m — %d passed, %d failed\n' "$pass" "$fail"
exit 1
