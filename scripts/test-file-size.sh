#!/usr/bin/env bash
#
# Tests for check-file-size.sh's LANGUAGE COVERAGE (#1563).
#
#   ./scripts/test-file-size.sh
#
# Run it after touching that script. Not part of `make gate`: it builds a
# handful of throwaway git repositories, the same posture as
# `make impacted-test`.
#
# ── Why this suite exists at all ─────────────────────────────────────────────
#
# The ratchet's language list has now been wrong twice, the same way both
# times, and neither failure was visible from its output:
#
#   #825   an 8,166-line Python analyzer sat under a guard watching only *.rs,
#          which reported OK every run.
#   #1563  the delivery-loop driver reached ~1,900 lines of shell under a guard
#          watching only *.rs and *.py, which also reported OK every run.
#
# A guard that silently covers less than it claims is worse than no guard: it
# reads as evidence the tree is meeting a limit it is not. The whole-repo run
# in `make gate` cannot catch this, because "no shell file is too long" and
# "shell is not being looked at" produce the identical green line.
#
# So each case here plants a file that IS over the limit, in one language, and
# asserts the guard says so. A language dropped from the pathspec list turns
# exactly one case red.
#
# ── The glob-expansion trap, which this suite also pins ──────────────────────
#
# The pathspecs are for *git* to match against its index, not for the shell to
# expand against the working directory. Written unquoted they are expanded
# first, and at a repo root `*.sh` matches only `install.sh` — so the guard
# would watch one shell file out of sixty-odd and still print a coverage count
# that looks right. `S2` below is that bug: it puts the oversized shell file in
# a subdirectory, where a shell-expanded `*.sh` cannot reach it.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-file-size.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

# A throwaway repository with the guard installed at the path it expects
# (it derives the repo root from its own location) and an empty baseline.
# $1 = case name. Echoes the repo path.
new_repo() {
  local dir="$TMP/$1"
  mkdir -p "$dir/scripts"
  cp "$SCRIPT" "$dir/scripts/check-file-size.sh"
  printf '# test baseline\n' >"$dir/scripts/file-size-baseline.txt"
  git -C "$dir" init -q
  git -C "$dir" config user.email t@t.invalid
  git -C "$dir" config user.name t
  echo "$dir"
}

# $1 = repo, $2 = path within it, $3 = line count.
plant() {
  mkdir -p "$(dirname "$1/$2")"
  awk -v n="$3" 'BEGIN { for (i = 0; i < n; i++) print "x" }' >"$1/$2"
  git -C "$1" add -A
}

# want <name> <expect-pass|expect-fail> <repo> [substring]
want() {
  local name="$1" expect="$2" dir="$3" sub="${4:-}" out rc
  out="$("$dir/scripts/check-file-size.sh" 2>&1)"
  rc=$?
  if [ "$expect" = "expect-pass" ]; then
    if [ "$rc" -eq 0 ]; then
      pass=$((pass + 1)); echo "ok   $name"
    else
      fail=$((fail + 1)); echo "FAIL $name — expected OK, got:"; echo "$out"
    fi
    return
  fi
  if [ "$rc" -eq 0 ]; then
    fail=$((fail + 1)); echo "FAIL $name — the guard passed a file it should have flagged:"; echo "$out"
    return
  fi
  case "$out" in
    *"$sub"*) pass=$((pass + 1)); echo "ok   $name" ;;
    *) fail=$((fail + 1)); echo "FAIL $name — flagged the wrong thing (wanted '$sub'):"; echo "$out" ;;
  esac
}

# ── One case per watched language ────────────────────────────────────────────
for lang in rs py sh; do
  r="$(new_repo "over_$lang")"
  plant "$r" "src/big.$lang" 1600
  want "L-${lang} a 1600-line .${lang} file is flagged" \
    expect-fail "$r" "src/big.${lang} is 1600 lines"
done

# The extensionless hook, which `*.sh` does not match.
r="$(new_repo "over_hook")"
plant "$r" ".githooks/pre-push" 1600
want "L-hook a 1600-line .githooks/ hook is flagged" \
  expect-fail "$r" ".githooks/pre-push is 1600 lines"

# ── The glob-expansion trap ──────────────────────────────────────────────────
# Both files matter, and the ROOT one is the whole case. If the pathspecs are
# unquoted, the shell expands `*.sh` against the repo root *before* git sees
# it: with `root.sh` sitting there, `*.sh` collapses to exactly `root.sh` and
# the oversized file below it becomes invisible. Without a root-level `.sh` the
# glob matches nothing, stays literal, and reaches git intact — so a fixture
# missing `root.sh` passes under both the correct and the broken spelling, and
# proves nothing.
#
# This is not hypothetical: `install.sh` sits at this repository's root, so the
# broken spelling would have watched one shell file out of sixty-odd and still
# printed a coverage count that looked plausible.
r="$(new_repo "nested_sh")"
plant "$r" "root.sh" 10
plant "$r" "tools/deep/nested/big.sh" 1600
want "G1 an oversized shell file below a root-level .sh is still flagged (pathspecs are git's, not the shell's)" \
  expect-fail "$r" "tools/deep/nested/big.sh is 1600 lines"

# ── The negative direction ───────────────────────────────────────────────────
# Without this the suite is satisfiable by a guard that fails on everything.
r="$(new_repo "under")"
plant "$r" "src/ok.rs" 1400
plant "$r" "src/ok.py" 1400
plant "$r" "tools/ok.sh" 1400
plant "$r" ".githooks/pre-push" 1400
want "N1 files under the limit in every language pass" expect-pass "$r"

# A language the ratchet deliberately does NOT watch must not be flagged —
# the fix for #1563 must widen coverage, not make it unbounded.
r="$(new_repo "unwatched")"
plant "$r" "docs/huge.md" 4000
want "N2 an unwatched language is left alone" expect-pass "$r"

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
