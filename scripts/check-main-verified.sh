#!/usr/bin/env bash
#
# Has every recent commit on `main` actually been verified? (#5027)
#
#   scripts/check-main-verified.sh [--limit N] [--stuck-minutes M]
#
# ── The gap this closes ──────────────────────────────────────────────────────
#
# Every existing mechanism here answers "is main KNOWN BROKEN". None answers
# "is main UNVERIFIED", and those are different states:
#
#   main-canary.yml   files only when its job RUNS and FAILS. A
#                     `startup_failure`, a cancellation, or a run that is never
#                     created produces no issue at all.
#   main-red-hold.yml asks the tracker whether a `main-red` issue is open. With
#                     none filed it passes, so merges keep flowing onto a tree
#                     nothing has checked.
#   gh run list       reads GREEN to a human skimming it, because a run that
#                     was never created leaves no row.
#
# On 2026-08-26 between 15:25 and 15:31 UTC, Actions stopped allocating
# runners. Four commits landed on `main` in that window and none got a
# completed `ci` run: one still `queued` with no runner ever assigned, one
# marked `failure` with all three jobs `queued` and zero steps executed, and
# two with no run created at all. `main` sat unverified for about 85 minutes
# and nothing said so. It happened to be fine — which is luck, not a signal.
# The same window would have looked identical if one of those merges had
# broken the tree.
#
# ── What counts as verified ──────────────────────────────────────────────────
#
# A `ci` run for that exact commit that reached a terminal conclusion of its
# own accord: `success`, `failure`, `cancelled`, `timed_out`. A FAILING run is
# verified — the canary and the hold deal with that, and it is not this
# script's business. What is reported is the absence of an answer:
#
#   missing          no `ci` run exists for the commit
#   queued too long  a run past --stuck-minutes with no runner
#   startup_failure  the workflow never began, so no check ran
#
# ── It fails OPEN, at every unknown ──────────────────────────────────────────
#
# No `gh`, an unreachable API, an unparseable answer: report and exit 0. This
# is a monitor, and a monitor that can itself block a merge is worse than the
# gap it watches — the same argument `main-red-claim.sh`'s header makes about
# a claim check. Every unknown is loud, never silent.

set -uo pipefail

# A decided verdict must survive a reader that closes the pipe early (#1815).
trap '' PIPE

limit=10
stuck_minutes=45
fixture_runs=""
fixture_commits=""
use_fixture=0

while [ $# -gt 0 ]; do
  case "$1" in
  --limit)
    limit="${2:-}"
    shift 2
    ;;
  --stuck-minutes)
    stuck_minutes="${2:-}"
    shift 2
    ;;
  # Test-only, and paired: a fixture that supplied runs but took real commits
  # would compare two different worlds and report nonsense.
  --fixture-runs)
    fixture_runs="${2:-}"
    use_fixture=1
    shift 2
    ;;
  --fixture-commits)
    fixture_commits="${2:-}"
    use_fixture=1
    shift 2
    ;;
  -h | --help)
    awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0"
    exit 0
    ;;
  *)
    echo "check-main-verified: unknown argument '$1'" >&2
    exit 2
    ;;
  esac
done

case "$limit" in '' | *[!0-9]*)
  echo "check-main-verified: --limit takes a whole number" >&2
  exit 2
  ;;
esac
case "$stuck_minutes" in '' | *[!0-9]*)
  echo "check-main-verified: --stuck-minutes takes a whole number" >&2
  exit 2
  ;;
esac

if [ "$use_fixture" -eq 1 ] && { [ -z "$fixture_runs" ] && [ -z "$fixture_commits" ]; }; then
  echo "check-main-verified: --fixture-runs and --fixture-commits go together" >&2
  exit 2
fi

# `unknown` is the only exit this script takes when it cannot answer, so it is
# one function rather than a repeated pair of lines that could drift apart.
unknown() {
  echo "check-main-verified: UNKNOWN — $1"
  echo "  Exiting 0: a monitor that can block a merge is worse than the gap it"
  echo "  watches. This run established nothing; it did not establish green."
  exit 0
}

if [ "$use_fixture" -eq 0 ] && ! command -v gh >/dev/null 2>&1; then
  unknown "gh is not installed, so no run could be looked up"
fi

# ── The two reads ────────────────────────────────────────────────────────────
#
# Commits first: the question is about commits that landed, and a run list with
# no commits to match it against cannot answer anything.

if [ "$use_fixture" -eq 1 ]; then
  commits="$fixture_commits"
elif ! commits="$(git log --format='%H %h %s' -n "$limit" origin/main 2>/dev/null)"; then
  unknown "could not read origin/main — fetch it first"
fi
if [ -z "$commits" ]; then
  unknown "origin/main has no commits to check"
fi

# `<sha> <status> <conclusion> <created_at>` per line, which is the shape the
# fixture supplies too so the two paths cannot diverge.
if [ "$use_fixture" -eq 1 ]; then
  runs="$fixture_runs"
elif ! runs="$(gh run list --workflow ci.yml --branch main --limit 60 \
  --json headSha,status,conclusion,createdAt \
  --jq '.[] | "\(.headSha) \(.status) \(.conclusion // "none") \(.createdAt)"' 2>/dev/null)"; then
  unknown "could not reach the Actions API"
fi

now_epoch="$(date -u +%s 2>/dev/null || echo 0)"

# Seconds since an RFC-3339 timestamp, or 0 when this platform's `date` will
# not parse it — BSD and GNU take different flags, which is what
# `stat-portability` is about. Zero reads as "just created" and so never
# reports stuck: an unparseable timestamp must not become a finding.
age_seconds() {
  local ts="$1" at
  at="$(date -u -j -f '%Y-%m-%dT%H:%M:%SZ' "$ts" +%s 2>/dev/null ||
    date -u -d "$ts" +%s 2>/dev/null || echo 0)"
  [ "$at" -eq 0 ] && echo 0 && return
  echo $((now_epoch - at))
}

unverified=""
count=0

while IFS= read -r commit; do
  [ -z "$commit" ] && continue
  sha="${commit%% *}"
  rest="${commit#* }"
  short="${rest%% *}"
  subject="${rest#* }"
  count=$((count + 1))

  verdict="missing"
  while IFS= read -r run; do
    [ -z "$run" ] && continue
    run_sha="${run%% *}"
    [ "$run_sha" = "$sha" ] || continue
    run_rest="${run#* }"
    status="${run_rest%% *}"
    run_rest="${run_rest#* }"
    conclusion="${run_rest%% *}"
    created="${run_rest##* }"

    case "$conclusion" in
    success | failure | cancelled | timed_out)
      # A FAILING run is a verified commit: the canary and the hold own that
      # state, and reporting it here would be a second voice saying the same
      # thing in different words.
      verdict="verified"
      break
      ;;
    startup_failure)
      verdict="startup_failure — the workflow never began, so no check ran"
      ;;
    *)
      if [ "$status" = "completed" ]; then
        verdict="completed as '$conclusion', which is not a verdict this build recognises"
      else
        age="$(age_seconds "$created")"
        if [ "$age" -gt $((stuck_minutes * 60)) ]; then
          verdict="$status for $((age / 60))m — past the ${stuck_minutes}m threshold, so no runner is coming"
        else
          verdict="verified"
        fi
      fi
      ;;
    esac
  done <<EOF
$runs
EOF

  if [ "$verdict" != "verified" ]; then
    unverified="${unverified}  $short  $verdict
      $subject
"
  fi
done <<EOF
$commits
EOF

if [ -z "$unverified" ]; then
  echo "check-main-verified: OK — each of the last $count commit(s) on main has a completed ci run."
  exit 0
fi

echo "check-main-verified: FAILED — main carries commits nothing verified."
echo
echo "$unverified"
cat <<'TXT'
This is NOT "main is red". A failing run is a verified commit and the canary
owns that. These commits have no answer at all, which every other mechanism
here reads as green: the canary files only when its job runs and fails, the
red-main hold passes when no issue is open, and `gh run list` shows no row for
a run that was never created.

Re-dispatch the missing runs and let them finish before merging onto this tree:

  gh workflow run ci.yml --ref main

An Actions incident is one cause. There the runs start on their own once
capacity returns, and nothing was checking, which is the part this exists to
fix. A push made with the token a workflow run holds is the other cause, and
the release version write-back is one of those: that push raised no event at
all, so no run was ever created and none is coming. Nothing will start on its
own. Ask for it:

  ./scripts/dispatch-main-verification.sh
TXT
exit 1
