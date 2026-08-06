#!/usr/bin/env bash
#
# Guard: a deferral marker in code must name the issue that tracks it.
# See #1454, and AGENTS.md § "Nothing left behind".
#
# AGENTS.md and CLAUDE.md require that anything noticed and not fixed inside the
# current change becomes a GitHub issue written as a handoff. That is the right
# rule and, until this script, nothing checked it. Every other standard this
# repository cares about has a gate behind it, for the reason stated in the
# header of scripts/check-file-size.sh: an unenforced standard reads as a
# standard the codebase is meeting. Three documents claimed a 1500-line cap
# while 31 files exceeded it.
#
# "Did the author notice something and not file it" is not mechanically
# decidable. Its most common residue is: a marker with no issue beside it. That
# is what this checks, and the scope is deliberately narrow — a marker naming an
# issue is *tracked work*, which is fine; a marker naming nothing is a thing
# left behind with no handoff.
#
# ── What counts as a marker ──────────────────────────────────────────────────
#
# TODO, FIXME, XXX or HACK, in a comment, not followed by an issue reference
# (`#` + digits) anywhere on the same line. Both of these are fine:
#
#     // TODO(#1454): wire the baseline into the hook
#     // FIXME: this is quadratic — see #1454
#
# and this is not:
#
#     // TODO: come back to this
#
# ── Why "in a comment" is defined the way it is ──────────────────────────────
#
# The word appears in this tree six times in code that is not a deferral at all:
# a doc comment describing a task node ("a unit of work (issue, ticket, TODO)"),
# a `code_map` search-pattern fixture, a store rule named "never commit a TODO",
# and three test fixtures whose *string literals* contain `// TODO:` because the
# thing under test is a tool that reads code. A guard that flagged those would
# be trained away within a week.
#
# So a marker is recognised only when:
#
#   A. it is the first word of a comment (`// TODO`, `# TODO`, `* TODO`), or
#   B. it follows a comment opener on a line with no quote character at all,
#      which catches `let x = 1; // TODO: fix` without ever looking inside a
#      string.
#
# Clause B is why the quoting rule is "no quotes on the line" rather than
# something cleverer: a shell script cannot reliably tell whether a `//` is
# inside a Rust string, and a guard that guesses wrong fails open in one
# direction and cries wolf in the other. A trailing marker on a line that also
# contains a string literal is the one shape this misses; it is rare, and the
# PR-template checkbox is what covers the judgment half regardless.
#
# ── The baseline ─────────────────────────────────────────────────────────────
#
# Modelled on scripts/check-file-size.sh, including its shrink-only property:
# an entry records how many untracked markers a file carries, a file may never
# exceed its recorded count, and once a count reaches zero the entry is obsolete
# and must be removed. An exemption must not outlive the problem it covered.
#
# It starts EMPTY, which is worth stating: unlike the file-size ratchet, this
# guard had nothing to grandfather. Every marker in the tree at the time it was
# written already named an issue or was one of the six false positives above.
# Keep it that way — the baseline exists for a future import, not as a hatch.
#
# Uses portable POSIX tools so it runs on a bare CI runner (macOS ships bash 3.2,
# so no associative arrays).
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

baseline="scripts/left-behind-baseline.txt"

if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "check-left-behind: not a git repository; skipping."
  exit 0
fi

# Emit "<count> <path>" for every tracked source file carrying at least one
# untracked marker. Prose is not scanned: a design document listing open
# questions is doing its job, and `docs/` has its own guards.
#
# One awk over the whole file list, not one awk per file. The per-file shape
# read the same tree in ~40s, which is too slow for a guard that runs on every
# push in GATE_GUARDS_FAST; this is well under a second. `/dev/null` is appended
# so awk always has at least one file argument — with an empty list it would
# read stdin and hang, and xargs runs the utility even when its input is empty.
#
# This script and its baseline are skipped by name: both necessarily spell the
# markers in prose, and a guard that fails on its own documentation is a guard
# someone deletes. Nothing else is exempt.
current_counts() {
  git ls-files -z '*.rs' '*.py' '*.sh' '.githooks/*' |
    xargs -0 awk '
      FILENAME == "scripts/check-left-behind.sh"      { next }
      FILENAME == "scripts/left-behind-baseline.txt"  { next }

      # An issue reference anywhere on the line means the work is tracked.
      /#[0-9]+/ { next }

      # A. the marker is the first word of a comment.
      /^[ \t]*(\/\/+!?|#+|\*+|\/\*)[ \t]*(TODO|FIXME|XXX|HACK)([ \t:(),.!-]|$)/ { n[FILENAME]++; next }

      # B. a trailing marker on a line that contains no string at all.
      /["'"'"'`]/ { next }
      /(\/\/|#)[ \t]*(TODO|FIXME|XXX|HACK)([ \t:(),.!-]|$)/ { n[FILENAME]++ }

      END { for (f in n) if (n[f] > 0) printf "%d %s\n", n[f], f }
    ' /dev/null
}

if [ "${1:-}" = "--update" ]; then
  {
    echo "# Files carrying deferral markers (TODO/FIXME/XXX/HACK) that name no"
    echo "# issue. See #1454 and scripts/check-left-behind.sh."
    echo "# Format: <count> <path>."
    echo "#"
    echo "# Regenerate with: make left-behind-update"
    echo "#"
    echo "# This file started empty and is meant to stay that way. An entry may"
    echo "# only ever shrink: a file must not exceed its recorded count, and a"
    echo "# count that reaches zero must be removed. The fix for a marker is to"
    echo "# file the issue and reference it — not to add a line here."
    # LC_ALL=C for byte-order collation, so the baseline regenerates
    # identically on every machine (see check-file-size.sh for the macOS case).
    current_counts | LC_ALL=C sort -k2
  } >"$baseline.tmp"
  mv "$baseline.tmp" "$baseline"
  echo "check-left-behind: baseline updated — $(grep -cv '^#' "$baseline" || true) file(s) with untracked markers."
  exit 0
fi

if [ ! -f "$baseline" ]; then
  echo "check-left-behind: missing $baseline. Run 'make left-behind-update' to create it." >&2
  exit 1
fi

report="$(
  {
    grep -v '^#' "$baseline" | sed 's/^/B /'
    current_counts | sed 's/^/C /'
  } | awk '
    $1 == "B" { allowed[$3] = $2; known[$3] = 1; next }
    $1 == "C" {
      path = $3; n = $2; seen[path] = 1
      if (path in allowed) {
        if (n > allowed[path])
          grew = grew sprintf("  %s now has %d untracked marker(s), over its baseline of %d\n", path, n, allowed[path])
      } else {
        newmark = newmark sprintf("  %s has %d untracked marker(s)\n", path, n)
      }
    }
    END {
      for (p in known)
        if (!(p in seen))
          obsolete = obsolete sprintf("  %s no longer has any — drop its baseline entry\n", p)
      if (newmark) printf "NEW\n%s", newmark
      if (grew) printf "GREW\n%s", grew
      if (obsolete) printf "OBSOLETE\n%s", obsolete
    }
  '
)"

if [ -n "$report" ]; then
  echo "check-left-behind: FAILED" >&2
  echo "$report" | awk '
    /^NEW$/      { print ""; print "These files carry a TODO/FIXME/XXX/HACK that names no issue."; print ""; print "The fix is to FILE THE ISSUE and reference it — not to delete the marker,"; print "and not to add a baseline entry. Write it as a handoff a fresh agent could"; print "execute: the problem, the file paths, how to verify, what done looks like"; print "(AGENTS.md, \"Nothing left behind\"). Then:"; print ""; print "    // TODO(#1234): the thing you deferred"; print ""; print "A marker that names an issue is tracked work and passes this guard."; next }
    /^GREW$/     { print ""; print "These files gained untracked markers beyond their baseline count. Same fix:"; print "file the issue and reference it."; next }
    /^OBSOLETE$/ { print ""; print "Obsolete baseline entries — the markers are gone or now reference an issue."; print "Run \"make left-behind-update\": an exemption must not outlive the problem it"; print "covered, or the file is free to accumulate again."; next }
    { print }
  ' >&2
  exit 1
fi

scanned=$(git ls-files '*.rs' '*.py' '*.sh' '.githooks/*' | wc -l | tr -d ' ')
carried=$(grep -cv '^#' "$baseline" || true)
echo "check-left-behind: OK — $scanned source file(s) scanned, $carried grandfathered."
