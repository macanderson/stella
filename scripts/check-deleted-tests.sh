#!/usr/bin/env bash
#
# Guard: a merge that deletes a test must say so out loud.
# See #1976, and AGENTS.md § "The definition of done: witness tests".
#
# Three times now a PR has landed on `main` that silently deleted code another
# PR added to the same file earlier the same day. The most recent (#1975):
# #1951 rewrote crates/stella-pipeline/src/pipeline/tests/verification_hardening.rs
# from a pre-#1945 base, deleting the `PassingShell` double, `shell_call_result`,
# and the witness `a_revision_halts_at_the_step_where_the_tracked_test_flips`
# that #1945 had added hours earlier.
#
# It was caught only by luck: #1945 had *also* added a `mod flip_halt_arming;`
# line that survived the rewrite and referenced two of the deleted symbols, so
# `main` went red. Had #1945 added only the test and its doubles — no new
# module — the deletion would have compiled clean and silently removed a
# witness from the tree. A witness that no longer exists cannot fail, so
# nothing downstream would ever have reported it. The same shape is on record
# for #1860 silently reverting #1836's forwarding in four crates.
#
# ── Why CI cannot already see this ───────────────────────────────────────────
#
# Both PRs are green against the `main` they branched from, and the merge is
# textually clean: git has no conflict to report, because one side simply does
# not contain the other's lines. Branch protection's "require branches to be up
# to date" would catch it, but it is off — and turning it on serializes every
# merge, which at this repository's merge rate costs more than the defect.
#
# ── What it compares, and why that exact pair ────────────────────────────────
#
# The merge BASE against the merge RESULT — never the PR's branch point.
#
# That distinction is the whole guard. Comparing against the branch point would
# miss precisely the defect this exists for: a test added to `main` *after* the
# PR branched is absent from the branch point, so its disappearance from the
# PR would look like nothing at all. Comparing `main`'s tip against the merged
# tree asks the question that matters — "does everything main had survive this
# merge?" — and is quiet on a stale branch, because git merges main's own
# additions in unless the PR's side actively removed them.
#
# On a `pull_request` event the checkout is `refs/pull/N/merge`, so HEAD is the
# merge commit, HEAD^1 is the base branch tip and HEAD^2 is the PR head. The
# guard reads HEAD^1 vs HEAD and needs no PR metadata. It is a two-TREE
# question, not a two-history one, so `fetch-depth: 2` is enough — there is no
# need for a full clone.
#
# ── It asks for an acknowledgement; it does not forbid deletion ──────────────
#
# A removed test is not automatically wrong. Tests are legitimately renamed,
# merged into a table-driven case, or deliberately dropped with the feature
# they covered. So a removal fails the guard only while it is UNNAMED: writing
# the test's name in the PR description (or in a commit message) passes it.
#
# That is the entire mechanism, and it is deliberately weak. The goal is not to
# adjudicate whether a deletion was correct — a script cannot — but to convert
# an invisible deletion into a sentence a reviewer reads.
#
# ── What it keys on, and the miss that follows ───────────────────────────────
#
# The bare function name of anything carrying `#[test]` or `#[tokio::test]`,
# unqualified by file or module. Unqualified is a choice: it means a test MOVED
# between files or modules is silently fine, which is the common, legitimate
# case and would otherwise be constant noise.
#
# The cost is that a duplicated name masks a deletion — delete one `fn works()`
# while another survives elsewhere and this guard sees the name still present.
# Measured rather than assumed: at the time of writing the tree carries 6927
# test attributes over 6867 distinct names, of which 51 (0.74%) are used more
# than once. Nearly all of those are the per-adapter provider suites, where the
# same contract is deliberately asserted against each vendor under one name
# (`complete_maps_401_to_auth_error` and its siblings appear once per adapter).
#
# So the masked case is real but narrow, and it is narrow *because* of this
# repository's house style of long sentence-shaped test names — it is not a
# general-purpose assumption, and a tree full of `fn test_1` would need the
# qualified key and the move-detection cost that comes with it.
#
# The second known miss is the mirror of the one documented in
# scripts/check-left-behind.sh: a `#[test]` inside a STRING LITERAL is counted.
# Two lines in the tree manage it (crates/stella-pipeline/src/witness/density.rs
# and crates/stella-cli/src/candidate_ws/witness_tools.rs — both code that
# analyses test code, whose fixtures put `#[test]` at the start of a Rust
# line-continuation inside a literal). A shell script cannot reliably tell
# whether a `#[test]` is inside a string, and this one does not try, for the
# reason that script gives: a guard that guesses wrong cries wolf.
#
# It costs nothing here, because this is a DIFFERENCE detector and the noise is
# symmetric — a fixture present in both trees cancels. It can only speak up if
# such a fixture is edited, and that lands in the acknowledge path, which is the
# benign direction.
#
# ── Deliberately NOT in `make gate` ──────────────────────────────────────────
#
# This is inherently a two-branch question, so there is nothing for it to
# compare on a local `make gate` run — it lives in CI, on the merge result.
# Running it by hand takes an explicit base:
#
#     ./scripts/check-deleted-tests.sh origin/main
#
# Uses portable POSIX tools so it runs on a bare CI runner (macOS ships bash
# 3.2, so no associative arrays).
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "check-deleted-tests: not a git repository; skipping."
  exit 0
fi

head_ref="${2:-HEAD}"

# Resolve the base. An explicit argument wins. Otherwise, if HEAD is a merge
# commit we are on a `refs/pull/N/merge` checkout and its first parent is the
# base branch tip — the pair this guard is designed around. A non-merge HEAD
# has no merge to inspect, so there is nothing to say.
if [ -n "${1:-}" ]; then
  base_ref="$1"
elif git rev-parse --verify --quiet "${head_ref}^2" >/dev/null; then
  base_ref="${head_ref}^1"
else
  echo "check-deleted-tests: $head_ref is not a merge commit and no base ref was given; skipping."
  echo "check-deleted-tests: pass a base explicitly to compare two trees, e.g. '$0 origin/main'."
  exit 0
fi

if ! git rev-parse --verify --quiet "$base_ref" >/dev/null; then
  echo "check-deleted-tests: base ref '$base_ref' is not present in this clone." >&2
  echo "check-deleted-tests: in CI this means the checkout was too shallow — fetch-depth must be at least 2." >&2
  exit 1
fi

# Every `#[test]` / `#[tokio::test]` function name in one tree, sorted and
# deduplicated.
#
# One `git grep` over the whole tree at that revision, not one `git show` per
# file: the per-file shape spawns a process for each of ~1100 source files,
# which is far too slow for something on the PR path. `-A4` covers the
# attributes that routinely sit between the test attribute and its `fn` line
# (`#[should_panic]`, `#[ignore]`, `#[allow(...)]`, a doc comment); the awk
# state machine gives up at the next test attribute rather than running on, so
# a wider window costs nothing but a longer read.
test_names_at() {
  git grep -h -A4 -E '^[[:space:]]*#\[(tokio::)?test[](]' "$1" -- '*.rs' 2>/dev/null |
    awk '
      # A new test attribute always restarts the search, so an attribute block
      # longer than the context window cannot bleed into the next test.
      /^[[:space:]]*#\[(tokio::)?test[](]/ { looking = 1; next }
      looking && match($0, /fn[[:space:]]+[A-Za-z0-9_]+/) {
        name = substr($0, RSTART, RLENGTH)
        sub(/^fn[[:space:]]+/, "", name)
        print name
        looking = 0
      }
    ' | LC_ALL=C sort -u
}

base_names="$(mktemp)"
head_names="$(mktemp)"
trap 'rm -f "$base_names" "$head_names"' EXIT

test_names_at "$base_ref" >"$base_names"
test_names_at "$head_ref" >"$head_names"

# In the base tree and not in the merged tree.
removed="$(LC_ALL=C comm -23 "$base_names" "$head_names")"

if [ -z "$removed" ]; then
  scanned=$(wc -l <"$base_names" | tr -d ' ')
  # The verdict is already decided; the write is best-effort. SIGPIPE is
  # ignored and the write's failure discarded, so a reader that closed the
  # pipe (`| head -1`) cannot turn a green verdict into a failure (#1815).
  trap '' PIPE
  echo "check-deleted-tests: OK — $scanned test(s) in $base_ref, none lost by the merge." || true
  exit 0
fi

# The acknowledgement text. `PR_BODY` is the reliable channel in CI — a
# shallow checkout has no commit messages to read — and `git log` is a
# best-effort addition for local runs and deeper clones, so a commit trailer
# naming the test also passes.
ack="${PR_BODY:-}"
if commits="$(git log --format='%B' "$base_ref..$head_ref" 2>/dev/null)"; then
  ack="$ack
$commits"
fi

unacknowledged=""
for name in $removed; do
  case "$ack" in
  *"$name"*) ;;
  *) unacknowledged="$unacknowledged  $name
" ;;
  esac
done

if [ -z "$unacknowledged" ]; then
  count=$(printf '%s\n' "$removed" | wc -l | tr -d ' ')
  trap '' PIPE
  echo "check-deleted-tests: OK — $count removed test(s), each named in the PR description or a commit." || true
  exit 0
fi

{
  echo "check-deleted-tests: FAILED"
  echo ""
  echo "These tests exist in $base_ref but not in the merged tree, and nothing"
  echo "in the PR description or the branch's commit messages names them:"
  echo ""
  printf '%s' "$unacknowledged"
  echo ""
  echo "Two things this can mean, and they need opposite fixes:"
  echo ""
  echo "  1. YOU DID NOT MEAN TO DELETE THEM. This is the case the guard exists"
  echo "     for (#1976): your branch rewrote a file from a base older than"
  echo "     someone else's merge, so their test is simply absent from your"
  echo "     side and git had no conflict to report. Rebase onto the base"
  echo "     branch and restore the lines you did not intend to drop. A witness"
  echo "     that no longer exists cannot fail, so nothing else will tell you."
  echo ""
  echo "  2. YOU DID. Renaming, folding into a table-driven case, or dropping a"
  echo "     test with the feature it covered are all fine. Name each test above"
  echo "     in the PR description (or a commit message) and this passes — the"
  echo "     point is that a reviewer reads the sentence, not that the deletion"
  echo "     is forbidden."
  echo ""
  echo "A moved or renamed test is reported here because this guard keys on the"
  echo "bare function name; that is deliberate, and naming it in the PR is the"
  echo "whole cost."
} >&2

exit 1
