#!/usr/bin/env bash
#
# The pre-flight a session runs before it repairs a red `main`: is someone
# already fixing this? See #4680, filed from the incident.
#
# On 2026-08-24 `main` was red from a single two-line composition break, and
# three separate PRs fixed it independently, all merging inside 95 seconds:
#
#   18:13:22  #4672 — box the two AgentEvent literals the #4612 witness left bare
#   18:13:—   #4673 — box the two FleetMsg::Event payloads #4663 added
#   18:13:48  #4674 — box the two FleetMsg::Event values its own tests build
#
# All three changed the same two call sites in
# `crates/stella-tui/src/fleet_dashboard/tests.rs` in the same way. The merges
# happened to compose, because identical edits resolve cleanly, so it cost
# duplicated work rather than correctness. It could as easily not have.
#
# Nobody was wrong. `main-canary.yml` files one `main-red` issue and
# `main-red-hold.yml` consumes it to hold merges — and with the hold in place
# every session holding an open PR notices the red at once, reaches the same
# correct conclusion, and writes the same patch. The signal said "main is
# broken"; no signal said "and it is being fixed". This file is that second
# signal. It is the `dispatch_claims` mechanic (#4300) with the tracker as
# the table, which is the one store every session already shares.
#
# ## Why a lapsing comment, and not an assignee or a linked PR
#
# Both alternatives were live in #4680 and both fail on this incident:
#
#   - An **assignee** never lapses. A session that crashes mid-repair holds
#     the claim until a human clears it, and the thing it is holding shut is
#     the repair of a red `main` — a deadlock the constraint below forbids.
#   - A **linked PR** cannot be seen until a branch is pushed, and all three
#     patches above were written before any of them opened. The window this
#     is meant to close is the window where there is nothing to link to yet.
#
# A comment carries an author and a timestamp, which are exactly the two facts
# the decision needs, and the window makes a crashed claim expire on its own.
# Twenty minutes is long enough to write a two-line fix and short enough that
# a dead session is not in the way for a second incident.
#
# ## Fail-open, deliberately, at every unknown
#
# Anything this cannot answer means PROCEED, loudly. An unreachable tracker, a
# `gh` that is not installed, an identity it cannot read, two open `main-red`
# issues at once: each prints a note and exits 0. That is not caution about
# GitHub's uptime, it is the ordering of costs — duplicated work is what this
# prevents, and a blocked repair is worse than what it prevents. Standing a
# session down is reserved for the one case it can actually establish: a
# claim, by somebody else, inside the window.
#
# ## What this is NOT
#
# It does not open, merge or label anything, it compiles nothing, and it is
# not a gate step — no CI job runs it, because the decision it informs is made
# by whoever is about to write the patch, before CI has anything to look at.
#
# Usage:
#   scripts/main-red-claim.sh check            # exit 0 proceed, 1 stand down
#   scripts/main-red-claim.sh claim            # check, then claim it
#   scripts/main-red-claim.sh check --window-minutes 20
#   scripts/main-red-claim.sh check --fixture-open-issues "4671" \
#                                   --fixture-login "ada" \
#                                   --fixture-claims "grace 300"
#
# Uses portable POSIX tools plus `gh` so it runs anywhere a clone does. The
# staleness arithmetic is done by `gh --jq`, not by `date`, because `date -d`
# and `date -j` disagree across the platforms this repository is cloned on.
set -uo pipefail

label="main-red"
marker="main-red-claim:"
mode=""
window_minutes=20
fixture_open_issues=""
fixture_login=""
fixture_claims=""
use_fixture=0

while [ $# -gt 0 ]; do
  case "$1" in
  check | claim)
    mode="$1"
    shift
    ;;
  --window-minutes)
    [ $# -ge 2 ] || {
      echo "main-red-claim: --window-minutes needs a number" >&2
      exit 2
    }
    window_minutes="$2"
    shift 2
    ;;
  # Test-only seams. Supplying any one of them stubs the tracker entirely: a
  # test that stubbed the issue list but let the comment lookup reach the
  # network would pass or fail for a reason the test did not choose — the
  # same trap check-main-red-hold.sh's paired fixtures avoid.
  --fixture-open-issues)
    [ $# -ge 2 ] || {
      echo "main-red-claim: --fixture-open-issues needs a value" >&2
      exit 2
    }
    fixture_open_issues="$2"
    use_fixture=1
    shift 2
    ;;
  --fixture-login)
    [ $# -ge 2 ] || {
      echo "main-red-claim: --fixture-login needs a value" >&2
      exit 2
    }
    fixture_login="$2"
    use_fixture=1
    shift 2
    ;;
  # Zero or more `<login> <age-seconds>` pairs, newline-separated, in any
  # order: the freshest is picked by age, not by position.
  --fixture-claims)
    [ $# -ge 2 ] || {
      echo "main-red-claim: --fixture-claims needs a value" >&2
      exit 2
    }
    fixture_claims="$2"
    use_fixture=1
    shift 2
    ;;
  -h | --help)
    # The whole comment block after the shebang, bounded by its first
    # non-comment line — hardcoded line numbers truncate silently the first
    # time the header grows. Same reader as check-main-red-hold.sh's --help.
    awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0"
    exit 0
    ;;
  *)
    echo "main-red-claim: unknown argument '$1'" >&2
    exit 2
    ;;
  esac
done

[ -n "$mode" ] || mode="check"

case "$window_minutes" in
'' | *[!0-9]*)
  echo "main-red-claim: --window-minutes takes a whole number of minutes" >&2
  exit 2
  ;;
esac
window_seconds=$((window_minutes * 60))

# `proceed` is the only exit this script takes when it is unsure, so it is
# one function rather than a repeated pair of lines that could drift apart.
proceed() {
  # SIGPIPE ignored and the write discarded, so a reader that closed the pipe
  # cannot turn a proceed into a failure (the #1815 shape).
  trap '' PIPE
  echo "$1" || true
  exit 0
}

if [ "$use_fixture" -eq 0 ] && ! command -v gh >/dev/null 2>&1; then
  echo "note: gh is not installed, so this run could not ask whether the" >&2
  echo "      repair is already claimed. Proceeding: a claim check that can" >&2
  echo "      block a repair is worse than the duplication it prevents." >&2
  proceed "ok  proceed (could not ask)"
fi

if [ "$use_fixture" -eq 1 ]; then
  open_issues="$fixture_open_issues"
elif ! open_issues="$(gh issue list --label "$label" --state open \
  --limit 20 --json number --jq '.[].number' 2>/dev/null)"; then
  echo "note: could not reach the issue tracker. Proceeding (fail-open); see" >&2
  echo "      this script's header for why an unknown always means proceed." >&2
  proceed "ok  proceed (tracker unreachable)"
fi

# Newlines to spaces, then squeezed — deliberately NOT `tr -d ' '`, which
# check-main-red-hold.sh can afford because it only ever prints this list.
# This one splits it, and deleting the separator turns "4671 4672" into the
# single issue 46714672: two open issues would read as one unambiguous one,
# and the ambiguity branch below could never fire. Caught by its own case.
open_issues="$(printf '%s' "$open_issues" | tr '\n' ' ' | tr -s ' ')"
open_issues="${open_issues# }"
open_issues="${open_issues% }"

if [ -z "$open_issues" ]; then
  proceed "ok  no open \`$label\` issue — main is not known-broken."
fi

# More than one open `main-red` issue means the canary filed twice, or a human
# filed alongside it. Which one a claim belongs on is then a judgement, and
# guessing it would put the claim where the next session does not look.
case "$open_issues" in
*' '*)
  echo "note: several open \`$label\` issues ($open_issues), so this run cannot" >&2
  echo "      tell which one a repair claims. Proceeding (fail-open)." >&2
  proceed "ok  proceed (ambiguous: $open_issues)"
  ;;
esac
issue="$open_issues"

if [ "$use_fixture" -eq 1 ]; then
  me="$fixture_login"
elif ! me="$(gh api user --jq .login 2>/dev/null)"; then
  me=""
fi
if [ -z "$me" ]; then
  echo "note: could not read this session's login, so a claim on #$issue cannot" >&2
  echo "      be told from one of its own. Proceeding (fail-open)." >&2
  proceed "ok  proceed (identity unknown)"
fi

# Every claim comment, as `<login> <age-in-seconds>`. The arithmetic is jq's:
# `date` spells the same conversion two incompatible ways across macOS and
# Linux, and this script is run from a clone on both.
if [ "$use_fixture" -eq 1 ]; then
  claims="$fixture_claims"
elif ! claims="$(gh issue view "$issue" --json comments --jq "
    .comments[]
    | select(.body | startswith(\"$marker\"))
    | \"\(.author.login) \((now - (.createdAt | fromdateiso8601)) | floor)\"
  " 2>/dev/null)"; then
  echo "note: could not read #$issue's comments. Proceeding (fail-open)." >&2
  proceed "ok  proceed (comments unreadable)"
fi

# The freshest claim by anybody else. A claim of this session's own is not a
# reason to stand it down: re-running the pre-flight is what a session does
# when it comes back to the repair it already started.
held_by=""
held_age=""
while read -r who age; do
  [ -n "$who" ] || continue
  [ "$who" = "$me" ] && continue
  case "$age" in
  '' | *[!0-9]*) continue ;;
  esac
  [ "$age" -lt "$window_seconds" ] || continue
  if [ -z "$held_age" ] || [ "$age" -lt "$held_age" ]; then
    held_by="$who"
    held_age="$age"
  fi
done <<EOF
$claims
EOF

if [ -n "$held_by" ]; then
  minutes=$((held_age / 60))
  echo "STAND DOWN  #$issue is already being repaired." >&2
  echo "" >&2
  echo "     claimed by @$held_by, ${minutes}m ago (window: ${window_minutes}m)" >&2
  echo "" >&2
  echo "     On 2026-08-24 three sessions each wrote the same two-line fix for" >&2
  echo "     the same break and merged them 95 seconds apart (#4680). Every" >&2
  echo "     one of them was reasoning correctly in isolation; what none of" >&2
  echo "     them could see was the other two." >&2
  echo "" >&2
  echo "     What to do:" >&2
  echo "       - Wait. The claim lapses by itself after ${window_minutes}m, so a" >&2
  echo "         session that died mid-repair cannot hold this shut." >&2
  echo "       - Repairing it anyway (the claim looks stale, or you are" >&2
  echo "         fixing a second, separate break)? Say so on #$issue, so the" >&2
  echo "         next session reads a reason rather than a collision." >&2
  exit 1
fi

if [ "$mode" = "check" ]; then
  proceed "ok  #$issue is unclaimed — the repair is yours to take."
fi

body="$marker $me
Repairing this. The claim lapses after ${window_minutes} minutes, so it cannot
hold the repair shut if this session dies (\`scripts/main-red-claim.sh\`, #4680)."

if [ "$use_fixture" -eq 1 ]; then
  proceed "ok  claimed #$issue as @$me (fixture: nothing was posted)"
fi

if ! gh issue comment "$issue" --body "$body" >/dev/null 2>&1; then
  echo "note: could not post the claim on #$issue. Proceeding anyway: the" >&2
  echo "      claim is an optimisation, and the repair is not." >&2
  proceed "ok  proceed (claim not posted)"
fi

trap '' PIPE
echo "ok  claimed #$issue as @$me" || true
