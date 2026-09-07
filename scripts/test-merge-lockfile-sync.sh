#!/usr/bin/env bash
#
# Tests for `scripts/check-merge-lockfile-sync.sh` (`#5943`).
#
# Hermetic. The fixture is a throwaway git repo with a tiny cargo workspace in
# it. So nothing here needs a network or this repo's own lock.
# `scripts/lib/lock-composition-fixture.sh` builds it and says what each
# branch is for.
#
# The case that matters is the one that broke `main` on 2026-09-06. A release
# sync bumps every version and writes a fresh lock. A branch cut before it
# adds a new crate. Its lock names that crate at the old version. Both trees
# pass the one-tree guard. Git merges them with no clash. The merged lock
# resolves for neither side.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
guard="$repo_root/scripts/check-merge-lockfile-sync.sh"
single="$repo_root/scripts/check-lockfile-sync.sh"

# shellcheck source=scripts/lib/lock-composition-fixture.sh
. "$repo_root/scripts/lib/lock-composition-fixture.sh"

if ! command -v cargo >/dev/null 2>&1; then
  echo "skip test-merge-lockfile-sync — no cargo toolchain on PATH"
  exit 0
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

pass=0
fail=0

repo="$tmp/repo"
build_lock_fixture "$repo"

# Run a command. Check the exit code, and that the output holds `needle`.
expect() {
  name="$1"
  want_code="$2"
  needle="$3"
  shift 3
  set +e
  out="$("$@" 2>&1)"
  code=$?
  set -e
  if [ "$code" -ne "$want_code" ]; then
    echo "FAIL  $name — exit $code, wanted $want_code"
    printf '      %s\n' "$out"
    fail=$((fail + 1))
    return
  fi
  case "$out" in
  *"$needle"*) ;;
  *)
    echo "FAIL  $name — output did not hold: $needle"
    printf '      %s\n' "$out"
    fail=$((fail + 1))
    return
    ;;
  esac
  echo "ok    $name"
  pass=$((pass + 1))
}

record() {
  if [ "$2" = "ok" ]; then
    echo "ok    $1"
    pass=$((pass + 1))
  else
    echo "FAIL  $1 — $2"
    fail=$((fail + 1))
  fi
}

# The branch is green on its own. That is the whole problem.
# Nothing about the branch is wrong. This is the pass that let the break
# through, so it is pinned first. A red here would mean the fixture is not
# the shape we want.
lock_fixture_git "$repo" checkout -q feature
expect "the branch alone passes the single-tree guard" 0 "OK — Cargo.lock resolves" \
  "$single" --manifest-dir "$repo"

# And the two of them merged do not resolve.
expect "a new member behind a bump is caught from the branch" 1 "does not resolve once" \
  "$guard" --repo-dir "$repo" --no-fetch main

# The same question from the other side. That side is `auto-tag.yml`.
lock_fixture_git "$repo" checkout -q main
expect "the same pair is caught from main" 1 "does not resolve once" \
  "$guard" --repo-dir "$repo" --no-fetch feature

# A branch cut after the bump writes the lock and still merges clean.
lock_fixture_git "$repo" checkout -q caught-up
expect "a member added after the bump passes" 0 "the lock still resolves" \
  "$guard" --repo-dir "$repo" --no-fetch main

# A branch that writes no manifest merges clean.
lock_fixture_git "$repo" checkout -q docs-only
expect "a branch touching no manifest passes" 0 "the lock still resolves" \
  "$guard" --repo-dir "$repo" --no-fetch main

# A ref already in HEAD is nothing to merge.
lock_fixture_git "$repo" checkout -q main
expect "a ref already held is reported as such" 0 "already holds" \
  "$guard" --repo-dir "$repo" --no-fetch main

# The tree comes back, even from the failing branch.
# The guard merges into the working tree. A run that left half a merge in it
# would cost an author their checkout. So the undo is proven, not assumed.
lock_fixture_git "$repo" checkout -q feature
before="$(git -C "$repo" rev-parse HEAD)"
set +e
"$guard" --repo-dir "$repo" --no-fetch main >/dev/null 2>&1
set -e
after="$(git -C "$repo" rev-parse HEAD)"
dirt="$(git -C "$repo" status --porcelain)"
if [ "$before" = "$after" ] && [ -z "$dirt" ]; then
  record "a failing run leaves the tree where it found it" ok
else
  record "a failing run leaves the tree where it found it" "HEAD or the tree moved"
fi

# It will not start on top of your edits.
echo "scratch" >"$repo/scratch.txt"
expect "a dirty tree is refused, not merged over" 2 "working tree has changes" \
  "$guard" --repo-dir "$repo" --no-fetch main
rm -f "$repo/scratch.txt"

# A bad argument is a usage error, never a silent pass.
expect "an unknown argument exits 2" 2 "unknown argument" "$guard" --nope
expect "two refs exit 2" 2 "one ref, not two" \
  "$guard" --repo-dir "$repo" --no-fetch main other
expect "--repo-dir with no value exits 2" 2 "needs a directory" "$guard" --repo-dir
expect "a missing repo exits 2" 2 "no such directory" \
  "$guard" --repo-dir "$tmp/nowhere" --no-fetch main
expect "a bare ref name with fetch on exits 2" 2 "not a <remote>/<branch> ref" \
  "$guard" --repo-dir "$repo" main
expect "a directory that is not a checkout exits 2" 2 "not a git checkout" \
  "$guard" --repo-dir "$tmp" --no-fetch main

# The verdict lives through a reader that shuts the pipe (`#1815`).
lock_fixture_git "$repo" checkout -q feature
set +e
"$guard" --repo-dir "$repo" --no-fetch main 2>&1 | head -1 >/dev/null
code=${PIPESTATUS[0]}
set -e
if [ "$code" -eq 1 ]; then
  record "a truncated reader still gets exit 1" ok
else
  record "a truncated reader still gets exit 1" "exit was $code"
fi

echo
echo "test-merge-lockfile-sync: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
