#!/usr/bin/env bash
#
# Hermetic tests for the two halves of the red-main hold.
# `check-main-red-hold.sh` blocks a merge while `main` is broken.
# `clear-main-red-holds.sh` clears those blocks once it is not.
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
# The clearing half has the same gap. It is built for the moment `main` gets
# fixed. Nothing else reaches that moment, since a green `main` leaves no
# stale hold to clear.
#
# Not a `make gate` step, matching `main-canary-test`: the thing
# under test is a CI-only guard that asks the issue tracker a question, and
# the gate is hermetic and offline by contract.
#
# bash 3.2 compatible.
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-main-red-hold.sh"
CLEAR="$repo_root/scripts/clear-main-red-holds.sh"

pass=0
fail=0

ok()  { printf '  \033[32m✓\033[0m %s\n' "$*"; pass=$((pass + 1)); }
bad() { printf '  \033[31m✗\033[0m %s\n' "$*"; fail=$((fail + 1)); }

# One shape for every case: pin both fixture seams, then check the exit code.
# A case that expects a failure names its reason too. A typo in the script
# exits 1 just as well as the defect does. `test-install-zsh-completions.sh`
# holds the same line.
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

printf '\n\033[1mclearing — a recovered main un-blocks the pull requests it stopped\033[0m\n'

# The stale-hold state, as fixtures. `main` is fixed, so no issue is open.
# Three pull requests are open. Two still carry a failed hold on the head
# they have now. That is the shape of 2026-09-05, when ten pull requests
# were stuck on a check that would have passed.
#
# A fixture run line is `<head> <run id>` for a failed hold, `<head> ok` for a
# hold that already passes, and `<head> none` for a head that carries no hold
# run at all. The last two used to be one state — an absent line — and the
# sweep read both as "nothing to do" (`#6052`).
recovered_prs="5903 aaaaaaa
5899 bbbbbbb
5894 ccccccc"
stale_runs="aaaaaaa 33951700124
bbbbbbb ok
ccccccc 33950666389"

# Every case checks the exit code too. This script runs inside the canary. A
# non-zero exit there would say `main` is broken when it builds.
clear_says() { # clear_says <name> <want-substring> <issues> <prs> <runs>
  local name="$1" want_text="$2" issues="$3" prs="$4" runs="$5"
  local out rc
  out="$("$CLEAR" --fixture-open-issues "$issues" --fixture-open-prs "$prs" \
    --fixture-stale-runs "$runs" 2>&1)"
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

clear_lacks() { # clear_lacks <name> <unwanted-substring> <issues> <prs> <runs>
  local name="$1" unwanted="$2" issues="$3" prs="$4" runs="$5"
  local out
  out="$("$CLEAR" --fixture-open-issues "$issues" --fixture-open-prs "$prs" \
    --fixture-stale-runs "$runs" 2>&1)"
  case "$out" in
  *"$unwanted"*) bad "$name — said '$unwanted' when it should not: $out" ;;
  *) ok "$name" ;;
  esac
}

# The witness. Before this fix, nothing ran the hold again. The failure from
# the outage stayed the last word on that commit. The pull request could not
# merge until someone pushed to it.
#
# Each case names the run as well as the pull request. Running the wrong run
# would still look like a sweep, and would clear nothing.
clear_says "a recovered main re-runs the hold on the first stale PR" \
  "5903 (head aaaaaaa, run 33951700124)" "" "$recovered_prs" "$stale_runs"

clear_says "...and on every other PR still carrying a stale failure" \
  "5894 (head ccccccc, run 33950666389)" "" "$recovered_prs" "$stale_runs"

# A green hold is already the right answer. Running it again would spend a
# job to change nothing.
clear_lacks "a PR whose hold already passes is left alone" \
  "5899" "" "$recovered_prs" "$stale_runs"

clear_says "the summary counts what it swept" \
  "cleared the hold on 2 of 3 open pull request" "" "$recovered_prs" "$stale_runs"

# The negative control, and the worse direction. Clearing a hold while `main`
# is still broken would drop the signal, not the leftovers.
clear_says "an open main-red issue keeps every hold in place" \
  "main is still known-broken (5901)" "5901" "$recovered_prs" "$stale_runs"

clear_lacks "...and re-runs nothing at all while it stands" \
  "re-run the hold" "5901" "$recovered_prs" "$stale_runs"

# No open pull request is a state, not an error.
clear_says "a repository with no open pull request says so and stops" \
  "cleared the hold on 0 of 0 open pull request" "" "" ""

# A head with no hold run at all. Nothing can be re-run, and `main is not
# known-broken` is a required check, so that pull request stays unmergeable —
# and the sweep used to pass over it in silence, counting it as swept.
unrun_prs="5903 aaaaaaa
5940 ddddddd"
unrun_runs="aaaaaaa 33951700124
ddddddd none"

clear_says "a head with no hold run at all is named" \
  "no main-red-hold.yml run exists on the head of #5940" "" "$unrun_prs" "$unrun_runs"

clear_says "...and the reader is told a push is what starts one" \
  "until its branch is pushed" "" "$unrun_prs" "$unrun_runs"

clear_says "...while the pull request that can be swept still is" \
  "5903 (head aaaaaaa, run 33951700124)" "" "$unrun_prs" "$unrun_runs"

# A hold that already passes is a different state, and must not be reported as
# a branch needing a push.
clear_lacks "a passing hold is not reported as a missing run" \
  "no main-red-hold.yml run exists" "" "$recovered_prs" "$stale_runs"

# The cap used to be silent, so a repository with more open pull requests than
# one page read as a clean sweep of a list it never saw the end of.
capped_prs="1 aaaaaaa
2 bbbbbbb
3 ccccccc"
capped_runs="aaaaaaa ok
bbbbbbb ok
ccccccc ok"
out="$("$CLEAR" --limit 2 --fixture-open-issues "" --fixture-open-prs "$capped_prs" \
  --fixture-stale-runs "$capped_runs" 2>&1)"
rc=$?
if [ "$rc" -ne 0 ]; then
  bad "a cut-short list exited $rc: $out"
else
  case "$out" in
  *"--limit 2 cuts the list short"*) ok "an explicit cap says out loud that it cut the list short" ;;
  *) bad "a cut-short list said nothing about it: $out" ;;
  esac
fi
case "$out" in
*"0 of 2 open pull request"*) ok "...and counts only what it actually swept" ;;
*) bad "the summary did not reflect the cap: $out" ;;
esac

# The default is no cap at all, so nothing is silently left out.
clear_says "with no --limit every open pull request is swept" \
  "0 of 3 open pull request" "" "$capped_prs" "$capped_runs"

out="$("$CLEAR" --limit 2>&1)"
if [ $? -eq 2 ]; then ok "clearing: a flag missing its value exits 2, not 0"; else bad "clearing: a flag missing its value did not exit 2"; fi

out="$("$CLEAR" --limit banana 2>&1)"
if [ $? -eq 2 ]; then ok "clearing: --limit given a word exits 2, not 0"; else bad "clearing: --limit given a word did not exit 2"; fi

out="$("$CLEAR" --nonsense 2>&1)"
if [ $? -eq 2 ]; then ok "clearing: an unknown flag exits 2, not 0"; else bad "clearing: an unknown flag did not exit 2"; fi

# A sweep nothing calls clears nothing, so the wiring is part of the fix. The
# three cases below read the workflow files. On a tree where recovery does not
# call the sweep, they fail.
holds_text() { # holds_text <name> <file> <pattern>
  local name="$1" file="$repo_root/.github/workflows/$2" pattern="$3"
  if [ -f "$file" ] && grep -q -- "$pattern" "$file"; then
    ok "$name"
  else
    bad "$name — $2 does not carry '$pattern'"
  fi
}

holds_text "the canary sweeps on the run that closes the issue" \
  main-canary.yml "clear-main-red-holds.sh"

holds_text "...and holds the write scope that a re-run needs" \
  main-canary.yml "actions: write"

# The canary closes with `GITHUB_TOKEN`, and no event from that token starts a
# workflow. So the issue event is the second path, not the only one.
holds_text "a person closing the issue by hand starts a sweep too" \
  main-red-clear.yml "types: \[closed, unlabeled\]"

printf '\n'
if [ "$fail" -eq 0 ]; then
  printf '\033[32mmain-red-hold-test: OK\033[0m — %d checks passed\n' "$pass"
  exit 0
fi
printf '\033[31mmain-red-hold-test: FAILED\033[0m — %d passed, %d failed\n' "$pass" "$fail"
exit 1
