#!/usr/bin/env bash
#
# Clear the merge hold a fixed `main` leaves behind. Filed as `#5913`.
#
# `main-red-hold.yml` reports the check `main is not known-broken` on every
# pull request. It is right when it runs. But the branch rules read the last
# check run on that commit, and nothing runs it again.
#
# So the hold outlives the outage. On 2026-09-05 a fix landed and the canary
# closed its issue. Ten open pull requests stayed stuck. Each was held by a
# check that would now pass.
#
# A push to the branch clears it. A finished pull request has no reason to
# push. The two moves a session tries first — an empty commit, and close and
# reopen — are the two AGENTS.md bans.
#
# So the fix has to clear the holds it caused. This script is that step. It
# asks whether `main` is still known-broken. If it is not, it runs the failed
# hold again on every open pull request. The new run lands under the same
# name on the same commit. That is all the branch rules read.
#
# ## Why run it again, and not post a new status
#
# The other way is to post a commit status with the same name. Then two
# things wear one name: a failed check run, and a passing status. Which one
# the branch rules trust is not a thing this repo can learn, short of
# breaking `main` to see. Running the old run again leaves one verdict.
#
# ## Who calls it
#
# Two callers, since neither one sees every fix.
#
#   - `main-canary.yml`, on the run that closes the `main-red` issue. That is
#     the normal path, and a workflow on the issue event cannot see it. The
#     canary closes with `GITHUB_TOKEN`, and no event from that token starts
#     a workflow.
#   - `main-red-clear.yml`, when a person closes the issue by hand, or takes
#     the `main-red` label off it.
#
# Two runs cost nothing. The second finds the holds green and runs nothing.
#
# ## Fails open, out loud
#
# Every unknown prints a note and exits 0. This runs inside the canary. A
# canary turned red by a `gh` blip would say `main` is broken when it is not.
# That is the one lie this machinery must not tell.
#
# ## What it names rather than passes over (`#6052`)
#
# Two shapes slipped past a sweep that still read as clean.
#
# The list was capped at one page, silently. A repository with more open pull
# requests than the cap left the rest unlooked-at, and the summary counted only
# what it saw. The list is paged now, and `--limit` is an explicit cap that says
# so when it bites.
#
# A head can also carry no hold run at all — an Actions outage, or a run that
# was never created. There is nothing to re-run, `main is not known-broken` is a
# required check, and the pull request stays unmergeable. The sweep counts those
# and names them, so a reader knows which branches still need a push.
#
# Usage:
#   scripts/clear-main-red-holds.sh
#   scripts/clear-main-red-holds.sh --dry-run
#
# Needs `gh` and a POSIX shell, so it runs on a bare CI runner.
set -uo pipefail

label="main-red"
workflow="main-red-hold.yml"
# Zero pages the whole list. A caller who wants a cap asks for one, and gets
# told when it cuts the list short.
limit=0
dry_run=0
fixture_open_issues=""
fixture_open_prs=""
fixture_stale_runs=""
use_fixture=0

while [ $# -gt 0 ]; do
  case "$1" in
  --dry-run)
    dry_run=1
    shift
    ;;
  --limit)
    [ $# -ge 2 ] || {
      echo "clear-main-red-holds: --limit needs a number" >&2
      exit 2
    }
    limit="$2"
    case "$limit" in '' | *[!0-9]*)
      echo "clear-main-red-holds: --limit takes a whole number" >&2
      exit 2
      ;;
    esac
    shift 2
    ;;
  # Test-only seams. Any one of them stubs every lookup, and stops the re-run
  # from being sent. So a case cannot half-reach the network, and cannot pass
  # or fail for a reason it did not pick. Same rule as the paired fixtures in
  # `check-main-red-hold.sh`.
  --fixture-open-issues)
    [ $# -ge 2 ] || {
      echo "clear-main-red-holds: --fixture-open-issues needs a value" >&2
      exit 2
    }
    fixture_open_issues="$2"
    use_fixture=1
    shift 2
    ;;
  --fixture-open-prs)
    [ $# -ge 2 ] || {
      echo "clear-main-red-holds: --fixture-open-prs needs a value" >&2
      exit 2
    }
    fixture_open_prs="$2"
    use_fixture=1
    shift 2
    ;;
  --fixture-stale-runs)
    [ $# -ge 2 ] || {
      echo "clear-main-red-holds: --fixture-stale-runs needs a value" >&2
      exit 2
    }
    fixture_stale_runs="$2"
    use_fixture=1
    shift 2
    ;;
  -h | --help)
    # The whole block after the shebang, cut off at its first line of code.
    # A line number here goes stale the first time the header grows. Same
    # reader as the one in `main-canary.sh`.
    awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0"
    exit 0
    ;;
  *)
    echo "clear-main-red-holds: unknown argument '$1'" >&2
    exit 2
    ;;
  esac
done

[ "$use_fixture" -eq 1 ] && dry_run=1

note() { printf 'clear-main-red-holds: %s\n' "$*" >&2 || true; }
say() {
  # Ignore SIGPIPE and drop the write. A reader that closes the pipe must not
  # turn this into a failure — the `#1815` shape.
  trap '' PIPE
  printf 'clear-main-red-holds: %s\n' "$*" || true
}

if [ "$use_fixture" -eq 0 ] && ! command -v gh >/dev/null 2>&1; then
  note "gh is not installed, so no hold could be cleared. A push to each"
  note "branch still clears its own hold."
  exit 0
fi

# ── Is main still known-broken? ──────────────────────────────────────────────
#
# This one question decides whether there is anything to clear. While an issue
# is open the holds are right, and they must stay red.

if [ "$use_fixture" -eq 1 ]; then
  open_issues="$fixture_open_issues"
elif ! open_issues="$(gh issue list --label "$label" --state open \
  --limit 20 --json number --jq '.[].number' 2>/dev/null)"; then
  note "could not reach the issue tracker, so this run could not ask whether"
  note "main has recovered. Clearing nothing."
  exit 0
fi

open_issues="$(printf '%s' "$open_issues" | tr -d ' ' | tr '\n' ' ')"
open_issues="${open_issues% }"

if [ -n "$open_issues" ]; then
  say "main is still known-broken ($open_issues) — the holds are correct."
  exit 0
fi

# ── Run the hold again on every open pull request ────────────────────────────

# `gh pr list --limit N` capped the sweep at one page and said nothing about
# what it never looked at. `--paginate` follows the Link headers instead, so
# the list is the whole list however long it is.
if [ "$use_fixture" -eq 1 ]; then
  open_prs="$fixture_open_prs"
elif ! open_prs="$(gh api --paginate \
  "repos/{owner}/{repo}/pulls?state=open&per_page=100" \
  --jq '.[] | "\(.number) \(.head.sha)"' 2>/dev/null)"; then
  note "could not list the open pull requests, so no hold was cleared."
  exit 0
fi

open_prs="$(printf '%s\n' "$open_prs" | sed '/^$/d')"

if [ "$limit" -gt 0 ]; then
  total="$(printf '%s\n' "$open_prs" | sed '/^$/d' | wc -l | tr -d ' ')"
  if [ "$total" -gt "$limit" ]; then
    note "--limit $limit cuts the list short: $total open pull requests, so"
    note "$((total - limit)) of them are not swept by this run."
    open_prs="$(printf '%s\n' "$open_prs" | head -n "$limit")"
  fi
fi

# `<state> <run id>` for the last hold run on one head. A re-run updates the
# run in place, so `[0]` is that run and its conclusion is the verdict the
# branch rules read.
#
#   failure <id>  the last run failed — a re-run is what clears it
#   ok      -     the last run is not a failure; nothing to do
#   none    -     no hold run on this head at all, so there is nothing to
#                 re-run and the required check stays unreported
#   unknown -     the API could not be read
hold_state() { # hold_state <head sha>
  local head="$1" answer rest total conclusion id
  if [ "$use_fixture" -eq 1 ]; then
    # A fixture line is `<head> <run id>`, or `<head> ok` / `<head> none` for
    # the two states that carry no run to name. A head the fixture does not
    # mention is `none`, which is what "nothing is known about it" means.
    answer="$(printf '%s\n' "$fixture_stale_runs" |
      awk -v head="$head" '$1 == head { print $2; exit }')"
    case "$answer" in
    '' | none) printf 'none -' ;;
    ok) printf 'ok -' ;;
    *) printf 'failure %s' "$answer" ;;
    esac
    return 0
  fi
  if ! answer="$(gh api \
    "repos/{owner}/{repo}/actions/workflows/$workflow/runs?head_sha=$head&per_page=20" \
    --jq '"\(.total_count) \(.workflow_runs[0].conclusion // "-") \(.workflow_runs[0].id // "-")"' \
    2>/dev/null)" || [ -z "$answer" ]; then
    printf 'unknown -'
    return 0
  fi
  total="${answer%% *}"
  rest="${answer#* }"
  conclusion="${rest%% *}"
  id="${rest##* }"
  if [ "$total" = "0" ]; then
    printf 'none -'
  elif [ "$conclusion" = "failure" ]; then
    printf 'failure %s' "$id"
  else
    printf 'ok -'
  fi
}

cleared=0
seen=0
unrun=""

while read -r pr head; do
  [ -n "$pr" ] || continue
  seen=$((seen + 1))
  state="$(hold_state "$head")"
  run="${state#* }"
  case "${state%% *}" in
  none)
    unrun="${unrun} #${pr}"
    continue
    ;;
  unknown)
    note "could not read the hold runs for PR #$pr (head $head); it is neither"
    note "swept nor counted as needing a push."
    continue
    ;;
  ok) continue ;;
  esac
  if [ "$dry_run" -eq 1 ]; then
    say "would re-run the hold on PR #$pr (head $head, run $run)"
    cleared=$((cleared + 1))
    continue
  fi
  if gh api -X POST --silent \
    "repos/{owner}/{repo}/actions/runs/$run/rerun-failed-jobs" 2>/dev/null; then
    say "re-ran the hold on PR #$pr (head $head, run $run)"
    cleared=$((cleared + 1))
  else
    # This needs `actions: write`. A job without it must say so, rather than
    # report a clean sweep it did not do.
    note "could not re-run run $run for PR #$pr — does this job have"
    note "\`actions: write\`? That hold stays red until the branch is pushed."
  fi
done <<EOF
$open_prs
EOF

say "main has recovered — cleared the hold on $cleared of $seen open pull request(s)."

if [ -n "$unrun" ]; then
  say "no $workflow run exists on the head of${unrun} — there is nothing to"
  say "re-run, and \`main is not known-broken\` is a required check, so each of"
  say "those stays unmergeable until its branch is pushed."
fi
exit 0
