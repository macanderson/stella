#!/usr/bin/env bash
#
# Tests for the session-scratch boundary (#448, #2888, #4996).
#
#   ./scripts/test-no-scratch.sh
#
# Not part of `make gate`: it builds throwaway git repositories, the same
# posture as `make impacted-test` and `make shellcheck-guard-test`.
#
# ── What is under test ───────────────────────────────────────────────────────
#
# scripts/check-no-scratch.sh enforces one rule: an ignored path must not
# be tracked. That is general — it catches every future scratch
# directory the moment someone ignores it — and it is also the whole property,
# so a scratch path this repository does not ignore is invisible to it.
#
# .scratch/ was that path. On 2026-08-11 a session wrote a commit message to
# .scratch/msg.txt, amended with `git add -A`, and pushed; `git check-ignore`
# exited 1, the guard printed OK, `make gate` and CI were green, and the file
# reached the remote. Nothing in the toolchain said so. `.tmp-msg.txt` was the
# same story two weeks later at a different name shape (#4996, cases E).
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
if out="$(cd "$C" && bash scripts/check-no-scratch.sh 2>&1)"; then
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

# ── E: the same hazard one file-name shape over ──────────────────────────────
#
# .scratch/ was a directory, and the rule that covers it is a directory entry.
# A loose scratch *file* at the root is the same motion with nothing to anchor
# on: on 2026-08-25 a session wrote its commit message to `.tmp-msg.txt` and
# `git add -A` swept it into PR #4994. `*.tmp` did not match — the name starts
# with `.tmp` and ends with `.txt` — so the guard printed OK while the file sat
# in the diff, and what found it was an unrelated colour sweep reading its text
# (#4996).
#
# The `.tmp-*`/`tmp-*` entries are what close it, and E1/E2 fail on a tree
# without them.

E="$(fixture tmp-prefix-sweep)"
echo scratch >"$E/.tmp-msg.txt"
echo scratch >"$E/tmp-msg.txt"
git -C "$E" add -A >/dev/null 2>&1
if git -C "$E" ls-files --error-unmatch .tmp-msg.txt >/dev/null 2>&1; then
  no "E1 \`git add -A\` leaves .tmp-msg.txt alone" \
    "     No .gitignore entry matches a .tmp- prefix, so #4996's sweep still works."
else
  ok "E1 \`git add -A\` leaves .tmp-msg.txt alone"
fi
if git -C "$E" ls-files --error-unmatch tmp-msg.txt >/dev/null 2>&1; then
  no "E2 the dotless spelling is covered too" \
    "     Only .tmp-* is entered; a session writing tmp-msg.txt still commits it."
else
  ok "E2 the dotless spelling is covered too"
fi

E2="$(fixture tmp-prefix-forced)"
echo scratch >"$E2/.tmp-msg.txt"
git -C "$E2" add -f .tmp-msg.txt >/dev/null 2>&1
out="$(cd "$E2" && bash scripts/check-no-scratch.sh 2>&1)"
rc=$?
if [ "$rc" -eq 0 ]; then
  no "E3 a tracked .tmp-msg.txt fails the guard" "$out"
else
  ok "E3 a tracked .tmp-msg.txt fails the guard"
fi
case "$out" in
*".tmp-msg.txt"*) ok "E4 the guard names it" ;;
*) no "E4 the guard names it" "$out" ;;
esac

E3="$(fixture tmp-prefix-nested)"
mkdir -p "$E3/crates/stella-core"
echo scratch >"$E3/crates/stella-core/.tmp-plan.md"
git -C "$E3" add -A >/dev/null 2>&1
if git -C "$E3" ls-files --error-unmatch crates/stella-core/.tmp-plan.md >/dev/null 2>&1; then
  no "E5 a nested .tmp-* is ignored too" \
    "     The entry is anchored and only covers the repository root."
else
  ok "E5 a nested .tmp-* is ignored too"
fi

# ── F: untracked scratch is not the subject ──────────────────────────────────
#
# check-no-scratch.sh's header names this as the one guard #4952 left on
# `--cached` rather than widening, and #4996 re-asked whether it should flag untracked
# scratch names. This pins the answer so nobody "fixes" it into the others'
# shape: with an ignored file present but never added, the guard passes. Its
# reach starts at the index, and an ignored-but-untracked file is the working
# tree behaving correctly — failing on one would fail every session's own
# directory.

F="$(fixture untracked-scratch)"
echo scratch >"$F/.tmp-msg.txt"
if out="$(cd "$F" && bash scripts/check-no-scratch.sh 2>&1)"; then
  ok "F1 an ignored file that was never added passes"
else
  no "F1 an ignored file that was never added passes" "$out"
fi

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]
