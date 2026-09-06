#!/usr/bin/env bash
#
# Is somebody already implementing issue #N? (#5224)
#
#   scripts/issue-claim.sh check <n>    # exit 0 proceed, 1 stand down
#   scripts/issue-claim.sh claim <n>    # check, then post the claim
#   scripts/issue-claim.sh select --now <unix-seconds>   # pure: JSON in, rows out
#
# ── Why this exists ──────────────────────────────────────────────────────────
#
# Two sessions implemented #5045 in parallel, on the same branch name, and one
# merge resolved "keeping this tree" — silently dropping the other
# implementation. The same shape hit #4336 and #5054. Three collisions in one
# afternoon, each costing a full implementation.
#
# Every signal said the branch was abandoned: it existed only locally, in a
# stale worktree at an old `main`, clean tree, no remote branch, no open PR.
# There was nothing to see, because nothing was writing anything down.
#
# `main-red-claim.sh` solves exactly this shape for a red-`main` repair, and
# this is that mechanic pointed at an issue. The rules are its rules:
#
#   the tracker is the table   a local claim is invisible to the peer session
#                              that needs it — the sessions are in different
#                              worktrees, often on different machines.
#   a comment is the claim     so it carries an author and a timestamp without
#                              any new storage.
#   it lapses                  an assignee never lapses, and a crashed session
#                              would then hold an issue shut forever.
#   every unknown proceeds     loudly. A claim check that can BLOCK work is
#                              worse than the duplication it prevents.
#
# ── The author alone is not the identity ─────────────────────────────────────
#
# One login is one account, and one person runs several agent sessions at once
# — a fleet, or several worktrees on one machine. Comparing the login alone
# read a peer session's claim as this session's own and cleared both to work
# the same issue, which is the collision the whole script exists to stop
# (`#5875`).
#
# So a claim carries a session word beside the login:
#
#   <!-- issue-claim --> <login> <session>
#
# and a claim is this session's own only when both match. The word comes from
# `scripts/lib/claim-session.sh`, shared with `main-red-claim.sh` so a fleet
# sets one environment variable rather than two;
# `./scripts/main-red-claim.sh session` prints the one this clone claims under.
#
# Fail-open holds on both sides of the comparison. A run with no word of its
# own, and a claim comment carrying none, both fall back to the author-only
# rule — so a claim written by an older copy of this script cannot block the
# author who wrote it.
#
# ── What it cannot see ───────────────────────────────────────────────────────
#
# A claim is a signal somebody chose to leave. It does not see a session that
# never ran this, and it is not a lock: two sessions that both check within a
# second of each other both proceed. It converts the common case — a peer who
# started ten minutes ago — from invisible into obvious, and that is all it
# claims to do.
#
# It also does not replace reading the open PRs. A merged or open PR closing
# the issue is a stronger signal than any claim, and this prints one when it
# finds it — that is the check that would have saved this session twice.

set -uo pipefail

# `plain_word` and `resolve_session` — the same word `main-red-claim.sh`
# claims under, so two scripts cannot disagree about which session this is.
# shellcheck source=scripts/lib/claim-session.sh
. "$(dirname "$0")/lib/claim-session.sh"

# A decided verdict must survive a reader that closes the pipe early (#1815).
trap '' PIPE

marker="<!-- issue-claim -->"
window_minutes=90

mode=""
issue=""
fixture_login=""
fixture_session=""
fixture_claims=""
fixture_prs=""
fixture_prs_failed=0
select_now=""
use_fixture=0

while [ $# -gt 0 ]; do
  case "$1" in
  check | claim | select)
    mode="$1"
    shift
    ;;
  --window-minutes)
    window_minutes="${2:-}"
    shift 2
    ;;
  # `select`'s own clock, so a test can pin it. Unrelated to the fixture
  # seams below: `select` reads real comments JSON on stdin and never stubs
  # the tracker.
  --now)
    select_now="${2:-}"
    shift 2
    ;;
  # Test-only, and paired: a fixture that supplied claims but read the real
  # tracker would compare two different worlds. The same trap
  # `main-red-claim.sh`'s paired fixtures avoid.
  --fixture-login)
    fixture_login="${2:-}"
    use_fixture=1
    shift 2
    ;;
  # This session's own word. An empty value is the run that has none, which
  # is the fail-open side of the comparison.
  --fixture-session)
    fixture_session="${2:-}"
    use_fixture=1
    shift 2
    ;;
  --fixture-claims)
    fixture_claims="${2:-}"
    use_fixture=1
    shift 2
    ;;
  --fixture-prs)
    fixture_prs="${2:-}"
    use_fixture=1
    shift 2
    ;;
  # Test-only: simulates the `gh pr list` call itself failing, as opposed to
  # succeeding with an empty result. --fixture-prs is meaningless paired with
  # this — a failed query has no rows to fake.
  --fixture-prs-failed)
    fixture_prs_failed=1
    use_fixture=1
    shift
    ;;
  -h | --help)
    awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0"
    exit 0
    ;;
  *)
    if [ -z "$issue" ]; then
      issue="$1"
      shift
    else
      echo "issue-claim: unknown argument '$1'" >&2
      exit 2
    fi
    ;;
  esac
done

# Every claim comment in a `gh issue view --json comments` payload, as
# `<login> <session> <age-in-seconds>` — `-` marks a claim with no session
# word, same as `--fixture-claims`. Pure: the payload comes in on stdin, and
# `now` is an argument, not the real clock. That lets `select` mode below, and
# the tests in scripts/test-issue-claim.sh, run the exact filter production
# uses.
#
# Only the first line is read, with CRLF stripped and runs of spaces collapsed
# to one field each. A claim's marker line is
# `<!-- issue-claim --> <login> [session]`, so the marker itself takes the
# first three fields and the session word is the fifth. Padding with `-`
# first is what lets a claim with no session word — written by an older copy
# of this script — parse `$word[4]` instead of erroring on a missing index.
#
# The login comes from `.author.login` rather than the marker line, because
# that is the one GitHub stamps and a body can say anything.
select_claims() {
  jq -r --arg marker "$marker" --argjson now "$1" '
    .comments[]
    | select(.body | startswith($marker))
    | ((.body | split("\n")[0] | gsub("\r"; "") | split(" ")
        | map(select(length > 0)))
       + ["-", "-", "-", "-", "-"]) as $word
    | "\(.author.login) \($word[4]) \(($now - (.createdAt | fromdateiso8601)) | floor)"
  '
}

# `select` is the seam a test can drive directly: real comments JSON goes in,
# the parsed rows come out. No tracker, no issue number. Production wires it
# to `gh issue view --json comments` below instead of writing this filter
# into that call. Break the filter, and a test fails here — not only a live
# claim, silently, later.
if [ "$mode" = "select" ]; then
  [ -n "$select_now" ] || {
    echo "issue-claim: select needs --now <unix-seconds>" >&2
    exit 2
  }
  select_claims "$select_now"
  exit $?
fi

if [ -z "$mode" ] || [ -z "$issue" ]; then
  echo "issue-claim: usage: issue-claim.sh check|claim <issue-number>" >&2
  exit 2
fi
case "$issue" in '' | *[!0-9]*)
  echo "issue-claim: '<issue>' takes a whole number" >&2
  exit 2
  ;;
esac
case "$window_minutes" in '' | *[!0-9]*)
  echo "issue-claim: --window-minutes takes a whole number of minutes" >&2
  exit 2
  ;;
esac
window_seconds=$((window_minutes * 60))

# `proceed` is the only exit this script takes when it is unsure, so it is one
# function rather than a repeated pair of lines that could drift apart.
proceed() {
  echo "$1" || true
  exit 0
}

if [ "$use_fixture" -eq 0 ] && ! command -v gh >/dev/null 2>&1; then
  echo "note: gh is not installed, so this run could not ask whether #$issue is" >&2
  echo "      already being worked. Proceeding: a claim check that can block" >&2
  echo "      work is worse than the duplication it prevents." >&2
  proceed "ok  proceed (could not ask)"
fi

# ── The stronger signal first ────────────────────────────────────────────────
#
# A PR that closes the issue beats any claim, and it is the check that would
# have caught the collisions this script was filed for: #5246 closed 44 issues
# in one sweep, and none of them showed a thing on the issue itself.

# Each hit is `<number> <state> <kind>`. `closes` means the body has
# `Closes #N`. `refs` means it has `Refs #N` but no `Closes #N`. `$issue` is
# already checked as digits only, so it is safe to put in the jq program
# below.
pr_jq='
  .[]
  | select(
      (.body | test("Closes #ISSUE\\b")) or (.body | test("Refs #ISSUE\\b"))
    )
  | "\(.number) \(.state) \(
      if (.body | test("Closes #ISSUE\\b")) then "closes" else "refs" end
    )"
'
pr_jq="${pr_jq//ISSUE/$issue}"

# prs_ok tracks whether this query actually ran, distinct from prs itself
# being empty. "no PR closes it" is a claim about a list that was read; a
# query that failed to even ask must never be reported in that shape.
prs_ok=1
if [ "$use_fixture" -eq 1 ]; then
  if [ "$fixture_prs_failed" -eq 1 ]; then
    prs=""
    prs_ok=0
  else
    prs="$fixture_prs"
  fi
elif ! prs="$(gh pr list --state all --limit 100 --json number,state,body \
  --jq "$pr_jq" 2>/dev/null)"; then
  echo "note: could not list pull requests. Proceeding (fail-open); the claim" >&2
  echo "      check below still runs." >&2
  prs=""
  prs_ok=0
fi

# Split the hits by kind. A `refs` hit never blocks: `Refs` is also how an
# unrelated PR cites this issue. A `closes` hit blocks only when its state
# is OPEN or MERGED. A CLOSED `closes` hit means a past try died. That is
# the state where the next session should move, not stop.
closing_hits=""
refs_hits=""
blocking=0
while IFS=' ' read -r pr_num pr_state pr_kind; do
  [ -n "$pr_num" ] || continue
  case "$pr_kind" in
  refs)
    refs_hits="$refs_hits$pr_num $pr_state
"
    ;;
  *)
    # Anything that is not `refs` counts as `closes`, even an unknown third
    # field. An unknown kind is not proof the hit is safe, so it blocks, the
    # way every hit did before this split.
    closing_hits="$closing_hits$pr_num $pr_state
"
    case "$pr_state" in
    CLOSED) : ;;
    *) blocking=1 ;;
    esac
    ;;
  esac
done <<EOF
$prs
EOF

if [ "$blocking" -eq 1 ]; then
  echo "STAND DOWN  #$issue is already claimed by a pull request." >&2
  echo "" >&2
  printf '     %s\n' "$closing_hits" >&2
  echo "" >&2
  echo "     A MERGED one means the work is done and the issue is stale." >&2
  echo "     An OPEN one means somebody is on it right now." >&2
  echo "     A CLOSED one only blocks here because an OPEN or MERGED hit is" >&2
  echo "     listed with it. It may be the first, dead try at the same fix," >&2
  echo "     so read it too." >&2
  echo "" >&2
  echo "     Read its diff before writing anything. Sometimes yours covers" >&2
  echo "     something theirs missed and belongs on top of \`main\`; more often" >&2
  echo "     it is the same work twice. A sweep PR can close forty issues at" >&2
  echo "     once and show nothing on any of them, which is why this asks the" >&2
  echo "     PRs rather than the issue." >&2
  exit 1
fi

if [ -n "$closing_hits" ]; then
  echo "note: the pull request(s) that close #$issue were closed, not merged." >&2
  echo "      That is a dead try, not a live claim. Read them, then proceed." >&2
  printf '     %s\n' "$closing_hits" >&2
fi

if [ -n "$refs_hits" ]; then
  echo "note: a pull request refs #$issue but does not close it. That is a" >&2
  echo "      weaker signal, not a claim by itself. Read it before you write:" >&2
  echo "      the live fix can sit in a Refs-only PR while the one that would" >&2
  echo "      close the issue is dead." >&2
  printf '     %s\n' "$refs_hits" >&2
fi

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
# comes back to work it already started. Same login and same session word is
# its own; same login and either word missing is unprovable, and an unknown
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
  echo "STAND DOWN  #$issue is already being implemented." >&2
  echo "" >&2
  echo "     claimed by @$held_by ($held_where), ${minutes}m ago" \
    "(window: ${window_minutes}m)" >&2
  echo "" >&2
  if [ "$held_by" = "$me" ]; then
    echo "     That login is yours, so this is another of your own sessions —" >&2
    echo "     a second agent in a second worktree, which is the case the" >&2
    echo "     session word exists to catch." >&2
    echo "" >&2
  fi
  echo "     Two sessions implemented #5045 in parallel and one merge kept one" >&2
  echo "     tree, dropping the other implementation without a conflict to" >&2
  echo "     report. Every signal said that branch was abandoned (#5224)." >&2
  echo "" >&2
  echo "     What to do:" >&2
  echo "       - Pick something else. The claim lapses by itself after" >&2
  echo "         ${window_minutes}m, so a session that died cannot hold this shut." >&2
  echo "       - Working it anyway (the claim looks stale, or you are doing a" >&2
  echo "         different part)? Say so on #$issue, so the next session reads" >&2
  echo "         a reason rather than a collision." >&2
  exit 1
fi

if [ "$mode" = "check" ]; then
  if [ "$prs_ok" -eq 0 ]; then
    proceed "ok  proceed (PR list unreadable)"
  fi
  proceed "ok  #$issue is unclaimed — nothing open or merged closes it, and no live claim holds it."
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
reads this as its own and proceeds."
fi

body="$marker $claimant
Implementing this. The claim lapses after ${window_minutes} minutes, so it
cannot hold the issue shut if this session dies
(\`scripts/issue-claim.sh\`, #5224).${session_note}"

if [ "$use_fixture" -eq 1 ]; then
  proceed "ok  claimed #$issue as @$me (fixture: nothing was posted)"
fi

if ! gh issue comment "$issue" --body "$body" >/dev/null 2>&1; then
  echo "note: could not post the claim on #$issue. Proceeding anyway: the claim" >&2
  echo "      is an optimisation, and the work is not." >&2
  proceed "ok  proceed (claim not posted)"
fi

echo "ok  claimed #$issue as @$me" || true
