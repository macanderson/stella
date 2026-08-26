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
# Deliberately not a `make gate` step, matching `main-red-hold-test`: the
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
want() { # want <name> <expect-proceed|expect-block> <want-substring> <mode> <issues> <login> <claims>
  local name="$1" expect="$2" want_text="$3" mode="$4" issues="$5" login="$6" claims="$7"
  local out rc
  out="$("$SCRIPT" "$mode" \
    --fixture-open-issues "$issues" \
    --fixture-login "$login" \
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
  expect-block "claimed by @grace" check "4671" "ada" "grace 300"

want "and it names the window it judged against" \
  expect-block "window: 20m" check "4671" "ada" "grace 300"

# The lapse. A session that died mid-repair must not hold the repair shut.
want "a claim past the window has lapsed" \
  expect-proceed "unclaimed" check "4671" "ada" "grace 1500"

# The window is a knob, so a case has to move it: 5m old is claimed at the
# default and lapsed at a one-minute window.
out="$("$SCRIPT" check --window-minutes 1 \
  --fixture-open-issues 4671 --fixture-login ada --fixture-claims "grace 300" 2>&1)"
rc=$?
if [ "$rc" -eq 0 ]; then
  ok "a shorter window lapses a claim the default still honours"
else
  bad "--window-minutes did not shorten the window: exit $rc: $out"
fi

# A session's own claim is not a reason to stand it down: re-running the
# pre-flight is what a session does when it returns to a repair it started.
want "this session's own claim does not block it" \
  expect-proceed "unclaimed" check "4671" "ada" "ada 60"

want "the freshest OTHER claim decides, not the freshest claim" \
  expect-block "claimed by @grace" check "4671" "ada" "ada 10
grace 300"

# Every unknown proceeds. One case per branch, because each is a separate
# early return and a regression in one is invisible from the others.
want "no open main-red issue proceeds" \
  expect-proceed "not known-broken" check "" "ada" ""

want "two open main-red issues are ambiguous, so it proceeds" \
  expect-proceed "ambiguous" check "4671 4672" "ada" "grace 60"

want "an unreadable identity proceeds" \
  expect-proceed "identity unknown" check "4671" "" "grace 60"

want "an unparseable claim age is ignored rather than trusted" \
  expect-proceed "unclaimed" check "4671" "ada" "grace soon"

want "no claims at all proceeds" \
  expect-proceed "unclaimed" check "4671" "ada" ""

# `claim` is `check` plus a post, so it must inherit the block.
want "claim stands down on somebody else's fresh claim" \
  expect-block "claimed by @grace" claim "4671" "ada" "grace 300"

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

# --help prints the whole header, however long it grows.
out="$("$SCRIPT" --help 2>&1)"
case "$out" in
*"Fail-open, deliberately, at every unknown"*) ok "--help reaches the end of the header" ;;
*) bad "--help truncated the header" ;;
esac

echo ""
echo "  $pass passed, $fail failed"
[ "$fail" -eq 0 ]
