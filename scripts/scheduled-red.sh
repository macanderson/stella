#!/usr/bin/env bash
#
# Keep one issue open while a scheduled workflow is red.
#
#   scripts/scheduled-red.sh --workflow <name> --conclusion <conclusion> \
#     [--run-url <url>] [--label <label>]
#
# Each flag can come from the environment instead: `SCHEDULED_RED_WORKFLOW`,
# `SCHEDULED_RED_CONCLUSION` and `SCHEDULED_RED_RUN_URL`. A flag wins over its
# variable. `.github/workflows/scheduled-red.yml` calls it that way, once per
# scheduled run of every workflow that has a schedule.
#
# A scheduled run has no pull request and no author. When it fails, the only
# trace is a red row in the Actions tab. Nobody reads that tab. The triage
# sweep failed on every run, and no issue said so.
#
# So a red run gets an issue. There is one issue per workflow. It carries the
# `scheduled-red` label, and its title names the workflow. A second red run
# adds a comment to it. The next green run closes it. Any other conclusion,
# such as `cancelled` or `skipped`, says nothing about the code. It changes
# no issue.
#
# This script only reports. So it fails open: every unknown exits 0 with a
# loud `::warning::` line. No `gh`, a tracker error, and two matching issues
# at once are the unknowns. Two issues means a person must pick one, so the
# script writes to neither. It needs no `jq`, since `gh --jq` has its own.
#
# `main-canary.sh` and `codeql-canary.sh` keep one issue by label the same
# way. Exit 2 is for a caller mistake, such as a missing flag.
set -uo pipefail

# The verdict here is the tracker write, never the output. A reader that
# closes the pipe must not kill the script before that write.
trap '' PIPE

workflow="${SCHEDULED_RED_WORKFLOW:-}"
conclusion="${SCHEDULED_RED_CONCLUSION:-}"
run_url="${SCHEDULED_RED_RUN_URL:-<no run URL given>}"
label="scheduled-red"

need_value() {
  if [ "$2" -lt 2 ]; then
    echo "scheduled-red: $1 needs a value" >&2
    exit 2
  fi
}

while [ $# -gt 0 ]; do
  case "$1" in
  --workflow)
    need_value "$1" $#
    workflow="$2"
    shift 2
    ;;
  --conclusion)
    need_value "$1" $#
    conclusion="$2"
    shift 2
    ;;
  --run-url)
    need_value "$1" $#
    run_url="$2"
    shift 2
    ;;
  --label)
    need_value "$1" $#
    label="$2"
    shift 2
    ;;
  *)
    echo "scheduled-red: unknown argument: $1" >&2
    exit 2
    ;;
  esac
done

if [ -z "$workflow" ]; then
  echo "scheduled-red: --workflow (or SCHEDULED_RED_WORKFLOW) is required" >&2
  exit 2
fi
if [ -z "$conclusion" ]; then
  echo "scheduled-red: --conclusion (or SCHEDULED_RED_CONCLUSION) is required" >&2
  exit 2
fi

say() {
  printf 'scheduled-red: %s\n' "$*" || true
}

# On stdout, where the runner reads workflow commands.
warn() {
  printf '::warning::scheduled-red: %s\n' "$*" || true
}

# A run that timed out is red. It did not finish its work.
case "$conclusion" in
success) want="close" ;;
failure | timed_out) want="open" ;;
*)
  say "'$workflow' concluded '$conclusion'. That is neither red nor green, so no issue changes."
  exit 0
  ;;
esac

if ! command -v gh >/dev/null 2>&1; then
  warn "UNAVAILABLE — gh is not on PATH, so the tracker was not asked. No issue was opened, updated or closed for '$workflow' ($conclusion)."
  exit 0
fi

title="Scheduled workflow failing: $workflow"

# One `<number><TAB><title>` line per open issue with the label. The title is
# compared here, in the shell. Putting the workflow name into a jq program
# would need quoting, and a name with a quote in it would break the query.
if ! listing="$(gh issue list --label "$label" --state open --limit 100 \
  --json number,title --jq '.[] | "\(.number)\t\(.title)"')"; then
  warn "the tracker could not be read (gh issue list failed). No issue was opened, updated or closed for '$workflow' ($conclusion)."
  exit 0
fi

matches=""
open_issue=""
count=0
while IFS=$'\t' read -r number issue_title; do
  [ -n "$number" ] || continue
  if [ "$issue_title" = "$title" ]; then
    matches="${matches:+$matches }#$number"
    open_issue="$number"
    count=$((count + 1))
  fi
done <<EOF
$listing
EOF

if [ "$count" -gt 1 ]; then
  warn "$count open '$label' issues are titled '$title' ($matches). Which one to keep is a question for a person, so none was written to. Close all but one."
  exit 0
fi

gh_write() {
  if ! gh "$@"; then
    warn "\"gh $1 $2\" failed, so the tracker may not match this run of '$workflow' ($conclusion)."
    return 1
  fi
}

if [ "$want" = "close" ]; then
  if [ "$count" -eq 0 ]; then
    say "'$workflow' is green, and no issue is open for it."
    exit 0
  fi
  body="\`$workflow\` passed its scheduled run again: $run_url

Closing this issue. Reopen it if the cause is not fixed.

**Definition of done**

- [x] The cause of the failing scheduled run is fixed on \`main\`.

<!-- scheduled-red -->"
  if gh_write issue comment "$open_issue" --body "$body" &&
    gh_write issue close "$open_issue" --reason completed; then
    say "'$workflow' is green again. Closed #$open_issue."
  fi
  exit 0
fi

if [ "$count" -eq 1 ]; then
  if gh_write issue comment "$open_issue" \
    --body "Still red. The scheduled run concluded \`$conclusion\`: $run_url"; then
    say "'$workflow' is still red. Commented on #$open_issue."
  fi
  exit 0
fi

body="The scheduled run of \`$workflow\` concluded \`$conclusion\`.

Run: $run_url

A scheduled run has no pull request and no author, so a red one is easy to
miss. This issue stays open while the workflow stays red. Each red scheduled
run adds a comment. The next green scheduled run closes it.

**How to look**

\`\`\`sh
gh run list --workflow \"$workflow\" --event schedule --limit 5
gh run view <run id> --log-failed
\`\`\`

**Definition of done**

- [ ] The cause of the failing scheduled run is fixed on \`main\`.

The next green scheduled run closes this issue, so the box can be ticked by
the pull request that fixes the cause.

<!-- scheduled-red -->"

# `gh issue create --label` fails when the label does not exist yet.
# `--force` makes the create safe to repeat.
gh_write label create "$label" \
  --color B60205 \
  --description "A scheduled workflow is failing. Filed by scheduled-red.yml." \
  --force || true
if gh_write issue create --title "$title" --label "$label" --body "$body"; then
  say "'$workflow' is red. Opened an issue for it."
fi
exit 0
