#!/usr/bin/env bash
#
# Guard: while `main` is known-broken, a pull request says so before it merges
# onto the breakage. See #3917, filed from the incident.
#
# `main-canary.yml` already answers "does main still compose?" after every
# merge, and files one labelled issue when the answer is no. On 2026-08-19 it
# worked exactly as designed and it did not help:
#
#   16:55:59  #3897 merges — the composition break (it added a theme const
#             naming a palette token #3890 had just deleted, and a test
#             calling a function #3896 had just deleted; neither branch was
#             wrong on its own)
#   16:57:01  the canary files #3904, "main is red: compile fails on the
#             merged tree"
#   16:57:13  #3898 merges — twelve seconds later
#   16:57:50  #3900 merges — and lands a second, unrelated break of its own
#   17:08:54  #3902 merges
#   17:30:56  the first fix merges
#   17:32:07  the canary closes #3904
#
# Four PRs merged onto a tree that did not compile, over 35 minutes, with the
# issue open the whole time. The detector was never the problem: the signal
# had no consumer at the one place a decision was still being made.
#
# That is invariant #10's shape — "every emitted signal names its consumer" —
# pointed at CI instead of at `AgentEvent`. This file is the consumer.
#
# ## Why piling on is the expensive part, not just untidy
#
# Once `main` is red, every PR's own checks are red too, so red stops
# distinguishing "your change is broken" from "the base is". #3900's missing
# import was a single-PR defect that its own CI would have caught on any other
# day; it landed because by then nobody could tell its red apart from the red
# everyone already had. One composition break became four breaks in three
# crates, and each one hid the next behind it — a compile error stops the
# build before the next crate is reached, so they had to be found one rebuild
# at a time.
#
# ## What this is NOT
#
# Not a second `ci.yml`, and not a second canary. It compiles nothing, checks
# out nothing, and asks one question of the issue tracker. The canary's own
# header warns against growing it into a duplicate gate; the same warning
# applies here with more force, because this one runs per pull request.
#
# ## The escape hatch, which is the whole design
#
# A hold that also blocks the repair is a deadlock, so the PR that unblocks
# `main` carries the `unblocks-main` label and passes. The label is deliberate
# rather than inferred: parsing a body for a closing keyword would guess, and
# a human labelling a PR "this is the fix" is a statement someone made on
# purpose and a reviewer can audit afterwards.
#
# ## Fail-open, deliberately, and said out loud
#
# If the tracker cannot be reached, this reports the failure loudly and
# **passes**. It is the second line of defence — the canary's issue is the
# first, and it is the one a human reads — so a GitHub API blip must not halt
# every merge in the repository. The cost of that choice is real and is
# stated rather than hidden: an unreachable tracker during a red `main` gets
# the old behaviour, which is the behaviour this guard exists to improve on.
#
# Usage:
#   scripts/check-main-red-hold.sh [--pr <number>]
#   scripts/check-main-red-hold.sh --fixture-open-issues "3904" \
#                                  --fixture-labels "bug,area:ci"
#
# Uses portable POSIX tools plus `gh` so it runs on a bare CI runner.
set -uo pipefail

label="main-red"
escape_label="unblocks-main"
pr=""
fixture_open_issues=""
fixture_labels=""
use_fixture=0

while [ $# -gt 0 ]; do
  case "$1" in
  --pr)
    [ $# -ge 2 ] || {
      echo "check-main-red-hold: --pr needs a number" >&2
      exit 2
    }
    pr="$2"
    shift 2
    ;;
  # Test-only seams. Both must be supplied together: a test that stubbed the
  # tracker but let the label lookup reach the network would pass or fail for
  # a reason the test did not choose.
  --fixture-open-issues)
    [ $# -ge 2 ] || {
      echo "check-main-red-hold: --fixture-open-issues needs a value" >&2
      exit 2
    }
    fixture_open_issues="$2"
    use_fixture=1
    shift 2
    ;;
  --fixture-labels)
    [ $# -ge 2 ] || {
      echo "check-main-red-hold: --fixture-labels needs a value" >&2
      exit 2
    }
    fixture_labels="$2"
    use_fixture=1
    shift 2
    ;;
  -h | --help)
    # The whole comment block after the shebang, bounded by its first
    # non-comment line — hardcoded line numbers truncate silently the first
    # time the header grows. Same reader as main-canary.sh's --help.
    awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0"
    exit 0
    ;;
  *)
    echo "check-main-red-hold: unknown argument '$1'" >&2
    exit 2
    ;;
  esac
done

# Which issues, if any, say main is currently broken.
if [ "$use_fixture" -eq 1 ]; then
  open_issues="$fixture_open_issues"
else
  if ! command -v gh >/dev/null 2>&1; then
    echo "note: gh is not installed, so this run could not ask whether main is" >&2
    echo "      red. Passing: this guard is the second line of defence and must" >&2
    echo "      not halt every merge when it cannot evaluate. See #3917." >&2
    exit 0
  fi
  if ! open_issues="$(gh issue list --label "$label" --state open \
    --limit 20 --json number --jq '.[].number' 2>/dev/null)"; then
    echo "note: could not reach the issue tracker, so this run could not ask" >&2
    echo "      whether main is red. Passing deliberately (fail-open); the" >&2
    echo "      canary's own issue remains the primary signal. See #3917." >&2
    exit 0
  fi
fi

open_issues="$(printf '%s' "$open_issues" | tr -d ' ' | tr '\n' ' ')"
open_issues="${open_issues% }"

if [ -z "$open_issues" ]; then
  # SIGPIPE ignored and the write discarded, so a reader that closed the pipe
  # cannot turn a green verdict into a failure (the #1815 shape).
  trap '' PIPE
  echo "ok  no open \`$label\` issue — main is not known-broken." || true
  exit 0
fi

# Main is red. The only question left is whether this PR is the repair.
if [ "$use_fixture" -eq 1 ]; then
  labels="$fixture_labels"
elif [ -n "$pr" ]; then
  if ! labels="$(gh pr view "$pr" --json labels --jq '.labels[].name' 2>/dev/null)"; then
    echo "note: main is red (see $open_issues) but this run could not read PR" >&2
    echo "      #$pr's labels, so it cannot tell a repair from a pile-on." >&2
    echo "      Passing deliberately (fail-open). See #3917." >&2
    exit 0
  fi
else
  labels=""
fi

if printf '%s\n' "$labels" | tr ',' '\n' | grep -qx "$escape_label"; then
  trap '' PIPE
  echo "ok  main is red ($open_issues), and this PR is labelled \`$escape_label\`." || true
  exit 0
fi

echo "FAIL main is red, and merging onto it is how one break becomes four." >&2
echo "" >&2
echo "     open \`$label\` issue(s): $open_issues" >&2
echo "" >&2
echo "     \`main-canary\` found the merged tree broken and filed the issue" >&2
echo "     above. Until it closes, every PR's own checks are red for a reason" >&2
echo "     that has nothing to do with the PR — which is exactly when a real" >&2
echo "     defect merges unnoticed, because nobody can tell the two reds" >&2
echo "     apart. That is not hypothetical: it is what happened on" >&2
echo "     2026-08-19, four times in 35 minutes (#3917)." >&2
echo "" >&2
echo "     What to do:" >&2
echo "       - Fixing main? Add the \`$escape_label\` label to this PR and" >&2
echo "         re-run. That is the designed way through, not a workaround." >&2
echo "       - Otherwise: wait. The canary closes its issue by itself the" >&2
echo "         moment main composes again, and this check goes green with no" >&2
echo "         action from you." >&2
exit 1
