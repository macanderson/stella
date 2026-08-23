#!/usr/bin/env bash
#
# Measure every deck under a tree against the fixed canvas (#3404).
#
#   ./scripts/deck-fit-all.sh [root]        # default: website/public/presentations
#
# The enumeration and the pass/skip/fail accounting used to live inline in
# .github/workflows/deck-fit.yml's `run:` block, which is why the enumeration
# bug #3376 fixed had no regression test: there was no addressable unit to
# point one at. A recursive trigger paired with a non-recursive glob measured
# less than the workflow's own comment claimed, twice over (#2425, #3376), and
# both times it was found by a person noticing rather than by a check.
#
# Covered by scripts/test-deck-fit-all.sh (`make deck-fit-all-test`), which
# stubs `node` and needs no browser -- so unlike deck-fit.yml itself, this half
# is a gate step.
#
# Exit: 0 every deck measured clean, 1 a deck overflowed or the tree was empty.
# A deck the measurer classifies as not-a-deck (its exit 3) is reported as a
# skip, by path and by count. A skip is never a pass; a SILENT skip is the bug
# this step keeps re-learning, so the count is always printed.

set -uo pipefail

script_dir="$(cd "$(dirname "$0")" && pwd -P)"
measurer="$script_dir/deck-fit.mjs"

root="${1:-website/public/presentations}"

if [ ! -d "$root" ]; then
  echo "deck-fit: no such directory: $root" >&2
  exit 1
fi

# `git ls-files`, not a `**` glob: `**` needs `shopt -s globstar`, and a shell
# without it (bash 3.2, still the system bash on macOS, where this gets run by
# hand) does not error -- it silently degrades `**` to `*` and drops every deck
# below the top level. A check that quietly measures less than it claims is the
# bug being fixed here, so the enumeration must not have a mode where it does
# that. A git pathspec matches across `/` at any depth on every version, sorts
# deterministically, and restricts the walk to tracked files, which is what CI
# measures.
#
# `find` covers the other case: a tree that git does not track. That is not a
# fallback for convenience -- it is what lets the test above point this script
# at a fixture directory outside the repository, which is the only way to
# exercise the enumeration without committing a deliberately-failing deck under
# website/public/presentations/, where the real job would measure it.
decks=()
if git -C "$root" rev-parse --is-inside-work-tree >/dev/null 2>&1 &&
  [ -n "$(git -C "$root" ls-files -- '*.html' 2>/dev/null | head -1)" ]; then
  while IFS= read -r -d '' rel; do
    decks+=("$root/$rel")
  done < <(git -C "$root" ls-files -z -- '*.html')
else
  while IFS= read -r -d '' deck; do
    decks+=("$deck")
  done < <(find "$root" -type f -name '*.html' -print0 | LC_ALL=C sort -z)
fi

if [ ${#decks[@]} -eq 0 ]; then
  echo "deck-fit: no HTML under $root" >&2
  exit 1
fi

status=0
measured=0
skipped=0

for deck in "${decks[@]}"; do
  echo "::group::$deck"
  # `|| rc=$?`, not a bare call followed by `rc=$?`: callers run this under
  # `bash -e` (the workflow's default shell), which would abort on the first
  # failing deck before the status was ever read -- losing both the remaining
  # decks and the summary line below.
  rc=0
  node "$measurer" "$deck" || rc=$?
  echo "::endgroup::"
  case $rc in
  0) measured=$((measured + 1)) ;;
  3)
    skipped=$((skipped + 1))
    echo "deck-fit: SKIPPED $deck (not a fixed-canvas deck)"
    ;;
  *)
    measured=$((measured + 1))
    status=1
    ;;
  esac
done

echo "deck-fit: ${#decks[@]} file(s) found, $measured measured, $skipped skipped."
exit $status
