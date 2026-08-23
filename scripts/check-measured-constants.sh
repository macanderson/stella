#!/usr/bin/env bash
#
# Guard: a constant set by a measurement must be pinned by a test.
# See #2495.
#
# A constant that was chosen by measuring something carries nothing that says
# so to the compiler, so a merge can revert it in total silence. That is not
# hypothetical — it shipped to `main` on 2026-08-09:
#
#   * #2414 moved a triage latency ceiling from 10s to 30s after measuring
#     that the 10s bound sat INSIDE the answering distribution: 27 of 34
#     triage calls burned the full 10,000ms and returned nothing, and the 7
#     that answered took 4,684–8,587ms.
#   * #2462, a large and otherwise good PR, rewrote the surrounding struct
#     literal from a branch that predated it and carried `from_secs(10)` over
#     the top.
#   * Git reported NO conflict, because one side simply did not contain the
#     other's lines. No test failed, because no test asserted the value. The
#     field's own doc comment — which explained the move and named its
#     7-sample caveat — was left describing a number the struct no longer had.
#   * It was found only because someone tried to start the follow-up issue
#     (#2429) that depends on the 30s ceiling.
#
# This is the same shape scripts/check-deleted-tests.sh exists for (#1976,
# #1860) — a textually clean merge where one branch's change is silently absent
# from the result — but for VALUES rather than tests.
#
# ── The marker ───────────────────────────────────────────────────────────────
#
# #2495 weighed three answers and this is its option 2: give a measured
# constant a recognisable form, and fail the gate when one has no assertion
# behind it. The form is a doc-comment line:
#
#     /// MEASURED: 34 triage calls on 2026-08-09; 27 burned the full 10s and
#     /// returned nothing, the 7 that answered took 4,684–8,587ms.
#     const TRIAGE_LATENCY_CEILING: Duration = Duration::from_secs(30);
#
# The marker is a claim about where the number came from, so it is prose and a
# reviewer reads it; what this guard enforces is the part a reviewer cannot
# check by eye — that some test names the constant, so a revert stops being
# silent. `check-left-behind.sh` is the idiom being copied: a marker that names
# nothing is the residue of a rule nobody enforced.
#
# ── What it checks ───────────────────────────────────────────────────────────
#
#   1. Every `MEASURED:` marker is attached to a `const` or `static` item —
#      a marker floating above a function or a struct is attached to nothing
#      the guard can follow, and reads as covered when it is not.
#   2. Every marker says something. A bare `/// MEASURED:` records no
#      measurement and is worse than no marker, because it looks like one.
#   3. Every marked constant's identifier appears in TEST CODE somewhere in
#      the workspace.
#
# ── How "test code" is decided, and what that approximates ───────────────────
#
# A region is test code when it is a file under a `tests/` directory, a file
# whose name ends `tests.rs`, or the tail of any `.rs` file from its
# `mod tests` line to the end — which is where this workspace puts a unit-test
# module, without exception at the time of writing.
#
# That is broader than "inside a `#[test]` function": a mention in a test
# helper counts. The failure direction is the one to accept — a helper naming
# the constant is still test code that breaks when the value moves, whereas a
# guard that tried to bound each `#[test]` body in awk would miss a constant
# used through a `const`-generic or a shared fixture and cry wolf. What it
# cannot be fooled by is the case that matters: a constant no test mentions at
# all fails, and the fix is to write the assertion.
#
# It deliberately does NOT check that the test pins the value with an
# `assert_eq!`. A test that computes over the constant and asserts an outcome
# is a better pin than one that restates the literal, and no grep tells them
# apart.
#
# ── No baseline ──────────────────────────────────────────────────────────────
#
# There is nothing to grandfather: the marker is new, so a marked constant is
# by construction a constant somebody marked in the same change. An exemption
# list here would only ever be a way to mark a constant and skip the test,
# which is the whole thing being prevented.
#
# Uses portable POSIX tools so it runs on a bare CI runner (macOS ships bash
# 3.2, so no associative arrays).
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

# Scanning an explicit root lets the hermetic self-test point this script at a
# fixture tree instead of the repository it lives in.
scan_root="${1:-.}"
if [ ! -d "$scan_root" ]; then
  echo "check-measured-constants: no such directory: $scan_root" >&2
  exit 1
fi

# The verdict is decided before anything is written (#1815). Failure lines are
# buffered while the checks run and emitted in one final write: a guard that
# prints as it scans dies mid-report when its reader exits early, and under
# `set -euo pipefail` whatever partial state it had reached becomes the exit
# status.
report=""
note() { report="${report}$1"$'\n'; }
emit() {
  trap '' PIPE
  printf '%s' "$report" >&2 || true
}

sources="$(mktemp "${TMPDIR:-/tmp}/stella-measured-src.XXXXXX")"
markers="$(mktemp "${TMPDIR:-/tmp}/stella-measured-marks.XXXXXX")"
tested="$(mktemp "${TMPDIR:-/tmp}/stella-measured-tested.XXXXXX")"
trap 'rm -f "$sources" "$markers" "$tested"' EXIT INT TERM

# Only `.rs`, which is also what keeps this script and its self-test out of
# their own scan: both necessarily spell the marker in prose, and a guard that
# fails on its own documentation is a guard someone deletes.
find "$scan_root" -name '*.rs' -type f \
  ! -path '*/target/*' ! -path '*/.git/*' \
  | LC_ALL=C sort >"$sources"

if [ ! -s "$sources" ]; then
  echo "check-measured-constants: OK — no Rust sources under $scan_root."
  exit 0
fi

# ── Pass one: the markers, and the item each is attached to ──────────────────
#
# A marker is a `///` line whose first word is `MEASURED:`. The item it
# describes is the first non-doc, non-attribute line after the doc block, which
# must declare a `const` or a `static`. Emitted as
# "<file>:<line><tab><verdict><tab><identifier>", with `-` for the identifier
# when there is no declaration to take one from.
# shellcheck disable=SC2016 # the awk program is deliberately single-quoted:
# `$0` is awk's own record, not a shell positional.
tr '\n' '\0' <"$sources" | xargs -0 awk '
  FILENAME != previous_file { previous_file = FILENAME; marker_line = 0; text = "" }

  # A marker line: record where it started and what it said. The first one in
  # a doc block owns it; later `///` lines are ordinary prose.
  /^[ \t]*\/\/\/[ \t]*MEASURED:/ {
    if (!marker_line) {
      marker_line = FNR
      text = $0
      sub(/^[ \t]*\/\/\/[ \t]*MEASURED:[ \t]*/, "", text)
    }
    next
  }

  # Still inside the doc block or the attributes under it: keep looking for
  # the item the marker describes.
  marker_line && /^[ \t]*(\/\/\/|\/\/!|#[[])/ { next }

  marker_line {
    stripped = $0
    sub(/^[ \t]*/, "", stripped)
    sub(/^pub[(][^)]*[)][ \t]+/, "", stripped)
    sub(/^pub[ \t]+/, "", stripped)
    if (stripped ~ /^(const|static)[ \t]+(mut[ \t]+)?[A-Za-z_][A-Za-z0-9_]*[ \t]*:/) {
      name = stripped
      sub(/^(const|static)[ \t]+(mut[ \t]+)?/, "", name)
      sub(/[ \t]*:.*$/, "", name)
      trimmed = text
      gsub(/[ \t]/, "", trimmed)
      if (trimmed == "")
        printf "%s:%d\tEMPTY\t%s\n", FILENAME, marker_line, name
      else
        printf "%s:%d\tOK\t%s\n", FILENAME, marker_line, name
    } else {
      printf "%s:%d\tUNATTACHED\t-\n", FILENAME, marker_line
    }
    marker_line = 0; text = ""
    next
  }

  END { if (marker_line) printf "%s:%d\tUNATTACHED\t-\n", FILENAME, marker_line }
' >"$markers"

# ── Pass two: every identifier mentioned in test code ────────────────────────
#
# One awk over the whole file list rather than one per file: the per-file shape
# is minutes on this tree, and this guard runs in GATE_GUARDS_FAST on every
# push.
# shellcheck disable=SC2016 # awk's `$0`, not the shell's.
tr '\n' '\0' <"$sources" | xargs -0 awk '
  FILENAME != previous_file {
    previous_file = FILENAME
    in_tests = (FILENAME ~ /(^|\/)tests\// || FILENAME ~ /tests[.]rs$/)
  }
  !in_tests && /^[ \t]*(pub[ \t]+)?mod[ \t]+tests[ \t]*[{]?[ \t]*$/ { in_tests = 1 }
  in_tests {
    line = $0
    while (match(line, /[A-Z][A-Z0-9_]*[A-Z0-9]/)) {
      print substr(line, RSTART, RLENGTH)
      line = substr(line, RSTART + RLENGTH)
    }
  }
' | LC_ALL=C sort -u >"$tested"

# ── The verdict ──────────────────────────────────────────────────────────────

fail=0
marked=0

while IFS=$'\t' read -r where verdict name; do
  [ -n "$where" ] || continue
  case "$verdict" in
  UNATTACHED)
    note "check-measured-constants: FAIL — $where"
    note "     A MEASURED: marker that is not directly above a \`const\` or"
    note "     \`static\` names nothing this guard can pin. Move it onto the"
    note "     declaration, or drop it."
    fail=1
    ;;
  EMPTY)
    note "check-measured-constants: FAIL — $where ($name)"
    note "     The marker records no measurement. Write what was measured,"
    note "     when, and over how many samples — a marker that says nothing"
    note "     looks like evidence and is not."
    fail=1
    ;;
  OK)
    marked=$((marked + 1))
    if ! LC_ALL=C grep -qxF -- "$name" "$tested"; then
      note "check-measured-constants: FAIL — $where"
      note "     \`$name\` is marked MEASURED: and no test names it, so a merge"
      note "     that reverts its value fails nothing. That is #2495, and it"
      note "     has already happened once on \`main\` (#2414 reverted by"
      note "     #2462, found weeks later)."
      note "     Write a test that names \`$name\` — asserting the value, or"
      note "     asserting an outcome computed from it. Do not delete the"
      note "     marker; the measurement is the thing worth keeping."
      fail=1
    fi
    ;;
  esac
done <"$markers"

if [ "$fail" -ne 0 ]; then
  emit
  exit 1
fi

scanned=$(wc -l <"$sources" | tr -d ' ')
trap '' PIPE
printf 'check-measured-constants: OK — %s Rust file(s) scanned, %s measured constant(s), each named by a test.\n' \
  "$scanned" "$marked" || true
