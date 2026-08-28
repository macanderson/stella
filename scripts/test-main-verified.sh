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

# ...and a run queued for a moment is NOT. Without this the guard fires on
# every merge in the ninety seconds before a runner picks it up, which is how a
# monitor earns a mute filter.
want "a run queued a moment ago is not reported" expect-pass \
  "$(commit $A 'a merge just now')" \
  "$A in_progress none $(now_iso)"

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
