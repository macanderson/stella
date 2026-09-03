#!/usr/bin/env bash
#
# Witness test. The release check must walk every page of the list. It must
# also refuse a list it knows it did not finish reading.
#
#   ./scripts/test-releases-published-pagination.sh
#
# scripts/test-releases-published.sh proves the RULE has no cap. It does not
# reach the FETCH. This test drives the fetch with a fake `gh` on PATH, so it
# needs no network.
#
# Two arms, both against the script this tree ships:
#
#   PAGES — 1001 releases arrive over 11 pages. All of them are walked, and
#           nothing reports truncation. A fixed cap fails here.
#   CEILING — a walk needs more pages than `max_pages` allows. The script
#           refuses. Dropping that guard fails here.
#
# Neither arm reads a second copy of the script out of a git ref. Say the
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
# inside it and is skipped. So the only thing the script can report is
# its own truncation guard. The fixture never has to fake a real git tag.
grace_secs=315360000 # 10 years
now="$(date -u +%s)"

# The fake `gh`. The script under test makes one call,
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

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
