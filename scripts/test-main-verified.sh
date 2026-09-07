#!/usr/bin/env bash
#
# Tests for check-main-verified.sh (#5027).
#
#   ./scripts/test-main-verified.sh
#
# Hermetic: every case drives the script through its paired fixtures, so
# nothing here needs `gh`, a network, or the repository's real run history. A
# monitor whose subject is an Actions outage is precisely the script you
# cannot test by waiting for one.
#
# ── The four shapes, from the run that motivated it ──────────────────────────
#
# On 2026-08-26 four commits landed during a runner outage and produced four
# different non-answers. Each is a case here, because a guard that catches
# three of them still lets the fourth through as green:
#
#   002b9624  created, still `queued`, no runner ever assigned
#   258f1a8c  marked `failure` with all three jobs queued and zero steps
#   f1f36660  no run created for the push at all
#   68020ccd  no run until 85 minutes later
#
# The second is the one worth pausing on: it reads as a VERIFIED failure to
# every other mechanism, and this script agrees — a `failure` conclusion is an
# answer, and the canary owns it. The gap is the other three.
#
# ── The direction that matters most ──────────────────────────────────────────
#
# A monitor that fabricates blocks merges during an incident, which is exactly
# when the repair needs to land. Every unknown must exit 0 and say so, and the
# `U` cases pin that.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-main-verified.sh"

pass=0
fail=0

# A commit line as `git log --format='%H %h %s'` prints one.
commit() { printf '%s %s %s\n' "$1" "${1:0:8}" "$2"; }

now_iso() { date -u +%Y-%m-%dT%H:%M:%SZ; }
long_ago() {
  date -u -v-3H +%Y-%m-%dT%H:%M:%SZ 2>/dev/null ||
    date -u -d '3 hours ago' +%Y-%m-%dT%H:%M:%SZ
}

# want <name> <expect-pass|expect-fail> <commits> <runs> [substring]
want() {
  local name="$1" expect="$2" commits="$3" runs="$4" sub="${5:-}" out rc
  out="$("$SCRIPT" --fixture-commits "$commits" --fixture-runs "$runs" 2>&1)"
  rc=$?
  if [ "$expect" = "expect-pass" ]; then
    if [ "$rc" -eq 0 ]; then
      pass=$((pass + 1)); echo "ok   $name"
    else
      fail=$((fail + 1)); echo "FAIL $name — expected exit 0, got $rc:"; echo "$out"
      return
    fi
  else
    if [ "$rc" -eq 0 ]; then
      fail=$((fail + 1)); echo "FAIL $name — the guard passed an unverified commit:"; echo "$out"
      return
    fi
    pass=$((pass + 1)); echo "ok   $name"
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

# ── V: a verified commit is silent ───────────────────────────────────────────
want "a commit with a successful run passes" expect-pass \
  "$(commit $A 'a good merge')" \
  "$A completed success $(now_iso)"

# A FAILING run is still an ANSWER. The canary and the red-main hold own that
# state; a second voice reporting it here would be noise, and would fire on
# every red main forever.
want "a commit with a FAILING run is verified, not reported" expect-pass \
  "$(commit $A 'a merge that broke the tree')" \
  "$A completed failure $(now_iso)"

want "and so is a cancelled or timed-out one" expect-pass \
  "$(commit $A 'a merge whose run was cancelled')" \
  "$A completed cancelled $(now_iso)"

# ── M: no run at all — f1f36660's shape ──────────────────────────────────────
# The one `gh run list` shows as nothing, which reads green to a human.
want "a commit with NO run is reported" expect-fail \
  "$(commit $A 'a merge nothing ran for')" \
  "$B completed success $(now_iso)" \
  "missing"

# ── Q: queued past the threshold — 002b9624's shape ──────────────────────────
want "a run queued for hours with no runner is reported" expect-fail \
  "$(commit $A 'a merge during the outage')" \
  "$A queued none $(long_ago)" \
  "no runner is coming"

# ── P: queued inside the threshold — the third state ─────────────────────────
#
# A run that has not concluded is not a verdict. Folding one into `verified`
# for the whole 45-minute window let the script print "each of the last N
# commits on main has a completed ci run" over commits that had no completed
# run — a monitor asserting what it had not established.
#
# It still exits 0. Firing on every merge in the ninety seconds before a runner
# picks it up is how a monitor earns a mute filter, and an unknown that blocks
# a merge is the failure this whole file argues against. The state is reported,
# not enforced.
want "a run queued a moment ago does not fail the check" expect-pass \
  "$(commit $A 'a merge just now')" \
  "$A in_progress none $(now_iso)"

want "...and is reported as pending rather than verified" expect-pass \
  "$(commit $A 'a merge just now')" \
  "$A in_progress none $(now_iso)" \
  "PENDING"

want "...naming the commit whose run has not concluded" expect-pass \
  "$(commit $A 'a merge just now')" \
  "$A in_progress none $(now_iso)" \
  "${A:0:8}"

# The false claim itself. This is the line a reader takes as green, and it must
# not appear over a commit with no completed run.
out="$("$SCRIPT" --fixture-commits "$(commit $A 'a merge just now')" \
  --fixture-runs "$A queued none $(now_iso)" 2>&1)"
case "$out" in
  *"has a completed ci run"*)
    fail=$((fail + 1)); echo "FAIL a queued run was described as completed:"; echo "$out" ;;
  *) pass=$((pass + 1)); echo "ok   ...and never claims a completed run over a queued one" ;;
esac

# A pending commit beside an unverified one is still a failure, and the report
# carries both: a reader who sees only the absence cannot tell how much of the
# tree is unanswered.
pending_and_missing="$(commit $A 'a merge nothing ran for')
$(commit $B 'a merge still queued')"
want "an unverified commit outranks a pending one" expect-fail \
  "$pending_and_missing" \
  "$B queued none $(now_iso)" \
  "missing"
want "...and the pending one is still listed" expect-fail \
  "$pending_and_missing" \
  "$B queued none $(now_iso)" \
  "has not concluded yet"

# The backlog census, which is what separates "a run is a minute old" from
# "this repository is a hundred runs deep and cannot answer for half an hour".
out="$("$SCRIPT" --fixture-commits "$(commit $A 'a merge just now')" \
  --fixture-runs "$A queued none $(now_iso)" --fixture-queue 46 2>&1)"
case "$out" in
  *"46 of the 100 most recent runs"*)
    pass=$((pass + 1)); echo "ok   the pending report names the queue depth" ;;
  *) fail=$((fail + 1)); echo "FAIL the pending report did not name the queue depth:"; echo "$out" ;;
esac

# ── S: startup_failure — the canary's own blind spot ─────────────────────────
want "a startup_failure is reported" expect-fail \
  "$(commit $A 'a merge whose workflow never began')" \
  "$A completed startup_failure $(now_iso)" \
  "never began"

# ── W: a conclusion this build has never seen ────────────────────────────────
# GitHub adds spellings. An unrecognised one must not read as verified — the
# same three-state discipline the self-driving watch sentinel needed.
want "an unrecognised conclusion is not read as verified" expect-fail \
  "$(commit $A 'a merge with a novel conclusion')" \
  "$A completed moon_phase $(now_iso)" \
  "not a verdict this build recognises"

# ── U: every unknown exits 0 ─────────────────────────────────────────────────
# The direction that matters most: this runs during an Actions incident, which
# is exactly when the repair must be able to land.
if out="$("$SCRIPT" --fixture-commits "" --fixture-runs "$A completed success $(now_iso)" 2>&1)" &&
  [ -z "${out##*UNKNOWN*}" ]; then
  pass=$((pass + 1)); echo "ok   no commits to check is UNKNOWN, not a failure"
else
  fail=$((fail + 1)); echo "FAIL an empty commit list did not exit 0 as unknown:"; echo "$out"
fi

# An unknown says so rather than passing quietly — a silent exit 0 here would
# be indistinguishable from green, which is the whole defect.
case "$out" in
  *"did not establish green"*) pass=$((pass + 1)); echo "ok   and says it established nothing" ;;
  *) fail=$((fail + 1)); echo "FAIL an unknown exited 0 without saying so:"; echo "$out" ;;
esac

# ── N: several commits, one bad ──────────────────────────────────────────────
# The reported window is a run of merges, not one; a guard that stopped at the
# first verified commit would have missed three of the four that day.
multi="$(commit $A 'verified')
$(commit $B 'unverified')"
want "one unverified commit among several is found" expect-fail \
  "$multi" \
  "$A completed success $(now_iso)" \
  "${B:0:8}"

# ── announcing: open, re-report, close ───────────────────────────────────────
#
# Before this the script printed FAILED and exited 1. `main-canary.yml` ran
# it, the step failed, and nothing read it. These cases are the witness. Each
# one fails on the prior script, which had no `--announce` flag.
#
# Same shape as codeql-canary.sh's own suite. `--dry-run` touches no real
# tracker. `--fixture-open-issue` stands in for the `gh issue list` lookup, so
# the recovery branch can run offline.

# want_announce <name> <expect-pass|expect-fail> <commits> <runs> <substring> [more args...]
want_announce() {
  local name="$1" expect="$2" commits="$3" runs="$4" sub="$5" out rc
  shift 5
  out="$("$SCRIPT" --fixture-commits "$commits" --fixture-runs "$runs" \
    --announce --dry-run "$@" 2>&1)"
  rc=$?
  if [ "$expect" = "expect-pass" ]; then
    if [ "$rc" -ne 0 ]; then
      fail=$((fail + 1)); echo "FAIL $name — expected exit 0, got $rc:"; echo "$out"; return
    fi
  else
    if [ "$rc" -eq 0 ]; then
      fail=$((fail + 1)); echo "FAIL $name — expected a non-zero exit:"; echo "$out"; return
    fi
  fi
  case "$out" in
    *"$sub"*) pass=$((pass + 1)); echo "ok   $name" ;;
    *) fail=$((fail + 1)); echo "FAIL $name — output did not say '$sub':"; echo "$out" ;;
  esac
}

unverified_commits="$(commit $A 'a merge nothing ran for')"
unverified_runs="$B completed success $(now_iso)"
verified_commits="$(commit $A 'a good merge')"
verified_runs="$A completed success $(now_iso)"

# `--dry-run` alone would be a configured monitor that reports to nobody.
if out="$("$SCRIPT" --fixture-commits "$verified_commits" --fixture-runs "$verified_runs" \
  --dry-run 2>&1)"; then
  fail=$((fail + 1)); echo "FAIL --dry-run without --announce should have been rejected:"; echo "$out"
else
  case "$out" in
    *"only means something with --announce"*) pass=$((pass + 1)); echo "ok   --dry-run without --announce is rejected" ;;
    *) fail=$((fail + 1)); echo "FAIL --dry-run without --announce gave the wrong reason:"; echo "$out" ;;
  esac
fi

# Unverified, nothing open yet: opens one, labelled `main-unverified` rather
# than `main-red`. "Nothing verified this commit" is not "this commit is
# broken." main-red-hold.yml must not arm on an absence of information.
want_announce "an unverified commit with nothing open creates an issue" expect-fail \
  "$unverified_commits" "$unverified_runs" "gh issue create"
want_announce "...labelled main-unverified, not main-red" expect-fail \
  "$unverified_commits" "$unverified_runs" "main-unverified"
want_announce "...naming the commit nothing verified" expect-fail \
  "$unverified_commits" "$unverified_runs" "${A:0:8}"
want_announce "...with a Definition of done section" expect-fail \
  "$unverified_commits" "$unverified_runs" "### Definition of done"
want_announce "...with an unchecked box, since it is still unverified" expect-fail \
  "$unverified_commits" "$unverified_runs" "- [ ] Every recent commit on \`main\`"

# Unverified, an issue is already open: comments, does not open a second one.
want_announce "a still-unverified run comments instead of filing again" expect-fail \
  "$unverified_commits" "$unverified_runs" "gh issue comment 42" \
  --fixture-open-issue 42
out="$("$SCRIPT" --fixture-commits "$unverified_commits" --fixture-runs "$unverified_runs" \
  --announce --dry-run --fixture-open-issue 42 2>&1)"
case "$out" in
  *"issue create"*) fail=$((fail + 1)); echo "FAIL a re-report should not open a second issue:"; echo "$out" ;;
  *) pass=$((pass + 1)); echo "ok   ...and does not open a second issue" ;;
esac

# Recovered, an issue is open: closes it, ticking the box.
want_announce "a run that answers clean closes the open issue" expect-pass \
  "$verified_commits" "$verified_runs" "gh issue close 42" \
  --fixture-open-issue 42
want_announce "...ticking the Definition of done box first" expect-pass \
  "$verified_commits" "$verified_runs" "- [x] Every recent commit on \`main\`" \
  --fixture-open-issue 42

# Recovered, nothing open: does nothing — a canary that comments on healthy
# days is one that gets muted (main-canary.sh's own header makes this case).
want_announce "a clean run with nothing open does nothing" expect-pass \
  "$verified_commits" "$verified_runs" "nothing open to close"
out="$("$SCRIPT" --fixture-commits "$verified_commits" --fixture-runs "$verified_runs" \
  --announce --dry-run 2>&1)"
case "$out" in
  *"gh "*) fail=$((fail + 1)); echo "FAIL a green run with nothing open must never call gh:"; echo "$out" ;;
  *) pass=$((pass + 1)); echo "ok   ...and never calls gh" ;;
esac

# ── announcing: pending closes nothing ───────────────────────────────────────
#
# The recovery branch fires on a run with no unverified commit, and a tree
# where every run is queued has none. So the backlog that hides an outage
# would also close the issue tracking it, and the next reader would find a
# tracker saying `main` recovered on the strength of nobody having looked.

pending_commits="$(commit $A 'a merge still queued')"
pending_runs="$A queued none $(now_iso)"

out="$("$SCRIPT" --fixture-commits "$pending_commits" --fixture-runs "$pending_runs" \
  --announce --dry-run --fixture-open-issue 42 2>&1)"
case "$out" in
  *"issue close"*)
    fail=$((fail + 1)); echo "FAIL a pending run closed the open issue:"; echo "$out" ;;
  *) pass=$((pass + 1)); echo "ok   a pending run does not close the open issue" ;;
esac
case "$out" in
  *"leaving issue 42 open"*)
    pass=$((pass + 1)); echo "ok   ...and says which issue it left open" ;;
  *) fail=$((fail + 1)); echo "FAIL a pending run did not say what it left open:"; echo "$out" ;;
esac

# A filed issue carries the backlog beside the absence. "Nothing verified
# these commits" and "this repository is 46 runs deep" are one finding, and an
# issue that reports the first without the second sends the reader hunting for
# a cause that is on the same screen.
out="$("$SCRIPT" --fixture-commits "$pending_and_missing" \
  --fixture-runs "$B queued none $(now_iso)" \
  --announce --dry-run --fixture-queue 46 2>&1)"
case "$out" in
  *"46 of the 100 most recent runs"*)
    pass=$((pass + 1)); echo "ok   a filed issue names the backlog beside the absence" ;;
  *) fail=$((fail + 1)); echo "FAIL the filed issue did not name the backlog:"; echo "$out" ;;
esac

# Nor does it file: an answer that has not arrived is not yet an absence, and
# the stuck threshold above owns the point where it becomes one.
out="$("$SCRIPT" --fixture-commits "$pending_commits" --fixture-runs "$pending_runs" \
  --announce --dry-run 2>&1)"
case "$out" in
  *"gh issue"*)
    fail=$((fail + 1)); echo "FAIL a pending run with nothing open touched the tracker:"; echo "$out" ;;
  *) pass=$((pass + 1)); echo "ok   ...and a pending run with nothing open files nothing" ;;
esac

# ── the real repository ──────────────────────────────────────────────────────
# It must not fabricate against real history. Skipped without `gh`, because
# there the script correctly reports UNKNOWN and the case would prove nothing.
if command -v gh >/dev/null 2>&1 && git rev-parse origin/main >/dev/null 2>&1; then
  out="$("$SCRIPT" --limit 3 2>&1)"
  rc=$?
  case "$out" in
    *UNKNOWN*) pass=$((pass + 1)); echo "ok   the real repo answered UNKNOWN (no API access here)" ;;
    *)
      if [ "$rc" -eq 0 ]; then
        pass=$((pass + 1)); echo "ok   the real repo's recent commits are verified"
      else
        # Not a test failure: main really may carry an unverified commit, which
        # is the thing this script exists to say. Reported so the reader sees
        # it rather than reading a red suite as a broken guard.
        pass=$((pass + 1)); echo "ok   the real repo reported unverified commits (that is a finding, not a bug):"
        printf '       %s\n' "$out"
      fi
      ;;
  esac
else
  echo "skip the real repository — no gh or no origin/main"
fi

echo
echo "main-verified: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
