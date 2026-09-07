#!/usr/bin/env bash
#
# Is somebody already sweeping a pull request, and is this finding already
# posted there? (`#5861`)
#
#   `scripts/pr-claim.sh check <n>     # exit 0 proceed, 1 stand down`
#   `scripts/pr-claim.sh claim <n>     # check, then post the claim`
#   `scripts/pr-claim.sh post <n> --finding <key> --body-file <path>`
#   `scripts/pr-claim.sh select --now <unix-seconds>      # JSON in, rows out`
#   `scripts/pr-claim.sh findings --finding <key> --now <unix-seconds>`
#
# ── Why this exists ──────────────────────────────────────────────────────────
#
# Three sweeps read one pull request in eight minutes. Each worked out the same
# merge conflict, wrote the same table, and posted it. A fourth reached the
# same answer and stopped, because it read the comments first. The maintainer
# was left to diff three long comments to learn they say one thing.
#
# The cost is not lost work. It is published work. That is what makes it worse
# than the same race over an issue. A spare comment lands on the maintainer's
# pull request and stays there.
#
# `scripts/issue-claim.sh` claims an issue. Nothing claimed a pull request.
# This is that mechanic pointed at one. Its rules are that script's rules.
# The tracker is the table, since a local note is hidden from the peer that
# needs it. A comment is the claim, so it carries an author and a time with no
# new storage. It lapses, so a session that dies cannot hold a pull request
# shut. And every unknown proceeds, out loud: a check that can block a sweep
# is worse than the waste it stops.
#
# ── Two gates ────────────────────────────────────────────────────────────────
#
# `check` asks whether a peer holds the pull request. `post` asks whether this
# finding is already up. They fire at different times, and only the second one
# would have stopped all three comments above.
#
# Take the claim when you start to read the pull request. Each of the three
# sweeps spent twenty minutes on the conflict before it had a word to say. A
# claim taken at the end saves none of that.
#
# ── The finding key ──────────────────────────────────────────────────────────
#
# The caller names the finding. This script does not read the words. Two
# sweeps that find one thing write it up two ways, so a digest of the text
# would call them different. The key is what they agree on:
#
#   `scripts/pr-claim.sh post 5835 --finding conflict:5828 --body-file note.md`
#
# A key that has to go stale carries what it rests on. A finding about the head
# commit puts the head sha in the key. A later push can then be reported again.
#
# ── The author alone is not the identity ─────────────────────────────────────
#
# One login is one account, and one person runs several sessions at once. So a
# claim carries a session word beside the login:
#
#   `<!-- pr-claim --> <login> <session>`
#
# A claim is this session's own only when both match. The word comes from
# `scripts/lib/claim-session.sh`, shared with the other two claim scripts.
#
# A finding takes no session word. A finding that stands is published. Who
# published it does not change what a second copy costs.
#
# ── What it cannot see ───────────────────────────────────────────────────────
#
# A claim is a signal somebody chose to leave. It is not a lock. Two sessions
# that check in the same second both proceed. It turns the common case, a peer
# who began ten minutes ago, from hidden into plain. That is all it offers.

set -uo pipefail

# `plain_word` and `resolve_session` — the same word the other two claim
# scripts use, so they cannot disagree about which session this is.
# shellcheck source=scripts/lib/claim-session.sh
. "$(dirname "$0")/lib/claim-session.sh"

# A decided verdict must survive a reader that closes the pipe early.
trap '' PIPE

marker="<!-- pr-claim -->"
window_minutes=90

mode=""
pr=""
finding=""
body=""
body_file=""
fixture_login=""
fixture_session=""
fixture_claims=""
fixture_claims_failed=0
fixture_findings=""
fixture_findings_failed=0
fixture_pr_state="OPEN"
fixture_pr_state_failed=0
select_now=""
use_fixture=0

while [ $# -gt 0 ]; do
  case "$1" in
  check | claim | post | select | findings)
    mode="$1"
    shift
    ;;
  --window-minutes)
    window_minutes="${2:-}"
    shift 2
    ;;
  --finding)
    finding="${2:-}"
    shift 2
    ;;
  --body)
    body="${2:-}"
    shift 2
    ;;
  --body-file)
    body_file="${2:-}"
    shift 2
    ;;
  # The clock the pure modes read, so a test can pin it.
  --now)
    select_now="${2:-}"
    shift 2
    ;;
  # Test-only, and paired: a fixture that supplied claims but read the real
  # tracker would compare two different worlds.
  --fixture-login)
    fixture_login="${2:-}"
    use_fixture=1
    shift 2
    ;;
  # This session's own word. An empty value is the run that has none, which is
  # the open side of the comparison.
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
  # Test-only: the comment read itself failing, as against answering with
  # nothing. The two must never report the same way.
  --fixture-claims-failed)
    fixture_claims_failed=1
    use_fixture=1
    shift
    ;;
  # Rows of `<login> <age-in-seconds>`, one per comment already carrying this
  # finding's marker.
  --fixture-findings)
    fixture_findings="${2:-}"
    use_fixture=1
    shift 2
    ;;
  --fixture-findings-failed)
    fixture_findings_failed=1
    use_fixture=1
    shift
    ;;
  # Test-only: the pull request's own state, as `gh pr view --json state`
  # reports it. Defaults to OPEN.
  --fixture-pr-state)
    fixture_pr_state="${2:-}"
    use_fixture=1
    shift 2
    ;;
  --fixture-pr-state-failed)
    fixture_pr_state_failed=1
    use_fixture=1
    shift
    ;;
  -h | --help)
    awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0"
    exit 0
    ;;
  *)
    if [ -z "$pr" ]; then
      pr="$1"
      shift
    else
      echo "pr-claim: unknown argument '$1'" >&2
      exit 2
    fi
    ;;
  esac
done

# Every claim comment in a `gh pr view --json comments` payload, as
# `<login> <session> <age-in-seconds>`. A `-` marks a claim with no session
# word. Pure: the payload comes in on stdin and `now` is an argument, so the
# tests below run the filter production runs.
#
# Only the first line is read, with CRLF stripped and runs of spaces collapsed.
# The marker takes three fields, so the session word is the fifth. Padding with
# `-` is what lets a two-word claim parse instead of erroring on a field that
# is not there.
#
# The login comes from `.author.login`, which GitHub stamps. A body can say
# anything.
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

# Every comment already carrying one finding's marker, as `<login> <age>`.
# The marker must open the comment, so a comment quoting one — this script's
# own report does — is not read as the finding itself.
select_findings() {
  jq -r --arg marker "$1" --argjson now "$2" '
    .comments[]
    | select(.body | startswith($marker))
    | "\(.author.login) \(($now - (.createdAt | fromdateiso8601)) | floor)"
  '
}

# A finding key rides into a marker and into a jq argument, so it takes a
# small alphabet. It also has to stay readable in a comment a person reads.
check_finding_key() {
  case "$finding" in
  "")
    echo "pr-claim: $mode needs --finding <key>" >&2
    exit 2
    ;;
  *[!A-Za-z0-9._:@/-]*)
    echo "pr-claim: --finding takes letters, digits and . _ : @ / - only" >&2
    exit 2
    ;;
  esac
  if [ "${#finding}" -gt 120 ]; then
    echo "pr-claim: --finding is capped at 120 characters" >&2
    exit 2
  fi
}

# The pure modes are the seam a test drives head on: real comments JSON goes
# in, the parsed rows come out. Production wires them to `gh pr view` below
# rather than writing these filters into that call. Break a filter and a test
# fails here, not a live sweep, silently, later.
if [ "$mode" = "select" ] || [ "$mode" = "findings" ]; then
  [ -n "$select_now" ] || {
    echo "pr-claim: $mode needs --now <unix-seconds>" >&2
    exit 2
  }
  if [ "$mode" = "select" ]; then
    select_claims "$select_now"
    exit $?
  fi
  check_finding_key
  select_findings "<!-- pr-finding: $finding -->" "$select_now"
  exit $?
fi

if [ -z "$mode" ] || [ -z "$pr" ]; then
  echo "pr-claim: usage: pr-claim.sh check|claim|post <pr-number>" >&2
  exit 2
fi
case "$pr" in '' | *[!0-9]*)
  echo "pr-claim: '<pr>' takes a whole number" >&2
  exit 2
  ;;
esac
case "$window_minutes" in '' | *[!0-9]*)
  echo "pr-claim: --window-minutes takes a whole number of minutes" >&2
  exit 2
  ;;
esac
window_seconds=$((window_minutes * 60))

# `proceed` is the only exit this script takes when it is unsure, so it is one
# function rather than a pair of lines that could drift apart.
proceed() {
  echo "$1" || true
  exit 0
}

if [ "$mode" = "post" ]; then
  check_finding_key
  if [ -n "$body_file" ]; then
    if [ -n "$body" ]; then
      echo "pr-claim: pass --body or --body-file, not both" >&2
      exit 2
    fi
    if ! body="$(cat "$body_file")"; then
      echo "pr-claim: could not read --body-file '$body_file'" >&2
      exit 2
    fi
  fi
  if [ -z "$body" ]; then
    echo "pr-claim: post needs --body <text> or --body-file <path>" >&2
    exit 2
  fi
fi

if [ "$use_fixture" -eq 0 ] && ! command -v gh >/dev/null 2>&1; then
  echo "note: gh is not installed, so this run could not ask whether #$pr is" >&2
  echo "      already being swept. Proceeding: a check that can block a sweep" >&2
  echo "      is worse than the duplication it stops." >&2
  proceed "ok  proceed (could not ask)"
fi

# One read of the comments, shared by both gates. `comments_ok` is whether the
# read answered at all. "nothing stands" is a claim about a list that was read;
# a read that failed to ask must never report in that shape.
comments_json=""
comments_ok=1
if [ "$use_fixture" -eq 0 ]; then
  if ! comments_json="$(CLICOLOR_FORCE=0 NO_COLOR=1 gh pr view "$pr" \
    --json comments 2>/dev/null)"; then
    comments_ok=0
  fi
fi

# ── post: has this finding already been published? ───────────────────────────
#
# This gate runs on its own. A sweep that means to publish asks this and
# nothing else, because a finding that already stands is a duplicate whoever
# holds the pull request.
if [ "$mode" = "post" ]; then
  hits=""
  hits_ok=1
  if [ "$use_fixture" -eq 1 ]; then
    if [ "$fixture_findings_failed" -eq 1 ]; then
      hits_ok=0
    else
      hits="$fixture_findings"
    fi
  elif [ "$comments_ok" -eq 0 ]; then
    hits_ok=0
  elif ! hits="$(printf '%s' "$comments_json" \
    | select_findings "<!-- pr-finding: $finding -->" "$(date -u +%s)")"; then
    hits_ok=0
  fi

  first_by=""
  first_age=""
  while read -r who age; do
    [ -n "$who" ] || continue
    case "$age" in
    '' | *[!0-9]*) continue ;;
    esac
    if [ -z "$first_age" ] || [ "$age" -gt "$first_age" ]; then
      first_by="$who"
      first_age="$age"
    fi
  done <<EOF
$hits
EOF

  if [ -n "$first_by" ]; then
    minutes=$((first_age / 60))
    echo "STAND DOWN  '$finding' is already posted on #$pr." >&2
    echo "" >&2
    echo "     posted by @$first_by, ${minutes}m ago" >&2
    echo "" >&2
    echo "     Three sweeps once posted one conflict analysis to one pull" >&2
    echo "     request in eight minutes. Each was right. The maintainer still" >&2
    echo "     had to read all three to learn they said one thing." >&2
    echo "" >&2
    echo "     Read the comment that stands. If yours adds something it does" >&2
    echo "     not have, post that part under a key of its own." >&2
    exit 1
  fi

  if [ "$hits_ok" -eq 0 ]; then
    echo "note: could not read #$pr's comments, so this run cannot tell whether" >&2
    echo "      '$finding' is already posted. Proceeding (fail-open) — read the" >&2
    echo "      pull request before you trust this." >&2
  fi

  if [ "$use_fixture" -eq 1 ]; then
    if [ "$hits_ok" -eq 0 ]; then
      proceed "ok  post '$finding' (duplicate check unreadable) (fixture: nothing was posted)"
    fi
    proceed "ok  post '$finding' on #$pr — nothing carries that key (fixture: nothing was posted)"
  fi

  if ! gh pr comment "$pr" --body "<!-- pr-finding: $finding -->
$body" >/dev/null 2>&1; then
    echo "pr-claim: could not post on #$pr" >&2
    exit 3
  fi
  if [ "$hits_ok" -eq 0 ]; then
    proceed "ok  posted '$finding' on #$pr (duplicate check unreadable)"
  fi
  proceed "ok  posted '$finding' on #$pr — nothing carried that key"
fi

# ── A merged or closed pull request is not work to sweep ─────────────────────
#
# The stronger signal, and the one a claim can never carry: the pull request is
# done. `state_ok` is whether the read answered.
pr_state=""
state_ok=1
if [ "$use_fixture" -eq 1 ]; then
  if [ "$fixture_pr_state_failed" -eq 1 ]; then
    state_ok=0
  else
    pr_state="$fixture_pr_state"
  fi
elif ! pr_state="$(CLICOLOR_FORCE=0 NO_COLOR=1 gh pr view "$pr" \
  --json state --jq .state 2>/dev/null)"; then
  state_ok=0
fi

if [ "$state_ok" -eq 0 ]; then
  echo "note: could not read #$pr's state, so this run cannot tell an open" >&2
  echo "      pull request from a merged one. Proceeding (fail-open); the" >&2
  echo "      claim check below still runs." >&2
elif [ "$pr_state" = "MERGED" ] || [ "$pr_state" = "CLOSED" ]; then
  echo "STAND DOWN  #$pr is $pr_state." >&2
  echo "" >&2
  echo "     A merged pull request is finished, and a closed one was dropped." >&2
  echo "     Neither takes a comment. If the work still matters, it belongs on" >&2
  echo "     the issue or on a new pull request." >&2
  exit 1
fi

if [ "$use_fixture" -eq 1 ]; then
  me="$fixture_login"
elif ! me="$(gh api user --jq .login 2>/dev/null)"; then
  me=""
fi
if [ -z "$me" ]; then
  echo "note: could not read this session's login, so a claim on #$pr cannot" >&2
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

claims=""
claims_ok=1
if [ "$use_fixture" -eq 1 ]; then
  if [ "$fixture_claims_failed" -eq 1 ]; then
    claims_ok=0
  else
    claims="$fixture_claims"
  fi
elif [ "$comments_ok" -eq 0 ]; then
  claims_ok=0
elif ! claims="$(printf '%s' "$comments_json" | select_claims "$(date -u +%s)")"; then
  claims_ok=0
fi
if [ "$claims_ok" -eq 0 ]; then
  echo "note: could not read #$pr's comments. Proceeding (fail-open)." >&2
  proceed "ok  proceed (comments unreadable)"
fi

# The freshest claim this session cannot account for. Its own is no reason to
# stand it down: re-running the pre-flight is what a session does when it comes
# back to work it started. Same login and same word is its own. Same login with
# either word missing is unprovable, and an unknown proceeds.
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
  echo "STAND DOWN  #$pr is already being swept." >&2
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
  echo "     What to do:" >&2
  echo "       - Pick another pull request. The claim lapses by itself after" >&2
  echo "         ${window_minutes}m, so a session that died cannot hold this shut." >&2
  echo "       - Working it anyway? Say so on #$pr, so the next session reads a" >&2
  echo "         reason rather than a collision." >&2
  exit 1
fi

if [ "$mode" = "check" ]; then
  if [ "$state_ok" -eq 0 ]; then
    proceed "ok  proceed (state unreadable) — no live claim holds #$pr."
  fi
  proceed "ok  #$pr is unswept — it is open, and no live claim holds it."
fi

# The session word rides on the marker line, where the check parses it. A run
# without one posts the two-word body, which every reader still accepts.
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
Sweeping this. The claim lapses after ${window_minutes} minutes, so it cannot
hold the pull request shut if this session dies
(\`scripts/pr-claim.sh\`, \`#5861\`).${session_note}"

if [ "$use_fixture" -eq 1 ]; then
  proceed "ok  claimed #$pr as @$me (fixture: nothing was posted)"
fi

if ! gh pr comment "$pr" --body "$body" >/dev/null 2>&1; then
  echo "note: could not post the claim on #$pr. Proceeding anyway: the claim is" >&2
  echo "      an aid, and the work is not." >&2
  proceed "ok  proceed (claim not posted)"
fi

echo "ok  claimed #$pr as @$me" || true
