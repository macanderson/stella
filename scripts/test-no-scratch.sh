#!/usr/bin/env bash
#
# Tests for the session-scratch boundary (#448, #2888).
#
#   ./scripts/test-no-scratch.sh
#
# Not part of `make gate`: it builds throwaway git repositories, the same
# posture as `make impacted-test` and `make shellcheck-guard-test`.
#
# ── What is under test ───────────────────────────────────────────────────────
#
# scripts/check-no-scratch.sh enforces one invariant: an ignored path must not
# be tracked. That is deliberately general — it catches every future scratch
# directory the moment someone ignores it — and it is also the whole property,
# so a scratch path this repository does not ignore is invisible to it.
#
# .scratch/ was that path. On 2026-08-11 a session wrote a commit message to
# .scratch/msg.txt, amended with `git add -A`, and pushed; `git check-ignore`
# exited 1, the guard printed OK, `make gate` and CI were green, and the file
# reached the remote. Nothing in the toolchain said so.
#
# The fix was one .gitignore line, which is why the subject here is the PAIR:
# the ignore rule and the guard together. Testing the script alone would pass
# on the tree that shipped the bug.
#
# The fixture copies this repository's real .gitignore, so a future edit that
# drops the rule fails these cases rather than passing a hand-written stand-in.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

ok() { pass=$((pass + 1)); echo "ok   $1"; }
no() { fail=$((fail + 1)); echo "FAIL $1"; shift; [ $# -gt 0 ] && printf '%s\n' "$@"; }

# A fresh repository carrying this tree's .gitignore and this tree's guard.
# $1 = fixture name. Echoes the path.
fixture() {
  local dir="$TMP/$1"
  mkdir -p "$dir/scripts"
  cp "$repo_root/.gitignore" "$dir/.gitignore"
  cp "$repo_root/scripts/check-no-scratch.sh" "$dir/scripts/check-no-scratch.sh"
  git -C "$dir" init -q
  git -C "$dir" config user.email test@example.com
  git -C "$dir" config user.name test
  git -C "$dir" add -A >/dev/null 2>&1
  git -C "$dir" commit -qm base >/dev/null 2>&1
  echo "$dir"
}

# ── A: the sweep that caused it ──────────────────────────────────────────────
#
# `git add -A` is the aggravating factor from the repro. It is not the defect,
# but it is the motion a session actually makes, so it is what the rule has to
# survive.

A="$(fixture add-all)"
mkdir -p "$A/.scratch"
echo scratch >"$A/.scratch/msg.txt"
git -C "$A" add -A >/dev/null 2>&1
if git -C "$A" ls-files --error-unmatch .scratch/msg.txt >/dev/null 2>&1; then
  no "A1 \`git add -A\` swept .scratch/ into the index" \
    "     .scratch/ is not ignored, so the sweep that shipped #2888 still works."
else
  ok "A1 \`git add -A\` leaves .scratch/ alone"
fi

# ── B: the guard sees it when it is forced in anyway ─────────────────────────
#
# `git add -f` bypasses the ignore rule, and a path already in the index stays
# there however it got in. That is the state the guard exists to find, and the
# state it could not see before the rule existed.

B="$(fixture forced)"
mkdir -p "$B/.scratch"
echo scratch >"$B/.scratch/note.md"
git -C "$B" add -f .scratch/note.md >/dev/null 2>&1
out="$(cd "$B" && bash scripts/check-no-scratch.sh 2>&1)"
rc=$?
if [ "$rc" -eq 0 ]; then
  no "B1 a tracked .scratch/ file fails the guard" "$out"
else
  ok "B1 a tracked .scratch/ file fails the guard"
fi
case "$out" in
*".scratch/note.md"*) ok "B2 the guard names the offending path" ;;
*) no "B2 the guard names the offending path" "$out" ;;
esac

# ── C: the guard still passes a clean tree ───────────────────────────────────
#
# An expect-fail case alone is satisfied by a guard that fails on everything.

C="$(fixture clean)"
echo hello >"$C/real.txt"
git -C "$C" add real.txt >/dev/null 2>&1
out="$(cd "$C" && bash scripts/check-no-scratch.sh 2>&1)"
if [ $? -eq 0 ]; then
  ok "C1 a tracked, unignored file passes"
else
  no "C1 a tracked, unignored file passes" "$out"
fi

# ── D: the rule reaches a subdirectory ───────────────────────────────────────
#
# The entry is unanchored, like .agent/ beside it: a session writing scratch
# inside a worktree subdirectory is the same hazard at a different path.

D="$(fixture nested)"
mkdir -p "$D/crates/stella-core/.scratch"
echo scratch >"$D/crates/stella-core/.scratch/plan.md"
git -C "$D" add -A >/dev/null 2>&1
if git -C "$D" ls-files --error-unmatch crates/stella-core/.scratch/plan.md >/dev/null 2>&1; then
  no "D1 a nested .scratch/ is ignored too" \
    "     The .gitignore entry is anchored and only covers the repository root."
else
  ok "D1 a nested .scratch/ is ignored too"
fi

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]
