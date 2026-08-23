#!/usr/bin/env bash
#
# Tests for the deck enumeration and its pass/skip/fail accounting (#3404).
#
#   ./scripts/test-deck-fit-all.sh
#
# Hermetic: a fixture tree in $TMPDIR and a `node` stub on PATH whose exit
# status is derived from the deck's filename. No browser, so unlike
# .github/workflows/deck-fit.yml this is a `make gate` step.
#
# ── Why this suite exists ────────────────────────────────────────────────────
#
# #3376 was an enumeration bug: a recursive trigger paired with a
# non-recursive glob, so the workflow started on a deck it then never
# measured. #3403 fixed it against a throwaway fixture tree that was deleted
# afterwards, leaving nothing to catch the same class again -- the enumeration
# silently covering fewer files than the trigger implies. That is the third
# occurrence of one shape (#2425, #3376), and the third one should be caught
# by a check rather than by somebody noticing.
#
# A committed always-failing fixture deck is not the alternative: anything
# under website/public/presentations/ is measured by construction, so it would
# red-line the real job. The fixture therefore lives outside the repository,
# which is why scripts/deck-fit-all.sh enumerates an untracked root with
# `find` -- an affordance for exactly this test, stated in that script.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
subject="$repo_root/scripts/deck-fit-all.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

ok() { pass=$((pass + 1)); echo "ok   $1"; }
no() {
  fail=$((fail + 1))
  echo "FAIL $1"
  shift
  [ $# -gt 0 ] && printf '%s\n' "$@"
  return 0
}

# A `node` stub, first on PATH. It is handed the measurer path and the deck, so
# it reads $2 -- and its exit status comes from the deck's basename, which is
# what lets one fixture tree exercise every arm of the case statement:
#
#   pass-*.html  -> 0, measured clean
#   skip-*.html  -> 3, "not a fixed-canvas deck"
#   fail-*.html  -> 1, overflowed
#
# It also appends every deck it was handed to $TMP/seen, which is how the
# enumeration itself (rather than the summary line's arithmetic) is asserted.
stub_bin="$TMP/bin"
mkdir -p "$stub_bin"
cat >"$stub_bin/node" <<'EOF'
#!/bin/sh
deck="$2"
printf '%s\n' "$deck" >>"$SEEN"
case "$(basename "$deck")" in
  skip-*) exit 3 ;;
  fail-*) exit 1 ;;
  *)      exit 0 ;;
esac
EOF
chmod +x "$stub_bin/node"

# run <fixture-root> -> stdout+stderr in $out, status in $rc, decks in $seen
out=""
rc=0
seen=""
run() {
  : >"$TMP/seen"
  # `bash -e`, because that is the workflow's default shell and the loop's
  # `|| rc=$?` guard exists only to survive it. Running the subject under a
  # forgiving shell here would pass on a script that aborts in production.
  out="$(SEEN="$TMP/seen" PATH="$stub_bin:$PATH" bash -e "$subject" "$1" 2>&1)"
  rc=$?
  seen="$(cat "$TMP/seen" 2>/dev/null)"
}

# ── A: an untracked tree ─────────────────────────────────────────────────────

A="$TMP/decks"
mkdir -p "$A/nested/assets"
: >"$A/pass-top.html"
: >"$A/nested/pass-nested.html"
: >"$A/nested/assets/skip-note.html"
: >"$A/nested/fail-clipped.html"
: >"$A/not-a-deck.txt"

run "$A"

case "$seen" in
*"/nested/pass-nested.html"*) ok "A1 a deck in a subdirectory is enumerated (#3376)" ;;
*) no "A1 a deck in a subdirectory is enumerated (#3376)" "$seen" ;;
esac

case "$seen" in
*"/pass-top.html"*) ok "A2 a top-level deck is still enumerated" ;;
*) no "A2 a top-level deck is still enumerated" "$seen" ;;
esac

case "$seen" in
*"/nested/assets/skip-note.html"*) ok "A3 the walk reaches the second level down" ;;
*) no "A3 the walk reaches the second level down" "$seen" ;;
esac

# The failing deck is third of four in sort order, so a loop that aborts on it
# never reaches the fourth. This is the assertion `bash -e` is run for.
if [ "$(printf '%s\n' "$seen" | grep -c .)" -eq 4 ]; then
  ok "A4 a failing deck does not abort the loop -- all four were measured"
else
  no "A4 a failing deck does not abort the loop -- all four were measured" "$seen"
fi

case "$out" in
*"SKIPPED"*"skip-note.html"*) ok "A5 an exit-3 deck is reported as a skip, by path" ;;
*) no "A5 an exit-3 deck is reported as a skip, by path" "$out" ;;
esac

case "$out" in
*"4 file(s) found, 3 measured, 1 skipped"*) ok "A6 the summary counts skips separately from measurements" ;;
*) no "A6 the summary counts skips separately from measurements" "$out" ;;
esac

if [ "$rc" -ne 0 ]; then
  ok "A7 a deck that overflows fails the run"
else
  no "A7 a deck that overflows fails the run" "$out"
fi

# ── B: a skip is not a failure ───────────────────────────────────────────────
#
# The pair to A7. A guard that failed on any non-zero measurer status would
# satisfy A7 and turn every non-deck HTML file under the tree into a red job,
# which is what the exit-3 classification exists to prevent.

B="$TMP/skips"
mkdir -p "$B/sub"
: >"$B/pass-one.html"
: >"$B/sub/skip-two.html"

run "$B"
if [ "$rc" -eq 0 ]; then
  ok "B1 a skip alongside a clean deck exits 0"
else
  no "B1 a skip alongside a clean deck exits 0" "$out"
fi
case "$out" in
*"2 file(s) found, 1 measured, 1 skipped"*) ok "B2 the skip is still counted and printed" ;;
*) no "B2 the skip is still counted and printed" "$out" ;;
esac

# ── C: an empty tree ─────────────────────────────────────────────────────────
#
# The failure mode the whole file guards: measuring nothing and saying nothing.

C="$TMP/empty"
mkdir -p "$C"
run "$C"
if [ "$rc" -eq 1 ]; then
  ok "C1 an empty tree exits 1 rather than reporting success"
else
  no "C1 an empty tree exits 1 rather than reporting success" "rc=$rc" "$out"
fi

run "$TMP/does-not-exist"
if [ "$rc" -eq 1 ]; then
  ok "C2 a missing root exits 1 and names the path"
else
  no "C2 a missing root exits 1 and names the path" "rc=$rc" "$out"
fi

# ── D: the tracked path, which is what CI actually runs ──────────────────────
#
# A and B exercise the `find` branch. Production takes the `git ls-files`
# pathspec, and the #3376 bug was specifically a non-recursive enumeration --
# so the recursion is asserted on that branch too, or the regression could
# come back on the only branch that matters.

D="$TMP/tracked"
mkdir -p "$D/deep/deeper"
: >"$D/pass-root.html"
: >"$D/deep/pass-mid.html"
: >"$D/deep/deeper/pass-bottom.html"
git -C "$D" init -q
git -C "$D" config user.email test@example.com
git -C "$D" config user.name test
git -C "$D" add -A >/dev/null 2>&1
git -C "$D" commit -qm decks >/dev/null 2>&1

run "$D"
if [ "$(printf '%s\n' "$seen" | grep -c .)" -eq 3 ]; then
  ok "D1 the git pathspec matches at every depth, not just the top level"
else
  no "D1 the git pathspec matches at every depth, not just the top level" "$seen"
fi

# An untracked deck sitting in a tracked tree is deliberately not measured: CI
# measures what it checked out. Pinning it stops a later "just use find
# everywhere" simplification from quietly changing what the job covers.
: >"$D/pass-untracked.html"
run "$D"
case "$seen" in
*pass-untracked*) no "D2 an untracked file in a tracked tree is not measured" "$seen" ;;
*) ok "D2 an untracked file in a tracked tree is not measured" ;;
esac

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]
