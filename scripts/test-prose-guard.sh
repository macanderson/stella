#!/usr/bin/env bash
#
# Tests for check-prose.py's ratchet direction and its citation exemption.
#
#   ./scripts/test-prose-guard.sh
#
# Run it after touching that script. Not part of `make gate`: it builds a
# handful of throwaway git trees, the same posture as
# `make typed-errors-test`.
#
# The guard's promise is one sentence: a (file, pattern) pair may shrink its
# count, never grow it, and a pair absent from the baseline must be at zero. P2
# is the witness
# for that promise -- it is the case that fails before the guard exists and
# passes after. P5 covers the half a ratchet usually gets wrong, which is the
# writer rather than the checker: a `--update` that grandfathers a first-time
# offender turns the gate green without deleting a word, and that is the
# expedient the rule forbids, performed by the guard on the author's behalf.
#
# P7 and P8 pull the other way. A guard that flagged its own rule's examples
# would make CLAUDE.md unwritable, so a backticked span and a fenced block are
# citations and never count.
#
# Fixtures are their own git roots because the guard enumerates with
# `git ls-files`, and because `--update` WRITES -- pointing it at this
# repository would rewrite scripts/prose-baseline.txt as a side effect of
# running the tests.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-prose.py"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

ok() {
  pass=$((pass + 1))
  printf 'ok   %s\n' "$1"
}

no() {
  fail=$((fail + 1))
  printf 'FAIL %s -- %s\n' "$1" "$2"
}

# A throwaway repository root with a scripts/ directory for the baseline.
# $1 = case name. Echoes the root path.
new_root() {
  dir="$TMP/$1"
  mkdir -p "$dir/scripts"
  (cd "$dir" && git init -q .) >/dev/null 2>&1
  echo "$dir"
}

# $1 = root, $2 = relative path, and the body on stdin.
doc() {
  mkdir -p "$(dirname "$1/$2")"
  cat >"$1/$2"
  (cd "$1" && git add -A) >/dev/null 2>&1
}

# The same, but deliberately NOT `git add`ed — the #4952 direction.
# $1 = root, $2 = relative path, body on stdin.
doc_untracked() {
  mkdir -p "$(dirname "$1/$2")"
  cat >"$1/$2"
}

# $1 = root, then `path pattern count` triples. Also writes an empty density
# baseline, so a case that says nothing about header length holds every unit to
# the new-unit ceiling rather than tripping the missing-file refusal.
baseline() {
  root="$1"
  shift
  printf '# test baseline\n' >"$root/scripts/prose-baseline.txt"
  if [ ! -f "$root/scripts/prose-density-baseline.txt" ]; then
    printf '# test density baseline\n' >"$root/scripts/prose-density-baseline.txt"
  fi
  while [ $# -gt 2 ]; do
    printf '%s %s %s\n' "$1" "$2" "$3" >>"$root/scripts/prose-baseline.txt"
    shift 3
  done
}

# $1 = root, then `unit mean` pairs.
density_baseline() {
  root="$1"
  shift
  printf '# test density baseline\n' >"$root/scripts/prose-density-baseline.txt"
  while [ $# -gt 1 ]; do
    printf '%s %s\n' "$1" "$2" >>"$root/scripts/prose-density-baseline.txt"
    shift 2
  done
}

# A Rust file whose module header is $3 lines long, under crate $2.
# $1 = root, $2 = crate name, $3 = header lines, $4 = file stem.
rs_with_header() {
  mkdir -p "$1/crates/$2/src"
  path="$1/crates/$2/src/$4.rs"
  : >"$path"
  i=0
  while [ "$i" -lt "$3" ]; do
    printf '//! A header line about what this file does.\n' >>"$path"
    i=$((i + 1))
  done
  printf '\npub fn f() {}\n' >>"$path"
  (cd "$1" && git add -A) >/dev/null 2>&1
}

# $1 = case name, $2 = root. The guard must exit 0.
expect_pass() {
  if python3 "$SCRIPT" "$2" >/dev/null 2>&1; then
    ok "$1"
  else
    no "$1" "the guard exited non-zero"
  fi
}

# $1 = case name, $2 = root. The guard must exit non-zero.
expect_fail() {
  if python3 "$SCRIPT" "$2" >/dev/null 2>&1; then
    no "$1" "the guard exited 0"
  else
    ok "$1"
  fi
}

# $1 = case name, $2 = root. `--update` must refuse.
expect_update_refused() {
  if python3 "$SCRIPT" --update "$2" >/dev/null 2>&1; then
    no "$1" "--update exited 0"
  else
    ok "$1"
  fi
}

# $1 = case name, $2 = root, $3 = path that must NOT be in the baseline.
expect_absent() {
  if grep -q "^$3 " "$2/scripts/prose-baseline.txt" 2>/dev/null; then
    no "$1" "$3 is listed in the baseline"
  else
    ok "$1"
  fi
}

# $1 = case name, $2 = root, $3 = a line that must be in the baseline.
expect_line() {
  if grep -qx "$3" "$2/scripts/prose-baseline.txt" 2>/dev/null; then
    ok "$1"
  else
    no "$1" "the baseline has no line \"$3\""
  fi
}

# ── P1: clean prose passes ───────────────────────────────────────────────────
r="$(new_root p1)"
doc "$r" docs/a.md <<'EOF'
The store keys every child table off `executions.id`.
EOF
baseline "$r"
expect_pass "P1 clean tree passes" "$r"

# ── P2: a first-time construction fails (the witness) ────────────────────────
r="$(new_root p2)"
doc "$r" docs/a.md <<'EOF'
Two things follow from that, and the second is the hard one.
EOF
baseline "$r"
expect_fail "P2 first-time construction fails" "$r"

# ── P3: a grandfathered count passes ─────────────────────────────────────────
r="$(new_root p3)"
doc "$r" docs/a.md <<'EOF'
Two things follow from that.
EOF
baseline "$r" docs/a.md enumerative-announcement 1
expect_pass "P3 grandfathered count passes" "$r"

# ── P4: growing past the baseline fails ──────────────────────────────────────
r="$(new_root p4)"
doc "$r" docs/a.md <<'EOF'
Two things follow from that.
Both halves matter here.
EOF
baseline "$r" docs/a.md enumerative-announcement 1
expect_fail "P4 growth past baseline fails" "$r"

# ── P5: --update refuses to grandfather a first-time offender ────────────────
# Two assertions, because the defect this guards against failed both ways: a
# writer can refuse loudly and still write the entry.
r="$(new_root p5)"
doc "$r" docs/a.md <<'EOF'
Two things follow from that.
EOF
baseline "$r"
expect_update_refused "P5 --update refuses to raise" "$r"
expect_absent "P5 --update wrote no entry" "$r" "docs/a.md"

# ── P6: --update retightens when prose is deleted ────────────────────────────
r="$(new_root p6)"
doc "$r" docs/a.md <<'EOF'
The order is required, not stylistic.
EOF
baseline "$r" docs/a.md empty-epigram 3
if python3 "$SCRIPT" --update "$r" >/dev/null 2>&1; then
  ok "P6 --update accepts a shrink"
else
  no "P6 --update accepts a shrink" "--update exited non-zero"
fi
expect_absent "P6 --update drops a file that reached zero" "$r" "docs/a.md"

# ── P7: a backticked citation is not a use ───────────────────────────────────
r="$(new_root p7)"
doc "$r" docs/a.md <<'EOF'
Never open a section with `Two things follow` or `Both halves matter`.
A span may also wrap, like `Two things
follow`, and still be a citation.
EOF
baseline "$r"
expect_pass "P7 backticked citation passes" "$r"

# ── P8: a fenced block is not prose ──────────────────────────────────────────
r="$(new_root p8)"
doc "$r" docs/a.md <<'EOF'
Bad openers, for reference:

```text
Two things follow from that.
Both halves matter.
```
EOF
baseline "$r"
expect_pass "P8 fenced block passes" "$r"

# ── P9: banned vocabulary fails ──────────────────────────────────────────────
# The witness for the vocabulary patterns: honesty declarations, interface
# jargon, and Rust terms in prose. Fails before those patterns exist, passes
# after.
r="$(new_root p9)"
doc "$r" docs/a.md <<'EOF'
The honest number is the one the TUI shows for each enum variant.
EOF
baseline "$r"
expect_fail "P9 banned vocabulary fails" "$r"

# ── P10: one file cannot trade one pattern's fix for another's growth ────────
# The witness for the per-pattern baseline. Under a single per-file total this
# tree passes: one construction was deleted and one was added, so the count is
# unchanged. Per pattern it fails, because `empty-epigram` grew from zero.
r="$(new_root p10)"
doc "$r" docs/a.md <<'EOF'
The order is required, not decoration.
EOF
baseline "$r" docs/a.md enumerative-announcement 1
expect_fail "P10 a swap between patterns fails" "$r"

# ── P11: --adopt records one pattern's debt and nothing else ─────────────────
# Adding a pattern to PATTERNS is the one case a count in the baseline
# legitimately goes up: the prose predates the check. --adopt is that door, and
# it must not touch any other pattern's number on the way through.
r="$(new_root p11)"
doc "$r" docs/a.md <<'EOF'
Two things follow from that.
The order is deliberately required.
EOF
baseline "$r" docs/a.md enumerative-announcement 5
if python3 "$SCRIPT" --adopt=filler-adverb "$r" >/dev/null 2>&1; then
  ok "P11 --adopt accepts a new pattern"
else
  no "P11 --adopt accepts a new pattern" "--adopt exited non-zero"
fi
expect_line "P11 --adopt records the new pattern" "$r" "docs/a.md filler-adverb 1"
expect_line "P11 --adopt leaves other patterns alone" "$r" \
  "docs/a.md enumerative-announcement 5"

# ── P12: --adopt refuses a pattern already in the baseline ──────────────────
# Once per pattern. A second adoption would be a raise wearing the name of the
# one legitimate exception.
r="$(new_root p12)"
doc "$r" docs/a.md <<'EOF'
The order is deliberately required, and deliberately documented.
EOF
baseline "$r" docs/a.md filler-adverb 1
if python3 "$SCRIPT" --adopt=filler-adverb "$r" >/dev/null 2>&1; then
  no "P12 --adopt refuses a second adoption" "--adopt exited 0"
else
  ok "P12 --adopt refuses a second adoption"
fi
expect_line "P12 --adopt wrote nothing" "$r" "docs/a.md filler-adverb 1"

# ── P13: --adopt rejects a name that is not a pattern ────────────────────────
r="$(new_root p13)"
doc "$r" docs/a.md <<'EOF'
The store keys every child table off `executions.id`.
EOF
baseline "$r"
if python3 "$SCRIPT" --adopt=no-such-pattern "$r" >/dev/null 2>&1; then
  no "P13 --adopt rejects an unknown pattern" "--adopt exited 0"
else
  ok "P13 --adopt rejects an unknown pattern"
fi

# A brand-new file the author has not staged yet is the file most likely to
# carry new prose, and it was invisible: `git ls-files` lists tracked files
# only, so the guard reported OK — which reads exactly like "I read it and
# found nothing". CI never agreed, because on a PR branch the file is
# committed (#4952).
r="$(new_root P14)"
doc "$r" docs/tracked.md <<'EOF'
The store keys every child table off `executions.id`.
EOF
doc_untracked "$r" docs/untracked.md <<'EOF'
Two things stated rather than hidden: the guard cannot see this file.
EOF
baseline "$r"
if python3 "$SCRIPT" "$r" >/dev/null 2>&1; then
  no "P14 an unstaged file is scanned" "the guard passed a file it never read"
else
  ok "P14 an unstaged file is scanned"
fi

# The other direction, so P14 cannot pass by scanning everything: a file
# `.gitignore` covers stays out. `--exclude-standard` is what keeps a build
# directory or a session scratch file from failing the gate.
r="$(new_root P15)"
printf 'ignored/\n' >"$r/.gitignore"
doc "$r" docs/tracked.md <<'EOF'
The store keys every child table off `executions.id`.
EOF
doc_untracked "$r" ignored/scratch.md <<'EOF'
Two things stated rather than hidden: this one is ignored and must stay out.
EOF
baseline "$r"
if python3 "$SCRIPT" "$r" >/dev/null 2>&1; then
  ok "P15 a gitignored file stays out of the scan"
else
  no "P15 a gitignored file stays out of the scan" "the guard read an ignored file"
fi

# ── The density ratchet (#4760) ─────────────────────────────────────────────
# A different question from every case above: not whether a sentence is
# content-free, but whether there are too many of them. D1 is its witness --
# a crate whose headers grew past what it recorded fails, and nothing in the
# count ratchet can see that, because a forty-line header of unobjectionable
# sentences scores zero there.

r="$(new_root d1)"
baseline "$r"
density_baseline "$r" crates/alpha 10.00
rs_with_header "$r" alpha 20 lib
expect_fail "D1 a unit whose headers grew fails" "$r"

r="$(new_root d2)"
baseline "$r"
density_baseline "$r" crates/alpha 20.00
rs_with_header "$r" alpha 20 lib
expect_pass "D2 a unit at its ceiling passes" "$r"

# The mean, not the worst file: one long header is paid for by short siblings,
# which is what makes the ceiling a density measure rather than a file-size one.
r="$(new_root d3)"
baseline "$r"
density_baseline "$r" crates/alpha 10.00
rs_with_header "$r" alpha 28 long
rs_with_header "$r" alpha 4 short_a
rs_with_header "$r" alpha 4 short_b
rs_with_header "$r" alpha 4 short_c
expect_pass "D3 the ceiling is a mean, not a worst case" "$r"

# --update is the half a ratchet usually gets wrong: refusing loudly and
# writing the looser number anyway turns the gate green with no header cut.
r="$(new_root d4)"
baseline "$r"
density_baseline "$r" crates/alpha 10.00
rs_with_header "$r" alpha 20 lib
expect_update_refused "D4 --update refuses to raise a mean" "$r"
if grep -qx "crates/alpha 10.00" "$r/scripts/prose-density-baseline.txt"; then
  ok "D4 --update left the ceiling alone"
else
  no "D4 --update left the ceiling alone" "the ceiling moved"
fi

# Retightening every unit's ceiling on every --update run is what left each
# one sitting at exactly its ceiling with zero headroom, so bare --update
# leaves a shortened header's ceiling alone; only --update --retighten
# reclaims that slack, as its own deliberate pass.
r="$(new_root d5)"
baseline "$r"
density_baseline "$r" crates/alpha 20.00
rs_with_header "$r" alpha 6 lib
if python3 "$SCRIPT" --update "$r" >/dev/null 2>&1; then
  if grep -qx "crates/alpha 20.00" "$r/scripts/prose-density-baseline.txt"; then
    ok "D5 bare --update leaves a shortened header's ceiling alone"
  else
    no "D5 bare --update leaves a shortened header's ceiling alone" "the ceiling moved"
  fi
else
  no "D5 bare --update leaves a shortened header's ceiling alone" "--update exited non-zero"
fi

r="$(new_root d5b)"
baseline "$r"
density_baseline "$r" crates/alpha 20.00
rs_with_header "$r" alpha 6 lib
if python3 "$SCRIPT" --update --retighten "$r" >/dev/null 2>&1; then
  if grep -qx "crates/alpha 6.00" "$r/scripts/prose-density-baseline.txt"; then
    ok "D5b --update --retighten reclaims a shortened header's slack"
  else
    no "D5b --update --retighten reclaims a shortened header's slack" "the ceiling did not fall"
  fi
else
  no "D5b --update --retighten reclaims a shortened header's slack" "--update --retighten exited non-zero"
fi

# A crate with no entry is held to the new-unit ceiling, so a new crate cannot
# arrive carrying essays and grandfather itself the first time --update runs.
r="$(new_root d6)"
baseline "$r"
density_baseline "$r"
rs_with_header "$r" newcrate 30 lib
expect_fail "D6 an unrecorded unit is held to the new-unit ceiling" "$r"
expect_update_refused "D6 --update refuses to record it" "$r"

r="$(new_root d7)"
baseline "$r"
density_baseline "$r"
rs_with_header "$r" newcrate 6 lib
expect_pass "D7 an unrecorded unit within the ceiling passes" "$r"

# --bootstrap-density is one-time, for the reason --bootstrap is: a regenerated
# baseline records today's tree as the ceiling.
r="$(new_root d8)"
baseline "$r"
density_baseline "$r" crates/alpha 10.00
rs_with_header "$r" alpha 20 lib
if python3 "$SCRIPT" --bootstrap-density "$r" >/dev/null 2>&1; then
  no "D8 --bootstrap-density refuses to overwrite" "it exited 0"
else
  ok "D8 --bootstrap-density refuses to overwrite"
fi
if grep -qx "crates/alpha 10.00" "$r/scripts/prose-density-baseline.txt"; then
  ok "D8 --bootstrap-density wrote nothing"
else
  no "D8 --bootstrap-density wrote nothing" "the ceiling moved"
fi

# A missing density baseline is a refusal, never a silent pass: without it
# every unit would be judged against the new-unit ceiling and a real tree would
# fail for a reason that has nothing to do with the change under review.
r="$(new_root d9)"
printf '# test baseline\n' >"$r/scripts/prose-baseline.txt"
rs_with_header "$r" alpha 6 lib
if python3 "$SCRIPT" "$r" >/dev/null 2>&1; then
  no "D9 a missing density baseline refuses" "the guard passed"
else
  ok "D9 a missing density baseline refuses"
fi

# ── R: a moved file takes its debt with it ───────────────────────────────────
#
# The baseline is keyed by path, so before this a rename read as prose someone
# had just written: the entry stayed stranded at the old path and the same
# sentences were judged at zero allowance in their new home. Splitting a module
# then meant rewording comments nobody was editing, and #5420 hand-edited the
# baseline instead — the one move this file exists to make unnecessary.
#
# R1 is the witness: it fails before `--update` learned to ask git what moved.
# R3 and R4 are the direction that must NOT change — carrying a debt forward is
# not forgiving one.

# $1 = root. Commit, so HEAD exists and git can answer what was renamed.
commit_all() {
  (cd "$1" &&
    git config user.email test@example.com &&
    git config user.name "Test" &&
    git add -A &&
    git commit -qm fixture) >/dev/null 2>&1
}

# $1 = root, $2 = old path, $3 = new path. Staged, which is what git needs to
# report a rename rather than a delete beside an untracked file.
move() {
  (cd "$1" && git mv "$2" "$3") >/dev/null 2>&1
}

# $1 = case name, $2 = root. `--update` must succeed.
expect_update_ok() {
  if python3 "$SCRIPT" --update "$2" >/dev/null 2>&1; then
    ok "$1"
  else
    no "$1" "--update exited non-zero"
  fi
}

r="$(new_root r1)"
doc "$r" docs/old.md <<'EOF'
The cost is deliberate: the retention it buys is not worth the residue.
EOF
baseline "$r" docs/old.md filler-adverb 1
commit_all "$r"
move "$r" docs/old.md docs/new.md
expect_fail "R1 a moved file fails the plain check until the tree is updated" "$r"
expect_update_ok "R1 --update accepts the move" "$r"
expect_line "R1 the entry lands at the new path" "$r" "docs/new.md filler-adverb 1"
expect_absent "R1 the old path is gone" "$r" "docs/old.md"
expect_pass "R1 the tree passes once the entry moved" "$r"

# R2: the count carried is the file's own, not a fresh grant. Rewording during
# the move must lower it, and the lower number is what gets written.
#
# The body is long and only one line changes, because the carry rides on git's
# similarity detection: rewrite most of a short file and git reports a delete
# beside an add, no rename to follow, and the entry is correctly not carried.
# That is the honest limit, and it is why a real split — which moves text
# verbatim — carries cleanly while a rewrite does not.
r="$(new_root r2)"
doc "$r" docs/old.md <<'EOF'
The store keys every child table off `executions.id`.
The hub replicates above a durable per-project cursor.
The cost is deliberate, and this clause is deliberately redundant.
Retention is opt-in, and dropping an execution cascades.
Reads never touch a project store.
The canary re-asks the composition questions after the merge.
EOF
baseline "$r" docs/old.md filler-adverb 2
commit_all "$r"
move "$r" docs/old.md docs/new.md
doc "$r" docs/new.md <<'EOF'
The store keys every child table off `executions.id`.
The hub replicates above a durable per-project cursor.
The cost is deliberate, and this clause repeats it.
Retention is opt-in, and dropping an execution cascades.
Reads never touch a project store.
The canary re-asks the composition questions after the merge.
EOF
expect_update_ok "R2 --update accepts a move that also reworded" "$r"
expect_line "R2 the carried count is the lower one" "$r" "docs/new.md filler-adverb 1"

# R3: a move is not an amnesty. Prose ADDED while the file moved is still new
# prose, and `--update` must refuse it exactly as it would in place.
r="$(new_root r3)"
doc "$r" docs/old.md <<'EOF'
The cost is deliberate: the retention it buys is not worth the residue.
EOF
baseline "$r" docs/old.md filler-adverb 1
commit_all "$r"
move "$r" docs/old.md docs/new.md
doc "$r" docs/new.md <<'EOF'
The cost is deliberate, and the second clause is deliberately redundant.
EOF
expect_update_refused "R3 --update refuses prose added while moving" "$r"

# R4: carrying one file's debt must not forgive another file's. A rename
# alongside a first-time offender is still refused, on the offender.
r="$(new_root r4)"
doc "$r" docs/old.md <<'EOF'
The cost is deliberate: the retention it buys is not worth the residue.
EOF
baseline "$r" docs/old.md filler-adverb 1
commit_all "$r"
move "$r" docs/old.md docs/new.md
doc "$r" docs/fresh.md <<'EOF'
Two things follow from that, and the second is the hard one.
EOF
expect_update_refused "R4 a rename does not grandfather an unrelated file" "$r"

# R5: no HEAD to diff against — a fresh repository with nothing committed.
# The rename map fails open, so the guard behaves exactly as it did before it
# could ask: a broken or absent git never turns a red gate green.
r="$(new_root r5)"
doc "$r" docs/a.md <<'EOF'
Two things follow from that, and the second is the hard one.
EOF
baseline "$r"
expect_update_refused "R5 an unaskable git still refuses a first-time offender" "$r"

# ── B: a unit's density is judged against the base tree too ─────────────────
#
# `--update` retightens every unit to its current value on every run, so a
# crate sits at exactly its ceiling with zero headroom the moment anyone runs
# it. B1 is the witness for the fix: a unit already over its recorded
# ceiling in the tree this branch started from must not fail a branch that
# never touched it. B2 is the mercy's boundary -- growing the header further
# still fails. B3 is `--absolute`, which the post-merge canary needs: it
# must see the same drift B1 forgives, or it stops being able to catch drift
# that already reached `main`.
#
# $1 = root. Point refs/remotes/origin/main at the current HEAD, so
# merge-base has something to compare against without a real remote.
mark_as_main() {
  (cd "$1" && git update-ref refs/remotes/origin/main HEAD) >/dev/null 2>&1
}

r="$(new_root b1)"
density_baseline "$r" crates/alpha 10.00
rs_with_header "$r" alpha 25 lib
commit_all "$r"
mark_as_main "$r"
doc "$r" docs/unrelated.md <<'EOF'
The store keys every child table off `executions.id`.
EOF
baseline "$r"
commit_all "$r"
expect_pass "B1 inherited drift on the base tree does not fail an untouched unit" "$r"

r="$(new_root b2)"
density_baseline "$r" crates/alpha 10.00
rs_with_header "$r" alpha 25 lib
commit_all "$r"
mark_as_main "$r"
rs_with_header "$r" alpha 30 lib
commit_all "$r"
expect_fail "B2 growing a header past even the base tree's mean still fails" "$r"

r="$(new_root b3)"
density_baseline "$r" crates/alpha 10.00
rs_with_header "$r" alpha 25 lib
commit_all "$r"
mark_as_main "$r"
doc "$r" docs/unrelated.md <<'EOF'
The store keys every child table off `executions.id`.
EOF
baseline "$r"
commit_all "$r"
if python3 "$SCRIPT" --absolute "$r" >/dev/null 2>&1; then
  no "B3 --absolute still sees inherited drift" "the guard passed"
else
  ok "B3 --absolute still sees inherited drift"
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
