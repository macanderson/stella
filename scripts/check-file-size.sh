#!/usr/bin/env bash
#
# Guard: no NEW Rust, Python or shell file may exceed the 1500-line ratchet.
# See #629 (Rust), #825 (Python — an 8,166-line analysis module had slipped
# through because the guard only looked at *.rs) and #1563 (shell — the
# delivery-loop driver reached ~1,900 lines while no gate could see it).
#
# The language list has now been widened twice for the same reason, which is
# the argument for widening it eagerly rather than after the next incident: a
# limit that watches one language is not a property of the repository, it is a
# property of that language's files, and the growth simply moves to whatever is
# unwatched. Shell was the last substantial unwatched surface here.
#
# Three fleet plans asserted this limit as a standard the tree follows
# (docs/spec/serve-surface.fleet.toml, plus two since-deleted siblings), and
# nothing enforced it. An unenforced limit reads as a standard the codebase is
# meeting, which it is not: at the time this guard was written 31 files already
# exceeded it and crates/stella-tui/src/deck_ui.rs had grown to 6,884 lines — over 4x
# the limit, and larger than the 6,632 the audit recorded weeks earlier. That
# growth *while* three documents claimed a 1500-line cap is the whole argument
# for this script.
#
# Enforcing bare would fail red on day one, so the existing offenders are
# grandfathered in a baseline file recording each one's length as its personal
# ceiling. The result is a ratchet, not a freeze:
#
#   - a file absent from the baseline must be <= LIMIT (blocks new bloat);
#   - a file in the baseline must not exceed its recorded ceiling (blocks
#     further growth of the known offenders);
#   - once a baseline file drops to <= LIMIT its entry is obsolete and must be
#     removed, so the baseline can only ever shrink. Without that rule a file
#     that got fixed would keep its exemption and be free to bloat again.
#
# Only the ceiling direction is enforced, on purpose: a baseline file shrinking
# from 2,400 to 2,100 lines does NOT fail the gate. Demanding a baseline edit for
# every incremental improvement would tax exactly the refactoring work this
# guard exists to encourage (see #458, which decomposes the two worst files).
# Run --update after such work to tighten the ceilings.
#
# What this guard does NOT do is forbid a grandfathered file from ever growing.
# Adding any subcommand to stella-cli, or any module to stella-store, costs a
# handful of irreducible lines in a lib.rs/main.rs that is already over the
# limit; a hard freeze there would make the guard obstruct every feature in the
# worst crates, and the predictable outcome is that someone deletes the guard.
# Instead the ceiling can be raised, but only by running --update and committing
# the baseline diff — so growth of an already-bloated file is impossible to land
# *silently* and shows up as a reviewable line in the PR. That visibility, not
# prohibition, is the enforceable property. New files over the limit remain a
# hard block, which is the rule that actually stops the tree getting worse.
#
# Test files count. A 2,750-line test module is as hard to navigate as any other,
# and exempting tests would be an obvious loophole.
#
# Uses portable POSIX tools so it runs on a bare CI runner (macOS ships bash 3.2,
# so no associative arrays).
set -euo pipefail

LIMIT=1500

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

baseline="scripts/file-size-baseline.txt"

if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "check-file-size: not a git repository; skipping."
  exit 0
fi

# The pathspecs the ratchet watches. Defined once because they are used twice —
# by current_sizes below and by the summary line at the end — and two copies of
# a file selector is how a language gets silently dropped from a guard that
# claims to cover it.
#
# An indexed array, and every element QUOTED at the point of use, because these
# are globs for *git* to match against the index, not for the shell to expand
# against the working directory. Unquoted they are expanded before git ever
# sees them: at the repo root `*.sh` matches only `install.sh`, so the guard
# would have watched exactly one of this tree's 63 shell files while reporting
# that it covered shell. (An indexed array is bash 3.2 safe; the portability
# note above rules out *associative* arrays, which is a different feature.)
#
# `.githooks/*` sits beside `*.sh` because the hook there carries no extension.
# That is the set `make shellcheck` lints, deliberately: this repository
# already treats everything under `.githooks/` as shell, so a non-shell file
# appearing there would break that guard first.
RATCHET_PATHSPECS=('*.rs' '*.py' '*.sh' '.githooks/*')

# Emit "<lines> <path>" for every tracked file in scope, NUL-safe on the git
# side. Python counts (#825): the analyzer under bench/ grew to 8,166 lines
# while the guard watched only *.rs. Shell counts (#1563) for the same reason.
current_sizes() {
  git ls-files -z "${RATCHET_PATHSPECS[@]}" | while IFS= read -r -d '' f; do
    printf '%s %s\n' "$(wc -l <"$f" | tr -d ' ')" "$f"
  done
}

if [ "${1:-}" = "--update" ]; then
  {
    echo "# Grandfathered files over the ${LIMIT}-line ratchet. See #629 and"
    echo "# scripts/check-file-size.sh. Format: <ceiling> <path>."
    echo "#"
    echo "# Regenerate with: make file-size-update"
    echo "#"
    echo "# No entry may be ADDED for a new file: a file that crosses the limit"
    echo "# must be split instead. Raising an existing ceiling is allowed but is"
    echo "# never silent — it lands as a visible diff here, to be justified in"
    echo "# review like any other change."
    # LC_ALL=C: byte-order collation, so the baseline regenerates identically
    # on every machine. A UTF-8 locale sorts punctuation differently (macOS
    # orders agent/tests.rs before agent.rs), which reshuffles untouched lines
    # and buries the one ceiling that actually moved.
    current_sizes | awk -v limit="$LIMIT" '$1 > limit' | LC_ALL=C sort -k2
  } >"$baseline.tmp"
  mv "$baseline.tmp" "$baseline"
  echo "check-file-size: baseline updated — $(grep -cv '^#' "$baseline") grandfathered file(s) over $LIMIT lines."
  exit 0
fi

if [ ! -f "$baseline" ]; then
  echo "check-file-size: missing $baseline. Run 'make file-size-update' to create it." >&2
  exit 1
fi

# Single awk pass: read the baseline into a map, then judge each current file.
# Baseline lines are tagged B, current sizes C, so one awk sees both streams.
report="$(
  {
    grep -v '^#' "$baseline" | sed 's/^/B /'
    current_sizes | sed 's/^/C /'
  } | awk -v limit="$LIMIT" '
    $1 == "B" { ceiling[$3] = $2; next }
    $1 == "C" {
      path = $3; n = $2; seen[path] = 1
      if (path in ceiling) {
        if (n <= limit)
          obsolete = obsolete sprintf("  %s is now %d lines (<= %d) — drop its baseline entry\n", path, n, limit)
        else if (n > ceiling[path])
          grew = grew sprintf("  %s grew to %d lines, over its baseline ceiling of %d (+%d)\n", path, n, ceiling[path], n - ceiling[path])
      } else if (n > limit) {
        newover = newover sprintf("  %s is %d lines, over the %d-line limit\n", path, n, limit)
      }
    }
    END {
      for (p in ceiling)
        if (!(p in seen))
          stale = stale sprintf("  %s (baseline entry, file no longer tracked)\n", p)
      if (newover) printf "NEWOVER\n%s", newover
      if (grew) printf "GREW\n%s", grew
      if (obsolete) printf "OBSOLETE\n%s", obsolete
      if (stale) printf "STALE\n%s", stale
    }
  '
)"

if [ -n "$report" ]; then
  echo "check-file-size: FAILED" >&2
  echo "$report" | awk '
    /^NEWOVER$/  { print ""; print "These files crossed the limit and are NOT grandfathered — split them into"; print "submodules. Do not add a baseline entry: the baseline only covers files"; print "that predate the guard, and this is the rule that stops the tree getting"; print "worse."; next }
    /^GREW$/     { print ""; print "These grandfathered files grew past their recorded ceiling. If the growth is"; print "irreducible (a subcommand or module declaration in an already-oversized"; print "lib.rs/main.rs), run \"make file-size-update\" and commit the baseline diff so"; print "the increase is visible in review. If it is not irreducible, put the new code"; print "in its own module instead."; next }
    /^OBSOLETE$/ { print ""; print "Obsolete baseline entries (the file is now under the limit). Run"; print "\"make file-size-update\" to retire them — an exemption must not outlive"; print "the problem it covered."; next }
    /^STALE$/    { print ""; print "Stale baseline entries. Run \"make file-size-update\"."; next }
    { print }
  ' >&2
  exit 1
fi

tracked=$(git ls-files "${RATCHET_PATHSPECS[@]}" | wc -l | tr -d ' ')
# The verdict is already decided; the write is best-effort. SIGPIPE is ignored
# and the write's failure discarded, so a reader that closed the pipe
# (`| head -1`, `| true`) cannot turn a green verdict into a failure (#1815).
grandfathered=$(grep -cv '^#' "$baseline")
trap '' PIPE
echo "check-file-size: OK — $tracked Rust/Python/shell files, none over $LIMIT lines except $grandfathered grandfathered (none grew)." || true
