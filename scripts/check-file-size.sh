#!/usr/bin/env bash
#
# Guard: no NEW source file may exceed the 1500-line ratchet, in any of the
# languages this repository writes. See #629 (Rust), #825 (Python — an
# 8,166-line analysis module had slipped through because the guard only looked
# at *.rs), #1563 (shell — the delivery-loop driver reached ~1,900 lines while
# no gate could see it) and #3811 (TypeScript and JavaScript).
#
# The language list has now been widened three times for the same reason, and
# the third time proved the argument the second one made: a limit that watches
# one language is not a property of the repository, it is a property of that
# language's files, and the growth simply moves to whatever is unwatched. It
# moved to `arenabench/ui/components/arena/transcript-page.tsx`, which reached
# 1,571 lines — over the limit AGENTS.md holds the tree to — and was found by
# a human reading it, because nothing in the gate could say so. So widen
# eagerly: the day a surface joins is the only day it is free.
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
# Run --update --retighten after such work to tighten the ceilings; see the
# next section for why the tightening is a separate, deliberate pass.
#
# ── --update raises; --retighten lowers, and only when asked (#4657) ──────────
#
# `--update` used to rewrite every ceiling to its file's current size on the
# branch it ran on. Correct for that branch, and it turned the PR *repairing* a
# red `main` into the next break, because a repair PR is the one PR guaranteed
# to be racing every other merge — main is red and everyone is waiting on it.
#
# On 2026-08-24 that happened twice in a row, the second time caused by the
# first one's fix. #4646 was `main` red on driver.rs and usage.rs. PR #4652
# repaired those two, and the same run also lowered command_deck.rs from 3752
# to 3656 — measured on #4652's branch, while main's copy was 3737 lines. The
# moment #4652 merged, `main` was red again on command_deck.rs (+81),
# deck_ui.rs (+15) and views/engine.rs (+1), which is #4654, and every open PR
# was blocked by `main-red-hold` for half an hour. The author could not decline
# those other twenty edits, and nothing in the diff said which of them were
# newly risky.
#
# So the two directions are split. `--update` is RAISE-ONLY: a ceiling that
# must go up to admit the tree goes up, every other live entry keeps the number
# it had, and a repair PR edits exactly the lines it needs. `--retighten` adds
# the lowering, for the deliberate pass that reclaims the slack — which is safe
# precisely when nothing is blocked on it.
#
# Neither mode ADDS an entry (see the refusal below), and both still RETIRE
# one: an entry whose file dropped to <= LIMIT, or whose file is gone, is a
# hard failure of the check with `--update` named as the only remedy, so
# raise-only that kept them would leave the gate red with no way out. Retiring
# an exemption whose subject no longer exists is not a tightening of a live
# ceiling, which is the thing that raced.
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
# `ceiling` here means the file's own limit, whichever kind it has: the baseline
# entry for a grandfathered file, and LIMIT for every other. The rule was first
# written for grandfathered files alone, and a file with no baseline entry went
# on failing on sight — so the very first crossing of the 1500-line limit on
# `main` turned every open PR red, byte-identical trees included, which is the
# exact failure #2004 exists to prevent (#2397). What differs between the two
# kinds is only the remedy the report names, not the arithmetic: a drifted
# ceiling is cleared by regenerating the baseline, while a first-time crossing
# is cleared by splitting the file. A first-time crossing is never grandfathered
# on the way past — inherited drift is reported and survived, not absorbed.
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
#
# TypeScript and JavaScript joined for the reason the header states and the
# tree then demonstrated (#3811): `arenabench/ui/components/arena/transcript-page.tsx`
# reached 1,571 lines — over the limit AGENTS.md holds this repository to, with
# nothing able to say so — and was split by hand, by someone who happened to
# look. `*.ts`/`*.tsx` covers the arena UI and the website; `*.mjs`/`*.js`
# covers the deck and parity scripts and the observatory's assets. Nothing
# crossed on the day they were added, which is the only day that is true for
# free.
#
# `*.mdx` is deliberately NOT here, and the omission is a judgement rather than
# an oversight. The website's documentation pages are prose (the longest,
# `website/content/docs/configuration/settings.mdx`, is 912 lines), and the
# remedy this guard names — "split them into submodules" — has no meaning for
# a page a reader reads top to bottom. A ratchet whose failure has no correct
# fix teaches people to edit the baseline.
#
# The one exclusion is generated: `docs/wire/*.d.ts` is written by
# scripts/export-agentevent-schema.sh and committed so drift is reviewable, and
# `wire-schema` fails the gate if it differs from what the exporter produces.
# Splitting it is not something a human may do — the next regeneration would
# undo it and redden that guard instead.
RATCHET_PATHSPECS=('*.rs' '*.py' '*.sh' '.githooks/*' '*.ts' '*.tsx' '*.mjs' '*.js'
  ':(exclude)docs/wire/*.d.ts')

# Emit "<lines> <path>" for every tracked file in scope, NUL-safe on the git
# side. Python counts (#825): the analyzer under bench/ grew to 8,166 lines
# while the guard watched only *.rs. Shell counts (#1563) and TypeScript and
# JavaScript count (#3811) for the same reason, each after the same incident.
current_sizes() {
  git ls-files -z "${RATCHET_PATHSPECS[@]}" | while IFS= read -r -d '' f; do
    # A tracked file deleted from the working tree but not yet staged is a
    # legitimate transient state on a working branch (#3268); judge the files
    # that exist rather than erroring on the ones that do not. Once the
    # deletion is staged, ls-files stops listing it and nothing changes.
    [ -f "$f" ] || continue
    printf '%s %s\n' "$(wc -l <"$f" | tr -d ' ')" "$f"
  done
}

# See `resolve_base_commit` for what this changes and why only the canary wants
# it. Read before the `--update` arm below so the two flags cannot be confused:
# `--update` rewrites the baseline, `--absolute` only judges against it.
ABSOLUTE_MODE=""
if [ "${1:-}" = "--absolute" ]; then
  ABSOLUTE_MODE=1
  shift
fi

if [ "${1:-}" = "--update" ]; then
  shift
  grandfathered_arg="$(mktemp)"
  trap 'rm -f "$grandfathered_arg"' EXIT
  retighten=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
    --retighten)
      retighten=1
      shift
      ;;
    --grandfather)
      if [ "$#" -lt 2 ]; then
        echo "check-file-size: --grandfather needs a path." >&2
        exit 2
      fi
      printf '%s\n' "$2" >>"$grandfathered_arg"
      shift 2
      ;;
    --grandfather=*)
      printf '%s\n' "${1#--grandfather=}" >>"$grandfathered_arg"
      shift
      ;;
    *)
      echo "check-file-size: unknown argument '$1' after --update." >&2
      exit 2
      ;;
    esac
  done

  over="$(current_sizes | awk -v limit="$LIMIT" '$1 > limit' | LC_ALL=C sort -k2)"

  if [ -f "$baseline" ]; then
    # `|| true`: an all-comment baseline — every god file split, which is the
    # goal — makes `grep -v` exit 1 under `set -e`, and this ran before any
    # verdict, so the whole command died silently with no message at all. Same
    # trap the `grep -cv` at the foot of this script carries a note about
    # (#1800); it is the second time this file has been bitten by it.
    known="$(grep -v '^#' "$baseline" 2>/dev/null | awk 'NF { print $2 }' | LC_ALL=C sort || true)"
    over_paths="$(printf '%s' "$over" | awk 'NF { print $2 }' | LC_ALL=C sort)"
    # Over the limit and carrying no prior decision: either the caller asked
    # for it by name, or this is the crossing that must be split instead.
    additions="$(LC_ALL=C comm -23 <(printf '%s\n' "$over_paths") <(printf '%s\n' "$known") | awk 'NF')"
    asked="$(LC_ALL=C sort -u "$grandfathered_arg" | awk 'NF')"
    refused="$(LC_ALL=C comm -23 <(printf '%s\n' "$additions") <(printf '%s\n' "$asked") | awk 'NF')"
    # A --grandfather that names nothing needing it: a stale flag left on a
    # command line, silently claiming a decision nobody is making today.
    pointless="$(LC_ALL=C comm -13 <(printf '%s\n' "$additions") <(printf '%s\n' "$asked") | awk 'NF')"

    if [ -n "$pointless" ]; then
      {
        echo "check-file-size: --grandfather named path(s) that need no new entry:"
        printf '%s\n' "$pointless" | sed 's/^/  /'
        echo ""
        echo "Each is either already in the baseline or under the ${LIMIT}-line limit."
        echo "Drop the flag: it is meant to record one deliberate exemption, and one"
        echo "that applies to nothing teaches a reader the opposite."
      } >&2
      exit 1
    fi

    if [ -n "$refused" ]; then
      {
        echo "check-file-size: refusing to ADD baseline entries for:"
        printf '%s\n' "$refused" | sed 's/^/  /'
        echo ""
        echo "These files crossed the ${LIMIT}-line limit for the first time; they are not"
        echo "grandfathered, and the baseline only ever covered files that predate the"
        echo "guard. Writing an entry here would grandfather a new god file inside a diff"
        echo "whose purpose is something else, and turn the gate green so nothing objects"
        echo "again. Split them into submodules instead (AGENTS.md, \"God files\")."
        echo ""
        echo "The baseline was NOT modified. If a crossing is genuinely irreducible, say so"
        echo "explicitly and justify it in review:"
        echo ""
        printf '%s\n' "$refused" | sed 's|^|  ./scripts/check-file-size.sh --update --grandfather |'
      } >&2
      exit 1
    fi
  fi

  # Raise-only unless --retighten was asked for (#4657). Every path here is
  # already over the limit and already carries a prior decision or an explicit
  # --grandfather, so the only question left is which number it gets: the size
  # measured on this branch, or the larger of that and the ceiling the baseline
  # already records. Taking the max is what stops a repair PR quietly lowering
  # twenty ceilings it never looked at.
  #
  # A path with no recorded ceiling — the --grandfather case — takes its
  # current size under both modes; `max(current, absent)` is `current`.
  written="$over"
  if [ -z "$retighten" ] && [ -f "$baseline" ]; then
    written="$(printf '%s\n' "$over" | awk 'NF' | awk -v baseline="$baseline" '
      BEGIN {
        while ((getline line < baseline) > 0) {
          if (line ~ /^#/) continue
          if (split(line, f, " ") >= 2) ceiling[f[2]] = f[1] + 0
        }
      }
      { n = $1 + 0; p = $2; if (p in ceiling && ceiling[p] > n) n = ceiling[p]; printf "%d %s\n", n, p }
    ' | LC_ALL=C sort -k2)"
  fi

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
    # Derived from `$over` above and deliberately not recomputed: the set of
    # PATHS written must be the exact set the refusal check judged.
    if [ -n "$written" ]; then printf '%s\n' "$written"; fi
  } >"$baseline.tmp"
  mv "$baseline.tmp" "$baseline"
  if [ -n "$retighten" ]; then
    mode="retightened to current sizes"
  else
    mode="raised where needed, no ceiling lowered"
  fi
  echo "check-file-size: baseline updated ($mode) — $(grep -cv '^#' "$baseline") grandfathered file(s) over $LIMIT lines."
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
  # `--absolute` asks the other question: not "did this change grow a file?"
  # but "is this tree within its ceilings at all?" Returning no base collapses
  # `max(ceiling, base)` to the ceiling, which is the strict read.
  #
  # It exists for the post-merge canary (#3447). Every rung below is
  # base-relative on purpose — inherited drift must not fail an author who did
  # not cause it (#2004, #2397) — but that same mercy makes drift already
  # sitting on `main` invisible to every branch in flight, and on a push to
  # `main` the last rung compares against `HEAD^1`, so a violation introduced
  # two commits ago goes unseen forever. Somebody has to ask the absolute
  # question once the merge has happened, and this is the flag that asks it.
  if [ -n "${ABSOLUTE_MODE:-}" ]; then
    printf ''
    return 0
  fi
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
# A file over its limit is emitted as a CANDIDATE, not a verdict: awk sees one
# tree and so cannot tell growth from inherited drift. The classification needs
# the base tree and happens below. Candidates are emitted in their section's
# position so the assembled report keeps its original section order.
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
        newover = newover sprintf("NEWCAND %s %d\n", path, n)
      }
    }
    END {
      for (p in ceiling)
        if (!(p in seen))
          stale = stale sprintf("  %s (baseline entry, file no longer tracked)\n", p)
      if (newover) printf "%s", newover
      if (grew) printf "%s", grew
      if (obsolete) printf "OBSOLETE\n%s", obsolete
      if (stale) printf "STALE\n%s", stale
    }
  '
)"

# max($2, size of $1 at the base): the largest this change may leave the file
# at, given the limit $2 it is held to. With no base commit the second term is
# absent and this is that limit alone — the original whole-tree check.
#
# Shared by both candidate kinds because the arithmetic is the same rule; only
# the limit passed in and the remedy reported differ (#2397).
effective_limit() {
  local own="$2" at_base
  if [ -n "$base_commit" ]; then
    at_base="$(size_at_base "$1")"
    if [ "$at_base" -gt "$own" ]; then
      own="$at_base"
    fi
  fi
  printf '%s\n' "$own"
}

# Classify each candidate against the base tree, preserving the stream order so
# the GREW section still lands between NEWOVER and OBSOLETE.
report=""
drift=""
newover_header_emitted=0
grew_header_emitted=0
while IFS= read -r line; do
  case "$line" in
  "NEWCAND "*)
    # Deliberate word split, as for GREWCAND below.
    # shellcheck disable=SC2086
    set -- $line
    cand_path="$2"
    cand_now="$3"
    if [ "$cand_now" -gt "$(effective_limit "$cand_path" "$LIMIT")" ]; then
      if [ "$newover_header_emitted" -eq 0 ]; then
        report="${report}NEWOVER
"
        newover_header_emitted=1
      fi
      report="$report$(printf '  %s is %d lines, over the %d-line limit' \
        "$cand_path" "$cand_now" "$LIMIT")
"
    else
      drift="$drift$(printf '  %s is %d lines, over the %d-line limit — already so at the base; split it' \
        "$cand_path" "$cand_now" "$LIMIT")
"
    fi
    ;;
  "GREWCAND "*)
    # Deliberate word split: these are this script's own awk-formatted records,
    # and the baseline format has never admitted a path containing a space.
    # shellcheck disable=SC2086
    set -- $line
    cand_path="$2"
    cand_now="$3"
    cand_ceiling="$4"
    if [ "$cand_now" -gt "$(effective_limit "$cand_path" "$cand_ceiling")" ]; then
      if [ "$grew_header_emitted" -eq 0 ]; then
        report="${report}GREW
"
        grew_header_emitted=1
      fi
      report="$report$(printf '  %s grew to %d lines, over its baseline ceiling of %d (+%d)' \
        "$cand_path" "$cand_now" "$cand_ceiling" "$((cand_now - cand_ceiling))")
"
    else
      drift="$drift$(printf '  %s is %d lines against a ceiling of %d — already so at the base; regenerate' \
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
    echo "These files were already over the line in the base tree, so another change"
    echo "put them there and this one only inherited it. Each line above names its"
    echo "own remedy, and they differ: \"regenerate\" is a grandfathered file whose"
    echo "ceiling drifted below it, cleared by \"make file-size-update\"; \"split it\" is"
    echo "a file that crossed the ${LIMIT}-line limit for the first time, which is never"
    echo "given a baseline entry. Either way it is that file's own change, landed on"
    echo "its own from a fresh main — not folded into this one."
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
# Say which mode ran. The change-relative rule is silent by nature — it only
# shows itself when something drifts — so a checkout too shallow to resolve a
# base would fall back to strict and read as an ordinary green line forever.
# That is exactly how .github/workflows/file-size.yml shipped its ratchet as a
# whole-tree check while this script believed otherwise, and naming the mode is
# what makes the difference legible in a log rather than a thing to re-derive.
if [ -n "$base_commit" ]; then
  mode_note=" Judged against $(git rev-parse --short "$base_commit" 2>/dev/null || echo "$base_commit")."
else
  mode_note=" No base resolved — strict whole-tree check."
fi
trap '' PIPE
echo "check-file-size: OK — $tracked watched files, $grandfathered grandfathered over $LIMIT lines, and nothing went over by this change.${mode_note}${drift_note}" || true
