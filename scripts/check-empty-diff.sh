#!/usr/bin/env bash
#
# Guard: a pull request that would merge as an empty commit is reported rather
# than landed in silence. See #1136, filed from the incident, and #1676.
#
# PR #1099 merged as a zero-file-diff commit 27 seconds after #1098 landed the
# identical fix, and nothing noticed. An empty merge is nearly always evidence
# that something upstream went wrong — two agent sessions scoped the same work,
# a rebase ate the change, a branch was pushed from the wrong worktree — and it
# is cheap to detect precisely because it is a fact about two commits and
# nothing else.
#
# This is the backstop, not the prevention. The dispatch lease in
# crates/stella-fleet/src/ledger/lease.rs stops the duplicate *inside one
# workspace*; it cannot see a duplicate that came from two clones, two
# machines, or a human. The lease stops the common cause; this says the
# prevention failed.
#
# ## Two different "empty", and only one of them is the incident
#
# 1. **The branch carries nothing.** `base...head` — the three-dot diff, what
#    GitHub's "Files changed" tab shows — is empty, so head adds nothing over
#    the point it forked from. A pushed-from-the-wrong-worktree branch.
#
# 2. **The merge would change nothing.** The base already contains this
#    branch's content, reached independently. This is #1099: two sessions
#    started from the same commit, wrote the same fix, and the second merge
#    was a no-op.
#
# Only (2) is the incident #1136 was filed from, and **(2) does not imply
# (1)**: two sessions branching from the same commit and writing the same
# patch produce a perfectly non-empty three-dot diff right up until the moment
# the first one merges. A check that tested only the three-dot diff — the
# shape #1676 sketched — would have run green on #1099. So the primary test
# here is `git merge-tree --write-tree`, which computes the tree the merge
# would produce: if it equals the base's own tree, merging changes nothing.
# Case (1) is reported separately because it has a different cause and a
# different fix, and because it still holds when merge-tree is unavailable.
#
# Usage: scripts/check-empty-diff.sh <base-ref> <head-ref>
#
# Uses portable POSIX tools plus git so it runs on a bare CI runner.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

if [ "$#" -ne 2 ]; then
  echo "usage: scripts/check-empty-diff.sh <base-ref> <head-ref>" >&2
  exit 2
fi

base="$1"
head="$2"

# An unresolvable ref is a misconfigured caller, not a passing check. Failing
# loudly here is the point: a guard that silently skips when it cannot
# evaluate is how the gap it covers reopens unnoticed.
for ref in "$base" "$head"; do
  if ! git rev-parse --verify --quiet "$ref^{commit}" >/dev/null; then
    echo "FAIL cannot resolve '$ref' to a commit." >&2
    echo "     A shallow clone is the usual cause — this check needs enough" >&2
    echo "     history to find a merge base (actions/checkout fetch-depth: 0)." >&2
    exit 1
  fi
done

if ! merge_base="$(git merge-base "$base" "$head")"; then
  echo "FAIL '$base' and '$head' have no common ancestor." >&2
  echo "     Nothing can be diffed against a merge base that does not exist;" >&2
  echo "     the branch was probably created from an unrelated history." >&2
  exit 1
fi

report_footer() {
  echo "" >&2
  echo "     base       $base" >&2
  echo "     head       $head" >&2
  echo "     merge base $merge_base" >&2
  echo "" >&2
  echo "     Merging would add a commit whose message describes changes it" >&2
  echo "     does not contain, which is what makes a duplicated run invisible" >&2
  echo "     to everyone reading git log afterwards." >&2
  echo "" >&2
  echo "     If the work is already on the base branch, close this PR rather" >&2
  echo "     than merging it. If the branch was meant to carry work, it was" >&2
  echo "     pushed from the wrong worktree or a rebase ate the commits." >&2
  echo "     See #1136." >&2
}

# (1) The branch carries nothing over its fork point.
#
# Compared against the merge base rather than the base branch tip: a branch
# that is merely *behind* the base has a perfectly good diff, and only a branch
# that adds nothing over where it forked is empty. Two-dot against an explicit
# merge base rather than the `base...head` shorthand, because the two mean the
# same thing here and only one of them is checkable by a reader.
if git diff --quiet "$merge_base" "$head"; then
  echo "FAIL this branch changes nothing against its merge base." >&2
  report_footer
  exit 1
fi

# (2) The merge itself would be a no-op — the #1099 shape.
#
# `--write-tree` (git 2.38+) reports the tree a real merge would produce, with
# no working tree and no index. Probe support once, by merging the merge base
# with itself: that is trivially clean, so a non-zero exit means the option is
# missing rather than that the branches conflict. Without the probe those two
# cases are indistinguishable — both are just a non-zero exit — and a
# conflicting merge would be silently reported as "old git".
if git merge-tree --write-tree "$merge_base" "$merge_base" >/dev/null 2>&1; then
  # A conflicting merge exits non-zero, which is the opposite of an empty
  # merge; only a clean result has a tree worth comparing.
  if merged_tree="$(git merge-tree --write-tree "$base" "$head" 2>/dev/null)"; then
    base_tree="$(git rev-parse "$base^{tree}")"
    if [ "$merged_tree" = "$base_tree" ]; then
      echo "FAIL merging this branch would change nothing." >&2
      echo "" >&2
      echo "     The base already contains this branch's content, so the merge" >&2
      echo "     commit would have a zero-file diff. The likely cause is that" >&2
      echo "     identical content already landed: a second agent session or a" >&2
      echo "     second clone did the same work, and the first one merged." >&2
      report_footer
      exit 1
    fi
  fi
else
  # Report the reduced coverage rather than a pass this run did not establish.
  echo "note: this git has no 'merge-tree --write-tree', so only the" >&2
  echo "      branch-carries-nothing half of this check ran. The" >&2
  echo "      already-landed-identical-content case (#1099) went untested." >&2
fi

changed="$(git diff --name-only "$merge_base" "$head" | wc -l | tr -d ' ')"
echo "ok  $changed file(s) changed against merge base ${merge_base}, and the merge is not a no-op."
