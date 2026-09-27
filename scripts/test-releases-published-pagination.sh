#!/usr/bin/env bash
#
# Witness test. The release check must walk every page of the list. It must
# also refuse a list it knows it did not finish reading, and it must check an
# empty list before it reports every tag as unpublished.
#
#   ./scripts/test-releases-published-pagination.sh
#
# scripts/test-releases-published.sh proves the RULE has no cap. It does not
# reach the FETCH. This test drives the fetch with a fake `gh` on PATH, so it
# needs no network.
#
# Five arms, all against the script this tree ships:
#
#   PAGES — 1001 releases arrive over 11 pages. All of them are walked, and
#           nothing reports truncation. A fixed cap fails here.
#   CEILING — a walk needs more pages than `max_pages` allows. The script
#           refuses. Dropping that guard fails here.
#   EMPTY-THEN-FOUND — the first list read is one empty page, and the second
#           holds the release. The run is clean. A script that trusts the
#           first empty read reports the tag as absent and fails here.
#   EMPTY-LATEST-FOUND — every list read is empty, but `releases/latest`
#           names a release. The script refuses with `::error::` and prints
#           no report. Dropping that check fails here.
#   EMPTY-NOTHING-PUBLISHED — every list read is empty and `releases/latest`
#           answers 404. The tag is still reported as absent. This is the
#           control: the real failure must stay loud.
#
# The three EMPTY arms need one tag older than the grace window, so they run
# in a throwaway git repository holding a single tag dated 2000-01-01. The
# script reads tags from its working directory, and that keeps this
# checkout's own tags out of it.
#
# No arm reads a second copy of the script out of a git ref. Say the
# fail-side asserted that the copy on `origin/main` errors. That holds until
# the fix merges. After that, `origin/main` has the fix too. The arm then
# watches the fixed script pass, and the guard fails on every branch while
# the tree is clean. Driving the shipping script's own ceiling keeps the
# fail-side on live code. It also needs no `git fetch`, so a shallow checkout
# and a full clone run the same test.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-releases-published.sh"

pass=0
fail=0

work="$(mktemp -d)"
cleanup() { rm -rf "$work"; }
trap cleanup EXIT

# A grace window wide enough that every real tag in this checkout falls
# inside it and is skipped. So in the PAGES and CEILING arms, the only thing
# the script can report is its own truncation guard. The EMPTY arms bring
# their own older tag, in a fixture repository further down.
grace_secs=315360000 # 10 years
now="$(date -u +%s)"

# The fake `gh` for PAGES and CEILING. On a list that is not empty, the
# script under test makes one call,
# `gh api --paginate --slurp .../releases?per_page=100`, and the shim answers
# it with `$pages` pages of `$per_page` releases each — the shape real `gh`
# returns under `--slurp`, an array of page arrays.
#
# `write_shim <dir> <pages> <per_page>` builds one in its own directory, so an
# arm's PATH holds exactly the fixture it asked for.
write_shim() {
  mkdir -p "$1"
  cat >"$1/gh" <<SHIM
#!/usr/bin/env bash
set -euo pipefail
if [ "\$1" = "api" ]; then
  jq -n --argjson pages $2 --argjson per_page $3 '
    [ range(0; \$pages)
      | . as \$p
      | [ range(0; \$per_page)
          | { tag_name: ("v0.0." + (((\$p * \$per_page) + .) | tostring)), draft: false } ]
    ]
  '
  exit 0
fi
echo "test-releases-published-pagination.sh: unhandled fake gh invocation: \$*" >&2
exit 3
SHIM
  chmod +x "$1/gh"
}

# 1001 releases over 11 pages: one past the cap the old fetch stopped at, in
# the page shape a real `--paginate` walk produces.
write_shim "$work/pages" 11 91

# `max_pages` in check-releases-published.sh is 1000, so 1001 pages is one past
# its sanity ceiling. One release per page keeps the fixture cheap — the script
# refuses on the page count before it ever flattens them.
write_shim "$work/ceiling" 1001 1

run_with_fake_gh() {
  local shim="$1"
  shift
  PATH="$shim:$PATH" "$@" --now "$now" --grace-secs "$grace_secs" 2>&1
}

# The fake `gh` for the EMPTY arms. It counts list reads in a file beside
# itself. Each list read returns one empty page until read number
# `<found_on_read>`, and from then on a page holding the published release
# v0.0.0. A `<found_on_read>` of 0 means every read is empty.
# `releases/latest` prints `<latest_tag>`, or fails with a 404 when that is
# empty, which is what real `gh` does for a repository with no release.
#
# `write_empty_shim <dir> <found_on_read> <latest_tag>`
write_empty_shim() {
  mkdir -p "$1"
  printf '%s\n' "$2" >"$1/found-on-read"
  printf '%s\n' "$3" >"$1/latest-tag"
  printf '0\n' >"$1/list-reads"
  cat >"$1/gh" <<'SHIM'
#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
case "$*" in
  *releases/latest*)
    tag="$(cat "$here/latest-tag")"
    if [ -z "$tag" ]; then
      echo '{"message":"Not Found","status":"404"}'
      echo 'gh: Not Found (HTTP 404)' >&2
      exit 1
    fi
    echo "$tag"
    exit 0
    ;;
  *releases\?per_page=100*)
    reads=$(( $(cat "$here/list-reads") + 1 ))
    echo "$reads" >"$here/list-reads"
    found_on="$(cat "$here/found-on-read")"
    if [ "$found_on" -gt 0 ] && [ "$reads" -ge "$found_on" ]; then
      echo '[[{"tag_name":"v0.0.0","draft":false}]]'
    else
      echo '[[]]'
    fi
    exit 0
    ;;
esac
echo "test-releases-published-pagination.sh: unhandled fake gh invocation: $*" >&2
exit 3
SHIM
  chmod +x "$1/gh"
}

# One lightweight tag, v0.0.0, on a commit dated 2000-01-01. That is past the
# ten-year grace window, so it is the one tag the EMPTY arms can report.
#
# A `GIT_DIR` inherited from a git hook would point these commands at the
# real repository. Unsetting it keeps the fixture commit out of this clone.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE
fixture_repo="$work/tags"
mkdir -p "$fixture_repo"
git -C "$fixture_repo" init -q
git -C "$fixture_repo" config user.email t@t.invalid
git -C "$fixture_repo" config user.name t
git -C "$fixture_repo" config commit.gpgsign false
git -C "$fixture_repo" config tag.gpgsign false
GIT_AUTHOR_DATE='2000-01-01T00:00:00Z' GIT_COMMITTER_DATE='2000-01-01T00:00:00Z' \
  git -C "$fixture_repo" commit -q --no-verify --allow-empty -m fixture
git -C "$fixture_repo" tag v0.0.0

# Runs the script from inside the fixture repository, with no baseline file,
# so v0.0.0 is not grandfathered.
run_in_fixture() {
  local shim="$1"
  (cd "$fixture_repo" && PATH="$shim:$PATH" "$SCRIPT" --now "$now" \
    --grace-secs "$grace_secs" --baseline "$work/no-baseline" 2>&1)
}

write_empty_shim "$work/empty-then-found" 2 ""
write_empty_shim "$work/empty-latest-found" 0 "v0.0.0"
write_empty_shim "$work/empty-nothing" 0 ""

pages_out="$(run_with_fake_gh "$work/pages" "$SCRIPT")"
pages_status=$?
if [ "$pages_status" -eq 0 ] && printf '%s' "$pages_out" | grep -q "^check-releases-published: OK"; then
  pass=$((pass + 1)); echo "ok   PAGES the fetch walks all 1001 releases and reports clean"
else
  fail=$((fail + 1))
  echo "FAIL PAGES the fetch walks all 1001 releases and reports clean — exit ${pages_status}, got: ${pages_out}"
fi

if printf '%s' "$pages_out" | grep -qi "page limit\|sanity ceiling"; then
  fail=$((fail + 1))
  echo "FAIL PAGES must not report a truncation/sanity error on 1001 releases — got: ${pages_out}"
else
  pass=$((pass + 1)); echo "ok   PAGES reports no truncation or page-sanity error on 1001 releases"
fi

ceiling_out="$(run_with_fake_gh "$work/ceiling" "$SCRIPT")"
ceiling_status=$?
if [ "$ceiling_status" -ne 0 ] && printf '%s' "$ceiling_out" | grep -qi "sanity ceiling"; then
  pass=$((pass + 1)); echo "ok   CEILING a walk past the page ceiling is refused, not answered from a short list"
else
  fail=$((fail + 1))
  echo "FAIL CEILING a walk past the page ceiling is refused, not answered from a short list — exit ${ceiling_status}, got: ${ceiling_out}"
fi

found_out="$(run_in_fixture "$work/empty-then-found")"
found_status=$?
found_reads="$(cat "$work/empty-then-found/list-reads")"
if [ "$found_status" -eq 0 ] \
  && printf '%s' "$found_out" | grep -q "^check-releases-published: OK" \
  && printf '%s' "$found_out" | grep -q "first read of the release list was empty" \
  && [ "$found_reads" -eq 2 ]; then
  pass=$((pass + 1)); echo "ok   EMPTY-THEN-FOUND an empty first read is read again, and the second read wins"
else
  fail=$((fail + 1))
  echo "FAIL EMPTY-THEN-FOUND an empty first read is read again, and the second read wins — exit ${found_status}, list reads ${found_reads}, got: ${found_out}"
fi

latest_out="$(run_in_fixture "$work/empty-latest-found")"
latest_status=$?
if [ "$latest_status" -ne 0 ] \
  && printf '%s' "$latest_out" | grep -q "releases/latest names v0.0.0" \
  && ! printf '%s' "$latest_out" | grep -q "never published"; then
  pass=$((pass + 1)); echo "ok   EMPTY-LATEST-FOUND an empty list that releases/latest contradicts is refused, and no report is printed"
else
  fail=$((fail + 1))
  echo "FAIL EMPTY-LATEST-FOUND an empty list that releases/latest contradicts is refused, and no report is printed — exit ${latest_status}, got: ${latest_out}"
fi

nothing_out="$(run_in_fixture "$work/empty-nothing")"
nothing_status=$?
if [ "$nothing_status" -ne 0 ] \
  && printf '%s' "$nothing_out" | grep -q "never published" \
  && printf '%s' "$nothing_out" | grep -Eq "^ +v0\.0\.0 +[0-9]+h +absent$"; then
  pass=$((pass + 1)); echo "ok   EMPTY-NOTHING-PUBLISHED a repository with no release still has its tag reported as absent"
else
  fail=$((fail + 1))
  echo "FAIL EMPTY-NOTHING-PUBLISHED a repository with no release still has its tag reported as absent — exit ${nothing_status}, got: ${nothing_out}"
fi

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
