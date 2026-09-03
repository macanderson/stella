#!/usr/bin/env bash
#
# Witness test. Neither release check may depend on a fixed count cap.
#
#   ./scripts/test-releases-published-pagination.sh
#
# scripts/test-releases-published.sh proves the RULE has no cap. It cannot
# reach the FETCH, which is where a cap lives. This test drives the fetch end
# to end with a fake `gh` on PATH, so a fake list of 1001 releases runs with
# no network at all.
#
# It runs two scripts against that same list. One is the shipping script. It
# must page through all 1001 and report clean. The other is a capped copy.
# This file derives that copy from the shipping script. It splices in a fixed
# `--limit` fetch and the truncation error that fetch raises. The copy must
# fail. A fixture is evidence only while something it runs against fails on
# it. So the failing side is derived here, not read from a branch.
#
# Reading the failing side from `origin/main` holds only until the fix merges.
# From that commit on, `main` carries the paged fetch. The failing side then
# reports clean. The arm that asserts it errors fails on every open pull
# request. Deriving the capped copy keeps both directions demonstrated for as
# long as the shipping script has a fetch to splice.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
NEW_SCRIPT="$repo_root/scripts/check-releases-published.sh"
CAPPED_SCRIPT="$repo_root/scripts/.check-releases-published.capped.sh"

pass=0
fail=0

work="$(mktemp -d)"
cleanup() { rm -rf "$work"; rm -f "$CAPPED_SCRIPT"; }
trap cleanup EXIT

# The capped fetch. `gh release list --limit N` asks for a fixed number of
# releases. A full answer may be cut off. Every release past the cut then
# reads as a tag that shipped nothing, so the fetch refuses to trust one. Its
# result is remapped to the `{ name, draft }` shape the rest of the script
# reads. This copy differs from the shipping script in the cap alone.
capped_fetch='releases_limit=1000
capped_json="$(CLICOLOR_FORCE=0 NO_COLOR=1 gh release list --limit "$releases_limit" --json tagName --jq '"'"'[.[].tagName]'"'"')"
if [ "$(printf '"'"'%s'"'"' "$capped_json" | jq '"'"'length'"'"')" -ge "$releases_limit" ]; then
  echo "::error::the release list hit the ${releases_limit} page limit, so it may be truncated — every release past the cut would be reported as an unpublished tag." >&2
  exit 1
fi
releases_json="$(printf '"'"'%s'"'"' "$capped_json" | jq '"'"'[ .[] | { name: ., draft: false } ]'"'"')"'

# The splice runs between these two anchors in the shipping script: the page
# ceiling that opens its fetch, and the assignment that closes it. Both are
# matched at the start of a line and both must appear exactly once. A rewrite
# of that fetch fails here rather than quietly producing a copy with no cap
# left in it, which would pass this test while proving nothing.
splice_start="$(grep -c '^max_pages=' "$NEW_SCRIPT")"
splice_end="$(grep -c '^releases_json=' "$NEW_SCRIPT")"
if [ "$splice_start" != "1" ] || [ "$splice_end" != "1" ]; then
  echo "FAIL SPLICE check-releases-published.sh does not have exactly one '^max_pages=' line (found ${splice_start}) and one '^releases_json=' line (found ${splice_end}), so the capped copy cannot be derived. Re-derive the anchors in $0 against the current fetch." >&2
  exit 1
fi

# `dirname "$0"` in the capped copy must find a folder with lib/help-header.sh,
# so it is written next to the real script rather than in the scratch dir. The
# trap above deletes it; it is never staged or committed.
{
  sed -n "1,$(($(grep -n '^max_pages=' "$NEW_SCRIPT" | cut -d: -f1) - 1))p" "$NEW_SCRIPT"
  printf '%s\n' "$capped_fetch"
  sed -n "$(($(grep -n '^releases_json=' "$NEW_SCRIPT" | cut -d: -f1) + 1)),\$p" "$NEW_SCRIPT"
} >"$CAPPED_SCRIPT"
chmod +x "$CAPPED_SCRIPT"

# A grace window wide enough that every real tag in this checkout falls
# inside it and is skipped. So the only thing either script can report is
# its own truncation guard. The fixture never has to fake a real git tag.
# A tag inside the window is skipped before the grandfather baseline is read.
grace_secs=315360000 # 10 years
now="$(date -u +%s)"

# The fake `gh`. Each script makes one call:
#   the capped copy:     `gh release list --limit 1000 ...`
#   the shipping script: `gh api --paginate --slurp .../releases?per_page=100`
# Both get 1001 fake releases, one past the cap. The shape matches real
# `gh` output: 11 pages of up to 100 each, the last one holding 1.
cat >"$work/gh" <<'SHIM'
#!/usr/bin/env bash
set -euo pipefail
n=1001
if [ "$1" = "api" ]; then
  jq -n --argjson n "$n" '
    [ range(0; $n) | { tag_name: ("v0.0." + (tostring)), draft: false } ]
    | . as $all
    | [ range(0; ($all | length); 100) as $i | $all[$i:$i + 100] ]
  '
  exit 0
fi
if [ "$1" = "release" ] && [ "${2:-}" = "list" ]; then
  jq -n --argjson n "$n" '[ range(0; $n) | ("v0.0." + (tostring)) ]'
  exit 0
fi
echo "test-releases-published-pagination.sh: unhandled fake gh invocation: $*" >&2
exit 3
SHIM
chmod +x "$work/gh"

run_with_fake_gh() {
  PATH="$work:$PATH" "$@" --now "$now" --grace-secs "$grace_secs" 2>&1
}

capped_out="$(run_with_fake_gh "$CAPPED_SCRIPT")"
capped_status=$?
if [ "$capped_status" -ne 0 ] && printf '%s' "$capped_out" | grep -q "page limit"; then
  pass=$((pass + 1)); echo "ok   CAPPED the capped fetch hits its 1000-item cap on 1001 releases and errors"
else
  fail=$((fail + 1))
  echo "FAIL CAPPED the capped fetch hits its 1000-item cap on 1001 releases and errors — exit ${capped_status}, got: ${capped_out}"
fi

new_out="$(run_with_fake_gh "$NEW_SCRIPT")"
new_status=$?
if [ "$new_status" -eq 0 ] && printf '%s' "$new_out" | grep -q "^check-releases-published: OK"; then
  pass=$((pass + 1)); echo "ok   NEW the shipping script pages through all 1001 releases and reports clean"
else
  fail=$((fail + 1))
  echo "FAIL NEW the shipping script pages through all 1001 releases and reports clean — exit ${new_status}, got: ${new_out}"
fi

if printf '%s' "$new_out" | grep -qi "page limit\|sanity ceiling"; then
  fail=$((fail + 1))
  echo "FAIL NEW must not report a truncation/sanity error on 1001 releases — got: ${new_out}"
else
  pass=$((pass + 1)); echo "ok   NEW reports no truncation or page-sanity error on 1001 releases"
fi

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
