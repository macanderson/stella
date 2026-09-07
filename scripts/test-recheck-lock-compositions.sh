#!/usr/bin/env bash
#
# Tests for `scripts/recheck-lock-compositions.sh` (`#5943`).
#
# Hermetic. The fixture is the repo the merge guard's own tests use, built by
# `scripts/lib/lock-composition-fixture.sh`. The seams `--fixture-open-prs`
# and `--fixture-runs` stand in for `gh`. So no case here can reach the
# network, and none can run a real workflow again.
#
# The sweep makes three calls, and each one is pinned below. Which pull
# requests are worth asking about. Which of those merge into a lock that will
# not resolve. And whether a run exists to carry the answer.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
sweep="$repo_root/scripts/recheck-lock-compositions.sh"

# shellcheck source=scripts/lib/lock-composition-fixture.sh
. "$repo_root/scripts/lib/lock-composition-fixture.sh"

if ! command -v cargo >/dev/null 2>&1; then
  echo "skip test-recheck-lock-compositions — no cargo toolchain on PATH"
  exit 0
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

pass=0
fail=0

repo="$tmp/repo"
build_lock_fixture "$repo"

stale="$(git -C "$repo" rev-parse feature)"
fine="$(git -C "$repo" rev-parse caught-up)"
prose="$(git -C "$repo" rev-parse docs-only)"

expect() {
  name="$1"
  want_code="$2"
  needle="$3"
  shift 3
  set +e
  out="$("$@" 2>&1)"
  code=$?
  set -e
  if [ "$code" -ne "$want_code" ]; then
    echo "FAIL  $name — exit $code, wanted $want_code"
    printf '      %s\n' "$out"
    fail=$((fail + 1))
    return
  fi
  case "$out" in
  *"$needle"*) ;;
  *)
    echo "FAIL  $name — output did not hold: $needle"
    printf '      %s\n' "$out"
    fail=$((fail + 1))
    return
    ;;
  esac
  echo "ok    $name"
  pass=$((pass + 1))
}

refuse() {
  name="$1"
  needle="$2"
  shift 2
  set +e
  out="$("$@" 2>&1)"
  code=$?
  set -e
  case "$out" in
  *"$needle"*)
    if [ "$code" -eq 0 ]; then
      echo "FAIL  $name — it said the right thing and then exited 0"
      fail=$((fail + 1))
      return
    fi
    ;;
  *)
    echo "FAIL  $name — output did not hold: $needle"
    printf '      %s\n' "$out"
    fail=$((fail + 1))
    return
    ;;
  esac
  echo "ok    $name"
  pass=$((pass + 1))
}

# A stale lock with a finished run gets that run again.
expect "a stale composition is sent back to ci" 0 "would run ci run 111 again" \
  "$sweep" --repo-dir "$repo" \
  --fixture-open-prs "11 $stale" \
  --fixture-runs "$stale 111"

# A branch cut after the bump is left alone.────
expect "a composing branch is left alone" 0 "its lock still resolves" \
  "$sweep" --repo-dir "$repo" \
  --fixture-open-prs "12 $fine" \
  --fixture-runs "$fine 222"

# A branch that writes no lock is never even asked.────
# This is what bounds the fan-out. Most open pull requests take this path. A
# sweep then costs one fetch and nothing more.
expect "a branch that writes no lock is skipped" 0 "asked 0 pull request(s)" \
  "$sweep" --repo-dir "$repo" \
  --fixture-open-prs "13 $prose" \
  --fixture-runs "$prose 333"

# A run still going is reported, not forced.─────
# A live run cannot be run again. It will report on its own. Saying so beats
# a silent skip.
expect "a run still going is named, not touched" 0 "its ci run is still going" \
  "$sweep" --repo-dir "$repo" \
  --fixture-open-prs "14 $stale" \
  --fixture-runs "$stale pending"

# A head with no run at all is reported.──────
expect "a head with no ci run is named" 0 "no ci run to run again" \
  "$sweep" --repo-dir "$repo" \
  --fixture-open-prs "15 $stale" \
  --fixture-runs "other 999"

# A lookup that did not answer is not read as a pass.────
expect "an unreadable run lookup says so" 0 "could not read its ci runs" \
  "$sweep" --repo-dir "$repo" \
  --fixture-open-prs "16 $stale" \
  --fixture-runs "$stale ?"

# Several at once. The count is of the ones worth asking.
expect "the summary counts only the lock writers" 0 "asked 2 pull request(s) that write the lock; 1 need ci again" \
  "$sweep" --repo-dir "$repo" \
  --fixture-open-prs "$(printf '11 %s\n12 %s\n13 %s\n' "$stale" "$fine" "$prose")" \
  --fixture-runs "$stale 111"

# A head this checkout does not hold is named, not guessed at.
expect "an unknown head is named" 0 "is not a commit here" \
  "$sweep" --repo-dir "$repo" \
  --fixture-open-prs "17 0000000000000000000000000000000000000000" \
  --fixture-runs "none none"

# No open pull requests is a clean answer.──────
expect "an empty list says so" 0 "no open pull requests" \
  "$sweep" --repo-dir "$repo" --fixture-open-prs "" --fixture-runs ""

# Every unknown fails open.──────────
# A red job here would call a pull request broken when nobody asked that. Its
# own checks are the one thing allowed to answer.
expect "a missing repo exits 0 and says why" 0 "no such directory" \
  "$sweep" --repo-dir "$tmp/nowhere" --fixture-open-prs "11 $stale" --fixture-runs ""

# A bad argument is a usage error, never a silent pass.
refuse "an unknown argument is refused" "unknown argument" "$sweep" --nope
refuse "--limit with no value is refused" "needs a number" "$sweep" --limit
refuse "--repo-dir with no value is refused" "needs a directory" "$sweep" --repo-dir

echo
echo "test-recheck-lock-compositions: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
