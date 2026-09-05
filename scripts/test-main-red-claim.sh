#!/usr/bin/env bash
#
# Hermetic tests for scripts/main-red-claim.sh.
#
#   ./scripts/test-main-red-claim.sh    (or: make main-red-claim-test)
#
# No network and no `gh`: every case drives the fixture seams, so the suite is
# the same on a bare runner as on a dev box with a live tracker.
#
# The cases that matter most are the two controls, one on each side:
#
#   - the **positive** control — a fresh claim by somebody else must actually
#     stand a session down. A pre-flight that cannot be shown to block is a
#     comment, not a guard, and this one spends its life proceeding, because
#     main is usually green.
#   - the **negative** controls — every unknown must proceed. That is the
#     whole safety argument (a claim check that can block a repair is worse
#     than the duplication it prevents), and it is one branch per unknown, so
#     it is one case per branch.
#
# Not a `make gate` step, matching `main-red-hold-test`: the
# subject asks the issue tracker a question, and the gate is hermetic and
# offline by contract.
#
# bash 3.2 compatible.
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/main-red-claim.sh"

pass=0
fail=0

ok() { printf '  \033[32m✓\033[0m %s\n' "$*"; pass=$((pass + 1)); }
bad() { printf '  \033[31m✗\033[0m %s\n' "$*"; fail=$((fail + 1)); }

# One assertion shape: run with every seam pinned and compare the exit code.
# An expected block must also name its reason, because "exit 1" is satisfied
# by a typo in the script just as well as by the case's own subject — the
# discipline test-main-red-hold.sh applies to its blocking branch.
want() { # want <name> <expect-proceed|expect-block> <want-substring> <mode> <issues> <login> <claims> [session]
  local name="$1" expect="$2" want_text="$3" mode="$4" issues="$5" login="$6" claims="$7"
  local session="${8-s-self}"
  local out rc
  out="$("$SCRIPT" "$mode" \
    --fixture-open-issues "$issues" \
    --fixture-login "$login" \
    --fixture-session "$session" \
    --fixture-claims "$claims" 2>&1)"
  rc=$?
  if [ "$expect" = "expect-proceed" ] && [ "$rc" -ne 0 ]; then
    bad "$name — expected proceed, got exit $rc: $out"
    return
  fi
  if [ "$expect" = "expect-block" ] && [ "$rc" -eq 0 ]; then
    bad "$name — expected STAND DOWN, but it proceeded: $out"
    return
  fi
  case "$out" in
  *"$want_text"*) ok "$name" ;;
  *) bad "$name — exit code was right but the reason was not; wanted '$want_text', got: $out" ;;
  esac
}

echo "main-red-claim:"

# The incident. Three sessions, one break: the second and third must stand down.
want "a fresh claim by somebody else stands this session down" \
  expect-block "claimed by @grace" check "4671" "ada" "grace s1 300"

want "and it names the window it judged against" \
  expect-block "window: 20m" check "4671" "ada" "grace s1 300"

# The lapse. A session that died mid-repair must not hold the repair shut.
want "a claim past the window has lapsed" \
  expect-proceed "unclaimed" check "4671" "ada" "grace s1 1500"

# The window is a knob, so a case has to move it: 5m old is claimed at the
# default and lapsed at a one-minute window.
out="$("$SCRIPT" check --window-minutes 1 \
  --fixture-open-issues 4671 --fixture-login ada \
  --fixture-session s-self --fixture-claims "grace s1 300" 2>&1)"
rc=$?
if [ "$rc" -eq 0 ]; then
  ok "a shorter window lapses a claim the default still honours"
else
  bad "--window-minutes did not shorten the window: exit $rc: $out"
fi

# A session's own claim is not a reason to stand it down: re-running the
# pre-flight is what a session does when it returns to a repair it started.
want "this session's own claim does not block it" \
  expect-proceed "unclaimed" check "4671" "ada" "ada s-self 60"

want "the freshest OTHER claim decides, not the freshest claim" \
  expect-block "claimed by @grace" check "4671" "ada" "ada s-self 10
grace s1 300"

# One person runs several agent sessions, so the login cannot answer "did I
# write this?". The second session must stand down naming the first.
want "a second session of the same author stands this one down" \
  expect-block "claimed by @ada (session s1)" check "4671" "ada" "ada s1 300"

want "and it says the collision is with a session of your own" \
  expect-block "another of your own sessions" check "4671" "ada" "ada s1 300"

want "the freshest claim decides across two sessions of one author" \
  expect-block "session s2" check "4671" "ada" "ada s1 900
ada s2 120"

# The unknowns on both sides of the comparison. Each proceeds, because a claim
# check that can block a repair is worse than the duplication it prevents.
want "a run with no session word of its own proceeds on its own login" \
  expect-proceed "unclaimed" check "4671" "ada" "ada s1 300" ""

want "and says why it could not tell them apart" \
  expect-proceed "no session word" check "4671" "ada" "ada s1 300" ""

want "a claim with no session word does not block its own author" \
  expect-proceed "unclaimed" check "4671" "ada" "ada - 300"

# ...but it is still somebody else's claim when the login differs, and the
# stand-down says which half it could not read.
want "a claim with no session word still blocks a different author" \
  expect-block "session unknown" check "4671" "ada" "grace - 300"

# Every unknown proceeds. One case per branch, because each is a separate
# early return and a regression in one is invisible from the others.
want "no open main-red issue proceeds" \
  expect-proceed "not known-broken" check "" "ada" ""

want "two open main-red issues are ambiguous, so it proceeds" \
  expect-proceed "ambiguous" check "4671 4672" "ada" "grace s1 60"

want "an unreadable identity proceeds" \
  expect-proceed "identity unknown" check "4671" "" "grace s1 60"

want "an unparseable claim age is ignored rather than trusted" \
  expect-proceed "unclaimed" check "4671" "ada" "grace s1 soon"

want "no claims at all proceeds" \
  expect-proceed "unclaimed" check "4671" "ada" ""

# `claim` is `check` plus a post, so it must inherit the block.
want "claim stands down on somebody else's fresh claim" \
  expect-block "claimed by @grace" claim "4671" "ada" "grace s1 300"

want "claim stands down on another session of the same author" \
  expect-block "claimed by @ada" claim "4671" "ada" "ada s1 300"

want "claim takes an unclaimed issue" \
  expect-proceed "claimed #4671 as @ada" claim "4671" "ada" ""

# A `gh` that is not installed is the one unknown the fixtures cannot pin,
# because supplying a fixture is what bypasses the lookup. A PATH of nothing
# is not the way to drive it — that breaks the shebang and exits 127, which
# is a broken test rather than a missing `gh`. Instead: a PATH holding every
# program this script does use, and not `gh`.
gh_less="$(mktemp -d)"
trap 'rm -rf "$gh_less"' EXIT
for tool in bash awk tr mktemp; do
  tool_path="$(command -v "$tool")"
  if [ -z "$tool_path" ]; then
    bad "the suite needs $tool on PATH to build its gh-less fixture"
    continue
  fi
  ln -s "$tool_path" "$gh_less/$tool"
done
out="$(PATH="$gh_less" "$SCRIPT" check 2>&1)"
rc=$?
if [ "$rc" -eq 0 ]; then
  case "$out" in
  *"could not ask"*) ok "a missing gh proceeds" ;;
  *) bad "a missing gh proceeded for the wrong reason: $out" ;;
  esac
else
  bad "a missing gh must proceed, got exit $rc: $out"
fi

# Bad input is a caller error, distinct from both verdicts: exit 2.
if "$SCRIPT" check --window-minutes soon >/dev/null 2>&1; then
  bad "a non-numeric window should exit 2, not proceed"
else
  rc=$?
  if [ "$rc" -eq 2 ]; then
    ok "a non-numeric window is a usage error, not a verdict"
  else
    bad "a non-numeric window should exit 2, got $rc"
  fi
fi

if "$SCRIPT" --nonsense >/dev/null 2>&1; then
  bad "an unknown argument should exit 2, not proceed"
else
  rc=$?
  if [ "$rc" -eq 2 ]; then
    ok "an unknown argument is a usage error"
  else
    bad "an unknown argument should exit 2, got $rc"
  fi
fi

# The fixtures above pin the session word, so the thing that produces one for
# real is covered here, in throwaway clones. `session` is the seam: it reads
# and mints exactly what `check` and `claim` do, and asks the tracker nothing.
if ! command -v git >/dev/null 2>&1; then
  bad "the suite needs git on PATH to cover the session word"
else
  clone_a="$(mktemp -d)"
  clone_b="$(mktemp -d)"
  no_repo="$(mktemp -d)"
  trap 'rm -rf "$gh_less" "$clone_a" "$clone_b" "$no_repo"' EXIT
  git -C "$clone_a" init -q
  git -C "$clone_b" init -q

  word_a="$(cd "$clone_a" && STELLA_CLAIM_SESSION="" "$SCRIPT" session 2>/dev/null)"
  again_a="$(cd "$clone_a" && STELLA_CLAIM_SESSION="" "$SCRIPT" session 2>/dev/null)"
  word_b="$(cd "$clone_b" && STELLA_CLAIM_SESSION="" "$SCRIPT" session 2>/dev/null)"

  if [ -n "$word_a" ] && [ "$word_a" = "$again_a" ]; then
    ok "one clone reads back the word it minted"
  else
    bad "a clone's session word did not survive a second run: '$word_a' then '$again_a'"
  fi

  if [ -n "$word_b" ] && [ "$word_a" != "$word_b" ]; then
    ok "two clones mint two different words"
  else
    bad "two clones shared a session word: '$word_a' and '$word_b'"
  fi

  # The token is state, not content: it must stay inside the git dir, where no
  # commit can pick it up.
  if [ -f "$clone_a/.git/main-red-claim-session" ] &&
    [ -z "$(git -C "$clone_a" status --porcelain)" ]; then
    ok "the word is kept in the git dir, so the work tree stays clean"
  else
    bad "minting a session word dirtied the work tree"
  fi

  out="$(cd "$clone_a" && STELLA_CLAIM_SESSION="fleet-7" "$SCRIPT" session 2>/dev/null)"
  if [ "$out" = "fleet-7" ]; then
    ok "STELLA_CLAIM_SESSION wins over the clone's own word"
  else
    bad "STELLA_CLAIM_SESSION was ignored: got '$out'"
  fi

  # A value that is not one word would split a claim line into a column the
  # parse does not have, so it is refused and the clone's own word is used.
  out="$(cd "$clone_a" && STELLA_CLAIM_SESSION="two words" "$SCRIPT" session 2>&1)"
  case "$out" in
  *"not one plain word"*"$word_a") ok "a session word with a space is refused" ;;
  *) bad "a session word with a space was not refused: $out" ;;
  esac

  # Nowhere near a clone: an unwalkable ceiling, so the answer cannot depend on
  # where the runner's temp directory happens to sit.
  out="$(cd "$no_repo" && GIT_CEILING_DIRECTORIES="$no_repo" \
    STELLA_CLAIM_SESSION="" "$SCRIPT" session 2>&1)"
  rc=$?
  if [ "$rc" -ne 0 ]; then
    case "$out" in
    *"no session word"*) ok "outside a clone there is no word to print" ;;
    *) bad "outside a clone it failed for the wrong reason: $out" ;;
    esac
  else
    bad "outside a clone it printed a word anyway: $out"
  fi
fi

# --help prints the whole header, however long it grows.
out="$("$SCRIPT" --help 2>&1)"
case "$out" in
*"Fail-open at every unknown"*) ok "--help reaches the end of the header" ;;
*) bad "--help truncated the header" ;;
esac

# ── The parse itself ──────────────────────────────────────────────────────
#
# Every case above drives `--fixture-claims`: already-parsed
# `<login> <session> <age>` rows, never the jq filter that makes them. These
# drive `select` instead — real `gh issue view --json comments` JSON, on
# stdin, through the real filter. A typo in it (a dropped `.author.login`, a
# broken session-word split) now fails one of these cases, not nothing.
NOW_SELECT=2000000000

# iso <unix-seconds> — jq's own conversion, not `date`. The filter under test
# uses jq for this arithmetic because `date -d` and `date -j` disagree across
# platforms. Building the fixture with `date` would make the test itself
# depend on the platform too.
iso() { jq -n --argjson t "$1" '$t | todateiso8601'; }

# comment <login> <first-line> <created-unix> — one comment object. The body
# is JSON-string-encoded by jq (`-Rs`) so a first line carrying a literal `\r`
# round-trips as one.
comment() {
  local login="$1" first_line="$2" created
  created="$(iso "$3")"
  printf '{"author":{"login":"%s"},"body":%s,"createdAt":%s}' \
    "$login" "$(printf '%s\nRepairing this.' "$first_line" | jq -Rs .)" "$created"
}

# want_select <name> <expect-line> <json>.
want_select() {
  local name="$1" expect="$2" json="$3" out rc
  out="$(printf '%s' "$json" | "$SCRIPT" select --now "$NOW_SELECT" 2>/dev/null)"
  rc=$?
  if [ "$out" = "$expect" ] && [ "$rc" -eq 0 ]; then
    ok "$name"
  else
    bad "$name — wanted '$expect' (exit 0), got '$out' (exit $rc)"
  fi
}

want_select "a live claim with a session word parses as <login> <session> <age>" \
  "grace s1 300" \
  "{\"comments\":[$(comment grace "main-red-claim: grace s1" $((NOW_SELECT - 300)))]}"

want_select "a claim with no session word parses with a '-' in that column" \
  "grace - 300" \
  "{\"comments\":[$(comment grace "main-red-claim: grace" $((NOW_SELECT - 300)))]}"

# A lapsed claim still parses here. `check` judges the window later, not
# `select`. The age still comes out right, even for a stale claim.
want_select "a lapsed claim still parses; the window is judged downstream" \
  "grace s1 5000" \
  "{\"comments\":[$(comment grace "main-red-claim: grace s1" $((NOW_SELECT - 5000)))]}"

out="$(printf '{"comments":[{"author":{"login":"grace"},"body":"just fixed a typo, unrelated","createdAt":%s}]}' \
  "$(iso $((NOW_SELECT - 10)))" | "$SCRIPT" select --now "$NOW_SELECT" 2>/dev/null)"
rc=$?
if [ -z "$out" ] && [ "$rc" -eq 0 ]; then
  ok "a comment that is not a claim produces no row"
else
  bad "a non-claim comment should produce no row, got '$out' (exit $rc)"
fi

# A marker line with runs of extra spaces (an older copy of this script, or a
# body a person hand-edited) must not shift the session word into the wrong
# column or leave stray empty fields in it.
want_select "a marker line with extra spaces still parses" \
  "grace s1 40" \
  "{\"comments\":[$(comment grace "main-red-claim:  grace   s1" $((NOW_SELECT - 40)))]}"

# A body whose first line arrived with a carriage return (Windows-authored, or
# an API that round-trips CRLF) must not leave the `\r` glued onto the session
# word, which would make it compare unequal to every other claim's.
crlf_json="$(printf '{"comments":[{"author":{"login":"grace"},"body":%s,"createdAt":%s}]}' \
  "$(printf 'main-red-claim: grace s1\r\nRepairing this.' | jq -Rs .)" "$(iso $((NOW_SELECT - 40)))")"
want_select "a CRLF first line still parses cleanly" "grace s1 40" "$crlf_json"

# The witness: break the filter above (drop the `.author.login` field, or
# turn `\$word[2]` into a literal `"-"`) and re-run `make main-red-claim-test`.
# Every `want_select` case that checks a login or a session word turns red.
# Fix it back, and the suite turns green again. This PR's description shows
# both runs.

# A malformed timestamp must fail the parse rather than silently emit a wrong
# age — the shape the tracker could return if a comment were ever
# hand-crafted or corrupted. Production treats a failed `select` as "comments
# unreadable" and proceeds (fail-open), so this failing closed is what keeps
# that path from reporting a wrong number.
malformed_json='{"comments":[{"author":{"login":"grace"},"body":"main-red-claim: grace s1","createdAt":"not-a-date"}]}'
out="$(printf '%s' "$malformed_json" | "$SCRIPT" select --now "$NOW_SELECT" 2>/dev/null)"
rc=$?
if [ "$rc" -ne 0 ] && [ -z "$out" ]; then
  ok "a malformed timestamp fails the parse rather than emitting a wrong age"
else
  bad "a malformed timestamp should fail closed, got exit $rc: '$out'"
fi

echo ""
echo "  $pass passed, $fail failed"
[ "$fail" -eq 0 ]
