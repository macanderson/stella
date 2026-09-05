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
# A comment carries an author and a timestamp, and the window makes a crashed
# claim expire on its own. Twenty minutes is long enough to write a two-line
# fix and short enough that a dead session is not in the way for a second
# incident.
#
# ## The author alone is not the identity
#
# One person runs several agent sessions at once, so "did I write this claim?"
# cannot be answered by the login. It was not, and on 2026-09-02 three sessions
# all running as one author each read their peers' claims as their own, each
# proceeded, and each opened a pull request splitting the same file the same
# way, eight minutes apart, against the one open `main-red` issue. That issue
# carried five claim comments from one login in fifteen minutes.
#
# So a claim carries a third fact, a session word:
#
#   main-red-claim: <login> <session>
#
# and a claim is this session's own only when both the login and the session
# match. The word is the first of:
#
#   - `STELLA_CLAIM_SESSION`, for a fleet that already has a run id;
#   - a random token minted on first use and kept in this clone's git dir, at
#     `$(git rev-parse --git-dir)/main-red-claim-session`.
#
# `git rev-parse --git-dir` answers per **worktree**, which is what makes the
# token tell three agent worktrees apart while a session that re-checks reads
# back the one it minted. It lives inside the git dir, so it never enters the
# work tree and cannot be committed. Two sessions sharing one worktree still
# read one token and cannot be told apart — that is what the env var is for.
#
# The token is minted by `check`, not only by `claim`, because the second
# session has to hold an identity before it claims anything or it cannot tell
# the first session's claim from its own.
#
# ## Fail-open at every unknown
#
# Anything this cannot answer means PROCEED, loudly. An unreachable tracker, a
# `gh` that is not installed, an identity it cannot read, two open `main-red`
# issues at once: each prints a note and exits 0. That is not caution about
# GitHub's uptime, it is the ordering of costs — duplicated work is what this
# prevents, and a blocked repair is worse than what it prevents. Standing a
# session down is reserved for the one case it can actually establish: a
# claim, by somebody else, inside the window.
#
# The session word obeys the same ordering, on both sides of the comparison. A
# run with no session word of its own — no git dir, a git dir it cannot write,
# a `STELLA_CLAIM_SESSION` that is not one plain word — reads every claim of
# its own login as its own, which is what this script did before the word
# existed. A claim comment with no session word does the same, so a claim left
# by an older copy of this script cannot block the author who wrote it.
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
#   scripts/main-red-claim.sh session          # print this clone's session word
#   scripts/main-red-claim.sh select --now <unix-seconds>   # pure: JSON in, rows out
#   scripts/main-red-claim.sh check --window-minutes 20
#   scripts/main-red-claim.sh check --fixture-open-issues "4671" \
#                                   --fixture-login "ada" \
#                                   --fixture-session "s2" \
#                                   --fixture-claims "grace s1 300"
#
# Uses portable POSIX tools plus `gh` so it runs anywhere a clone does. The
# staleness arithmetic runs in `select_claims`'s own jq filter below, not
# embedded in the `gh` call. `select` is that filter's seam: a test can feed
# it a real payload instead of only the already-parsed `--fixture-claims`
# shape. Not `date`: `date -d` and `date -j` disagree across the platforms
# this repository is cloned on, which is why the arithmetic stays in jq
# either way.
set -uo pipefail

label="main-red"
marker="main-red-claim:"
mode=""
window_minutes=20
fixture_open_issues=""
fixture_login=""
fixture_claims=""
fixture_session=""
select_now=""
use_fixture=0

while [ $# -gt 0 ]; do
  case "$1" in
  check | claim | session | select)
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
  # `select`'s own clock, so a test can pin it. Unrelated to the fixture
  # seams below: `select` reads real comments JSON on stdin and never stubs
  # the tracker.
  --now)
    [ $# -ge 2 ] || {
      echo "main-red-claim: --now needs a unix timestamp" >&2
      exit 2
    }
    select_now="$2"
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
  # Zero or more `<login> <session> <age-seconds>` rows, newline-separated, in
  # any order: the freshest is picked by age, not by position. A `-` in the
  # session column is a claim carrying no session word, which is what an older
  # copy of this script wrote.
  --fixture-claims)
    [ $# -ge 2 ] || {
      echo "main-red-claim: --fixture-claims needs a value" >&2
      exit 2
    }
    fixture_claims="$2"
    use_fixture=1
    shift 2
    ;;
  # This session's own word. An empty value is the run that has none, which
  # every unknown resolves to.
  --fixture-session)
    [ $# -ge 2 ] || {
      echo "main-red-claim: --fixture-session needs a value" >&2
      exit 2
    }
    fixture_session="$2"
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

# A session word is one plain word, so it survives a whitespace-split parse of
# a comment's first line and cannot smuggle a second column into it.
plain_word() {
  case "$1" in
  '' | *[!A-Za-z0-9._-]*) return 1 ;;
  esac
  return 0
}

# This session's own word, minted once and then read back.
resolve_session() {
  if [ -n "${STELLA_CLAIM_SESSION-}" ]; then
    if plain_word "$STELLA_CLAIM_SESSION"; then
      printf '%s' "$STELLA_CLAIM_SESSION"
      return 0
    fi
    echo "note: STELLA_CLAIM_SESSION is not one plain word, so it cannot be a" >&2
    echo "      session word. Falling back to this clone's own." >&2
  fi

  git_dir="$(git rev-parse --git-dir 2>/dev/null)" || return 1
  [ -n "$git_dir" ] || return 1
  session_file="$git_dir/main-red-claim-session"

  if [ ! -f "$session_file" ]; then
    token="$(od -An -N8 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')"
    plain_word "$token" || token="$$-$(date +%s 2>/dev/null)"
    plain_word "$token" || return 1
    printf '%s\n' "$token" >"$session_file" 2>/dev/null || return 1
  fi

  # Read back rather than keep what was just minted, so two sessions racing to
  # create the file in one worktree end up agreeing instead of each reading the
  # other's claim as a stranger's.
  token="$(tr -d ' \t\r\n' <"$session_file" 2>/dev/null)" || return 1
  plain_word "$token" || return 1
  printf '%s' "$token"
}

# `session` asks nothing of the tracker: it prints the word this clone claims
# under and exits. A diagnostic rather than a verdict, so an absent word is a
# failure here instead of the proceed the two verdict modes take.
if [ "$mode" = "session" ]; then
  if session_word="$(resolve_session)" && [ -n "$session_word" ]; then
    trap '' PIPE
    echo "$session_word" || true
    exit 0
  fi
  echo "main-red-claim: no session word — no git dir, or one this run cannot" >&2
  echo "                write. \`check\` proceeds on its own login without it." >&2
  exit 1
fi

# Every claim comment in a `gh issue view --json comments` payload, as
# `<login> <session> <age-in-seconds>` — `-` marks a claim with no session
# word, same as `--fixture-claims`. Pure: the payload comes in on stdin, and
# `now` is an argument, not the real clock. That lets `select` mode below,
# and the tests in scripts/test-main-red-claim.sh, run the exact filter
# production uses.
#
# Only the first line is read, with CRLF stripped and runs of spaces
# collapsed to one field each. A claim's marker line is
# `main-red-claim: <login> [session]`; later lines never enter this parse.
# Padding with three `-` first is what lets a two-column claim — no session
# word, written by an older copy of this script — parse `$word[2]` instead
# of erroring on a missing index.
select_claims() {
  jq -r --arg marker "$marker" --argjson now "$1" '
    .comments[]
    | select(.body | startswith($marker))
    | ((.body | split("\n")[0] | gsub("\r"; "") | split(" ")
        | map(select(length > 0)))
       + ["-", "-", "-"]) as $word
    | "\(.author.login) \($word[2]) \(($now - (.createdAt | fromdateiso8601)) | floor)"
  '
}

# `select` is the seam a test can drive directly: real comments JSON goes in,
# the parsed rows come out. No tracker, no fixture. Production wires it to
# `gh issue view --json comments` below instead of writing this filter into
# that call. Break the filter, and a test fails here — not only a live
# claim, silently, later.
if [ "$mode" = "select" ]; then
  [ -n "$select_now" ] || {
    echo "main-red-claim: select needs --now <unix-seconds>" >&2
    exit 2
  }
  select_claims "$select_now"
  exit $?
fi

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

# Newlines to spaces, then squeezed, and not `tr -d ' '` — which
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

if [ "$use_fixture" -eq 1 ]; then
  my_session="$fixture_session"
elif ! my_session="$(resolve_session)"; then
  my_session=""
fi
if [ -z "$my_session" ]; then
  echo "note: this run has no session word, so a claim of its own login cannot" >&2
  echo "      be told from a peer session's. Proceeding on those (fail-open)." >&2
fi

# Every claim comment, via `select_claims` above rather than a `--jq` program
# embedded in this call.
if [ "$use_fixture" -eq 1 ]; then
  claims="$fixture_claims"
elif ! comments_json="$(CLICOLOR_FORCE=0 NO_COLOR=1 gh issue view "$issue" --json comments 2>/dev/null)"; then
  echo "note: could not read #$issue's comments. Proceeding (fail-open)." >&2
  proceed "ok  proceed (comments unreadable)"
elif ! claims="$(printf '%s' "$comments_json" | select_claims "$(date -u +%s)")"; then
  echo "note: could not read #$issue's comments. Proceeding (fail-open)." >&2
  proceed "ok  proceed (comments unreadable)"
fi

# The freshest claim this session cannot account for. Its own is not a reason
# to stand it down: re-running the pre-flight is what a session does when it
# comes back to the repair it already started. Same login and same session word
# is its own; same login and either word missing is unprovable, and an unknown
# proceeds.
held_by=""
held_session=""
held_age=""
while read -r who claim_session age; do
  [ -n "$who" ] || continue
  if [ "$who" = "$me" ]; then
    [ -z "$my_session" ] && continue
    [ "$claim_session" = "-" ] && continue
    [ "$claim_session" = "$my_session" ] && continue
  fi
  case "$age" in
  '' | *[!0-9]*) continue ;;
  esac
  [ "$age" -lt "$window_seconds" ] || continue
  if [ -z "$held_age" ] || [ "$age" -lt "$held_age" ]; then
    held_by="$who"
    held_session="$claim_session"
    held_age="$age"
  fi
done <<EOF
$claims
EOF

if [ -n "$held_by" ]; then
  minutes=$((held_age / 60))
  if [ "$held_session" = "-" ]; then
    held_where="session unknown"
  else
    held_where="session $held_session"
  fi
  echo "STAND DOWN  #$issue is already being repaired." >&2
  echo "" >&2
  echo "     claimed by @$held_by ($held_where), ${minutes}m ago" \
    "(window: ${window_minutes}m)" >&2
  echo "" >&2
  echo "     On 2026-08-24 three sessions each wrote the same two-line fix for" >&2
  echo "     the same break and merged them 95 seconds apart (#4680). Every" >&2
  echo "     one of them was reasoning correctly in isolation; what none of" >&2
  echo "     them could see was the other two." >&2
  echo "" >&2
  if [ "$held_by" = "$me" ]; then
    echo "     That login is yours, so this is another of your own sessions —" >&2
    echo "     a second agent in a second worktree, which is the case the" >&2
    echo "     session word exists to catch." >&2
    echo "" >&2
  fi
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

# The session word rides on the marker line, where the check parses it. A run
# without one posts the old two-word body, which every reader still accepts.
if [ -n "$my_session" ]; then
  claimant="$me $my_session"
  session_note="
The second word is this session's own, so another session of the same author
reads this as somebody else's claim."
else
  claimant="$me"
  session_note="
This run had no session word to add, so another session of the same author
cannot tell this claim from one of its own."
fi
body="$marker $claimant
Repairing this. The claim lapses after ${window_minutes} minutes, so it cannot
hold the repair shut if this session dies (\`scripts/main-red-claim.sh\`, #4680).$session_note"

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
