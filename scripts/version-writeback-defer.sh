#!/usr/bin/env bash
#
# One shared issue for the version write-back (`#5673`).
#
# `auto-tag.yml`'s merge step ends in `exit 0` on four failure paths:
#
#   2. a required check went red, or never reported
#   3. the required checks did not finish inside 45 minutes
#   4. merging would break `main`'s `Cargo.lock` (`#3336`)
#   5. the merge did not land, so auto-merge is armed instead
#
# `exit 0` is right. A problem that fixes itself should not red the
# release job. But before this script, only case 4 opened an issue. The
# other three left no trace. That let `main` sit one version behind its
# own tag for two weeks, with every check reading green (`#5589`).
#
# This script gives all four cases one shared issue. It follows
# `main-canary.sh`'s own pattern: find the open issue by its label, and
# comment on it instead of filing a new one.
#
# It closes an issue only when a caller says the merge landed. It never
# closes one just because a case's own check cleared. Clearing case 4's
# lockfile check is not the same fact as a merge landing. A run can clear
# that check and still end up armed at case 5, with nothing merged yet.
#
# Usage:
#   scripts/version-writeback-defer.sh open --case <2|3|4|5> \
#     --branch <BRANCH> --version <VERSION> --sha <SHA>
#   scripts/version-writeback-defer.sh close --branch <BRANCH> --sha <SHA>
#
# `--branch` and `--sha` are used by both modes. `open` also needs
# `--case` (which failure fired) and `--version` (the tag this run wants
# to write back). `close` needs neither. Call it only after a merge lands.
#
# `--fixture-open-issue <N>` is for tests. It stands in for the `gh issue
# list` lookup below, so a test can run with no network at all.
#
# bash 3.2 compatible.
set -euo pipefail

label="version-writeback-deferred"

mode="${1:-}"
case "$mode" in
open | close)
  shift
  ;;
-h | --help)
  awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0"
  exit 0
  ;;
*)
  echo "version-writeback-defer: first argument must be 'open' or 'close'" >&2
  exit 2
  ;;
esac

case_num=""
branch=""
version=""
sha=""
fixture_open_issue=""

while [ $# -gt 0 ]; do
  case "$1" in
  --case)
    [ $# -ge 2 ] || {
      echo "version-writeback-defer: --case needs a value" >&2
      exit 2
    }
    case_num="$2"
    shift 2
    ;;
  --branch)
    [ $# -ge 2 ] || {
      echo "version-writeback-defer: --branch needs a value" >&2
      exit 2
    }
    branch="$2"
    shift 2
    ;;
  --version)
    [ $# -ge 2 ] || {
      echo "version-writeback-defer: --version needs a value" >&2
      exit 2
    }
    version="$2"
    shift 2
    ;;
  --sha)
    [ $# -ge 2 ] || {
      echo "version-writeback-defer: --sha needs a value" >&2
      exit 2
    }
    sha="$2"
    shift 2
    ;;
  # Test-only. Stands in for the `gh issue list` lookup below, so a test
  # can hit the "already open" path with no network. `main-canary.sh` has
  # the same flag.
  --fixture-open-issue)
    [ $# -ge 2 ] || {
      echo "version-writeback-defer: --fixture-open-issue needs a number" >&2
      exit 2
    }
    fixture_open_issue="$2"
    shift 2
    ;;
  -h | --help)
    awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0"
    exit 0
    ;;
  *)
    echo "version-writeback-defer: unknown argument '$1'" >&2
    exit 2
    ;;
  esac
done

[ -n "$branch" ] || {
  echo "version-writeback-defer: --branch is required" >&2
  exit 2
}
[ -n "$sha" ] || {
  echo "version-writeback-defer: --sha is required" >&2
  exit 2
}

if [ "$mode" = "open" ]; then
  case "$case_num" in
  2 | 3 | 4 | 5) ;;
  *)
    echo "version-writeback-defer: --case must be 2, 3, 4 or 5, not '${case_num}'" >&2
    exit 2
    ;;
  esac
  [ -n "$version" ] || {
    echo "version-writeback-defer: open needs --version" >&2
    exit 2
  }
else
  # `close` fires only when a caller says the merge landed. It takes no
  # case number and no version. Those describe why a merge was refused,
  # not whether one landed. Refusing them keeps the two calls separate: a
  # caller cannot pass "case 4 cleared" and mean "the merge landed".
  [ -z "$case_num" ] || {
    echo "version-writeback-defer: close takes no --case; it means the merge is confirmed, not that a case cleared" >&2
    exit 2
  }
  [ -z "$version" ] || {
    echo "version-writeback-defer: close takes no --version" >&2
    exit 2
  }
fi

# One plain sentence per case. The wording matches auto-tag.yml's own
# warnings. Keeping this table here, instead of passing free text from the
# workflow, keeps each call site to one line.
case_reason() {
  # shellcheck disable=SC2016
  # The backticks are literal markdown, not command substitution. `%s` is
  # printf's placeholder; it expands from the argument, not from quoting.
  case "$1" in
  2) printf 'a required check on `%s` went red or never reported' "$branch" ;;
  3) printf 'the required checks on `%s` did not finish within 45 minutes' "$branch" ;;
  4) printf "merging \`%s\` would leave main's \`Cargo.lock\` unresolvable (\`#3336\`)" "$branch" ;;
  5) printf 'the merge of `%s` did not go through in this run; auto-merge is armed instead' "$branch" ;;
  esac
}

# Best-effort, like `main-canary.sh`'s own `gh_run`. A `gh` outage here
# says nothing about the write-back itself. Each caller decides which
# calls must fail the script, by leaving off `|| true`, and which are
# just narration.
gh_run() {
  if ! gh "$@"; then
    echo "version-writeback-defer: WARN — \"gh $*\" failed" >&2
    return 1
  fi
}

open_issue=""
if [ -n "$fixture_open_issue" ]; then
  open_issue="$fixture_open_issue"
else
  open_issue="$(gh issue list --label "$label" --state open --limit 1 \
    --json number --jq '.[0].number // empty' 2>/dev/null || true)"
fi

if [ "$mode" = "close" ]; then
  if [ -z "$open_issue" ]; then
    echo "version-writeback-defer: close — no open ${label} issue; nothing to do."
    exit 0
  fi
  gh_run issue comment "$open_issue" \
    --body "The \`${branch}\` version write-back merged at \`${sha}\` — closing. Reopen if you disagree." || true
  gh_run issue close "$open_issue" --reason completed
  echo "version-writeback-defer: closed #${open_issue} — ${branch} merged at ${sha}."
  exit 0
fi

# mode = open
reason="$(case_reason "$case_num")"

if [ -n "$open_issue" ]; then
  gh_run issue comment "$open_issue" \
    --body "Still deferred at \`${sha}\`, branch \`${branch}\`, writing back \`${version}\` — case ${case_num}: ${reason}." || true
  echo "version-writeback-defer: case ${case_num} — commented on #${open_issue}."
  exit 0
fi

# Built as an array of whole lines. Each line of the issue body is then
# its own line here too.
defer_lines=(
  "Auto-tag refused to merge the \`${branch}\` version-sync PR (writing back \`${version}\`) at \`${sha}\`: ${reason}."
  ""
  "This is usually self-healing — whatever merge moves \`main\` under this branch starts another \`auto-tag\` run, which force-updates \`${branch}\` from the then-current tip and retries. That recovery only fires while commits keep landing on \`main\`, though: if this happens on the last merge before a quiet period, the write-back never lands and nothing else would have said so (\`#3842\`)."
  ""
  "**Case ${case_num}:** ${reason}."
  ""
  "**If this is still open and no \`auto-tag\` run has retried recently**, run it by hand:"
  ""
  '```sh'
  "gh workflow run auto-tag.yml"
  '```'
  ""
  "Closes automatically the run the write-back actually merges — not merely the run where the blocking condition clears."
)
defer_body="$(printf '%s\n' "${defer_lines[@]}")"

# `--force` makes label creation safe to repeat. `main-canary.sh` does
# the same before its first issue.
gh_run label create "$label" \
  --color FBCA04 \
  --description "the version write-back to main was refused and has not since landed (\`#3842\`)" \
  --force || true
gh_run issue create \
  --title "version write-back to main is deferred (${branch})" \
  --label "$label" \
  --body "$defer_body"
echo "version-writeback-defer: case ${case_num} — opened a new ${label} issue."
