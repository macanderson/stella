#!/usr/bin/env bash
#
# Make every open pull request's `main is not known-broken` check answer
# today's question. Filed as `#5913` and `#5949`.
#
# `main-red-hold.yml` reports that check on every pull request. It is right
# when it runs. But the branch rules read the last check run on that commit,
# and the workflow only fires on `pull_request`. A pull request that is
# finished and waiting raises no such event, so its answer is frozen at the
# moment it was produced.
#
# The state it is answering about is not frozen. `main` breaks and gets fixed
# under it. So the check goes stale in both directions, and each direction
# costs something different:
#
#   - Stale red. `main` is fixed, the hold still fails. On 2026-09-05 ten
#     pull requests stayed unmergeable on a check whose own question had
#     already changed its answer (`#5913`).
#   - Stale green. `main` breaks, the hold still passes, and the pull request
#     merges onto the breakage with a green required check. `#5928` did
#     exactly that: its hold ran at 09:41, the `main-red` issue was filed at
#     10:07, and it merged during the red window on a run 26 minutes older
#     than the outage (`#5949`).
#
# A push to the branch fixes either one. A finished pull request has no reason
# to push. The two moves a session tries first — an empty commit, and close
# and reopen — are the two AGENTS.md bans.
#
# So this script re-asks for them. It reads the tracker once. An open
# `main-red` issue means every hold should be failing; no open issue means
# every hold should be passing. Then it runs the hold again on each open pull
# request whose last run says the other thing. The new run lands under the
# same name on the same commit. That is all the branch rules read.
#
# One sweep covers both directions because there is only one question. Asking
# it twice, in two scripts, is how the two halves drift apart.
#
# ## Why run it again, and not post a new status
#
# The other way is to post a commit status with the same name. Then two
# things wear one name: a check run, and a status. Which one the branch rules
# trust is not a thing this repo can learn, short of breaking `main` to see.
# Running the old run again leaves one verdict.
#
# ## Who calls it
#
# Two callers, since neither one sees every transition.
#
#   - `main-canary.yml`, on the push run that files or closes the `main-red`
#     issue. That is the normal path for both directions, and a workflow on
#     the issue event cannot see it. The canary writes with `GITHUB_TOKEN`,
#     and no event from that token starts a workflow.
#   - `main-red-clear.yml`, when a person closes the issue by hand, or takes
#     the `main-red` label off it.
#
# Two runs cost nothing. The second finds each hold already answering today's
# question and runs nothing.
#
# A pull request labelled `unblocks-main` is swept like any other. Its hold
# passes while `main` is red, by design, so a sweep on the break path re-runs
# it and it passes again. That is one wasted job on the repair pull request,
# and the alternative is reading every pull request's labels to save it.
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
# ## The drill (`#6051`)
#
# The lookup half of a sweep runs constantly. Every push to `main` exercises
# it. The re-run call fires only while a hold disagrees with the tracker, and
# that needs a real outage. So the one step that moves a check was the one
# step with no live evidence. The token might lack a scope. The endpoint might
# refuse a run of that age. Nothing would say so until the next outage, which
# is the worst moment to find out.
#
# `--drill <pr>` rehearses that exact call on one named pull request's latest
# hold run, whatever its conclusion. It never asks the tracker anything and
# never looks at any other pull request, so it is safe to run any day:
# re-running a run that already passed still passes. A failure here — a
# missing scope, a stale run id the API refuses — exits non-zero, on purpose,
# unlike the rest of this script: the drill exists to surface that failure
# now, not to fail open through it.
#
# Usage:
#   scripts/refresh-main-red-holds.sh
#   scripts/refresh-main-red-holds.sh --dry-run
#   scripts/refresh-main-red-holds.sh --drill <pr-number>
#
# Needs `gh` and a POSIX shell, so it runs on a bare CI runner.
set -uo pipefail

label="main-red"
workflow="main-red-hold.yml"
# Zero pages the whole list. A caller who wants a cap asks for one, and gets
# told when it cuts the list short.
limit=0
dry_run=0
drill_pr=""
fixture_open_issues=""
fixture_open_prs=""
fixture_hold_runs=""
fixture_drill_run=""
use_fixture=0

while [ $# -gt 0 ]; do
  case "$1" in
  --dry-run)
    dry_run=1
    shift
    ;;
  --limit)
    [ $# -ge 2 ] || {
      echo "refresh-main-red-holds: --limit needs a number" >&2
      exit 2
    }
    limit="$2"
    case "$limit" in '' | *[!0-9]*)
      echo "refresh-main-red-holds: --limit takes a whole number" >&2
      exit 2
      ;;
    esac
    shift 2
    ;;
  --drill)
    [ $# -ge 2 ] || {
      echo "refresh-main-red-holds: --drill needs a pull request number" >&2
      exit 2
    }
    drill_pr="$2"
    case "$drill_pr" in '' | *[!0-9]*)
      echo "refresh-main-red-holds: --drill takes a pull request number" >&2
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
      echo "refresh-main-red-holds: --fixture-open-issues needs a value" >&2
      exit 2
    }
    fixture_open_issues="$2"
    use_fixture=1
    shift 2
    ;;
  --fixture-open-prs)
    [ $# -ge 2 ] || {
      echo "refresh-main-red-holds: --fixture-open-prs needs a value" >&2
      exit 2
    }
    fixture_open_prs="$2"
    use_fixture=1
    shift 2
    ;;
  --fixture-hold-runs)
    [ $# -ge 2 ] || {
      echo "refresh-main-red-holds: --fixture-hold-runs needs a value" >&2
      exit 2
    }
    fixture_hold_runs="$2"
    use_fixture=1
    shift 2
    ;;
  --fixture-drill-run)
    [ $# -ge 2 ] || {
      echo "refresh-main-red-holds: --fixture-drill-run needs a value" >&2
      exit 2
    }
    fixture_drill_run="$2"
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
    echo "refresh-main-red-holds: unknown argument '$1'" >&2
    exit 2
    ;;
  esac
done

[ "$use_fixture" -eq 1 ] && dry_run=1

note() { printf 'refresh-main-red-holds: %s\n' "$*" >&2 || true; }
say() {
  # Ignore SIGPIPE and drop the write. A reader that closes the pipe must not
  # turn this into a failure — the `#1815` shape.
  trap '' PIPE
  printf 'refresh-main-red-holds: %s\n' "$*" || true
}

if [ "$use_fixture" -eq 0 ] && ! command -v gh >/dev/null 2>&1; then
  note "gh is not installed, so no hold could be refreshed. A push to each"
  note "branch still refreshes its own hold."
  exit 0
fi

# ── The drill: rehearse the call on one named pull request (`#6051`) ────────
#
# The drill asks the tracker nothing. That question is what makes the
# ordinary re-run untestable most days. So the drill rehearses the call
# without waiting for `main` to break. It never looks past the one pull
# request it is given.

if [ -n "$drill_pr" ]; then
  drill_head=""
  if [ "$use_fixture" -eq 1 ]; then
    drill_head="$(printf '%s\n' "$fixture_open_prs" |
      awk -v pr="$drill_pr" '$1 == pr { print $2; exit }')"
  elif ! drill_head="$(gh pr view "$drill_pr" --json headRefOid \
    --jq '.headRefOid' 2>/dev/null)" || [ -z "$drill_head" ]; then
    drill_head=""
  fi
  if [ -z "$drill_head" ]; then
    note "could not find pull request #$drill_pr, so there is nothing to drill."
    exit 1
  fi

  drill_run=""
  if [ "$use_fixture" -eq 1 ]; then
    drill_run="$fixture_drill_run"
  elif ! drill_run="$(gh api \
    "repos/{owner}/{repo}/actions/workflows/$workflow/runs?head_sha=$drill_head&per_page=1" \
    --jq '.workflow_runs[0].id // "none"' 2>/dev/null)"; then
    drill_run=""
  fi

  if [ -z "$drill_run" ] || [ "$drill_run" = "none" ]; then
    note "no $workflow run exists on the head of #$drill_pr — there is nothing"
    note "to drill. Push a commit to that pull request first."
    exit 1
  fi

  if [ "$dry_run" -eq 1 ]; then
    say "would re-run the hold on PR #$drill_pr (head $drill_head, run $drill_run)"
    exit 0
  fi

  if gh api -X POST --silent \
    "repos/{owner}/{repo}/actions/runs/$drill_run/rerun" 2>/dev/null; then
    say "re-ran the hold on PR #$drill_pr (head $drill_head, run $drill_run)"
    repo_slug="${GITHUB_REPOSITORY:-$(gh repo view --json nameWithOwner \
      --jq '.nameWithOwner' 2>/dev/null)}"
    [ -n "$repo_slug" ] &&
      say "https://github.com/$repo_slug/actions/runs/$drill_run"
    exit 0
  fi
  note "could not re-run run $drill_run for PR #$drill_pr — does this job have"
  note "\`actions: write\`? That is exactly the gap this drill exists to find."
  exit 1
fi

# ── What should every hold be saying right now? ─────────────────────────────
#
# One question to the tracker, and it settles the whole sweep. While a
# `main-red` issue is open the holds belong red; with none open they belong
# green. Either way the sweep only touches a run that says the other thing.

if [ "$use_fixture" -eq 1 ]; then
  open_issues="$fixture_open_issues"
elif ! open_issues="$(gh issue list --label "$label" --state open \
  --limit 20 --json number --jq '.[].number' 2>/dev/null)"; then
  note "could not reach the issue tracker, so this run could not ask what the"
  note "holds should be saying. Refreshing nothing."
  exit 0
fi

open_issues="$(printf '%s' "$open_issues" | tr -d ' ' | tr '\n' ' ')"
open_issues="${open_issues% }"

if [ -n "$open_issues" ]; then
  want="failure"
  standing="main is known-broken ($open_issues)"
else
  want="success"
  standing="main has recovered"
fi

# ── Run the hold again wherever it disagrees ────────────────────────────────

# `gh pr list --limit N` capped the sweep at one page and said nothing about
# what it never looked at. `--paginate` follows the Link headers instead, so
# the list is the whole list however long it is.
if [ "$use_fixture" -eq 1 ]; then
  open_prs="$fixture_open_prs"
elif ! open_prs="$(gh api --paginate \
  "repos/{owner}/{repo}/pulls?state=open&per_page=100" \
  --jq '.[] | "\(.number) \(.head.sha)"' 2>/dev/null)"; then
  note "could not list the open pull requests, so no hold was refreshed."
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

# `<conclusion> <run id>` for the last hold run on one head. A re-run updates
# the run in place, so `[0]` is that run and its conclusion is the verdict the
# branch rules read.
#
#   success <id>  The hold passed.
#   failure <id>  The hold failed.
#   running -     A run is under way. It reports today's answer by itself.
#   none    -     No hold run on this head at all. Nothing to re-run, and the
#                 required check stays unreported.
#   unknown -     The API could not be read.
#
# Any other conclusion is neither verdict. `cancelled` and `timed_out` never
# match what the sweep wants, so they get re-run. That is right. The branch
# rules read those as not-passing too.
hold_state() { # hold_state <head sha>
  local head="$1" answer rest total conclusion id
  if [ "$use_fixture" -eq 1 ]; then
    # A fixture line is `<head> <conclusion> <run id>`; the run id may be left
    # off for a state that carries none. A head the fixture does not mention is
    # `none`, which is what "nothing is known about it" means.
    answer="$(printf '%s\n' "$fixture_hold_runs" |
      awk -v head="$head" '$1 == head { print $2, ($3 == "" ? "-" : $3); exit }')"
    case "$answer" in
    '') printf 'none -' ;;
    *) printf '%s' "$answer" ;;
    esac
    return 0
  fi
  if ! answer="$(gh api \
    "repos/{owner}/{repo}/actions/workflows/$workflow/runs?head_sha=$head&per_page=20" \
    --jq '"\(.total_count) \(.workflow_runs[0].conclusion // "running") \(.workflow_runs[0].id // "-")"' \
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
  else
    printf '%s %s' "$conclusion" "$id"
  fi
}

refreshed=0
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
  running) continue ;;
  "$want") continue ;;
  esac
  if [ "$dry_run" -eq 1 ]; then
    say "would re-run the hold on PR #$pr (head $head, run $run)"
    refreshed=$((refreshed + 1))
    continue
  fi
  if gh api -X POST --silent \
    "repos/{owner}/{repo}/actions/runs/$run/rerun" 2>/dev/null; then
    say "re-ran the hold on PR #$pr (head $head, run $run)"
    refreshed=$((refreshed + 1))
  else
    # This needs `actions: write`. A job without it must say so, rather than
    # report a clean sweep it did not do.
    note "could not re-run run $run for PR #$pr — does this job have"
    note "\`actions: write\`? That hold keeps its old answer until the branch"
    note "is pushed."
  fi
done <<EOF
$open_prs
EOF

say "$standing — re-ran the stale hold on $refreshed of $seen open pull request(s)."

if [ -n "$unrun" ]; then
  say "no $workflow run exists on the head of${unrun} — there is nothing to"
  say "re-run, and \`main is not known-broken\` is a required check, so each of"
  say "those stays unmergeable until its branch is pushed."
fi
exit 0
