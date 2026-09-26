#!/usr/bin/env bash
#
# Tests for check-merged-branch-drift.sh's rule.
#
#   ./scripts/test-check-merged-branch-drift.sh
#
# Run it after you change that script. It is not part of `make gate`. It is
# a handful of jq calls over fixtures, like
# scripts/test-releases-published.sh.
#
# Most cases below drive the rule with the I/O taken out. They call the
# script's own --select mode. The last two run the whole script. A fake gh
# and a fake git on PATH stand in for GitHub. No live call runs here.
#
# The rule: report a branch only when it is still on origin, its own newest
# pull request has merged, and its tip does not match what that pull
# request shipped. Stay silent in every other case. A deleted branch, a
# branch whose tip still matches its merge, and a branch reused by a fresh
# open pull request are all healthy and must stay silent.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-merged-branch-drift.sh"

NOW=2000000
GRACE=300

pass=0
fail=0

# want checks one case. Give it a name, the branches you expect back
# (space-separated, or empty), and the input JSON.
want() {
  local name="$1" expect="$2" json="$3" got
  got="$(printf '%s' "$json" | "$SCRIPT" --select --now "$NOW" --grace-secs "$GRACE" 2>&1 | cut -f1 | tr '\n' ' ')"
  got="${got% }"
  if [ "$got" = "$expect" ]; then
    pass=$((pass + 1)); echo "ok   $name"
  else
    fail=$((fail + 1)); echo "FAIL $name — wanted '${expect}', got '${got}'"
  fi
}

# merged builds one merged-pull-request record. Give it the branch, its
# merged sha, how many seconds ago it merged, and the PR number.
merged() {
  local at
  at="$(date -u -d "@$((NOW - $3))" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -r "$((NOW - $3))" +%Y-%m-%dT%H:%M:%SZ)"
  printf '{"branch":"%s","sha":"%s","mergedAt":"%s","number":%s}' "$1" "$2" "$at" "$4"
}

day=86400

# The real case: an old merge, and the branch is still live with a new sha.
want "L1 a live branch whose tip moved past its merge is reported" \
  "fix/arenabench-dind-host-netns" \
  "{\"merged\":[$(merged fix/arenabench-dind-host-netns 35e435f05 $((2 * day)) 2180)],\"liveTips\":{\"fix/arenabench-dind-host-netns\":\"ffaa21e33c\"},\"openHeads\":[]}"

# The normal case: GitHub already deleted the branch. Nothing to compare.
want "H1 a branch GitHub already deleted is silent" \
  "" \
  "{\"merged\":[$(merged fix/foo abc123 $((2 * day)) 10)],\"liveTips\":{},\"openHeads\":[]}"

# The branch is still there, but no one has touched it since the merge.
want "H2 a live branch whose tip still matches its merge is silent" \
  "" \
  "{\"merged\":[$(merged fix/foo abc123 $((2 * day)) 10)],\"liveTips\":{\"fix/foo\":\"abc123\"},\"openHeads\":[]}"

# Reused on purpose: the branch merged once, then a fresh pull request
# opened from it for new work. The open pull request is what makes the new
# tip expected, not lost.
want "H3 a branch reused by a currently open pull request is silent" \
  "" \
  "{\"merged\":[$(merged fix/foo abc123 $((2 * day)) 10)],\"liveTips\":{\"fix/foo\":\"def456\"},\"openHeads\":[\"fix/foo\"]}"

# The release bot's shape: one branch, many merges over time. Only the
# newest merge should be checked against the live tip.
want "B1 only the branch's newest merge is compared against its live tip" \
  "" \
  "{\"merged\":[$(merged bot/version-sync sha1 $((10 * day)) 100),$(merged bot/version-sync sha2 $((5 * day)) 101),$(merged bot/version-sync sha3 $((1 * day)) 102)],\"liveTips\":{\"bot/version-sync\":\"sha3\"},\"openHeads\":[]}"

# Comparing against an older merge of the same name would flag plain reuse.
want "B2 a branch reused for many merges still catches drift after the newest one" \
  "bot/version-sync" \
  "{\"merged\":[$(merged bot/version-sync sha1 $((10 * day)) 100),$(merged bot/version-sync sha2 $((5 * day)) 101),$(merged bot/version-sync sha3 $((1 * day)) 102)],\"liveTips\":{\"bot/version-sync\":\"extra-commit\"},\"openHeads\":[]}"

# GitHub can lag a few seconds before it deletes a merged branch. A merge
# inside the grace window proves nothing yet.
want "G1 a merge inside the grace window is not reported" \
  "" \
  "{\"merged\":[$(merged fix/foo abc123 60 10)],\"liveTips\":{\"fix/foo\":\"different\"},\"openHeads\":[]}"

want "G2 a merge exactly at the grace boundary is not reported" \
  "" \
  "{\"merged\":[$(merged fix/foo abc123 $GRACE 10)],\"liveTips\":{\"fix/foo\":\"different\"},\"openHeads\":[]}"

want "G3 a merge one second past the grace boundary is reported" \
  "fix/foo" \
  "{\"merged\":[$(merged fix/foo abc123 $((GRACE + 1)) 10)],\"liveTips\":{\"fix/foo\":\"different\"},\"openHeads\":[]}"

# Several findings at once. Oldest merge should come first.
want "M1 several drifted branches are reported oldest merge first" \
  "fix/a fix/b" \
  "{\"merged\":[$(merged fix/b sha-b $((2 * day)) 20),$(merged fix/a sha-a $((5 * day)) 21)],\"liveTips\":{\"fix/a\":\"other-a\",\"fix/b\":\"other-b\"},\"openHeads\":[]}"

want "M2 a healthy branch does not hide a drifted one" \
  "fix/b" \
  "{\"merged\":[$(merged fix/a sha-a $((3 * day)) 20),$(merged fix/b sha-b $((2 * day)) 21)],\"liveTips\":{\"fix/a\":\"sha-a\",\"fix/b\":\"other-b\"},\"openHeads\":[]}"

want "E1 no merges at all reports nothing" "" '{"merged":[],"liveTips":{},"openHeads":[]}'

# GitHub search returns at most 1000 pull requests, whatever --limit asks
# for. A list of exactly 1000 may be cut off, so the script must refuse it.
# The fake gh returns FAKE_MERGED merges and no open pull requests. The
# fake git lists no live branches.
fake_bin="$(mktemp -d)"
trap 'rm -rf "$fake_bin"' EXIT
cat >"$fake_bin/gh" <<'FAKE'
#!/usr/bin/env bash
case "$*" in
  *"--state merged"*)
    jq -n --argjson n "$FAKE_MERGED" \
      '[range($n) | {branch: "b\(.)", sha: "s\(.)", mergedAt: "2026-01-01T00:00:00Z", number: .}]' ;;
  *) echo '[]' ;;
esac
FAKE
printf '#!/bin/sh\nexit 0\n' >"$fake_bin/git"
chmod +x "$fake_bin/gh" "$fake_bin/git"

# full_run checks one whole run. Give it a name, the merge count the fake
# gh returns, the exit code you expect, and text the output must hold.
full_run() {
  local name="$1" count="$2" want_rc="$3" want_text="$4" out rc
  out="$(PATH="$fake_bin:$PATH" FAKE_MERGED="$count" "$SCRIPT" 2>&1)"
  rc=$?
  case "$rc:$out" in
    "$want_rc:"*"$want_text"*) pass=$((pass + 1)); echo "ok   $name" ;;
    *) fail=$((fail + 1)); echo "FAIL $name — wanted exit ${want_rc} with '${want_text}', got exit ${rc}: ${out}" ;;
  esac
}

full_run "C1 a list that hits the search ceiling is refused" 1000 1 "1000-result ceiling"
full_run "C2 a list one under the ceiling is read" 999 0 "999 merge(s) checked"

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
