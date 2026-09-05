#!/usr/bin/env bash
#
# Run the DoD check again after its issue changed. Filed as `#6079`.
#
# `dod-check` reads the checklist on the linked issue. But it runs on pull
# request events. So the thing it reads and the events it hears are two
# different objects. Tick the last box on the issue and nothing happens. The
# pull request keeps a red `dod / dod` until someone touches the pull request.
#
# On 2026-09-05 four pull requests sat red on that check alone. Every other
# check was green. Three of them had to be nudged by editing the pull request
# body, which rewrites the description of a pull request someone else wrote.
#
# This script is the missing step. Give it an issue number. It finds the open
# pull requests whose body names that issue, finds the last `dod-check` run on
# each head, and runs it again when that run failed.
#
# ## Why run it again, and not post a new check
#
# A new check would have to be reported against a head this event does not
# own. Running the old run again needs no such thing. The new run lands under
# the same name on the same commit, which is all the branch rules read. This
# is the shape `#5913` settled for the red-main hold, and the reason is the
# same both times.
#
# ## What bounds the fan-out
#
# One issue can be named by many pull requests, and a body edit is cheap. So
# the failure itself is the scope: a pull request whose `dod / dod` already
# passes is left alone. Only a head whose last run failed is run again.
#
# ## Who calls it
#
# `dod-recheck.yml`, on an edit to any issue. An edit by `GITHUB_TOKEN` starts
# no workflow, so a box ticked by that token is the one case this misses.
#
# ## Fails open, out loud
#
# Every unknown prints a note and exits 0. A red job here would say the
# checklist is unmet when nobody asked it that. The check itself is the only
# thing allowed to answer that question.
#
# Usage:
#   scripts/dod-recheck.sh 6079
#   scripts/dod-recheck.sh --dry-run 6079
#
# Needs `gh` and a POSIX shell, so it runs on a bare CI runner.
set -uo pipefail

workflow="dod-check.yml"
limit=100
dry_run=0
issue=""
fixture_open_prs=""
fixture_failed_runs=""
use_fixture=0

while [ $# -gt 0 ]; do
  case "$1" in
  --dry-run)
    dry_run=1
    shift
    ;;
  --limit)
    [ $# -ge 2 ] || {
      echo "dod-recheck: --limit needs a number" >&2
      exit 2
    }
    limit="$2"
    shift 2
    ;;
  # Test-only seams. Either one stubs every lookup, and stops the re-run from
  # being sent. So a case cannot half-reach the network, and cannot pass or
  # fail for a reason it did not pick. Same rule as the fixtures in
  # `clear-main-red-holds.sh`.
  --fixture-open-prs)
    [ $# -ge 2 ] || {
      echo "dod-recheck: --fixture-open-prs needs a value" >&2
      exit 2
    }
    fixture_open_prs="$2"
    use_fixture=1
    shift 2
    ;;
  --fixture-failed-runs)
    [ $# -ge 2 ] || {
      echo "dod-recheck: --fixture-failed-runs needs a value" >&2
      exit 2
    }
    fixture_failed_runs="$2"
    use_fixture=1
    shift 2
    ;;
  -h | --help)
    # The whole block after the shebang, cut off at its first line of code. A
    # line number here goes stale the first time the header grows. Same reader
    # as the one in `clear-main-red-holds.sh`.
    awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0"
    exit 0
    ;;
  -*)
    echo "dod-recheck: unknown argument '$1'" >&2
    exit 2
    ;;
  *)
    if [ -n "$issue" ]; then
      echo "dod-recheck: one issue number, not two ('$issue' and '$1')" >&2
      exit 2
    fi
    issue="$1"
    shift
    ;;
  esac
done

[ "$use_fixture" -eq 1 ] && dry_run=1

note() { printf 'dod-recheck: %s\n' "$*" >&2 || true; }
say() {
  # Ignore SIGPIPE and drop the write. A reader that closes the pipe must not
  # turn this into a failure — the `#1815` shape.
  trap '' PIPE
  printf 'dod-recheck: %s\n' "$*" || true
}

# A missing or malformed number is a wiring bug in the caller, not an unknown
# about the world. It exits loudly, where every other failure here fails open.
case "$issue" in
"")
  echo "dod-recheck: needs an issue number" >&2
  exit 2
  ;;
*[!0-9]*)
  echo "dod-recheck: '$issue' is not an issue number" >&2
  exit 2
  ;;
esac

if [ "$use_fixture" -eq 0 ] && ! command -v gh >/dev/null 2>&1; then
  note "gh is not installed, so no check could be run again. A push to the"
  note "branch still re-runs it."
  exit 0
fi

# ── The open pull requests, one per line ─────────────────────────────────────
#
# Each line is the number, the head commit, then the body with its line breaks
# flattened to spaces. The match happens below, on the line, so the fixture
# seam and the live path are read by the same matcher.
#
# The bodies are read rather than searched. Code search is an index, and an
# index lags. A pull request opened a minute ago is the exact case this has to
# get right.

if [ "$use_fixture" -eq 1 ]; then
  open_prs="$fixture_open_prs"
elif ! open_prs="$(gh pr list --state open --limit "$limit" \
  --json number,headRefOid,body \
  --jq '.[] | "\(.number) \(.headRefOid) \(.body // "" | gsub("[\r\n]+"; " "))"' \
  2>/dev/null)"; then
  note "could not list the open pull requests, so nothing was run again."
  exit 0
fi

# The last `dod-check` run on this head, and only if it failed. A re-run
# updates the run in place, so `[0]` is that run, and its conclusion is the
# verdict the branch rules read.
failed_run_for() {
  head="$1"
  if [ "$use_fixture" -eq 1 ]; then
    printf '%s\n' "$fixture_failed_runs" |
      awk -v head="$head" '$1 == head { print $2; exit }'
    return 0
  fi
  gh api "repos/{owner}/{repo}/actions/workflows/$workflow/runs?head_sha=$head&per_page=20" \
    --jq '.workflow_runs[0] | select(.conclusion == "failure") | .id' 2>/dev/null || true
}

rerun=0
named=0

while read -r pr head rest; do
  [ -n "$pr" ] || continue
  # `#6079` and not `#60791`. A bare `6079` is not a reference, and would
  # match a run id or a date as readily as an issue.
  printf '%s' "$rest" | grep -Eq "#${issue}([^0-9]|\$)" || continue
  named=$((named + 1))
  run="$(failed_run_for "$head")"
  if [ -z "$run" ]; then
    say "PR #$pr names the issue and its dod check is not failing — left alone."
    continue
  fi
  if [ "$dry_run" -eq 1 ]; then
    say "would re-run the dod check on PR #$pr (head $head, run $run)"
    rerun=$((rerun + 1))
    continue
  fi
  if gh api -X POST --silent \
    "repos/{owner}/{repo}/actions/runs/$run/rerun-failed-jobs" 2>/dev/null; then
    say "re-ran the dod check on PR #$pr (head $head, run $run)"
    rerun=$((rerun + 1))
  else
    # This needs `actions: write`. A job without it must say so, rather than
    # report a sweep it did not do.
    note "could not re-run run $run for PR #$pr — does this job have"
    note "\`actions: write\`? That check stays red until the branch is pushed."
  fi
done <<EOF
$open_prs
EOF

say "#$issue is named by $named open pull request(s); ran the dod check again on $rerun."
exit 0
