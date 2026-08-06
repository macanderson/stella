#!/usr/bin/env bash
#
# Guard: two open pull requests claiming the same issue are reported rather
# than both merged. See #1722, filed from the incident.
#
# Issue #1616 was implemented THREE times by three parallel sessions and every
# one of them merged, producing two red-main incidents in under an hour and a
# witness test (`a_panic_drains_the_console_pump`) that was deleted twice.
#
# Note what CI did not catch, because it explains why this guard is not
# redundant with anything: **none of those merges was a git conflict**. Every
# one was textually clean and semantically broken, and each branch's own checks
# were green on its own base. A merge product can be red while every input to
# it is green, and no per-branch check can see that.
#
# ## Why the claim, and not the diff
#
# The signal this reads is the PR body's `Closes #N` trailer — the thing the
# authors already write, that AGENTS.md already requires, and that GitHub
# already parses. Two PRs claiming to close one issue is a near-perfect
# predictor of two implementations of one feature, and it is knowable the
# moment the second PR opens rather than after both have merged.
#
# Comparing diffs instead would be the obvious alternative and is much worse:
# two implementations of one feature routinely touch different files under
# different names (#1616's two designs disagreed on `park_deadline` vs
# `with_deadline`), so a textual comparison sees nothing while the claim is
# identical.
#
# ## What this is NOT
#
# - Not #1645. That covers a red *branch* landing because `enforce_admins` is
#   off. Here every branch was green; the merge product was not.
# - Not scripts/check-empty-diff.sh. That fires after the duplicate has already
#   merged as a no-op — the backstop. This fires while both are still open,
#   which is the only point at which a human can choose which one to keep.
# - Not a blocker by default. Two PRs may legitimately share a `Refs #N`, and
#   even a duplicated `Closes` is sometimes a deliberate split. Only `Closes`,
#   `Fixes` and `Resolves` count (the keywords that actually close an issue),
#   and the exit code is advisory unless --strict is passed.
#
# Usage:
#   scripts/check-duplicate-claims.sh [--strict] [--fixture FILE]
#
# Reads open PRs from `gh` by default. --fixture takes the same JSON shape
# (`[{"number":N,"title":"…","body":"…"}, …]`) from a file instead, which is
# what makes this testable without a network or a token.
#
# Uses portable POSIX tools plus jq so it runs on a bare CI runner.
set -euo pipefail

strict=0
fixture=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --strict) strict=1; shift ;;
    --fixture) fixture="${2:-}"; shift 2 ;;
    -h|--help)
      echo "usage: scripts/check-duplicate-claims.sh [--strict] [--fixture FILE]"
      exit 0
      ;;
    *)
      echo "check-duplicate-claims: unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if ! command -v jq >/dev/null 2>&1; then
  echo "check-duplicate-claims: SKIPPED — jq is not installed."
  exit 0
fi

if [ -n "$fixture" ]; then
  if [ ! -f "$fixture" ]; then
    echo "check-duplicate-claims: no such fixture: $fixture" >&2
    exit 2
  fi
  prs="$(cat "$fixture")"
else
  if ! command -v gh >/dev/null 2>&1; then
    echo "check-duplicate-claims: SKIPPED — gh is not installed."
    exit 0
  fi
  # A failure here is not a finding: no token, rate limit, or an outage must
  # not be reported as "no duplicates". Skip loudly instead.
  #
  # NO_COLOR/CLICOLOR_FORCE are set explicitly rather than trusted: `gh` styles
  # even `--json` output when CLICOLOR_FORCE is exported (a common developer
  # setting), and the ANSI escapes make the result invalid JSON. Caught while
  # writing this guard — it reported "no duplicates" against a repository whose
  # open PRs all carried `Closes #N`, because the parse failed and the failure
  # was being read as an empty result.
  if ! prs="$(NO_COLOR=1 CLICOLOR_FORCE=0 GH_FORCE_TTY='' gh pr list \
      --state open --limit 200 --json number,title,body 2>/dev/null)"; then
    echo "check-duplicate-claims: SKIPPED — could not list open pull requests."
    exit 0
  fi
fi

# Parse failure is an ERROR, never an empty result. The distinction is the
# whole difference between "there are no duplicates" and "I could not tell",
# and collapsing the two is how a guard reports green forever.
if ! printf '%s' "$prs" | jq -e 'type == "array"' >/dev/null 2>&1; then
  echo "check-duplicate-claims: FAILED — pull-request data is not a JSON array." >&2
  echo "  first bytes: $(printf '%s' "$prs" | head -c 120 | tr -d '\000')" >&2
  exit 2
fi

# Extract (issue, pr) pairs. Only the three closing keywords count; `Refs #N`
# is deliberately ignored because advancing an issue from two directions is
# normal and is not this defect.
#
# `ascii_downcase` on the whole body first, so `CLOSES`, `Closes` and `closes`
# are one keyword rather than three patterns to keep in step.
pairs="$(printf '%s' "$prs" | jq -r '
  .[]
  | . as $pr
  | ($pr.body // "" | ascii_downcase)
  | [ scan("(?:closes|fixes|resolves)\\s+#([0-9]+)") ]
  | flatten
  | unique
  | .[]
  | "\(.)\t\($pr.number)\t\($pr.title // "")"
')"

if [ -z "$pairs" ]; then
  echo "check-duplicate-claims: OK — no open pull request claims to close an issue."
  exit 0
fi

# Group by issue; a group of two or more is a finding.
duplicates=""
issues="$(printf '%s\n' "$pairs" | cut -f1 | sort -un)"
claim_count=0
for issue in $issues; do
  claim_count=$((claim_count + 1))
  matches="$(printf '%s\n' "$pairs" | awk -F'\t' -v i="$issue" '$1 == i')"
  count="$(printf '%s\n' "$matches" | wc -l | tr -d ' ')"
  if [ "$count" -gt 1 ]; then
    duplicates="${duplicates}
issue #${issue} is claimed by ${count} open pull requests:"
    while IFS=$'\t' read -r _ pr title; do
      duplicates="${duplicates}
    #${pr}  ${title}"
    done <<EOF
$matches
EOF
  fi
done

if [ -z "$duplicates" ]; then
  echo "check-duplicate-claims: OK — ${claim_count} issue(s) claimed, none more than once."
  exit 0
fi

cat >&2 <<MSG
check-duplicate-claims: FAILED
${duplicates}

Two open pull requests closing one issue is how #1616 was implemented three
times and merged three times, for two red-main incidents in under an hour.
None of those merges conflicted: each branch was green on its own base, and
only the merge product was broken.

Decide which one to keep BEFORE either lands. The other should be closed, or
re-scoped and its body changed from "Closes" to "Refs" so it no longer claims
the issue.
MSG

if [ "$strict" -eq 1 ]; then
  exit 1
fi
echo "check-duplicate-claims: advisory — not failing the build (pass --strict to enforce)." >&2
exit 0
