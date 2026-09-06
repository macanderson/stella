#!/usr/bin/env bash
#
# Hermetic tests for scripts/issue-claim.sh (#5224).
#
#   ./scripts/test-issue-claim.sh    (or: make issue-claim-test)
#
# No network and no `gh`: every case drives the fixture seams, so the suite is
# the same on a bare runner as on a dev box with a live tracker.
#
# The cases that matter most are the two controls, one on each side:
#
#   - the **positive** controls — a fresh claim by somebody else, and a PR
#     that closes the issue, must each actually stand a session down. A
#     pre-flight that cannot be shown to block is a comment, not a guard, and
#     this one spends its life proceeding because most issues are unclaimed.
#   - the **negative** controls — every unknown must proceed. That is the whole
#     safety argument (a claim check that can block work is worse than the
#     duplication it prevents), and it is one branch per unknown, so it is one
#     case per branch.
#
# Not a `make gate` step, matching `main-red-claim-test`: the subject asks the
# issue tracker a question, and the gate is hermetic and offline by contract.
#
# bash 3.2 compatible.
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/issue-claim.sh"

pass=0
fail=0

ok() { printf '  \033[32m✓\033[0m %s\n' "$*"; pass=$((pass + 1)); }
bad() { printf '  \033[31m✗\033[0m %s\n' "$*"; fail=$((fail + 1)); }

# One assertion shape: run with every seam pinned and compare the exit code.
# An expected block must also name its reason, because "exit 1" is satisfied by
# a typo in the script just as well as by the case's own subject.
want() { # want <name> <expect-proceed|expect-block> <want-substring> <mode> <login> <claims> <prs>
  local name="$1" expect="$2" want_text="$3" mode="$4" login="$5" claims="$6" prs="$7"
  local out rc
  out="$("$SCRIPT" "$mode" 5045 \
    --fixture-login "$login" \
    --fixture-claims "$claims" \
    --fixture-prs "$prs" 2>&1)"
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

echo "issue-claim"

# ── the positive controls: the two things that must actually block ───────────

# A live peer. Without this case the script is a comment.
want "a fresh claim by somebody else stands this session down" \
  expect-block "claimed by @grace" check "ada" "grace - 300" ""

# A PR that closes the issue — the signal that would have caught the
# collisions this script was filed for, and the one the issue itself never
# shows. #5246 closed forty-four issues in one sweep with nothing on any of
# them.
want "an OPEN pull request closing the issue stands this session down" \
  expect-block "5246 OPEN" check "ada" "" "5246 OPEN closes"

want "and so does a MERGED one — the work is already done" \
  expect-block "5246 MERGED" check "ada" "" "5246 MERGED closes"

# The PR check runs FIRST and on its own: a claim-free issue with a live PR
# must still block, or the stronger signal would be reachable only when the
# weaker one happened to be absent too.
want "a PR blocks even with no claim comments at all" \
  expect-block "already claimed by a pull request" check "ada" "" "5246 OPEN closes"

# ── closed-unmerged and refs-only pull requests must not block ───────────────

# A pull request that was closed, not merged, is dead. It is not proof the
# work is done or in progress. A PR closed as superseded minutes after it
# opened is this exact shape. It must not stand a session down. That is the
# state where the next session should move, not stop.
want "a closed-unmerged closing pull request does not stand this session down" \
  expect-proceed "1234 CLOSED" check "ada" "" "1234 CLOSED closes"

# A closed hit next to an OPEN hit still blocks: the OPEN hit is live. Both
# get named, each with its own state, so the closed one can be read as a
# past, dead try at the same fix.
#
# The report must also say why the closed one is in the list, or a reader
# takes it for a second live claim. Naming both states is not enough to tell
# this run from the pre-fix one, which blocked on any hit and printed the
# same two lines; the explanation is what only this version writes.
out="$("$SCRIPT" check 5045 \
  --fixture-login ada --fixture-claims "" \
  --fixture-prs "1234 CLOSED closes
1235 OPEN closes" 2>&1)"
rc=$?
case "$rc,$out" in
1,*"1234 CLOSED"*"1235 OPEN"*"only blocks here because an OPEN or MERGED hit is"*)
  ok "a closed-unmerged hit and an open one both get named, and the closed one is explained"
  ;;
*)
  bad "expected exit 1 naming both '1234 CLOSED' and '1235 OPEN' and explaining the closed hit, got exit $rc: $out"
  ;;
esac

# A pull request can refs an issue without closing it. `dod-check` reads
# every linked issue's checklist, and a closing keyword would hold an
# unrelated fix to a checklist that is not its own. So a refs-only hit is a
# weak signal. It must not block, and it must not stay hidden either.
want "a Refs-only pull request does not stand this session down, but is named" \
  expect-proceed "6203 OPEN" check "ada" "" "6203 OPEN refs"

# ── the negative controls: every unknown proceeds ────────────────────────────

want "an unclaimed issue proceeds" \
  expect-proceed "is unclaimed" check "ada" "" ""

# A session's own claim is not a reason to stand it down — re-running the
# pre-flight is what a session does when it comes back to work it started.
want "a claim of this session's own does not stand it down" \
  expect-proceed "is unclaimed" check "ada" "ada - 300" ""

# A lapsed claim is not a claim. Without this a crashed session holds an issue
# shut forever, which is why this is a comment rather than an assignee.
want "a claim past the window has lapsed" \
  expect-proceed "is unclaimed" check "ada" "grace - 999999" ""

# An unreadable identity cannot tell a claim of its own from a peer's, so it
# proceeds rather than guessing.
want "an unknown identity proceeds" \
  expect-proceed "identity unknown" check "" "grace - 300" ""

# A malformed age is not a claim. The alternative is a parse failure standing a
# session down on a comment nobody can date.
want "a claim with an unreadable age is ignored" \
  expect-proceed "is unclaimed" check "ada" "grace - notanumber" ""

# A query that never ran is not proof the list was empty. The unfixed script
# prints "no PR closes it" whether it read the list or failed to ask. That
# let a duplicate start while a real pull request already closed the issue
# and sat open, green and ready to merge. This case is the fail→pass witness:
# it fails on the unfixed script, because the printed line is false, and it
# passes on the fixed script, because the line names the failure instead.
out="$("$SCRIPT" check 5045 \
  --fixture-login ada --fixture-claims "" --fixture-prs-failed 2>&1)"
rc=$?
case "$rc,$out" in
0,*"no PR closes it"*)
  bad "a failed PR-list query claimed to have read the list: $out"
  ;;
0,*"PR list unreadable"*)
  ok "a failed PR-list query proceeds without claiming no PR closes it"
  ;;
0,*)
  bad "a failed PR-list query proceeded with unexpected wording: $out"
  ;;
*)
  bad "a failed PR-list query blocked — expected exit 0, got $rc: $out"
  ;;
esac

# ── two sessions of one author ───────────────────────────────────────────────
#
# The positive control for the session word, and the case the login alone
# could never answer: a fleet, or several agent worktrees on one machine, all
# run as one login. Comparing the login read a peer's claim as this session's
# own and cleared both to work the same issue (#5875).
#
# `want` cannot drive this: it pins no session word, which is the fail-open
# side. Each case here sets both halves.
want_session() { # want_session <name> <expect-proceed|expect-block> <want-text> <login> <session> <claims>
  local name="$1" expect="$2" want_text="$3" login="$4" session="$5" claims="$6"
  local out rc
  out="$("$SCRIPT" check 5045 \
    --fixture-login "$login" \
    --fixture-session "$session" \
    --fixture-claims "$claims" \
    --fixture-prs "" 2>&1)"
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

want_session "a second session of the same author is stood down" \
  expect-block "claimed by @ada (session s1)" ada s2 "ada s1 300"

want_session "...and the report says whose sessions those are" \
  expect-block "another of your own sessions" ada s2 "ada s1 300"

# The other direction, and the one that must not regress: a session that
# re-runs its own pre-flight has to keep proceeding.
want_session "this session's own claim still does not stand it down" \
  expect-proceed "is unclaimed" ada s1 "ada s1 300"

# Fail-open, both sides. A claim written before the word existed carries none,
# and a run that could not mint one has none either. Each falls back to the
# author-only rule rather than blocking on something it cannot establish.
want_session "a claim with no session word falls back to the author-only rule" \
  expect-proceed "is unclaimed" ada s1 "ada - 300"

want_session "a run with no session word of its own proceeds on its own login" \
  expect-proceed "is unclaimed" ada "" "ada s1 300"

want_session "...and says so, rather than proceeding silently" \
  expect-proceed "no session word" ada "" "ada s1 300"

# A peer's claim still blocks whatever the words are, or the fail-open path
# would have widened into a hole.
want_session "another author's claim still blocks a run with no word" \
  expect-block "claimed by @grace" ada "" "grace s1 300"

# ── claim mode ───────────────────────────────────────────────────────────────

want "claim posts when the issue is free" \
  expect-proceed "claimed #5045 as @ada" claim "ada" "" ""

# ...and refuses to post over somebody else's live claim, which is the whole
# point: `claim` is `check` plus a write, never a write that skips the check.
want "claim stands down rather than posting over a peer" \
  expect-block "claimed by @grace" claim "ada" "grace - 300" ""

# ── argument handling ────────────────────────────────────────────────────────

out="$("$SCRIPT" check 2>&1)"
if [ $? -eq 2 ] && [ -z "${out##*usage*}" ]; then
  ok "a missing issue number is a usage error, not a silent proceed"
else
  bad "a missing issue number did not error: $out"
fi

out="$("$SCRIPT" check abc 2>&1)"
if [ $? -eq 2 ]; then
  ok "a non-numeric issue is refused"
else
  bad "a non-numeric issue was accepted: $out"
fi

# The window is configurable, and the case proves the flag reaches the
# comparison rather than being parsed and dropped.
if out="$("$SCRIPT" check 5045 --window-minutes 1 \
  --fixture-login ada --fixture-claims "grace - 300" --fixture-prs "" 2>&1)"; then
  ok "--window-minutes narrows the window (a 5m claim lapses under 1m)"
else
  bad "--window-minutes did not reach the comparison: $out"
fi

# ── The parse itself ──────────────────────────────────────────────────────
#
# Every case above drives `--fixture-claims`: already-parsed `<login> <age>`
# rows, never the jq filter that makes them. These drive `select` instead —
# real `gh issue view --json comments` JSON, on stdin, through the real
# filter. A typo in it (a dropped `.author.login`, a broken marker check) now
# fails one of these cases, not nothing.
NOW_SELECT=2000000000

iso() { jq -n --argjson t "$1" '$t | todateiso8601'; }

# comment <login> <body> <created-unix> — one comment object.
comment() {
  local login="$1" body="$2" created
  created="$(iso "$3")"
  printf '{"author":{"login":"%s"},"body":%s,"createdAt":%s}' \
    "$login" "$(printf '%s' "$body" | jq -Rs .)" "$created"
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

want_select "a live claim parses as <login> <session> <age>" \
  "grace s7 300" \
  "{\"comments\":[$(comment grace "<!-- issue-claim --> grace s7" $((NOW_SELECT - 300)))]}"

# A claim written before the word existed has two words on its marker line.
# It has to parse with a `-` rather than erroring on the missing field.
want_select "a claim with no session word parses with a '-' in that column" \
  "grace - 300" \
  "{\"comments\":[$(comment grace "<!-- issue-claim --> grace" $((NOW_SELECT - 300)))]}"

# The parse reads fields, not columns, so extra spacing and a CRLF line
# ending cannot shift the session word into the wrong slot.
want_select "a marker line with extra spaces still parses" \
  "grace s7 300" \
  "{\"comments\":[$(comment grace "<!-- issue-claim -->   grace   s7" $((NOW_SELECT - 300)))]}"

want_select "a CRLF first line still parses cleanly" \
  "grace s7 300" \
  "{\"comments\":[$(comment grace "$(printf '<!-- issue-claim --> grace s7\r\nmore')" $((NOW_SELECT - 300)))]}"

# A lapsed claim still parses correctly here — the window is judged by `check`
# downstream, not by `select`.
want_select "a lapsed claim still parses; the window is judged downstream" \
  "grace - 999999" \
  "{\"comments\":[$(comment grace "<!-- issue-claim --> grace" $((NOW_SELECT - 999999)))]}"

out="$(printf '{"comments":[%s]}' \
  "$(comment grace "just an unrelated comment" $((NOW_SELECT - 10)))" \
  | "$SCRIPT" select --now "$NOW_SELECT" 2>/dev/null)"
rc=$?
if [ -z "$out" ] && [ "$rc" -eq 0 ]; then
  ok "a comment that is not a claim produces no row"
else
  bad "a non-claim comment should produce no row, got '$out' (exit $rc)"
fi

# The witness: break the filter above by dropping the `.author.login`
# interpolation (or the `startswith($marker)` filter) and re-run
# `make issue-claim-test` — every `want_select` case above fails, naming the
# parse. Restore it and the suite is green again. Captured as a red run
# followed by a green one in this PR's description rather than claimed here.

# A malformed timestamp must fail the parse rather than silently emit a wrong
# age. Production treats a failed `select` as "comments unreadable" and
# proceeds (fail-open), so this failing closed is what keeps that path from
# reporting a wrong number.
malformed_json='{"comments":[{"author":{"login":"grace"},"body":"<!-- issue-claim --> grace","createdAt":"not-a-date"}]}'
out="$(printf '%s' "$malformed_json" | "$SCRIPT" select --now "$NOW_SELECT" 2>/dev/null)"
rc=$?
if [ "$rc" -ne 0 ] && [ -z "$out" ]; then
  ok "a malformed timestamp fails the parse rather than emitting a wrong age"
else
  bad "a malformed timestamp should fail closed, got exit $rc: '$out'"
fi

# The real `check` path, end to end, through a `gh` stub instead of a
# fixture. `gh` colorizes its output whenever it thinks it has a terminal,
# even inside `$(...)`, which turns the comments payload into text `jq`
# cannot parse — `check-releases-published.sh`'s own header names this
# hazard. `gh --jq` hides it for free, because `gh` never colorizes what it
# filters internally; splitting `--json comments` out of that call, as this
# PR does, loses that free protection, so production has to force color off
# itself. A stub `gh` plays back the hazard: valid JSON only when
# `NO_COLOR`/`CLICOLOR_FORCE` are set, broken text otherwise.
gh_colorish="$(mktemp -d)"
trap 'rm -rf "$gh_colorish"' EXIT
# Production stamps `select_claims`'s `now` from the real clock, so the
# fixture's `createdAt` has to sit a few minutes behind it too — a future
# timestamp reads as a negative age and is dropped as unparseable.
claim_time="$(jq -n --argjson t "$(($(date -u +%s) - 300))" '$t | todateiso8601')"
cat >"$gh_colorish/gh" <<'STUB'
#!/usr/bin/env bash
set -uo pipefail
case "$*" in
"pr list "*)
  echo ""
  ;;
"api user --jq .login")
  echo "ada"
  ;;
"issue view 5045 --json comments")
  if [ "${NO_COLOR:-}" = "1" ] && [ "${CLICOLOR_FORCE:-}" = "0" ]; then
    printf '{"comments":[{"author":{"login":"grace"},"body":"<!-- issue-claim --> grace","createdAt":__CLAIM_TIME__}]}'
  else
    printf '\033[1;37m{\033[0m broken, uncolored callers never see this'
  fi
  ;;
*)
  echo "gh stub: unhandled invocation: gh $*" >&2
  exit 1
  ;;
esac
STUB
sed -i.bak "s|__CLAIM_TIME__|${claim_time}|" "$gh_colorish/gh"
rm -f "$gh_colorish/gh.bak"
chmod +x "$gh_colorish/gh"
for tool in bash awk tr mktemp date jq; do
  tool_path="$(command -v "$tool")"
  [ -n "$tool_path" ] && ln -s "$tool_path" "$gh_colorish/$tool"
done
out="$(PATH="$gh_colorish" "$SCRIPT" check 5045 2>&1)"
rc=$?
case "$rc,$out" in
1,*"claimed by @grace"*)
  ok "check survives gh's own colorized JSON and still sees the live claim"
  ;;
*)
  bad "check should stand down on the live claim through gh's real output shape, got exit $rc: $out"
  ;;
esac

echo
if [ "$fail" -eq 0 ]; then
  printf 'issue-claim: %d passed\n' "$pass"
else
  printf 'issue-claim: %d passed, %d FAILED\n' "$pass" "$fail"
fi
[ "$fail" -eq 0 ]
