#!/usr/bin/env bash
#
# Update the branch of ONE armed auto-merge PR that has fallen behind. See #1527.
#
#   ./scripts/automerge-nudge.sh [--base main] [--dry-run]
#   ./scripts/automerge-nudge.sh --select [--base main]   # pure: JSON on stdin
#
# ── The deadlock ─────────────────────────────────────────────────────────────
#
# Branch protection on `main` sets `required_status_checks.strict` — head
# branches must be up to date with base. Auto-merge does not update branches on
# its own. So a PR with every check green and auto-merge armed sits forever at
# `mergeStateStatus=BEHIND` as soon as `main` moves under it. On 2026-08-05 six
# of them (#1502/#1504/#1505/#1514/#1515/#1523) piled up that way and only
# drained because a human loop kept running `gh pr update-branch` by hand.
#
# The strict rule is NOT the bug and must stay: the same evening produced the
# counterexample that justifies it, when two individually-green PRs merged into
# a semantically incompatible stella-pipeline (#1482/#1462) and broke main.
#
# ── One at a time ────────────────────────────────────────────────────────────
#
# Each update re-runs the ~12-minute fmt+clippy+test job, so nudging every
# BEHIND PR at once costs O(N²) CI runs for N stacked PRs: each update makes
# main move again, which puts all the others behind again. This script updates
# exactly one — the oldest — per invocation, so the queue drains in N updates
# instead of N².
#
# ── Why this needs a PAT ─────────────────────────────────────────────────────
#
# GitHub suppresses workflow runs for events raised by GITHUB_TOKEN (everything
# except workflow_dispatch and repository_dispatch). Updating a branch creates a
# new head commit and therefore a `pull_request: synchronize` — so under
# GITHUB_TOKEN the required checks would never run on the new head, and the PR
# would go from BEHIND-and-green to pending-forever. That is a worse deadlock
# than the one being fixed, which is why a missing token is a loud no-op here
# rather than a fallback.
set -euo pipefail

base="main"
dry_run=0
select_only=0

while [ $# -gt 0 ]; do
  case "$1" in
    --base) base="${2:?--base needs a branch}"; shift 2 ;;
    --dry-run) dry_run=1; shift ;;
    --select) select_only=1; shift ;;
    -h|--help) sed -n '2,8p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# ── The decision, as a pure function ─────────────────────────────────────────
#
# Reads the `gh pr list --json` array on stdin and prints the number of the one
# PR to nudge, or nothing. Kept free of I/O so scripts/test-automerge-nudge.sh
# can drive every rule from a fixture instead of from a live queue — the same
# reason stella-core's decision logic takes owned data rather than reading the
# world (invariant 2).
#
# The rules, and why each one is here:
#
#   autoMergeRequest != null   only PRs whose author has already said "merge
#                              this when it is ready". Nudging anything else
#                              would be updating branches nobody asked for.
#   isDraft == false           a draft cannot merge, so updating it just burns
#                              a CI run.
#   mergeStateStatus BEHIND    the deadlock state. BLOCKED means a check is
#                              failing or a review is missing — a human
#                              problem, and re-running CI does not fix it.
#   baseRefName == base        a stacked PR targeting another branch is not
#                              subject to main's protection rule.
#   oldest first               the one that has waited longest, tie-broken by
#                              the lower number so the choice is deterministic
#                              and two runs cannot disagree.
select_pr() {
  jq -r --arg base "$base" '
    [ .[]
      | select(.autoMergeRequest != null)
      | select(.isDraft == false)
      | select(.mergeStateStatus == "BEHIND")
      | select(.baseRefName == $base)
    ]
    | sort_by(.createdAt, .number)
    | if length == 0 then empty else .[0].number end
  '
}

if [ "$select_only" -eq 1 ]; then
  select_pr
  exit 0
fi

command -v gh >/dev/null 2>&1 || { echo "::error::gh is not installed" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "::error::jq is not installed" >&2; exit 1; }

if [ -z "${GH_TOKEN:-}" ]; then
  echo "::warning::No GH_TOKEN — skipping. This job needs a PAT (see the header): GITHUB_TOKEN cannot raise the pull_request event that re-runs the required checks, so nudging with it would replace a BEHIND deadlock with a pending-forever one."
  exit 0
fi

queue="$(gh pr list --state open --base "$base" --limit 100 \
  --json number,title,createdAt,isDraft,baseRefName,mergeStateStatus,autoMergeRequest)"

behind_count="$(printf '%s' "$queue" | jq '[ .[] | select(.autoMergeRequest != null) | select(.isDraft == false) | select(.mergeStateStatus == "BEHIND") ] | length')"
target="$(printf '%s' "$queue" | select_pr)"

if [ -z "$target" ]; then
  echo "no armed auto-merge PR is behind ${base} — nothing to do."
  exit 0
fi

echo "${behind_count} armed auto-merge PR(s) behind ${base}; updating the oldest, #${target}."
printf '%s' "$queue" | jq -r --argjson n "$target" '.[] | select(.number == $n) | "  #\(.number)  \(.createdAt)  \(.title)"'

if [ "$dry_run" -eq 1 ]; then
  echo "--dry-run: not calling update-branch."
  exit 0
fi

# `|| true` on purpose: losing this race is the normal outcome, not an error.
# The PR may have merged, been closed, or been updated by someone else between
# the list above and this call, and a red job for "it already got what it
# needed" would train everyone to ignore this workflow.
if gh pr update-branch "$target"; then
  echo "updated #${target}; its checks will re-run against the new head."
else
  echo "::notice::could not update #${target} — it likely merged, closed, or was updated already. The next run will pick up whatever is still behind."
fi
