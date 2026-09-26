#!/usr/bin/env bash
#
# Find a branch that is still alive after its pull request merged, and has
# moved past the commit that merged.
#
#   ./scripts/check-merged-branch-drift.sh [--days N] [--grace-secs N]
#   ./scripts/check-merged-branch-drift.sh --select --now <unix>   # pure: JSON on stdin
# --help text ends here.
#
# A branch kept getting new commits after its pull request merged. Those
# commits never joined a pull request. They never reached main. They lived
# only in one clone and in one build image. The image kept working. Main did
# not. Nobody saw the gap for two days.
#
# GitHub deletes a branch the moment its pull request merges. That is the
# safe case. This script looks only at branches still alive on origin. For
# each one: does its tip match the commit that actually merged? If not,
# someone pushed new work to a branch whose job was already done. Those
# commits live nowhere else. If the branch is deleted next, they are gone
# for good.
#
# One branch name can merge more than once. The release bot reuses one
# branch for every version bump, and each merge moves that branch forward on
# purpose. So the rule below compares a branch only to its own newest merge,
# never to an older one. That is what keeps normal reuse quiet.
#
# A branch can also come back for new work on purpose: the same name merges,
# then someone opens a fresh pull request from it. That is fine too. A
# branch that is the head of a pull request open right now is skipped.
#
# The grace window covers GitHub's own lag. Branch deletion can trail a
# merge by a few seconds. A merge inside that window is skipped, not
# reported.
#
# The default lookback is 14 days. Git prunes old objects after about two
# weeks by default. This check has to run well inside that window, or the
# commits it looks for may already be gone.

set -euo pipefail

# shellcheck source=scripts/lib/help-header.sh
. "$(dirname "$0")/lib/help-header.sh"

days=14
grace_secs=300
select_only=0
now=""

while [ $# -gt 0 ]; do
  case "$1" in
    --days) days="${2:?--days needs a number}"; shift 2 ;;
    --grace-secs) grace_secs="${2:?--grace-secs needs a number}"; shift 2 ;;
    --now) now="${2:?--now needs a unix timestamp}"; shift 2 ;;
    --select) select_only=1; shift ;;
    -h|--help) print_help_header "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# The rule, with the I/O taken out. Reads one JSON document on stdin:
#
#   { "merged":   [ { "branch": "fix/foo", "sha": "...", "mergedAt": "...",
#                     "number": 2180 }, ... ],
#     "liveTips": { "fix/foo": "<current sha on origin>", ... },
#     "openHeads": [ "fix/bar", ... ] }
#
# Prints one line per branch whose newest merge does not match what is live
# on origin: `<branch>\t<number>\t<mergedAt>\t<mergedSha>\t<tipSha>`. Oldest
# merge first. This mode has no I/O so a fixture can drive it, in
# scripts/test-check-merged-branch-drift.sh, with no live repository.
select_drift() {
  jq -r --argjson now "$now" --argjson grace "$grace_secs" '
    ( [ (.openHeads // [])[] | { (.): true } ] | add // {} ) as $open
    | (.liveTips // {}) as $live
    | ( .merged
        | group_by(.branch)
        | map(max_by(.mergedAt))
      ) as $latest
    | [ $latest[]
        | select($open[.branch] | not)
        | select($live[.branch] != null)
        | select($live[.branch] != .sha)
        | select(($now - (.mergedAt | fromdateiso8601)) > $grace)
        | . + { tip: $live[.branch] }
      ]
    | sort_by(.mergedAt)
    | .[]
    | "\(.branch)\t\(.number)\t\(.mergedAt)\t\(.sha)\t\(.tip)"
  '
}

if [ "$select_only" -eq 1 ]; then
  [ -n "$now" ] || { echo "--select needs --now" >&2; exit 2; }
  select_drift
  exit 0
fi

command -v gh >/dev/null 2>&1 || { echo "::error::gh is not installed" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "::error::jq is not installed" >&2; exit 1; }
[ -n "$now" ] || now="$(date -u +%s)"

since="$(date -u -d "@$((now - days * 86400))" +%Y-%m-%d 2>/dev/null || date -u -r "$((now - days * 86400))" +%Y-%m-%d)"

# The date bound (`merged:>=X`) is what keeps this cheap. This repository
# merges hundreds of pull requests in 30 days. A limit of 500 once cut off a
# real count of 630 and no one noticed. A cap that hides a cut-off list is
# worse than no cap, so a result that lands right on the ceiling is refused,
# not trusted.
#
# `--search` goes through GitHub search, and search returns at most 1000
# results whatever `--limit` asks for. So the ceiling is 1000. A higher
# limit would never be reached, and a cut-off list would pass as whole.
gh_limit=1000
merged_json="$(CLICOLOR_FORCE=0 NO_COLOR=1 gh pr list --state merged \
  --search "merged:>=${since}" --limit "$gh_limit" \
  --json number,headRefName,headRefOid,mergedAt \
  --jq '[ .[] | { branch: .headRefName, sha: .headRefOid, mergedAt: .mergedAt, number: .number } ]')"
merged_count_raw="$(printf '%s' "$merged_json" | jq 'length')"
if [ "$merged_count_raw" -eq "$gh_limit" ]; then
  echo "::error::the merged pull request list hit GitHub search's ${gh_limit}-result ceiling for --days ${days}. The list was likely cut off. Narrow --days." >&2
  exit 1
fi

# The open list shares the same limit. If it were ever cut off, a branch
# with an open pull request could be reported by mistake. A cut-off open
# list can add a report. It cannot hide one.
open_heads_json="$(CLICOLOR_FORCE=0 NO_COLOR=1 gh pr list --state open --limit "$gh_limit" \
  --json headRefName --jq '[ .[].headRefName ]')"

# One call reads every live branch tip at once. A separate gh call per
# merged pull request would be slow: this repository holds dozens of
# branches and merges all the time.
live_tips_json="$(git ls-remote --heads origin \
  | awk '{ sub("refs/heads/", "", $2); printf "%s\t%s\n", $2, $1 }' \
  | jq -R -s '
      split("\n") | map(select(length > 0) | split("\t"))
      | map({ (.[0]): .[1] }) | add // {}
    ')"

doc="$(jq -n --argjson merged "$merged_json" --argjson open "$open_heads_json" \
  --argjson live "$live_tips_json" \
  '{ merged: $merged, openHeads: $open, liveTips: $live }')"

report="$(printf '%s' "$doc" | select_drift)"

total_merged="$(printf '%s' "$merged_json" | jq 'length')"
if [ -z "$report" ]; then
  echo "check-merged-branch-drift: OK — no live branch has moved past its own merge (${total_merged} merge(s) checked since ${since})."
  exit 0
fi

count="$(printf '%s\n' "$report" | wc -l | tr -d ' ')"
echo "::error::${count} branch(es) are still on origin and have moved past the pull request that merged them. New commits on a dead branch join no pull request. They are lost the moment the branch is deleted."
echo ""
echo "Branch                                   PR      Merged                 Merged sha    Live tip"
printf '%s\n' "$report" | while IFS="$(printf '\t')" read -r branch number merged_at sha tip; do
  printf '  %-40s #%-6s %-22s %-12s %s\n' "$branch" "$number" "$merged_at" "${sha:0:12}" "${tip:0:12}"
done
echo ""
echo "Save the extra commits before the branch is pruned:"
echo "  git fetch origin <branch>"
echo "  git log --oneline <merged sha>..origin/<branch>"
echo "Open a fresh pull request from main with those commits. Then delete the old branch."
exit 1
