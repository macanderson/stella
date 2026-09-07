#!/usr/bin/env bash
#
# Guard: `Cargo.lock` still resolves once this checkout takes in `main` (`#5943`).
#
# `Cargo.lock` is a shared cell. Two branches can each write it right and
# still land a tree cargo cannot resolve. `check-lockfile-sync.sh` reads one
# tree, so it cannot see that. This one merges first, then asks it.
#
# On 2026-09-06 the sync to `0.9.330` merged at 09:51. The branch adding the
# `stella-learn` crate merged at 09:59 with a lock entry at `0.9.329`. Git had
# no clash to report. Every `--locked` build on `main` broke, and three
# sessions each wrote the same repair (`#5939`).
#
# `auto-tag.yml` asked this question of its own sync branch and nothing else,
# so the break landed from the other side. This is that check, made shared and
# pointed both ways: `ci.yml` runs it on every pull request, `auto-tag.yml`
# runs it before the write-back merges, and `recheck-lockfile-compositions.sh`
# runs it again for open pull requests each time `main` moves.
#
# The answer goes stale. A pull request is green against the `main` of the
# moment it ran, and `strict` is off, so no rule makes it catch up. The sweep
# above is what re-asks. Read its header for how a stale green is replaced.
#
# A pair that will not merge at all gets exit 1 too, with git's own words in
# the report. Such a branch cannot land either way, so the red costs it
# nothing. A green here would be an answer this guard did not earn.
#
# The merge is undone before this exits, whatever happens. It refuses to start
# on a dirty tree, so it can never eat your work.
#
# Usage. The ref to merge in, `origin/main` by default.
#
#   `scripts/check-merge-lockfile-sync.sh`
#   `scripts/check-merge-lockfile-sync.sh other/branch`
#   `scripts/check-merge-lockfile-sync.sh --no-fetch main`
#   `scripts/check-merge-lockfile-sync.sh --repo-dir DIR`
#
# `--repo-dir` is there for `scripts/test-merge-lockfile-sync.sh`. It builds a
# throwaway repo with two branches, so the failing branch is shown and not
# just claimed. A guard nobody has seen fail reads like one that always
# passes.
#
# Exit 0 means the merged lock resolves. Exit 1 means it does not, or the two
# trees clash. Exit 2 means the guard could not run at all.
set -euo pipefail

repo_dir=""
ref=""
fetch=1

while [ $# -gt 0 ]; do
  case "$1" in
  --no-fetch)
    fetch=0
    shift
    ;;
  --repo-dir)
    [ $# -ge 2 ] || {
      echo "check-merge-lockfile-sync: --repo-dir needs a directory" >&2
      exit 2
    }
    repo_dir="$2"
    shift 2
    ;;
  -h | --help)
    # The whole comment block, cut off at its first line of code. A line
    # number here goes stale the first time the header grows.
    awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0"
    exit 0
    ;;
  -*)
    echo "check-merge-lockfile-sync: unknown argument '$1'" >&2
    exit 2
    ;;
  *)
    if [ -n "$ref" ]; then
      echo "check-merge-lockfile-sync: one ref, not two ('$ref' and '$1')" >&2
      exit 2
    fi
    ref="$1"
    shift
    ;;
  esac
done

script_dir="$(cd "$(dirname "$0")" && pwd)"
[ -n "$ref" ] || ref="origin/main"
[ -n "$repo_dir" ] || repo_dir="$(cd "$script_dir/.." && pwd)"

if [ ! -d "$repo_dir" ]; then
  echo "check-merge-lockfile-sync: no such directory: $repo_dir" >&2
  exit 2
fi
cd "$repo_dir"

# The verdict is decided before a word is printed (`#1815`). A guard that
# prints as it scans dies part way through when its reader quits early, as
# `| head -1` does. Under `set -euo pipefail` that half state becomes the
# exit code.
report=""
note() { report="${report}check-merge-lockfile-sync: $1"$'\n'; }
emit() {
  trap '' PIPE
  printf '%s' "$report" >&2 || true
}

if ! git rev-parse --git-dir >/dev/null 2>&1; then
  note "FAIL — $repo_dir is not a git checkout, so no merge can be tried."
  emit
  exit 2
fi

# A merge already in flight, or a tree with edits in it, would leave the undo
# below to guess what to put back. Refuse instead.
git_dir="$(git rev-parse --git-dir)"
for marker in MERGE_HEAD CHERRY_PICK_HEAD REVERT_HEAD rebase-merge rebase-apply; do
  if [ -e "$git_dir/$marker" ]; then
    note "FAIL — a merge, rebase or cherry-pick is already in flight here."
    note "     Finish or abort it, then run this again."
    emit
    exit 2
  fi
done

if [ -n "$(git status --porcelain 2>/dev/null)" ]; then
  note "FAIL — the working tree has changes in it. This guard merges into the"
  note "     tree and then undoes it, so it will not start on top of your"
  note "     edits. Commit or stash them first."
  emit
  exit 2
fi

# Fetch by hand. Do not trust a stale remote ref. The point is to ask about
# the tip as it stands now, not the one this clone last saw.
if [ "$fetch" -eq 1 ]; then
  remote="${ref%%/*}"
  branch="${ref#*/}"
  if [ "$remote" = "$ref" ] || [ -z "$branch" ]; then
    note "FAIL — '$ref' is not a <remote>/<branch> ref. Pass one, or pass"
    note "     --no-fetch to read a ref this clone already holds."
    emit
    exit 2
  fi
  if ! git fetch --no-tags --quiet "$remote" "$branch" 2>/dev/null; then
    note "FAIL — could not fetch $branch from $remote."
    emit
    exit 2
  fi
  ref="FETCH_HEAD"
fi

if ! other="$(git rev-parse --verify --quiet "${ref}^{commit}")"; then
  note "FAIL — '$ref' does not name a commit here."
  emit
  exit 2
fi

# CI hands us a shallow clone. `ci.yml` checks out two commits. That is right
# for the guards that read two trees, and too little to merge with. Deepen
# only when the merge base is missing, so a full clone pays nothing.
if ! git merge-base HEAD "$other" >/dev/null 2>&1; then
  if [ "$(git rev-parse --is-shallow-repository 2>/dev/null || echo false)" = "true" ]; then
    git fetch --no-tags --quiet --deepen=500 "${remote:-origin}" >/dev/null 2>&1 || true
    if ! git merge-base HEAD "$other" >/dev/null 2>&1; then
      git fetch --no-tags --quiet --unshallow >/dev/null 2>&1 || true
    fi
  fi
fi
if ! git merge-base HEAD "$other" >/dev/null 2>&1; then
  note "FAIL — HEAD and $(git rev-parse --short "$other") share no history"
  note "     this clone can see, so the merged tree cannot be built."
  emit
  exit 2
fi

# Put the tree back on every exit path, the failing ones included. Inline
# rather than in a function, so nothing reads as dead code to a linter.
head_sha="$(git rev-parse HEAD)"
trap 'git merge --abort >/dev/null 2>&1 || true
      git reset --hard "$head_sha" >/dev/null 2>&1 || true' EXIT

short="$(git rev-parse --short "$other")"

# Nothing to merge when the ref is already in. The one-tree guard has
# answered for this tree, and a merge here would do nothing.
if git merge-base --is-ancestor "$other" HEAD; then
  emit
  printf 'check-merge-lockfile-sync: OK — HEAD already holds %s; nothing to merge.\n' "$short" || true
  exit 0
fi

# An identity of its own. No commit is made and the merge is undone, so this
# reaches nothing that lasts. Without it a box that has none — a CI runner is
# one — fails the merge on "Committer identity unknown", and the guard would
# read that as a clash and fail every branch it looked at.
merge_error=""
if ! merge_error="$(git -c user.name="stella merged-lock guard" \
  -c user.email="guard@localhost" \
  merge --no-commit --no-ff "$other" 2>&1)"; then
  note "FAIL — this checkout does not merge with $short."
  note ""
  note "git said:"
  while IFS= read -r line; do
    note "     $line"
  done <<<"$merge_error"
  note ""
  note "Merge it and settle that, then run this again."
  emit
  exit 1
fi

verdict=0
# Always `--manifest-dir`. The guard next door works out its own repo root
# from its path when nobody hands it one. A run against a fixture tree would
# then read this repo and report on the wrong lock.
merged_report="$("$script_dir/check-lockfile-sync.sh" --manifest-dir "$repo_dir" 2>&1)" || verdict=$?

if [ "$verdict" -eq 0 ]; then
  emit
  printf 'check-merge-lockfile-sync: OK — the lock still resolves once %s is in.\n' "$short" || true
  exit 0
fi

note "FAIL — the lock does not resolve once $short is merged in."
note "     Both trees are fine alone. The two writes collide."
note ""
note "The single-tree guard said:"
while IFS= read -r line; do
  note "     $line"
done <<<"$merged_report"
note ""
note "Fix it by taking the other side in and relocking:"
note ""
note "       git merge $short"
note "       cargo metadata --format-version 1 >/dev/null"
note "       git add Cargo.lock"
note ""
note "Landing this as it stands breaks every --locked build on main (`#5943`)."
emit
exit 1
