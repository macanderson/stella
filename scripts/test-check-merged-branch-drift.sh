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
# script's own --select mode. The C cases at the end run the whole script.
# A fake `gh` and a fake `git` on PATH stand in for GitHub. No live call
# runs here.
#
# The rule: report a branch only when it is still on origin, its own newest
# pull request has merged, and its tip does not match what that pull
# request shipped. Stay silent in every other case. A deleted branch, a
# branch whose tip still matches its merge, and a branch reused by a fresh
# open pull request are all healthy and must stay silent. So are the
# release bot's own branch and any pull request from a fork.
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
    fail=$((fail + 1)); echo "FAIL $name: wanted '${expect}', got '${got}'"
  fi
}

# merged builds one merged-pull-request record. Give it the branch, its
# merged sha, how many seconds ago it merged, and the PR number. Add `true`
# last for a pull request from a fork.
merged() {
  local at
  at="$(date -u -d "@$((NOW - $3))" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -r "$((NOW - $3))" +%Y-%m-%dT%H:%M:%SZ)"
  printf '{"branch":"%s","sha":"%s","mergedAt":"%s","number":%s,"fork":%s}' "$1" "$2" "$at" "$4" "${5:-false}"
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

# One branch, many merges over time. Only the newest merge should be
# checked against the live tip.
want "B1 only the branch's newest merge is compared against its live tip" \
  "" \
  "{\"merged\":[$(merged lane/reused sha1 $((10 * day)) 100),$(merged lane/reused sha2 $((5 * day)) 101),$(merged lane/reused sha3 $((1 * day)) 102)],\"liveTips\":{\"lane/reused\":\"sha3\"},\"openHeads\":[]}"

# Comparing against an older merge of the same name would flag plain reuse.
want "B2 a branch reused for many merges still catches drift after the newest one" \
  "lane/reused" \
  "{\"merged\":[$(merged lane/reused sha1 $((10 * day)) 100),$(merged lane/reused sha2 $((5 * day)) 101),$(merged lane/reused sha3 $((1 * day)) 102)],\"liveTips\":{\"lane/reused\":\"extra-commit\"},\"openHeads\":[]}"

# The release bot pushes a new bump to its branch before it opens the pull
# request. A run in that gap sees a moved tip and no open pull request.
want "B3 the release bot's own branch is skipped even with a moved tip" \
  "" \
  "{\"merged\":[$(merged bot/version-sync sha1 $((1 * day)) 100)],\"liveTips\":{\"bot/version-sync\":\"next-bump\"},\"openHeads\":[]}"

# A merged pull request from a fork. Its branch lives in the fork. Its name
# can be `main`, which origin also has, at a sha that has nothing to do
# with the fork.
want "F1 a merged fork pull request named main is silent" \
  "" \
  "{\"merged\":[$(merged main fork-sha $((2 * day)) 30 true)],\"liveTips\":{\"main\":\"origin-main\"},\"openHeads\":[]}"

# A fork merge of the same name, newer than the origin one, must not stand
# in for it.
want "F2 a newer fork merge does not hide drift on the origin branch" \
  "fix/foo" \
  "{\"merged\":[$(merged fix/foo sha-a $((5 * day)) 31),$(merged fix/foo sha-f $((1 * day)) 32 true)],\"liveTips\":{\"fix/foo\":\"moved\"},\"openHeads\":[]}"

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

# The rest run the whole script. A fake `gh` and a fake `git` sit first on
# PATH. The fake `gh` hands back raw pull request fields and runs the
# script's own `--jq` on them, the way `gh` does. It keeps only the branch
# that `--head` names. It also keeps only the merges a `merged:>=` search
# would find, so a script that used a date bound would show it here. The
# fake `git` prints `FAKE_LIVE` for `ls-remote`.
fake_bin="$(mktemp -d)"
trap 'rm -rf "$fake_bin"' EXIT
cat >"$fake_bin/gh" <<'FAKE'
#!/usr/bin/env bash
expr='.' state='' head='' search='' prev=''
for arg in "$@"; do
  case "$prev" in
    --jq) expr="$arg" ;;
    --state) state="$arg" ;;
    --head) head="$arg" ;;
    --search) search="$arg" ;;
  esac
  prev="$arg"
done
case "$state" in
  merged)
    if [ -n "${FAKE_MERGED_JSON:-}" ]; then
      raw="$FAKE_MERGED_JSON"
    else
      raw="$(jq -n --argjson n "${FAKE_MERGED:-0}" '[range($n) | {
        number: ., headRefName: "b\(.)", headRefOid: "s\(.)",
        mergedAt: "2026-01-01T00:00:00Z", isCrossRepository: false }]')"
    fi ;;
  open) raw="${FAKE_OPEN_JSON:-[]}" ;;
  *) raw='[]' ;;
esac
if [ -n "$head" ]; then
  raw="$(printf '%s' "$raw" | jq -c --arg h "$head" 'map(select(.headRefName == $h))')"
fi
case "$search" in
  'merged:>='*)
    raw="$(printf '%s' "$raw" | jq -c --arg d "${search#merged:>=}" 'map(select(.mergedAt >= $d))')" ;;
esac
printf '%s' "$raw" | jq -c "$expr"
FAKE
cat >"$fake_bin/git" <<'FAKE'
#!/bin/sh
case "$*" in
  *ls-remote*) printf '%b' "${FAKE_LIVE:-}" ;;
esac
exit 0
FAKE
chmod +x "$fake_bin/gh" "$fake_bin/git"

# One day after the fake merges, so every one of them is past the grace
# window.
FULL_NOW=1767312000

# fakes sets what the fakes return. Give it a merge count, a raw merged
# list to use in place of the count, the raw open list, and the `ls-remote`
# lines.
fakes() {
  FAKE_MERGED="$1" FAKE_MERGED_JSON="$2" FAKE_OPEN_JSON="$3" FAKE_LIVE="$4"
}

# full_run checks one whole run. Give it a name, the exit code you expect,
# and each piece of text the output must hold.
full_run() {
  local name="$1" want_rc="$2" out rc text miss=""
  shift 2
  out="$(PATH="$fake_bin:$PATH" FAKE_MERGED="$FAKE_MERGED" \
    FAKE_MERGED_JSON="$FAKE_MERGED_JSON" FAKE_OPEN_JSON="$FAKE_OPEN_JSON" \
    FAKE_LIVE="$FAKE_LIVE" "$SCRIPT" --now "$FULL_NOW" 2>&1)"
  rc=$?
  [ "$rc" = "$want_rc" ] || miss="exit ${rc}"
  for text in "$@"; do
    case "$out" in
      *"$text"*) ;;
      *) miss="${miss:+${miss}, }no '${text}'" ;;
    esac
  done
  if [ -z "$miss" ]; then
    pass=$((pass + 1)); echo "ok   $name"
  else
    fail=$((fail + 1)); echo "FAIL $name: wanted exit ${want_rc}, got ${miss}: ${out}"
  fi
}

# A branch that merged two years before the run and has moved since. Its
# stray commits are still on origin, so the run must still report it.
fakes 0 '[{"number":7,"headRefName":"old/branch","headRefOid":"old-sha","mergedAt":"2024-01-01T00:00:00Z","isCrossRepository":false}]' \
  "[]" 'ffff\trefs/heads/old/branch\n'
full_run "C1 a merge from long ago on a moved branch is still reported" 1 \
  "  old/branch " "#7 " "2024-01-01T00:00:00Z"

# Three live branches, each still at the sha that merged.
fakes 3 "" "[]" 's0\trefs/heads/b0\ns1\trefs/heads/b1\ns2\trefs/heads/b2\n'
full_run "C2 a clean run counts each live branch it checked" 0 \
  "3 live branch(es) checked, 3 merge(s) read"

# A live branch whose tip moved. The run must fail and print the branch, its
# pull request, its merged sha and its live tip.
fakes 1 "" "[]" 'aaaa\trefs/heads/main\nffff\trefs/heads/b0\n'
full_run "C3 a drifted branch fails the run and fills in its row" 1 \
  "1 branch(es) are still on origin" "  b0 " "#0 " " s0 " "ffff"

# A fork's merged `main` must not be held up against origin's `main`.
fakes 0 '[{"number":0,"headRefName":"main","headRefOid":"fork-sha","mergedAt":"2026-01-01T00:00:00Z","isCrossRepository":true}]' \
  "[]" 'aaaa\trefs/heads/main\n'
full_run "C4 a merged fork pull request named main is silent" 0 \
  "1 live branch(es) checked, 1 merge(s) read"

# An open pull request from a fork has its branch in the fork. It must not
# hide drift on the origin branch of the same name.
fakes 1 "" '[{"headRefName":"b0","isCrossRepository":true}]' 'ffff\trefs/heads/b0\n'
full_run "C5 an open fork pull request does not hide drift" 1 "  b0 "

# An open pull request from origin does reuse the branch. That run is clean.
fakes 1 "" '[{"headRefName":"b0","isCrossRepository":false}]' 'ffff\trefs/heads/b0\n'
full_run "C6 an open pull request from origin hides the moved tip" 0 \
  "1 live branch(es) checked, 1 merge(s) read"

# The release bot's branch is not looked up at all. It merges hundreds of
# times, and the rule skips it anyway.
fakes 0 '[{"number":9,"headRefName":"bot/version-sync","headRefOid":"sha1","mergedAt":"2026-01-01T00:00:00Z","isCrossRepository":false}]' \
  "[]" 'next\trefs/heads/bot/version-sync\n'
full_run "C7 the release bot's branch is not looked up" 0 \
  "0 live branch(es) checked, 0 merge(s) read"

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
