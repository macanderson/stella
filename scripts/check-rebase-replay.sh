#!/usr/bin/env bash
#
# Guard: a branch that edits a file and then undoes the edit must flatten the
# pair, because a rebase turns it into a live revert of somebody else's work.
# See #4979.
#
# ── The defect ───────────────────────────────────────────────────────────────
#
# Two branches make the same edit to one file — which a mechanical sweep does by
# construction. Branch B then reverts its copy, so B's tree matches base and
# `git diff base -- <file>` is empty. B looks like it never touched the file.
#
# Once A lands, rebasing B onto the new base deduplicates B's edit and keeps
# B's revert:
#
#     warning: skipped previously applied commit 541a41f
#
# Git recognises B's edit as already upstream and drops it. It has no reason to
# drop the revert. The inert pair becomes a bare revert of A's change, B's diff
# for that file goes from empty to `-beta +alpha`, and it squash-merges cleanly.
#
# Merging B without rebasing keeps A's edit, because B contributes no change to
# that file relative to the merge base. Only the rebase path reverts — and this
# repository rebases constantly, because branches go stale behind required
# checks.
#
# The real instance: #4954 edited crates/stella-observatory/src/accept.rs and
# reverted it in a later commit, because both copies of that file had to travel
# in #4951 instead. Rebasing #4954 onto a tree carrying #4951 replayed the
# revert and `make prose` on the composed tree went red. Both branches were
# green on their own.
#
# ── Why nothing else can see it ──────────────────────────────────────────────
#
# The branch's diff for that file is empty in BOTH cases — before the rebase
# and, as far as any per-branch check is concerned, in the tree it was green
# against. So no check that reads a diff can tell the two apart. This one reads
# the branch's HISTORY instead: a path some commit on the branch touched, which
# the branch's own final diff no longer mentions, is an edit and an undo with
# nothing between them but a rebase waiting to happen.
#
# Purely local — it needs no other branch, no network and no toolchain, which is
# what makes it cheap enough to run before every merge. #4979 lists three other
# answers to the same defect (a periodic composer, a merge queue, documenting
# the shape); this is the one that runs on the pull request itself.
#
# ── No acknowledgement, unlike check-deleted-tests.sh ────────────────────────
#
# That guard lets a deletion pass once it is named in the pull-request
# description, because a deleted test is sometimes right and a script cannot
# adjudicate which. This one has no such escape, and does not need one: the
# branch's tree is identical either way, so flattening the pair changes nothing
# a reviewer can see and loses nothing. There is no version of this finding
# where keeping the pair is the better answer, so an acknowledgement would only
# offer the wrong remedy in reviewable-looking clothes.
#
# ── NOT in `make gate` ───────────────────────────────────────────────────────
#
# `make gate` runs on a working tree; this asks about a range of commits, which
# is a question a pull request has and a dirty checkout does not. It runs in
# .github/workflows/rebase-replay.yml, and by hand as:
#
#     ./scripts/check-rebase-replay.sh                 # against origin/main
#     ./scripts/check-rebase-replay.sh origin/main HEAD
#
# scripts/test-rebase-replay.sh is its self-test, and reproduces #4979's
# from-empty-repository repro as one of its cases, so the suite proves the
# hazard is real as well as that the guard fires on it.
#
# Portable POSIX tools, no toolchain, bash 3.2 compatible (macOS ships it).
set -euo pipefail

# The verdict is decided before anything is written (#1815): buffer the report
# and emit it once, so a reader that closes the pipe can change neither the
# report nor the exit status.
report=""
note() { report="${report}check-rebase-replay: $1"$'\n'; }
emit() {
  trap '' PIPE
  printf '%s' "$report" >&2 || true
}

if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "check-rebase-replay: not a git repository; THIS CHECK DID NOT RUN."
  exit 0
fi

# The repository the caller is standing in, not the one this script is filed in.
# Every other guard here reads the tree around its own path, but this one reads a
# range of commits — so pointing it at another checkout is the whole mechanism
# scripts/test-rebase-replay.sh needs, and copying the script into a fixture
# would test the copy. Toplevel rather than the caller's cwd, so `git log
# --name-only` reports one set of paths regardless of the subdirectory it is
# invoked from.
cd "$(git rev-parse --show-toplevel)"

# Resolve the range. An explicit base (and optional head) wins. Otherwise, a
# merge HEAD means a `refs/pull/N/merge` checkout, whose second parent is the
# branch and whose first is the base branch tip — the pair this guard is about.
# Failing both, compare against the default branch.
if [ -n "${1:-}" ]; then
  base_ref="$1"
  head_ref="${2:-HEAD}"
elif git rev-parse --verify --quiet "HEAD^2" >/dev/null; then
  base_ref="HEAD^1"
  head_ref="HEAD^2"
else
  head_ref="HEAD"
  if git rev-parse --verify --quiet "origin/main" >/dev/null; then
    base_ref="origin/main"
  elif git rev-parse --verify --quiet "main" >/dev/null; then
    base_ref="main"
  else
    echo "check-rebase-replay: no base ref (origin/main and main are both absent);"
    echo "check-rebase-replay: pass one explicitly. THIS CHECK DID NOT RUN."
    exit 0
  fi
fi

for ref in "$base_ref" "$head_ref"; do
  if ! git rev-parse --verify --quiet "$ref" >/dev/null; then
    note "FAIL — ref '$ref' is not present in this clone."
    note "     In CI this means the checkout was too shallow: this guard reads a"
    note "     whole branch's history, so it needs fetch-depth 0."
    emit
    exit 1
  fi
done

# The branch point, never the base tip: a commit that landed upstream after the
# branch started is not this branch's doing, and reading the range from the tip
# would attribute it here.
if ! base="$(git merge-base "$base_ref" "$head_ref" 2>/dev/null)"; then
  note "FAIL — '$base_ref' and '$head_ref' share no history."
  note "     In CI this means the checkout was too shallow (fetch-depth 0)."
  emit
  exit 1
fi
head="$(git rev-parse "$head_ref")"

# `core.quotePath=false` so a non-ASCII path arrives as itself rather than as an
# escaped string that would not match the other list.
git_c() { git -c core.quotePath=false "$@"; }

# What the branch's final diff mentions, and what its commits touched. Merges
# are excluded: `--name-only` reports nothing for one anyway, and a conflict
# resolution that happened to match base is not this branch editing a file.
net="$(git_c diff --name-only "$base" "$head")"
touched="$(git_c log --no-merges --name-only --pretty=format: "$base..$head" |
  sed '/^$/d' | sort -u)"

if [ -z "$touched" ]; then
  echo "check-rebase-replay: OK — no commit between $(git rev-parse --short "$base") and $(git rev-parse --short "$head") touches a file."
  exit 0
fi

# A path some commit touched that the final diff does not mention. `comm` needs
# both sides sorted; `net` may be empty, which is the pathological case rather
# than an excuse to skip.
suspects="$(comm -23 <(printf '%s\n' "$touched") <(printf '%s\n' "$net" | sed '/^$/d' | sort -u))"

if [ -z "$suspects" ]; then
  count="$(printf '%s\n' "$touched" | wc -l | tr -d ' ')"
  echo "check-rebase-replay: OK — $count path(s) touched, every one of them still in the branch's diff."
  exit 0
fi

found=0
while IFS= read -r path; do
  [ -n "$path" ] || continue
  found=$((found + 1))
  note ""
  note "  $path"
  # Captured rather than piped into the loop: a pipeline's right-hand side runs
  # in a subshell, where `note`'s appends to `report` would be discarded.
  commits="$(git_c log --no-merges --format='      %h  %s' "$base..$head" -- "$path")"
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    note "$line"
  done <<COMMITS
$commits
COMMITS
done <<EOF
$suspects
EOF

report="check-rebase-replay: FAIL — $found path(s) edited and then undone inside this branch."$'\n'"$report"
note ""
note "     The branch's diff against $(git rev-parse --short "$base") does not mention those"
note "     paths, so they read as untouched. Rebase onto a base that already"
note "     carries the same edit and git drops the branch's copy (\"skipped"
note "     previously applied commit\") while keeping the undo — which leaves a"
note "     live revert of whatever landed upstream, and it merges cleanly (#4979)."
note ""
note "     Flatten it, so the branch's history stops mentioning the path at all:"
note "       git rebase -i $(git rev-parse --short "$base")   # drop the pair"
note "       git reset --soft $(git rev-parse --short "$base") && git commit   # or recommit as one"
emit
exit 1
