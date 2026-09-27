#!/usr/bin/env bash
#
# Find a branch that is still alive after its pull request merged, and has
# moved past the commit that merged.
#
#   ./scripts/check-merged-branch-drift.sh [--grace-secs N]
#   ./scripts/check-merged-branch-drift.sh --select --now <unix>   # pure: JSON on stdin
# --help text ends here.
#
# A branch kept getting new commits after its pull request merged. Those
# commits never joined a pull request. They never reached main. They lived
# only in one clone and in one build image. The image kept working. Main did
# not. Nobody saw the gap for two days.
#
# GitHub most often deletes a branch when its pull request merges. That is
# the safe case. This script looks only at branches still alive on origin.
# For each one: does its tip match the commit that actually merged? If not,
# someone pushed new work to a branch whose job was already done. Those
# commits live nowhere else. If the branch is deleted next, they are gone
# for good.
#
# One branch name can merge more than once. So the rule below compares a
# branch only to its own newest merge, never to an older one. That is what
# keeps normal reuse quiet.
#
# The release bot's own branch is skipped. It is `bot/version-sync`, set as
# `SYNC_BRANCH` in `auto-tag.yml`. It stays on origin after each merge. The
# bot force-pushes a new bump to it, then opens the pull request. A check
# that runs between those two steps would see a moved tip and no open pull
# request. Nothing a person wrote lives on that branch, so there is nothing
# to lose.
#
# A branch can also come back for new work on purpose: the same name merges,
# then someone opens a fresh pull request from it. That is fine too. A
# branch that is the head of a pull request open right now is skipped.
#
# A pull request from a fork is skipped too. Its branch lives in the fork,
# not on origin. A fork's `main` would match origin's `main` by name alone.
#
# The grace window covers GitHub's own lag. Branch deletion can trail a
# merge by a few seconds. A merge inside that window is skipped, not
# reported.
#
# There is no lookback window. The check starts from the branches on
# origin and asks each one for its own merges. A merge of any age counts.
# Commits on a live branch stay as long as the branch does. So an old merge
# can strand them just as a new one can.

set -euo pipefail

# shellcheck source=scripts/lib/help-header.sh
. "$(dirname "$0")/lib/help-header.sh"

grace_secs=300
select_only=0
now=""

while [ $# -gt 0 ]; do
  case "$1" in
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
#                     "number": 1, "fork": false }, ... ],
#     "liveTips": { "fix/foo": "<current sha on origin>", ... },
#     "openHeads": [ "fix/bar", ... ] }
#
# `fork` may be left out. It counts as false.
#
# Prints one line per branch whose newest merge does not match what is live
# on origin: `<branch>\t<number>\t<mergedAt>\t<mergedSha>\t<tipSha>`. Oldest
# merge first. This mode has no I/O so a fixture can drive it, in
# scripts/test-check-merged-branch-drift.sh, with no live repository.
#
# Fork merges drop out before the newest merge is picked. Were they dropped
# after, a newer fork merge of the same name would hide the origin one.
bot_branches='["bot/version-sync"]'

select_drift() {
  jq -r --argjson now "$now" --argjson grace "$grace_secs" \
    --argjson bots "$bot_branches" '
    ( [ (.openHeads // [])[] | { (.): true } ] | add // {} ) as $open
    | ( [ $bots[] | { (.): true } ] | add // {} ) as $bot
    | (.liveTips // {}) as $live
    | ( .merged
        | map(select(.fork | not))
        | group_by(.branch)
        | map(max_by(.mergedAt))
      ) as $latest
    | [ $latest[]
        | select($bot[.branch] | not)
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

# One call reads every live branch tip at once.
live_tips_json="$(git ls-remote --heads origin \
  | awk '{ sub("refs/heads/", "", $2); printf "%s\t%s\n", $2, $1 }' \
  | jq -R -s '
      split("\n") | map(select(length > 0) | split("\t"))
      | map({ (.[0]): .[1] }) | add // {}
    ')"

# Then one call per live branch reads its merged pull requests. A few dozen
# branches live on origin at a time, so this stays cheap. A search for all
# recent merges would need a date bound. A merge older than the bound would
# drop out. Its branch could still hold the stray commits.
#
# `gh` lists the newest pull request first. A branch with more than 100
# merges loses its oldest ones, not its newest. The rule reads only the
# newest.
#
# The release bot's branch is left out here. The rule skips it anyway. It
# merges hundreds of times.
branches="$(printf '%s' "$live_tips_json" | jq -r --argjson bots "$bot_branches" 'keys - $bots | .[]')"
merged_json='[]'
branch_count=0
while IFS= read -r branch; do
  [ -n "$branch" ] || continue
  branch_count=$((branch_count + 1))
  one="$(CLICOLOR_FORCE=0 NO_COLOR=1 gh pr list --state merged --head "$branch" \
    --limit 100 \
    --json number,headRefName,headRefOid,mergedAt,isCrossRepository \
    --jq '[ .[] | { branch: .headRefName, sha: .headRefOid, mergedAt: .mergedAt, number: .number, fork: .isCrossRepository } ]')"
  merged_json="$(jq -n --argjson a "$merged_json" --argjson b "$one" '$a + $b')"
done <<EOF
$branches
EOF

# The open list is capped at 1000. If it were ever cut off, a branch with
# an open pull request could be reported by mistake. A cut-off open list
# can add a report. It cannot hide one.
#
# An open pull request from a fork is left out. Its branch is not on
# origin, so it says nothing about the origin branch of the same name.
open_heads_json="$(CLICOLOR_FORCE=0 NO_COLOR=1 gh pr list --state open --limit 1000 \
  --json headRefName,isCrossRepository \
  --jq '[ .[] | select(.isCrossRepository | not) | .headRefName ]')"

doc="$(jq -n --argjson merged "$merged_json" --argjson open "$open_heads_json" \
  --argjson live "$live_tips_json" \
  '{ merged: $merged, openHeads: $open, liveTips: $live }')"

report="$(printf '%s' "$doc" | select_drift)"

total_merged="$(printf '%s' "$merged_json" | jq 'length')"
if [ -z "$report" ]; then
  echo "check-merged-branch-drift: OK. No live branch has moved past its own merge (${branch_count} live branch(es) checked, ${total_merged} merge(s) read)."
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
echo "Save the extra commits before the branch is deleted. Fetch the branch and the head that merged:"
echo "  git fetch origin <branch> pull/<PR>/head"
echo "Then list the commits the merge did not carry:"
echo "  if git merge-base --is-ancestor <merged sha> origin/<branch>; then"
echo "    git log --oneline <merged sha>..origin/<branch>"
echo "  else"
echo "    git log --oneline origin/main..origin/<branch>"
echo "  fi"
echo "The first range fits a branch that grew on top of its merge. The second fits a branch rebased onto main or made again from main."
echo "Open a fresh pull request from main with those commits. Then delete the old branch."
exit 1
