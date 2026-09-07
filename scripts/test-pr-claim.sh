#!/usr/bin/env bash
#
# Hermetic tests for `scripts/pr-claim.sh` (`#5861`).
#
#   `./scripts/test-pr-claim.sh`    (or: `make pr-claim-test`)
#
# No network and no `gh`. Every case drives a fixture seam. The suite reads the
# same on a bare runner as on a box with a live tracker.
#
# The two cases the issue asks for come first, one on each side. The spare
# comment is stopped: two sweeps that find one thing post one comment. A gate
# that cannot be shown to block is a comment, not a gate, and this one spends
# its life letting sweeps through. The unknown proceeds, and says so: a run
# that could not read the comments must post, and must not report that nothing
# stands. Those are two sentences and only one of them is true.
#
# Not a `make gate` step, matching the other two claim suites. The subject asks
# a tracker a question. The gate is offline by contract.
#
# bash 3.2 compatible.
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/pr-claim.sh"

# Named first, so a missing subject reads as a missing subject. Without this
# every case below fails on exit 127, and a case that only asks for a non-zero
# exit would pass on nothing at all.
if [ ! -x "$SCRIPT" ]; then
  printf '  \033[31m✗\033[0m %s\n' "$SCRIPT is missing or not executable"
  exit 1
fi

pass=0
fail=0

ok() {
  printf '  \033[32m✓\033[0m %s\n' "$*"
  pass=$((pass + 1))
}
bad() {
  printf '  \033[31m✗\033[0m %s\n' "$*"
  fail=$((fail + 1))
}

# One shape for the whole suite. Pin every seam, then compare the exit code
# and the reason. An expected block names its reason too. A typo can satisfy
# "exit 1" just as well as the case can.
want() { # want <name> <expect-proceed|expect-block> <want-text> <args...>
  local name="$1" expect="$2" want_text="$3"
  shift 3
  local out rc
  out="$("$SCRIPT" "$@" 2>&1)"
  rc=$?
  if [ "$expect" = "expect-proceed" ] && [ "$rc" -ne 0 ]; then
    bad "$name — expected proceed, got exit $rc: $out"
    return
  fi
  if [ "$expect" = "expect-block" ] && [ "$rc" -eq 0 ]; then
    bad "$name — expected a stand-down, got exit 0: $out"
    return
  fi
  case "$out" in
  *"$want_text"*) ok "$name" ;;
  *) bad "$name — right exit, wrong reason (wanted '$want_text'): $out" ;;
  esac
}

echo "pr-claim"

# ── The finding gate ────────────────────────────────────────────────────────
#
# At most one comment per finding. Three sweeps posted one conflict analysis to
# one pull request in eight minutes. The second and third must stop here.

want "a finding already posted stops a second copy" \
  expect-block "is already posted on" \
  post 5835 --finding conflict:5828 --body "the same analysis" \
  --fixture-login ada --fixture-findings "grace 300"

want "...and the report names who posted it and when" \
  expect-block "posted by @grace, 5m ago" \
  post 5835 --finding conflict:5828 --body "the same analysis" \
  --fixture-login ada --fixture-findings "grace 300"

# The other way round, and the one that must not break. A sweep with something
# new to say still posts.
want "a finding nobody has posted goes through" \
  expect-proceed "nothing carries that key" \
  post 5835 --finding conflict:5828 --body "the analysis" \
  --fixture-login ada --fixture-findings ""

# A different key is a different finding. The gate keys on what the caller
# named. One sweep's second finding is not held back by its first.
want "a comment carrying another key does not block this one" \
  expect-proceed "nothing carries that key" \
  post 5835 --finding stale-golden --body "a second finding" \
  --fixture-login ada --fixture-findings ""

# Age is not a window here. A finding that stands is published. A copy of it a
# week later costs a reader the same diff. A sweep that means to report again
# after a push puts the head sha in the key.
want "an old finding still stops a copy — a published comment does not lapse" \
  expect-block "is already posted on" \
  post 5835 --finding conflict:5828 --body "the same analysis" \
  --fixture-login ada --fixture-findings "grace 999999"

# ── The unknown proceeds ────────────────────────────────────────────────────
#
# A read that failed is not an empty list. The wrong report here is the one
# that says nothing stands. A sweep would believe it.
out="$("$SCRIPT" post 5835 --finding conflict:5828 --body "the analysis" \
  --fixture-login ada --fixture-findings-failed 2>&1)"
rc=$?
case "$rc,$out" in
0,*"nothing carries that key"*)
  bad "a failed comment read claimed to have read the comments: $out"
  ;;
0,*"duplicate check unreadable"*)
  ok "a failed comment read posts anyway, and says it could not check"
  ;;
0,*)
  bad "a failed comment read proceeded with unexpected wording: $out"
  ;;
*)
  bad "a failed comment read blocked — expected exit 0, got $rc: $out"
  ;;
esac

# ── The claim gate ──────────────────────────────────────────────────────────

want "a fresh claim by somebody else stands this sweep down" \
  expect-block "claimed by @grace" \
  check 5835 --fixture-login ada --fixture-claims "grace - 300"

want "an unclaimed open pull request proceeds" \
  expect-proceed "is unswept" \
  check 5835 --fixture-login ada --fixture-claims ""

want "a claim of this session's own does not stand it down" \
  expect-proceed "is unswept" \
  check 5835 --fixture-login ada --fixture-claims "ada - 300"

want "a claim past the window has lapsed" \
  expect-proceed "is unswept" \
  check 5835 --fixture-login ada --fixture-claims "grace - 999999"

want "--window-minutes reaches the comparison" \
  expect-proceed "is unswept" \
  check 5835 --window-minutes 1 --fixture-login ada --fixture-claims "grace - 300"

want "a claim with an unreadable age is ignored" \
  expect-proceed "is unswept" \
  check 5835 --fixture-login ada --fixture-claims "grace - notanumber"

want "an unknown identity proceeds" \
  expect-proceed "identity unknown" \
  check 5835 --fixture-login "" --fixture-claims "grace - 300"

want "an unreadable comment list proceeds" \
  expect-proceed "comments unreadable" \
  check 5835 --fixture-login ada --fixture-claims-failed

want "claim posts when the pull request is free" \
  expect-proceed "ok  claimed" \
  claim 5835 --fixture-login ada --fixture-claims ""

want "claim stands down rather than posting over a peer" \
  expect-block "claimed by @grace" \
  claim 5835 --fixture-login ada --fixture-claims "grace - 300"

# ── A merged or closed pull request is done ─────────────────────────────────

want "a merged pull request stands this sweep down" \
  expect-block "is MERGED" \
  check 5835 --fixture-login ada --fixture-claims "" --fixture-pr-state MERGED

want "so does a closed one" \
  expect-block "is CLOSED" \
  check 5835 --fixture-login ada --fixture-claims "" --fixture-pr-state CLOSED

# An unreadable state proceeds, and the claim check still runs after it. A
# peer's live claim must block even when the state would not read.
out="$("$SCRIPT" check 5835 --fixture-login ada --fixture-claims "grace - 300" \
  --fixture-pr-state-failed 2>&1)"
rc=$?
case "$rc,$out" in
1,*"state, so this run cannot tell an open"*"claimed by @grace"*)
  ok "an unreadable state proceeds to the claim check, which still blocks"
  ;;
*)
  bad "expected the state note and then a stand-down on @grace, got exit $rc: $out"
  ;;
esac

want "an unreadable state proceeds when nothing else blocks" \
  expect-proceed "state unreadable" \
  check 5835 --fixture-login ada --fixture-claims "" --fixture-pr-state-failed

# ── Two sessions of one author ──────────────────────────────────────────────
#
# The case a login alone cannot answer. A fleet, or several agent worktrees on
# one machine, all run as one login.

want "a second session of the same author is stood down" \
  expect-block "claimed by @ada (session s1)" \
  check 5835 --fixture-login ada --fixture-session s2 --fixture-claims "ada s1 300"

want "this session's own claim still does not stand it down" \
  expect-proceed "is unswept" \
  check 5835 --fixture-login ada --fixture-session s1 --fixture-claims "ada s1 300"

want "a claim with no session word falls back to the author-only rule" \
  expect-proceed "is unswept" \
  check 5835 --fixture-login ada --fixture-session s1 --fixture-claims "ada - 300"

want "a run with no session word of its own proceeds on its own login" \
  expect-proceed "no session word" \
  check 5835 --fixture-login ada --fixture-session "" --fixture-claims "ada s1 300"

want "another author's claim still blocks a run with no word" \
  expect-block "claimed by @grace" \
  check 5835 --fixture-login ada --fixture-session "" --fixture-claims "grace s1 300"

# ── Argument handling ───────────────────────────────────────────────────────

out="$("$SCRIPT" check 2>&1)"
if [ $? -eq 2 ] && [ -z "${out##*usage*}" ]; then
  ok "a missing pull request number is a usage error, not a silent proceed"
else
  bad "a missing pull request number did not error: $out"
fi

out="$("$SCRIPT" check abc 2>&1)"
if [ $? -eq 2 ]; then
  ok "a non-numeric pull request number is refused"
else
  bad "a non-numeric pull request number was accepted: $out"
fi

# A post with no key would key on nothing. Every finding would then look like
# every other one. That has to be an error, not a default.
out="$("$SCRIPT" post 5835 --body x --fixture-login ada --fixture-findings "" 2>&1)"
if [ $? -eq 2 ] && [ -z "${out##*needs --finding*}" ]; then
  ok "a post with no finding key is refused"
else
  bad "a post with no finding key was accepted: $out"
fi

out="$("$SCRIPT" post 5835 --finding 'a key with spaces' --body x \
  --fixture-login ada --fixture-findings "" 2>&1)"
if [ $? -eq 2 ]; then
  ok "a finding key outside the alphabet is refused"
else
  bad "a finding key outside the alphabet was accepted: $out"
fi

out="$("$SCRIPT" post 5835 --finding k --fixture-login ada --fixture-findings "" 2>&1)"
if [ $? -eq 2 ] && [ -z "${out##*needs --body*}" ]; then
  ok "a post with no body is refused"
else
  bad "a post with no body was accepted: $out"
fi

# `--body-file` is the shape a sweep uses. The write-up is long, and a long
# write-up does not belong on a command line.
tmp_body="$(mktemp)"
trap 'rm -f "$tmp_body"' EXIT
printf 'the analysis\n' >"$tmp_body"
want "--body-file is read" \
  expect-proceed "nothing carries that key" \
  post 5835 --finding conflict:5828 --body-file "$tmp_body" \
  --fixture-login ada --fixture-findings ""

# ── The parse itself ────────────────────────────────────────────────────────
#
# Every case above drives a fixture. Those rows are already parsed, so the jq
# filters that make them run in none of it. These cases drive the pure modes:
# real `gh pr view --json comments` JSON, on stdin, through the real filters.
# A typo in one now fails a case here, not a live sweep, later.
NOW_SELECT=2000000000

iso() { jq -n --argjson t "$1" '$t | todateiso8601'; }

# comment <login> <body> <created-unix> — one comment object.
comment() {
  local login="$1" body="$2" created
  created="$(iso "$3")"
  printf '{"author":{"login":"%s"},"body":%s,"createdAt":%s}' \
    "$login" "$(printf '%s' "$body" | jq -Rs .)" "$created"
}

# want_select <name> <expect-line> <json> [mode-args...]
want_select() {
  local name="$1" expect="$2" json="$3"
  shift 3
  local out rc
  out="$(printf '%s' "$json" | "$SCRIPT" "$@" --now "$NOW_SELECT" 2>/dev/null)"
  rc=$?
  if [ "$out" = "$expect" ] && [ "$rc" -eq 0 ]; then
    ok "$name"
  else
    bad "$name — wanted '$expect' (exit 0), got '$out' (exit $rc)"
  fi
}

want_select "a live claim parses as <login> <session> <age>" \
  "grace s7 300" \
  "{\"comments\":[$(comment grace "<!-- pr-claim --> grace s7" $((NOW_SELECT - 300)))]}" \
  select

want_select "a claim with no session word parses with a '-' in that column" \
  "grace - 300" \
  "{\"comments\":[$(comment grace "<!-- pr-claim --> grace" $((NOW_SELECT - 300)))]}" \
  select

want_select "a marker line with extra spaces still parses" \
  "grace s7 300" \
  "{\"comments\":[$(comment grace "<!-- pr-claim -->   grace   s7" $((NOW_SELECT - 300)))]}" \
  select

want_select "a CRLF first line still parses cleanly" \
  "grace s7 300" \
  "{\"comments\":[$(comment grace "$(printf '<!-- pr-claim --> grace s7\r\nmore')" $((NOW_SELECT - 300)))]}" \
  select

want_select "a finding comment parses as <login> <age>" \
  "grace 300" \
  "{\"comments\":[$(comment grace "$(printf '<!-- pr-finding: conflict:5828 -->\nthe analysis')" $((NOW_SELECT - 300)))]}" \
  findings --finding conflict:5828

# The marker has to open the comment. A reply that quotes one is talking about
# the finding, not publishing it. It must not stand the next sweep down.
out="$(printf '{"comments":[%s]}' \
  "$(comment grace "$(printf 'I read the\n<!-- pr-finding: conflict:5828 -->\ncomment')" $((NOW_SELECT - 10)))" \
  | "$SCRIPT" findings --finding conflict:5828 --now "$NOW_SELECT" 2>/dev/null)"
rc=$?
if [ -z "$out" ] && [ "$rc" -eq 0 ]; then
  ok "a comment that only quotes a marker is not the finding"
else
  bad "a quoted marker should produce no row, got '$out' (exit $rc)"
fi

out="$(printf '{"comments":[%s]}' \
  "$(comment grace "<!-- pr-finding: other-key -->" $((NOW_SELECT - 10)))" \
  | "$SCRIPT" findings --finding conflict:5828 --now "$NOW_SELECT" 2>/dev/null)"
rc=$?
if [ -z "$out" ] && [ "$rc" -eq 0 ]; then
  ok "another key's finding produces no row"
else
  bad "another key should produce no row, got '$out' (exit $rc)"
fi

out="$(printf '{"comments":[%s]}' \
  "$(comment grace "just an unrelated comment" $((NOW_SELECT - 10)))" \
  | "$SCRIPT" select --now "$NOW_SELECT" 2>/dev/null)"
rc=$?
if [ -z "$out" ] && [ "$rc" -eq 0 ]; then
  ok "a comment that is not a claim produces no row"
else
  bad "a non-claim comment should produce no row, got '$out' (exit $rc)"
fi

# A bad timestamp must fail the parse, not emit a wrong age. Production reads
# a failed parse as "comments unreadable" and proceeds. So failing closed here
# is what stops that path reporting a made-up number.
malformed='{"comments":[{"author":{"login":"grace"},"body":"<!-- pr-claim --> grace","createdAt":"not-a-date"}]}'
out="$(printf '%s' "$malformed" | "$SCRIPT" select --now "$NOW_SELECT" 2>/dev/null)"
rc=$?
if [ "$rc" -ne 0 ] && [ -z "$out" ]; then
  ok "a bad timestamp fails the parse rather than emitting a wrong age"
else
  bad "a bad timestamp should fail closed, got exit $rc: '$out'"
fi

echo ""
echo "  $pass passed, $fail failed"
[ "$fail" -eq 0 ]
