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
# ── What the ratchet judges: the CHANGE, not the tree (#2004) ─────────────────
#
# The baseline is a single shared cell that every growing PR must write, and for
# three occurrences running (#1761, #1782, #2003) two PRs that each wrote it
# CORRECTLY composed into a red `main`. Each regenerates the whole baseline
# against a snapshot of `main` that does not yet carry the other's growth, so
# each records a stale ceiling for a file it never touched. The merge is
# textually clean — the two sides edit different lines — and the result is a
# ceiling one line below an actual. `main` then stays red, and every subsequent
# PR inherits a failure it did not cause; #1992 sat blocked on exactly this with
# a tree byte-identical to `main`.
#
# Note what did NOT happen in that composition: the file never grew. Its ceiling
# moved DOWN underneath it. A guard asking "is this tree consistent with this
# baseline snapshot?" cannot tell those apart, because it has only one tree to
# look at. So it asks the per-change question instead, against the base:
#
#     fail only when  current > max(ceiling, size at base)
#
# The `max` is the whole rule, and each half earns its place:
#
#   - `ceiling` alone is today's check, and fails the innocent PR above.
#   - `size at base` alone would pass a file that is already over its ceiling
#     and that THIS change grows further — drift would become a licence to
#     bloat, which is the one outcome worse than the red main.
#
# Taking the larger fails a change that genuinely grows a god file past what it
# inherited, and is silent when the violation arrived from somewhere else. A
# ceiling raised deliberately via `--update` still passes exactly as before:
# the regenerated ceiling equals the current size.
#
# The base is the same pair `scripts/check-deleted-tests.sh` uses, for the same
# reason: on a `pull_request` event the checkout is `refs/pull/N/merge`, so HEAD
# is the merge commit and HEAD^1 is the base branch tip. That asks "does this
# MERGE grow a god file?", which is the question a required check should answer,
# and it needs `fetch-depth: 2` — which ci.yml already sets.
#
# When no base can be resolved the guard falls back to judging the tree, exactly
# as it did before. That direction is deliberate: an unresolvable base must
# never make the ratchet weaker, only stricter.
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

# The commit this change is measured against, or empty for the strict
# whole-tree check. Resolved once; see the header for why the pair matters.
#
# The order is by how precisely each candidate answers "what did this change
# inherit", and every rung is verified to exist before it is taken, so a
# shallow clone or a fresh repository falls through to strict rather than
# erroring.
#
# Every rung tolerates its own failure explicitly rather than leaning on the
# errexit suppression that `|| true` at the call site would grant: this function
# is the one place where "could not resolve" must stay an ordinary answer, and a
# reader should not have to know that rule to see it.
resolve_base_commit() {
  local candidate mb
  # An explicit override wins, for hand runs and for the hermetic tests in
  # scripts/test-file-size.sh, which have no origin to infer one from.
  if [ -n "${FILE_SIZE_BASE_REF:-}" ]; then
    candidate="$(git rev-parse --verify --quiet "${FILE_SIZE_BASE_REF}^{commit}" 2>/dev/null || true)"
    printf '%s' "$candidate"
    return 0
  fi
  # A merge commit means a `refs/pull/N/merge` checkout: HEAD^1 is the base
  # branch tip and HEAD^2 the PR head. This is the CI path on a pull request,
  # and the same pair check-deleted-tests.sh is built on.
  if git rev-parse --verify --quiet "HEAD^2" >/dev/null 2>&1; then
    candidate="$(git rev-parse --verify --quiet "HEAD^1^{commit}" 2>/dev/null || true)"
    printf '%s' "$candidate"
    return 0
  fi
  # A local feature branch: judge every commit on it at once, not just the
  # last. Skipped when the merge base IS HEAD — that means HEAD carries no
  # change of its own relative to `main`, so any violation is inherited drift
  # and the strict read ("main owes a regeneration") is the honest one.
  mb="$(git merge-base HEAD origin/main 2>/dev/null || true)"
  if [ -n "$mb" ] && [ "$mb" != "$(git rev-parse HEAD 2>/dev/null || true)" ]; then
    printf '%s' "$mb"
    return 0
  fi
  # A linear commit with no merge and no origin/main ahead of it — a push to
  # `main` in CI, where the previous commit is exactly what it inherited.
  candidate="$(git rev-parse --verify --quiet "HEAD^1^{commit}" 2>/dev/null || true)"
  printf '%s' "$candidate"
  return 0
}
base_commit="$(resolve_base_commit)"

# Line count of $1 in the base tree, or 0 when the path did not exist there.
# Zero is the fail-closed answer: it makes `max(ceiling, base)` collapse to the
# ceiling, i.e. the strict check.
size_at_base() {
  local n
  n="$(git show "$base_commit:$1" 2>/dev/null | wc -l | tr -d ' ')" || n=""
  [ -n "$n" ] || n=0
  printf '%s\n' "$n"
}

# Single awk pass: read the baseline into a map, then judge each current file.
# Baseline lines are tagged B, current sizes C, so one awk sees both streams.
#
# A file over its ceiling is emitted as a CANDIDATE, not a verdict: awk sees one
# tree and so cannot tell growth from inherited drift. The classification needs
# the base tree and happens below. Candidates are emitted in GREW's position so
# the assembled report keeps its original section order.
raw_report="$(
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
          grew = grew sprintf("GREWCAND %s %d %d\n", path, n, ceiling[path])
      } else if (n > limit) {
        newover = newover sprintf("  %s is %d lines, over the %d-line limit\n", path, n, limit)
      }
    }
    END {
      for (p in ceiling)
        if (!(p in seen))
          stale = stale sprintf("  %s (baseline entry, file no longer tracked)\n", p)
      if (newover) printf "NEWOVER\n%s", newover
      if (grew) printf "%s", grew
      if (obsolete) printf "OBSOLETE\n%s", obsolete
      if (stale) printf "STALE\n%s", stale
    }
  '
)"

# Classify each candidate against the base tree, preserving the stream order so
# the GREW section still lands between NEWOVER and OBSOLETE.
#
# `current > max(ceiling, size at base)` — see the header. With no base commit
# the second term is absent and this is the original whole-tree check.
report=""
drift=""
grew_header_emitted=0
while IFS= read -r line; do
  case "$line" in
  "GREWCAND "*)
    # Deliberate word split: these are this script's own awk-formatted records,
    # and the baseline format has never admitted a path containing a space.
    # shellcheck disable=SC2086
    set -- $line
    cand_path="$2"
    cand_now="$3"
    cand_ceiling="$4"
    cand_effective="$cand_ceiling"
    if [ -n "$base_commit" ]; then
      cand_base="$(size_at_base "$cand_path")"
      if [ "$cand_base" -gt "$cand_effective" ]; then
        cand_effective="$cand_base"
      fi
    fi
    if [ "$cand_now" -gt "$cand_effective" ]; then
      if [ "$grew_header_emitted" -eq 0 ]; then
        report="${report}GREW
"
        grew_header_emitted=1
      fi
      report="$report$(printf '  %s grew to %d lines, over its baseline ceiling of %d (+%d)' \
        "$cand_path" "$cand_now" "$cand_ceiling" "$((cand_now - cand_ceiling))")
"
    else
      drift="$drift$(printf '  %s is %d lines against a ceiling of %d — already so at the base' \
        "$cand_path" "$cand_now" "$cand_ceiling")
"
    fi
    ;;
  "") ;;
  *)
    report="${report}${line}
" ;;
  esac
done <<EOF
$raw_report
EOF

# Drift is reported whichever way the verdict goes: it is real — the baseline
# owes a regeneration — but it is not THIS change's debt, and failing the next
# PR to walk past it is the bug this guard was rewritten to stop (#2004).
if [ -n "$drift" ]; then
  {
    echo "check-file-size: baseline drift (not caused by this change, not fatal)"
    printf '%s' "$drift"
    echo ""
    echo "These files were already over their recorded ceiling in the base tree,"
    echo "so another change put them there and this one only inherited it. Run"
    echo "\"make file-size-update\" on a fresh main and land the baseline diff on"
    echo "its own to clear it."
  } >&2
fi

if [ -n "$report" ]; then
  echo "check-file-size: FAILED" >&2
  echo "$report" | awk '
    /^NEWOVER$/  { print ""; print "These files crossed the limit and are NOT grandfathered — split them into"; print "submodules. Do not add a baseline entry: the baseline only covers files"; print "that predate the guard, and this is the rule that stops the tree getting"; print "worse."; next }
    /^GREW$/     { print ""; print "These grandfathered files grew past their recorded ceiling. If the growth is"; print "irreducible (a subcommand or module declaration in an already-oversized"; print "lib.rs/main.rs), run \"make file-size-update\" and commit the baseline diff so"; print "the increase is visible in review. If it is not irreducible, put the new code"; print "in its own module instead."; next }
    /^OBSOLETE$/ { print ""; print "Obsolete baseline entries (the file is now under the limit). Run"; print "\"make file-size-update\" to retire them — an exemption must not outlive"; print "the problem it covered."; print ""; print "A file leaving the baseline also leaves the god-file list, so the same"; print "commit must drop it from the per-crate table in AGENTS.md AND from the"; print "\"God files\" section of that crate README, or \"make god-files\" fails"; print "next. All three copies are cross-checked, and the baseline wins."; next }
    /^STALE$/    { print ""; print "Stale baseline entries. Run \"make file-size-update\"."; next }
    { print }
  ' >&2
  exit 1
fi

tracked=$(git ls-files "${RATCHET_PATHSPECS[@]}" | wc -l | tr -d ' ')
# The verdict is already decided; the write is best-effort. SIGPIPE is ignored
# and the write's failure discarded, so a reader that closed the pipe
# (`| head -1`, `| true`) cannot turn a green verdict into a failure (#1815).
# `|| true`: `grep -c` exits 1 when the count is ZERO, and this runs under
# `set -e`. An empty baseline — every god file split, which is the goal — would
# otherwise abort the guard here, AFTER the verdict was decided, printing
# nothing and exiting non-zero. A clean tree reported as a failure with no
# message is the worst reading of a green result, and it is why every case in
# `scripts/test-file-size.sh` (which plants an empty baseline by design) was
# red (#1800).
grandfathered=$(grep -cv '^#' "$baseline" || true)
# A drifted baseline still passes, so the green line must say so rather than
# read as an unqualified clean bill — the summary is what most readers see.
drift_note=""
if [ -n "$drift" ]; then
  drift_note=" $(printf '%s' "$drift" | grep -c '^') file(s) carry inherited drift (see above)."
fi
trap '' PIPE
echo "check-file-size: OK — $tracked Rust/Python/shell files, none over $LIMIT lines except $grandfathered grandfathered (none grew by this change).${drift_note}" || true
