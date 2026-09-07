#!/usr/bin/env bash
#
# Hermetic tests for the two halves of the red-main hold.
# `check-main-red-hold.sh` blocks a merge while `main` is broken.
# `refresh-main-red-holds.sh` keeps that block matching the tracker after the
# pull request has stopped raising events of its own.
#
#   ./scripts/test-main-red-hold.sh     (or: make main-red-hold-test)
#
# No network and no `gh`: every case drives the fixture seams, so the suite is
# the same on a bare runner as on a dev box with a live tracker.
#
# The case that matters most is the negative control. A hold that cannot be
# shown to *block* is a claim rather than a guard. And this one spends its
# life passing, because main is usually green, so nothing else would ever
# exercise the branch it exists for.
#
# The refreshing half has the same gap, twice over. It is built for the two
# moments `main` changes state. Neither is reached on an ordinary day. So each
# direction gets its own case, driven from fixtures: the stale failure a
# recovery leaves behind, and the stale pass a break leaves behind.
#
# Not a `make gate` step, matching `main-canary-test`: the thing
# under test is a CI-only guard that asks the issue tracker a question, and
# the gate is hermetic and offline by contract.
#
# bash 3.2 compatible.
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-main-red-hold.sh"
REFRESH="$repo_root/scripts/refresh-main-red-holds.sh"

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

printf '\n\033[1mrefreshing — the hold keeps up with a main that moved under it\033[0m\n'

# A stub `gh`, ahead of the real one on `PATH` for the rest of this file.
# `--fixture-*` stubs the reads: open issues, open PRs, and which run belongs
# to a head. The write past that point is the `-X POST .../rerun` call this
# script exists to make, and it reaches the stub instead. So a case exercises
# the mutation for real. The stub records each call in `$gh_calls_log`. When
# `GH_STUB_FAIL_MATCH` names a substring the call contains, it fails the call
# the way a missing `actions: write` scope would.
gh_stub_dir="$(mktemp -d)"
trap 'rm -rf "$gh_stub_dir"' EXIT
gh_calls_log="$gh_stub_dir/calls.log"
: >"$gh_calls_log"
cat >"$gh_stub_dir/gh" <<'GH_STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$GH_STUB_CALLS_LOG"
if [ -n "${GH_STUB_FAIL_MATCH:-}" ]; then
  case "$*" in
  *"$GH_STUB_FAIL_MATCH"*) exit 1 ;;
  esac
fi
exit 0
GH_STUB
chmod +x "$gh_stub_dir/gh"
export GH_STUB_CALLS_LOG="$gh_calls_log"
export PATH="$gh_stub_dir:$PATH"

# The stale-hold state, as fixtures. Three pull requests are open, and each
# one's hold last ran at some earlier moment. Two failed, one passed. Which of
# those three answers is stale depends on what the tracker says now, so one
# fixture table drives both directions.
#
# A fixture run line is `<head> <conclusion> <run id>`. The run id may be left
# off for a state that carries none: `<head> none` is a head with no hold run
# at all, and `<head> running` is one whose run has not finished. Without
# those two spellings a missing line is one state, and the sweep reads it as
# "nothing to do" (`#6052`).
held_prs="5903 aaaaaaa
5899 bbbbbbb
5894 ccccccc"
hold_runs="aaaaaaa failure 33951700124
bbbbbbb success 33951700200
ccccccc failure 33950666389"

# Every case checks the exit code too. This script runs inside the canary. A
# non-zero exit there would say `main` is broken when it builds.
sweep_says() { # sweep_says <name> <want-substring> <issues> <prs> <runs>
  local name="$1" want_text="$2" issues="$3" prs="$4" runs="$5"
  local out rc
  out="$("$REFRESH" --fixture-open-issues "$issues" --fixture-open-prs "$prs" \
    --fixture-hold-runs "$runs" 2>&1)"
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

sweep_lacks() { # sweep_lacks <name> <unwanted-substring> <issues> <prs> <runs>
  local name="$1" unwanted="$2" issues="$3" prs="$4" runs="$5"
  local out
  out="$("$REFRESH" --fixture-open-issues "$issues" --fixture-open-prs "$prs" \
    --fixture-hold-runs "$runs" 2>&1)"
  case "$out" in
  *"$unwanted"*) bad "$name — said '$unwanted' when it should not: $out" ;;
  *) ok "$name" ;;
  esac
}

# ── A recovered main un-blocks the pull requests it stopped (`#5913`) ────────
#
# Before that fix, nothing ran the hold again. The failure from the outage
# stayed the last word on that commit. The pull request could not merge until
# someone pushed to it.
#
# Each case names the run as well as the pull request. Running the wrong run
# would still look like a sweep, and would move nothing.
sweep_says "a recovered main re-runs the hold on the first stale PR" \
  "5903 (head aaaaaaa, run 33951700124)" "" "$held_prs" "$hold_runs"

sweep_says "...and on every other PR still carrying a stale failure" \
  "5894 (head ccccccc, run 33950666389)" "" "$held_prs" "$hold_runs"

# A green hold is already the right answer. Running it again would spend a
# job to change nothing.
sweep_lacks "a PR whose hold already passes is left alone" \
  "5899" "" "$held_prs" "$hold_runs"

sweep_says "the summary counts what it swept" \
  "re-ran the stale hold on 2 of 3 open pull request" "" "$held_prs" "$hold_runs"

# ── A broken main turns the passing holds red (`#5949`) ──────────────────────
#
# The other direction, and the one that lets a bad merge through rather than
# blocking a good one. `#5928`'s hold ran at 09:41 and passed. The `main-red`
# issue for that outage was filed at 10:07. Nothing re-ran the hold, so
# `#5928` merged onto the broken tree with a 26-minute-old green as its
# required check. PR 5899 below is that pull request: its hold passed, and
# `main` is red now.
sweep_says "a filed main-red issue re-runs the hold that still passes" \
  "5899 (head bbbbbbb, run 33951700200)" "5901" "$held_prs" "$hold_runs"

# The negative control for this direction. A sweep that read the tracker
# backwards would clear the two failing holds — and those are the signal, not
# a leftover.
sweep_lacks "a hold that already fails is left alone while main is red" \
  "5903 (head aaaaaaa" "5901" "$held_prs" "$hold_runs"

sweep_lacks "...and the sweep never claims a recovery that has not happened" \
  "main has recovered" "5901" "$held_prs" "$hold_runs"

sweep_says "the summary names the issue that decided the sweep" \
  "main is known-broken (5901) — re-ran the stale hold on 1 of 3" \
  "5901" "$held_prs" "$hold_runs"

# A run still in flight will report today's answer by itself. Re-running it
# would cancel the run that is already asking the right question.
running_runs="aaaaaaa running
bbbbbbb success 33951700200
ccccccc failure 33950666389"

sweep_lacks "a hold still running is left to finish" \
  "5903" "" "$held_prs" "$running_runs"

# No open pull request is a state, not an error.
sweep_says "a repository with no open pull request says so and stops" \
  "re-ran the stale hold on 0 of 0 open pull request" "" "" ""

# A head with no hold run at all. Nothing can be re-run, and `main is not
# known-broken` is a required check, so that pull request stays unmergeable.
# An unsplit sweep passes over it in silence and counts it as swept.
unrun_prs="5903 aaaaaaa
9 ddddddd"
unrun_runs="aaaaaaa failure 33951700124
ddddddd none"

sweep_says "a head with no hold run at all is named" \
  "no main-red-hold.yml run exists on the head of #9" "" "$unrun_prs" "$unrun_runs"

sweep_says "...and the reader is told a push is what starts one" \
  "until its branch is pushed" "" "$unrun_prs" "$unrun_runs"

sweep_says "...while the pull request that can be swept still is" \
  "5903 (head aaaaaaa, run 33951700124)" "" "$unrun_prs" "$unrun_runs"

# A hold that already passes is a different state, and must not be reported as
# a branch needing a push.
sweep_lacks "a passing hold is not reported as a missing run" \
  "no main-red-hold.yml run exists" "" "$held_prs" "$hold_runs"

# A silent cap makes a repository with more open pull requests than one page
# read as a clean sweep of a list nothing ever saw the end of.
capped_prs="1 aaaaaaa
2 bbbbbbb
3 ccccccc"
capped_runs="aaaaaaa success 1
bbbbbbb success 2
ccccccc success 3"
out="$("$REFRESH" --limit 2 --fixture-open-issues "" --fixture-open-prs "$capped_prs" \
  --fixture-hold-runs "$capped_runs" 2>&1)"
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
sweep_says "with no --limit every open pull request is swept" \
  "0 of 3 open pull request" "" "$capped_prs" "$capped_runs"

out="$("$REFRESH" --limit 2>&1)"
if [ $? -eq 2 ]; then ok "sweeping: a flag missing its value exits 2, not 0"; else bad "sweeping: a flag missing its value did not exit 2"; fi

out="$("$REFRESH" --limit banana 2>&1)"
if [ $? -eq 2 ]; then ok "sweeping: --limit given a word exits 2, not 0"; else bad "sweeping: --limit given a word did not exit 2"; fi

out="$("$REFRESH" --nonsense 2>&1)"
if [ $? -eq 2 ]; then ok "sweeping: an unknown flag exits 2, not 0"; else bad "sweeping: an unknown flag did not exit 2"; fi

printf '\n\033[1mthe write itself — the mutation, not the sweep'"'"'s account of it\033[0m\n'

# Every case above drove the read paths through fixtures and asked only what
# the sweep says it did. `$gh_calls_log` is what `gh` actually saw, so these
# ask the mutation directly — a wrong endpoint, the wrong verb, or a POST
# that never fires all turn this section red without changing a word the
# sweep prints.
: >"$gh_calls_log"
"$REFRESH" --fixture-open-issues "" --fixture-open-prs "$held_prs" \
  --fixture-hold-runs "$hold_runs" >/dev/null 2>&1
calls="$(cat "$gh_calls_log")"
want_calls="api -X POST --silent repos/{owner}/{repo}/actions/runs/33951700124/rerun
api -X POST --silent repos/{owner}/{repo}/actions/runs/33950666389/rerun"
if [ "$(printf '%s\n' "$calls" | sort)" = "$(printf '%s\n' "$want_calls" | sort)" ]; then
  ok "the sweep POSTs the exact rerun endpoint for each stale run, and only those"
else
  bad "gh calls did not match — got [$calls] want [$want_calls]"
fi

# `rerun`, not `rerun-failed-jobs`, and the break direction is why. The run
# being refreshed there passed, so it has no failed job and that endpoint
# would re-run nothing. On this path the two are the same call, because
# `main-red-hold.yml` has one job.
case "$calls" in
*rerun-failed-jobs*) bad "the sweep still calls rerun-failed-jobs: $calls" ;;
*) ok "the endpoint is one that works in both directions" ;;
esac

# The PR whose hold already passes (5899, head bbbbbbb) is not stale on this
# path. The call log above already proves nothing was sent for it: the
# comparison is exact rather than a substring, so a spurious third call would
# have failed it. This case says so in a name the checklist can point at.
case "$calls" in
*"5899"* | *"bbbbbbb"*) bad "the sweep called gh for a PR whose hold already passes: $calls" ;;
*) ok "no POST is sent for a PR whose hold already passes" ;;
esac

# The same question the other way round: while `main` is red, the one POST
# must be for the hold that still passes, and for nothing else.
: >"$gh_calls_log"
"$REFRESH" --fixture-open-issues "5901" --fixture-open-prs "$held_prs" \
  --fixture-hold-runs "$hold_runs" >/dev/null 2>&1
red_calls="$(cat "$gh_calls_log")"
want_red="api -X POST --silent repos/{owner}/{repo}/actions/runs/33951700200/rerun"
if [ "$red_calls" = "$want_red" ]; then
  ok "a broken main POSTs exactly one rerun, for the hold that still passes"
else
  bad "gh calls did not match — got [$red_calls] want [$want_red]"
fi

# The write can fail — a job without `actions: write`, say. The sweep must
# stay fail-open (exit 0), name the run it could not re-run rather than
# claim it, and still credit the run that did succeed.
: >"$gh_calls_log"
out="$(GH_STUB_FAIL_MATCH="33951700124" "$REFRESH" \
  --fixture-open-issues "" --fixture-open-prs "$held_prs" \
  --fixture-hold-runs "$hold_runs" 2>&1)"
rc=$?
if [ "$rc" -ne 0 ]; then
  bad "a failed POST changed the exit code — expected 0 (fail-open), got $rc: $out"
else
  ok "a failed POST still exits 0"
fi
case "$out" in
*"run 33951700124 for"*"5903 — does this job have"*) ok "a failed POST reports which run it could not re-run" ;;
*) bad "a failed POST gave no reason: $out" ;;
esac
case "$out" in
*"5903 (head aaaaaaa, run 33951700124)"*) bad "a failed POST was also reported as swept: $out" ;;
*) ok "the failed PR is not double-counted as swept" ;;
esac
case "$out" in
*"5894 (head ccccccc, run 33950666389)"*) ok "the run that did succeed is still swept" ;;
*) bad "a failed POST for one PR swallowed the successful one: $out" ;;
esac
case "$out" in
*"re-ran the stale hold on 1 of 3 open pull request"*) ok "the summary counts only what actually succeeded" ;;
*) bad "the summary did not reflect the failed POST: $out" ;;
esac

# `--dry-run` is the one caller-facing way to suppress the write. With
# fixtures active it must still block every call: none reaches `gh` at all.
: >"$gh_calls_log"
"$REFRESH" --dry-run --fixture-open-issues "" --fixture-open-prs "$held_prs" \
  --fixture-hold-runs "$hold_runs" >/dev/null 2>&1
if [ -s "$gh_calls_log" ]; then
  bad "--dry-run still called gh: $(cat "$gh_calls_log")"
else
  ok "--dry-run suppresses the POST entirely"
fi

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

# The canary is the caller for both transitions: the push run that files the
# `main-red` issue is the same shape as the one that closes it.
holds_text "the canary sweeps on the run that files or closes the issue" \
  main-canary.yml "refresh-main-red-holds.sh"

holds_text "...and holds the write scope that a re-run needs" \
  main-canary.yml "actions: write"

# The canary closes with `GITHUB_TOKEN`, and no event from that token starts a
# workflow. So the issue event is the second path, not the only one.
holds_text "a person closing the issue by hand starts a sweep too" \
  main-red-clear.yml "types: \[closed, unlabeled\]"

holds_text "the workflow carries a drill input to rehearse the re-run call" \
  main-red-clear.yml "drill:"

holds_text "...and passes it to the script rather than running blind" \
  main-red-clear.yml "refresh-main-red-holds.sh --drill"

# The sweep reads the tracker, so it has to run after the step that writes to
# it. Reversed, a break path would sweep against the answer from before the
# issue was filed and re-run nothing — which is the `#5949` bug with extra
# machinery in front of it.
canary="$repo_root/.github/workflows/main-canary.yml"
announce_line="$(grep -n 'main-canary.sh --announce' "$canary" | head -1 | cut -d: -f1)"
sweep_line="$(grep -n 'refresh-main-red-holds.sh' "$canary" | head -1 | cut -d: -f1)"
if [ -n "$announce_line" ] && [ -n "$sweep_line" ] &&
  [ "$announce_line" -lt "$sweep_line" ]; then
  ok "the sweep runs after the step that files or closes the issue"
else
  bad "the canary sweeps before it writes the issue the sweep reads"
fi

printf '\n\033[1mdrilling — the re-run call is rehearsed without an outage\033[0m\n'

# The drill asks the tracker nothing, and it sweeps no second pull request.
# So its fixtures are one PR/head line and a bare run id. The sweep's issue
# and multi-PR fixtures play no part here.
drill_says() { # drill_says <name> <want-substring> <expect-rc> <pr> <prs> <run>
  local name="$1" want_text="$2" expect_rc="$3" pr="$4" prs="$5" run="$6"
  local out rc
  out="$("$REFRESH" --drill "$pr" --fixture-open-prs "$prs" \
    --fixture-drill-run "$run" 2>&1)"
  rc=$?
  if [ "$rc" -ne "$expect_rc" ]; then
    bad "$name — expected exit $expect_rc, got $rc: $out"
    return
  fi
  case "$out" in
  *"$want_text"*) ok "$name" ;;
  *) bad "$name — wanted '$want_text': $out" ;;
  esac
}

# `#6051`'s own fixture: one pull request, and a run id the drill must name
# back exactly, regardless of what that run's real conclusion was.
drill_prs="5903 aaaaaaa"

drill_says "a drill re-runs the named pull request's latest hold run" \
  "5903 (head aaaaaaa, run 99999999)" 0 \
  "5903" "$drill_prs" "99999999"

# The point of the drill: a sweep would never touch a run that already
# passed — nothing failed. The drill must rehearse it anyway, which is what
# lets it run on an ordinary day with nothing broken.
drill_says "...even when that run already passed" \
  "5903 (head aaaaaaa, run 12345678)" 0 \
  "5903" "$drill_prs" "12345678"

# The one negative case the definition of done names by name: a pull request
# that carries no hold run at all. Nothing to rehearse, and the drill says so
# loudly rather than reporting a quiet, meaningless success.
drill_says "a pull request with no hold run at all fails loudly" \
  "no main-red-hold.yml run exists on the head of" 1 \
  "5903" "$drill_prs" "none"

# A pull request number the fixture does not know is a caller mistake, not a
# silent no-op.
drill_says "an unknown pull request number fails loudly, not silently" \
  "could not find pull request" 1 \
  "9999" "$drill_prs" ""

out="$("$REFRESH" --drill 2>&1)"
if [ $? -eq 2 ]; then ok "drilling: a flag missing its value exits 2, not 0"; else bad "drilling: a flag missing its value did not exit 2"; fi

out="$("$REFRESH" --drill banana 2>&1)"
if [ $? -eq 2 ]; then ok "drilling: --drill given a word exits 2, not 0"; else bad "drilling: --drill given a word did not exit 2"; fi

printf '\n'
if [ "$fail" -eq 0 ]; then
  printf '\033[32mmain-red-hold-test: OK\033[0m — %d checks passed\n' "$pass"
  exit 0
fi
printf '\033[31mmain-red-hold-test: FAILED\033[0m — %d passed, %d failed\n' "$pass" "$fail"
exit 1
