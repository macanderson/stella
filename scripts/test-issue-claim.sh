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
  expect-block "claimed by @grace" check "ada" "grace 300" ""

# A PR that closes the issue — the signal that would have caught the
# collisions this script was filed for, and the one the issue itself never
# shows. #5246 closed forty-four issues in one sweep with nothing on any of
# them.
want "an OPEN pull request closing the issue stands this session down" \
  expect-block "5246 OPEN" check "ada" "" "5246 OPEN"

want "and so does a MERGED one — the work is already done" \
  expect-block "5246 MERGED" check "ada" "" "5246 MERGED"

# The PR check runs FIRST and on its own: a claim-free issue with a live PR
# must still block, or the stronger signal would be reachable only when the
# weaker one happened to be absent too.
want "a PR blocks even with no claim comments at all" \
  expect-block "already claimed by a pull request" check "ada" "" "5246 OPEN"

# ── the negative controls: every unknown proceeds ────────────────────────────

want "an unclaimed issue proceeds" \
  expect-proceed "is unclaimed" check "ada" "" ""

# A session's own claim is not a reason to stand it down — re-running the
# pre-flight is what a session does when it comes back to work it started.
want "a claim of this session's own does not stand it down" \
  expect-proceed "is unclaimed" check "ada" "ada 300" ""

# A lapsed claim is not a claim. Without this a crashed session holds an issue
# shut forever, which is why this is a comment rather than an assignee.
want "a claim past the window has lapsed" \
  expect-proceed "is unclaimed" check "ada" "grace 999999" ""

# An unreadable identity cannot tell a claim of its own from a peer's, so it
# proceeds rather than guessing.
want "an unknown identity proceeds" \
  expect-proceed "identity unknown" check "" "grace 300" ""

# A malformed age is not a claim. The alternative is a parse failure standing a
# session down on a comment nobody can date.
want "a claim with an unreadable age is ignored" \
  expect-proceed "is unclaimed" check "ada" "grace notanumber" ""

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

# ── claim mode ───────────────────────────────────────────────────────────────

want "claim posts when the issue is free" \
  expect-proceed "claimed #5045 as @ada" claim "ada" "" ""

# ...and refuses to post over somebody else's live claim, which is the whole
# point: `claim` is `check` plus a write, never a write that skips the check.
want "claim stands down rather than posting over a peer" \
  expect-block "claimed by @grace" claim "ada" "grace 300" ""

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
  --fixture-login ada --fixture-claims "grace 300" --fixture-prs "" 2>&1)"; then
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

want_select "a live claim parses as <login> <age>" \
  "grace 300" \
  "{\"comments\":[$(comment grace "<!-- issue-claim --> grace" $((NOW_SELECT - 300)))]}"

# A lapsed claim still parses correctly here — the window is judged by `check`
# downstream, not by `select`.
want_select "a lapsed claim still parses; the window is judged downstream" \
  "grace 999999" \
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
