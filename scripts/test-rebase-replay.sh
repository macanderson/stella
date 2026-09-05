#!/usr/bin/env bash
#
# Tests for check-rebase-replay.sh — the guard that catches an edit-and-undo
# pair before a rebase turns it into a live revert (#4979).
#
#   ./scripts/test-rebase-replay.sh
#
# Not part of `make gate`: it builds throwaway git repositories, the same
# posture as scripts/test-website-inputs.sh.
#
# ── Why this suite exists ────────────────────────────────────────────────────
#
# The guard's subject is invisible by construction — the branch's diff for the
# path is empty whether or not the hazard is present — so nothing about a green
# run distinguishes a healthy branch from a guard that has stopped looking. R2
# below is the other half: it runs #4979's own reproduction to completion and
# asserts the rebase really does eat the upstream edit, so the suite proves the
# hazard is real rather than assuming the issue was right about it.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
GUARD="$repo_root/scripts/check-rebase-replay.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

# A throwaway repository on `main` with one committed file. $1 = case name;
# echoes the repository path.
new_repo() {
  local dir="$TMP/$1"
  mkdir -p "$dir"
  git -C "$dir" init -q -b main
  git -C "$dir" config user.email t@t.invalid
  git -C "$dir" config user.name t
  git -C "$dir" config commit.gpgsign false
  printf 'alpha\n' >"$dir/f.txt"
  git -C "$dir" add f.txt
  git -C "$dir" commit -qm base
  echo "$dir"
}

# want <name> <expect-pass|expect-fail> <repo> <base> [substring]
want() {
  local name="$1" expect="$2" dir="$3" base="$4" sub="${5:-}" out rc
  out="$(cd "$dir" && "$GUARD" "$base" HEAD 2>&1)"
  rc=$?
  if [ "$expect" = "expect-pass" ] && [ "$rc" -ne 0 ]; then
    fail=$((fail + 1))
    echo "FAIL $name — expected OK, got:"
    echo "$out"
    return
  fi
  if [ "$expect" = "expect-fail" ] && [ "$rc" -eq 0 ]; then
    fail=$((fail + 1))
    echo "FAIL $name — the guard passed something it should have flagged:"
    echo "$out"
    return
  fi
  case "$out" in
  *"$sub"*)
    pass=$((pass + 1))
    echo "ok   $name"
    ;;
  *)
    fail=$((fail + 1))
    echo "FAIL $name — verdict was right, report was not (wanted '$sub'):"
    echo "$out"
    ;;
  esac
}

# ── An ordinary branch ───────────────────────────────────────────────────────
#
# Without this the suite would prove the guard can fail and nothing else.
r="$(new_repo kept)"
git -C "$r" checkout -q -b b
printf 'beta\n' >"$r/f.txt"
git -C "$r" commit -qam "edit f"
want "R0 a branch that edits a file and keeps the edit passes" \
  expect-pass "$r" main "check-rebase-replay: OK"

# ── 1. The shape ─────────────────────────────────────────────────────────────
r="$(new_repo reverted)"
git -C "$r" checkout -q -b b
printf 'beta\n' >"$r/f.txt"
git -C "$r" commit -qam "edit f"
printf 'alpha\n' >"$r/f.txt"
git -C "$r" commit -qam "revert it"
want "R1 an edit undone later in the same branch is flagged" \
  expect-fail "$r" main "f.txt"

# ...and the report names the remedy rather than only the finding.
want "R1b the report names the flatten remedy" \
  expect-fail "$r" main "git rebase -i"

# ── 2. The hazard is real ────────────────────────────────────────────────────
#
# #4979's reproduction, run to its conclusion. This asserts nothing about the
# guard: it asserts the premise the guard rests on, so a future reader does not
# have to take the issue's word for it.
r="$(new_repo replay)"
git -C "$r" checkout -q -b a
printf 'beta\n' >"$r/f.txt"
git -C "$r" commit -qam "A: edit"

git -C "$r" checkout -q main
git -C "$r" checkout -q -b b
printf 'beta\n' >"$r/f.txt"
git -C "$r" commit -qam "B: same edit"
printf 'alpha\n' >"$r/f.txt"
git -C "$r" commit -qam "B: revert it"

# B contributes nothing to that file, which is what makes the pair invisible.
if [ -n "$(git -C "$r" diff --name-only main b -- f.txt)" ]; then
  fail=$((fail + 1))
  echo "FAIL R2a B's diff against main should be empty for f.txt"
else
  pass=$((pass + 1))
  echo "ok   R2a B's diff against main does not mention the file at all"
fi

git -C "$r" checkout -q main
git -C "$r" merge -q --no-edit a
git -C "$r" checkout -q b
git -C "$r" rebase main >"$TMP/rebase.log" 2>&1
if [ "$(cat "$r/f.txt")" = "alpha" ]; then
  pass=$((pass + 1))
  echo "ok   R2b rebasing B onto A's landing reverts A (skipped the edit, kept the undo)"
else
  fail=$((fail + 1))
  echo "FAIL R2b the rebase did not reproduce the hazard; f.txt reads $(cat "$r/f.txt")"
  cat "$TMP/rebase.log"
fi

# ── 3. A branch that leaves the file alone ───────────────────────────────────
r="$(new_repo untouched)"
git -C "$r" checkout -q -b b
printf 'x\n' >"$r/other.txt"
git -C "$r" add other.txt
git -C "$r" commit -qm "add other"
want "R3 a branch that never touches the file passes" \
  expect-pass "$r" main "check-rebase-replay: OK"

# ── 4. Created and deleted inside the branch ─────────────────────────────────
#
# The same hazard wearing different clothes: rebase drops the creation as
# already upstream and keeps the deletion, which deletes somebody else's file.
r="$(new_repo added_then_removed)"
git -C "$r" checkout -q -b b
printf 'new\n' >"$r/g.txt"
git -C "$r" add g.txt
git -C "$r" commit -qm "add g"
git -C "$r" rm -q g.txt
git -C "$r" commit -qm "drop g again"
want "R4 a file created and deleted inside the branch is flagged" \
  expect-fail "$r" main "g.txt"

# ── 5. The remedy clears it ──────────────────────────────────────────────────
#
# The same tree, flattened. If this failed, the guard would be unsatisfiable
# and the remedy it prints would be a lie.
r="$(new_repo flattened)"
git -C "$r" checkout -q -b b
printf 'beta\n' >"$r/f.txt"
git -C "$r" commit -qam "edit f"
printf 'alpha\n' >"$r/f.txt"
git -C "$r" commit -qam "revert it"
git -C "$r" reset -q --soft main
git -C "$r" commit -q --allow-empty -m "flattened: the pair is gone"
want "R5 flattening the pair clears the finding" \
  expect-pass "$r" main "check-rebase-replay: OK"

# ── 6. An undo the final diff still mentions ─────────────────────────────────
#
# Edited, reverted, then edited to something else. The path IS in the branch's
# diff, so a rebase has a real change to carry and nothing is invisible. The
# guard must not fire on every revert — only on one the diff has swallowed.
r="$(new_repo reverted_then_changed)"
git -C "$r" checkout -q -b b
printf 'beta\n' >"$r/f.txt"
git -C "$r" commit -qam "edit f"
printf 'alpha\n' >"$r/f.txt"
git -C "$r" commit -qam "revert it"
printf 'gamma\n' >"$r/f.txt"
git -C "$r" commit -qam "edit it differently"
want "R6 a revert the final diff still mentions is not flagged" \
  expect-pass "$r" main "check-rebase-replay: OK"

# ── 7. Nothing to judge ──────────────────────────────────────────────────────
r="$(new_repo no_commits)"
git -C "$r" checkout -q -b b
want "R7 a branch with no commits of its own passes" \
  expect-pass "$r" main "touches a file"

# ── 8. A base that is not there ──────────────────────────────────────────────
#
# Loud rather than green: a guard that cannot see its range must not report the
# same line as one that looked and found nothing.
r="$(new_repo missing_base)"
git -C "$r" checkout -q -b b
printf 'beta\n' >"$r/f.txt"
git -C "$r" commit -qam "edit f"
want "R8 an absent base ref is a failure naming the shallow checkout" \
  expect-fail "$r" origin/nonexistent "fetch-depth"

# ── 9. A rename the base branch made, absorbed through a merge ───────────────
#
# The branch edits a file. Upstream moves it. The branch merges that, then
# re-lands the work at the new path.
#
# The old path drops out of the branch's diff, but the branch did not remove
# it. A rebase has no copy of it to revert. It replays the edit onto a tree
# with no such path, and conflicts loudly.
#
# The record-plane extraction made two of these in one morning. Both branches
# had followed the move correctly.
r="$(new_repo moved_upstream)"
mkdir -p "$r/old"
printf 'alpha\n' >"$r/old/bar.txt"
git -C "$r" add old/bar.txt
git -C "$r" commit -qm "add old/bar.txt"
git -C "$r" checkout -q -b b
printf 'beta\n' >"$r/old/bar.txt"
git -C "$r" commit -qam "B: edit old/bar.txt"

git -C "$r" checkout -q main
mkdir -p "$r/new"
git -C "$r" mv old/bar.txt new/bar.txt
git -C "$r" commit -qm "upstream: move bar.txt to new/"

git -C "$r" checkout -q b
git -C "$r" merge -q --no-edit main -m "merge main" >/dev/null 2>&1
printf 'beta\n' >"$r/new/bar.txt"
git -C "$r" commit -qam "B: re-land the edit at the new path" >/dev/null 2>&1
want "R9 a path the base branch renamed, absorbed by a merge, is not flagged" \
  expect-pass "$r" main "check-rebase-replay: OK"

# ...and the branch's work really did survive the move, so R9 is not passing
# because the edit was lost.
if [ "$(cat "$r/new/bar.txt")" = "beta" ]; then
  pass=$((pass + 1))
  echo "ok   R9b the edit survived the rename rather than being dropped"
else
  fail=$((fail + 1))
  echo "FAIL R9b new/bar.txt reads $(cat "$r/new/bar.txt"), so the edit did not survive"
fi

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
