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

# Overwrite the baseline. $1 = repo, then one "<ceiling> <path>" per entry.
set_baseline() {
  local dir="$1"
  shift
  {
    printf '# test baseline\n'
    for entry in "$@"; do printf '%s\n' "$entry"; done
  } >"$dir/scripts/file-size-baseline.txt"
}

# $1 = repo, $2 = message. The skew cases need real commits: the guard reads
# the BASE tree, and there is no base without history.
commit() {
  git -C "$1" add -A
  git -C "$1" commit -q -m "$2"
}

# want <name> <expect-pass|expect-fail> <repo> [substring]
#
# The substring is checked on BOTH verdicts. On expect-fail it pins which file
# was flagged; on expect-pass it pins what a passing run still reported — a
# drift note is a real finding that happens not to be fatal, and a guard that
# passed by forgetting the file would satisfy a bare expect-pass.
want() {
  local name="$1" expect="$2" dir="$3" sub="${4:-}" out rc
  out="$("$dir/scripts/check-file-size.sh" 2>&1)"
  rc=$?
  if [ "$expect" = "expect-pass" ]; then
    if [ "$rc" -ne 0 ]; then
      fail=$((fail + 1)); echo "FAIL $name — expected OK, got:"; echo "$out"
      return
    fi
    case "$out" in
      *"$sub"*) pass=$((pass + 1)); echo "ok   $name" ;;
      *) fail=$((fail + 1)); echo "FAIL $name — passed, but did not report '$sub':"; echo "$out" ;;
    esac
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

# ── The baseline skew: judging the CHANGE, not the tree (#2004) ───────────────
#
# Three times running (#1761, #1782, #2003) two PRs that each wrote the shared
# baseline CORRECTLY composed into a red `main`, and every subsequent PR
# inherited a failure it did not cause. These cases reconstruct that skew from
# scratch — two commits in a throwaway repo, no reliance on this repository's
# real history — and pin both directions of the rule.
#
# B1 and B2 are the witnesses: each FAILS under the old whole-tree check and
# passes now. B3 and B4 are what stop the fix from being a hole, and B4 is the
# one that matters most — it is the case a naive "already over at the base, so
# ignore it" rule would wave through, turning inherited drift into a standing
# licence to bloat. That would be strictly worse than the red main this fixes,
# so the rule takes max(ceiling, size at base) rather than either alone.

# The observed shape: a sibling's regeneration, computed against a main that
# lacked this file's growth, writes a ceiling one line low. The file itself is
# untouched here — it did not grow, the ceiling moved down underneath it.
r="$(new_repo "skew_ceiling_lowered")"
plant "$r" "src/big.rs" 1600
set_baseline "$r" "1600 src/big.rs"
commit "$r" "base: the ceiling matches the file"
set_baseline "$r" "1599 src/big.rs"
commit "$r" "head: a sibling's stale regeneration lowers the ceiling"
export FILE_SIZE_BASE_REF="HEAD^1"
want "B1 a ceiling lowered under an untouched file is not this change's failure" \
  expect-pass "$r"
unset FILE_SIZE_BASE_REF

# The already-landed instance: `main` is over its ceiling before this change
# exists, and the change touches something else entirely. This is PR #1992,
# which sat blocked with a tree byte-identical to main.
r="$(new_repo "skew_already_over")"
plant "$r" "src/big.rs" 1600
set_baseline "$r" "1599 src/big.rs"
commit "$r" "base: main already carries the drift"
plant "$r" "src/unrelated.rs" 10
commit "$r" "head: an unrelated change walks past it"
export FILE_SIZE_BASE_REF="HEAD^1"
want "B2 inherited drift does not fail a change that touched something else" \
  expect-pass "$r"
unset FILE_SIZE_BASE_REF

# The bloat the ratchet exists to block, with its message unchanged.
r="$(new_repo "genuine_growth")"
plant "$r" "src/big.rs" 1600
set_baseline "$r" "1600 src/big.rs"
commit "$r" "base"
plant "$r" "src/big.rs" 1650
commit "$r" "head: grow a god file past its ceiling"
export FILE_SIZE_BASE_REF="HEAD^1"
want "B3 a change that genuinely grows a god file still fails" \
  expect-fail "$r" "src/big.rs grew to 1650 lines, over its baseline ceiling of 1600 (+50)"
unset FILE_SIZE_BASE_REF

# Drift must not become a licence: the base is ALREADY over its ceiling, and
# this change grows the file another 50 lines on top. Passing this would be
# worse than the bug being fixed.
r="$(new_repo "growth_on_drift")"
plant "$r" "src/big.rs" 1600
set_baseline "$r" "1599 src/big.rs"
commit "$r" "base: already drifted"
plant "$r" "src/big.rs" 1650
commit "$r" "head: grow it further still"
export FILE_SIZE_BASE_REF="HEAD^1"
want "B4 growth on top of inherited drift still fails" \
  expect-fail "$r" "src/big.rs grew to 1650"
unset FILE_SIZE_BASE_REF

# No base to compare against — a root commit, a shallow clone, a repository
# with no origin. The guard must get STRICTER, never weaker, when it cannot
# tell what the change inherited.
r="$(new_repo "no_base")"
plant "$r" "src/big.rs" 1600
set_baseline "$r" "1599 src/big.rs"
commit "$r" "the only commit: nothing to compare against"
want "B5 an unresolvable base falls back to the strict whole-tree check" \
  expect-fail "$r" "src/big.rs grew to 1600"

# The shape CI actually runs, with NO explicit base: on a `pull_request` event
# the checkout is `refs/pull/N/merge`, so HEAD is a merge commit and the guard
# must find HEAD^1 — the base branch tip — by itself. B1-B4 all pass the base
# in by hand, so without this case the rung that matters most in production is
# the one rung nothing exercises.
r="$(new_repo "merge_checkout")"
plant "$r" "src/big.rs" 1600
set_baseline "$r" "1600 src/big.rs"
commit "$r" "root"
trunk="$(git -C "$r" rev-parse --abbrev-ref HEAD)"
git -C "$r" checkout -q -b feature
plant "$r" "src/unrelated.rs" 10
commit "$r" "the PR: an unrelated change"
git -C "$r" checkout -q "$trunk"
set_baseline "$r" "1599 src/big.rs"
commit "$r" "the base branch drifts while the PR is open"
git -C "$r" merge -q --no-ff -m "Merge feature" feature
want "B6 a refs/pull/N/merge checkout finds its base branch tip unaided" \
  expect-pass "$r"

# ── The same rule for a file with NO baseline entry (#2397) ──────────────────
#
# B1-B4 all concern grandfathered files, and so did the fix: the allowance ran
# in the branch that reads a baseline ceiling, and a file with no entry went on
# failing on sight. So the first time any file crossed 1500 lines on `main`,
# every open PR went red — including PRs whose tree was byte-identical to the
# base, which is precisely the shape #2004 was written to end. B7-B9 pin the
# rule to both kinds of file, and B8/B9 are what stop that from becoming a
# licence: a first-time crossing is survived only while the branch does not
# make it worse, and it is never absorbed into the baseline.

# The blocking case: over the limit at the base, with no baseline entry, and
# this change touches something else entirely. It must pass — and it must still
# SAY so, because the debt is real and only the ownership of it is in question.
# A guard that passed by forgetting the file would satisfy the verdict alone.
r="$(new_repo "newover_at_base")"
plant "$r" "src/big.rs" 1600
commit "$r" "base: a first-time crossing lands on main"
plant "$r" "src/unrelated.rs" 10
commit "$r" "head: an unrelated change walks past it"
export FILE_SIZE_BASE_REF="HEAD^1"
want "B7 a first-time crossing inherited from the base is drift, not this change's failure" \
  expect-pass "$r" "src/big.rs is 1600 lines, over the 1500-line limit — already so at the base; split it"
unset FILE_SIZE_BASE_REF

# The bloat the ratchet exists to block, in its commonest form: the branch
# itself pushes a file over the limit for the first time.
r="$(new_repo "newover_grown_here")"
plant "$r" "src/big.rs" 1400
commit "$r" "base: under the limit"
plant "$r" "src/big.rs" 1600
commit "$r" "head: this change crosses the limit"
export FILE_SIZE_BASE_REF="HEAD^1"
want "B8 a file this change pushes over the limit still fails" \
  expect-fail "$r" "src/big.rs is 1600 lines, over the 1500-line limit"
unset FILE_SIZE_BASE_REF

# Drift must not become a licence here either — the B4 asymmetry, for a file
# with no baseline entry to grow against.
r="$(new_repo "newover_growth_on_drift")"
plant "$r" "src/big.rs" 1600
commit "$r" "base: already over, and not grandfathered"
plant "$r" "src/big.rs" 1650
commit "$r" "head: grow it further still"
export FILE_SIZE_BASE_REF="HEAD^1"
want "B9 growth on top of an inherited first-time crossing still fails" \
  expect-fail "$r" "src/big.rs is 1650 lines, over the 1500-line limit"
unset FILE_SIZE_BASE_REF

# ── What --update is allowed to WRITE (#3441) ────────────────────────────────
#
# Everything above tests the checker. These test the generator, which had the
# complementary bug: the checker refuses a first-time crossing and names
# "split it" as the remedy, while `--update` — the documented remedy for the
# OTHER failure — silently recorded every over-limit file it could see and so
# wrote exactly the entry the checker forbids. The two failures share a trigger
# (the baseline going stale), so the moment `--update` is run is the moment an
# unrelated crossing is most likely to be absorbed by it.
#
# U1 is the witness: it fails on the old script, which wrote the entry.

# want_update <name> <expect-pass|expect-fail> <repo> <substring> [args...]
#
# Asserts the verdict, the message, AND — on a refusal — that the baseline is
# byte-identical afterwards. The last part is the half that matters: a refusal
# that has already rewritten the file has not refused anything.
want_update() {
  local name="$1" expect="$2" dir="$3" sub="$4"
  shift 4
  local bl="$dir/scripts/file-size-baseline.txt" before after out rc
  before="$(cat "$bl" 2>/dev/null || true)"
  out="$("$dir/scripts/check-file-size.sh" --update "$@" 2>&1)"
  rc=$?
  after="$(cat "$bl" 2>/dev/null || true)"
  if [ "$expect" = "expect-fail" ]; then
    if [ "$rc" -eq 0 ]; then
      fail=$((fail + 1)); echo "FAIL $name — --update succeeded where it should have refused:"; echo "$out"
      return
    fi
    if [ "$before" != "$after" ]; then
      fail=$((fail + 1)); echo "FAIL $name — refused, but rewrote the baseline anyway."
      return
    fi
  elif [ "$rc" -ne 0 ]; then
    fail=$((fail + 1)); echo "FAIL $name — expected --update to succeed, got:"; echo "$out"
    return
  fi
  case "$out" in
    *"$sub"*) pass=$((pass + 1)); echo "ok   $name" ;;
    *) fail=$((fail + 1)); echo "FAIL $name — wrong message (wanted '$sub'):"; echo "$out" ;;
  esac
}

# The witness. A tree carrying BOTH failures at once, which is the realistic
# shape: one grandfathered ceiling has drifted (the reason anyone runs
# --update) and one unrelated file has crossed the limit for the first time.
r="$(new_repo "update_refuses_new_entry")"
plant "$r" "src/god.rs" 2004
plant "$r" "src/fresh.rs" 1600
set_baseline "$r" "2000 src/god.rs"
commit "$r" "a drifted ceiling and an unrelated first-time crossing"
want_update "U1 --update refuses to grandfather a file that is not already in the baseline" \
  expect-fail "$r" "src/fresh.rs"

# The down-ratchet, and the deliberate raise, must both keep working — this is
# the whole reason --update exists and the refusal must not cost it.
r="$(new_repo "update_rewrites_existing")"
plant "$r" "src/shrunk.rs" 1600
plant "$r" "src/grown.rs" 2004
set_baseline "$r" "2400 src/shrunk.rs" "2000 src/grown.rs"
commit "$r" "one entry retightens, one is deliberately raised"
want_update "U2 --update still rewrites existing entries in both directions" \
  expect-pass "$r" "baseline updated"
bl="$r/scripts/file-size-baseline.txt"
if grep -q '^1600 src/shrunk.rs$' "$bl" && grep -q '^2004 src/grown.rs$' "$bl"; then
  pass=$((pass + 1)); echo "ok   U2b both ceilings were rewritten to the current sizes"
else
  fail=$((fail + 1)); echo "FAIL U2b ceilings not rewritten:"; cat "$bl"
fi

# An entry that dropped under the limit must still be retired, which is a
# removal — the refusal only ever guards ADDITIONS.
r="$(new_repo "update_retires_obsolete")"
plant "$r" "src/fixed.rs" 900
set_baseline "$r" "2000 src/fixed.rs"
commit "$r" "the file was split and no longer needs an exemption"
want_update "U3 --update still retires an obsolete entry" expect-pass "$r" "baseline updated"
if grep -q 'src/fixed.rs' "$r/scripts/file-size-baseline.txt"; then
  fail=$((fail + 1)); echo "FAIL U3b the obsolete entry survived --update"
else
  pass=$((pass + 1)); echo "ok   U3b the obsolete entry was retired"
fi

# The escape hatch: explicit, and it works.
r="$(new_repo "update_grandfather_hatch")"
plant "$r" "src/irreducible.rs" 1600
set_baseline "$r"
commit "$r" "a crossing someone has decided to grandfather deliberately"
want_update "U4 --grandfather admits the named path" \
  expect-pass "$r" "baseline updated" --grandfather src/irreducible.rs
if grep -q '^1600 src/irreducible.rs$' "$r/scripts/file-size-baseline.txt"; then
  pass=$((pass + 1)); echo "ok   U4b the deliberately grandfathered entry was written"
else
  fail=$((fail + 1)); echo "FAIL U4b --grandfather did not write the entry"
fi

# ...and it is scoped to the path it names, so passing it does not open the
# door for everything else over the line in the same run.
r="$(new_repo "update_grandfather_is_scoped")"
plant "$r" "src/irreducible.rs" 1600
plant "$r" "src/other.rs" 1700
set_baseline "$r"
commit "$r" "two crossings, one of them decided"
want_update "U5 --grandfather does not admit the paths it did not name" \
  expect-fail "$r" "src/other.rs" --grandfather src/irreducible.rs

# A hatch that can be passed pointlessly stops meaning anything.
r="$(new_repo "update_grandfather_pointless")"
plant "$r" "src/small.rs" 10
set_baseline "$r"
commit "$r" "nothing is over the limit"
want_update "U6 --grandfather naming a path that needs no entry is an error" \
  expect-fail "$r" "need no new entry" --grandfather src/small.rs

# Bootstrap: with no baseline at all there is no prior decision to contradict,
# and the checker itself tells a fresh tree to run --update to create one.
r="$(new_repo "update_bootstrap")"
rm -f "$r/scripts/file-size-baseline.txt"
plant "$r" "src/big.rs" 1600
commit "$r" "a tree with no baseline yet"
want_update "U7 --update bootstraps a missing baseline without refusing" \
  expect-pass "$r" "baseline updated"

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
