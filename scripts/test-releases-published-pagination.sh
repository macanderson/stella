#!/usr/bin/env bash
#
# Witness test. Neither release check may depend on a fixed count cap.
#
#   ./scripts/test-releases-published-pagination.sh
#
# scripts/test-releases-published.sh proves the RULE has no cap. It cannot
# reach the FETCH, where the old cap actually lived. This test drives the
# fetch end to end with a fake `gh` on PATH, so a fake list of 1001 releases
# runs with no network at all.
#
# Two scripts run against that same list. One is the capped script named by
# CAPPED_REV below. The other is the one in this working tree. The capped one
# hits its cap and errors. This tree's one pages through all 1001 and reports
# clean.
#
# The fail-side names a commit, never `origin/main`. Point it at a moving ref
# and it proves nothing once the fix merges there. It then asserts that the
# fixed script still carries the bug, and fails on every open pull request at
# once. CAPPED_REV is the last commit that carried the cap. Both halves of
# this witness hold for as long as that commit does.
#
# bash 3.2 compatible.

set -uo pipefail

# 5096b3f — the last commit whose check-releases-published.sh still called
# `gh release list --limit` rather than `gh api --paginate`. Its copy of that
# script is the capped one this test drives.
CAPPED_REV="5096b3ff7df2e12986957287fe13b03ebbeacc3e"

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
NEW_SCRIPT="$repo_root/scripts/check-releases-published.sh"
CAPPED_SCRIPT="$repo_root/scripts/.check-releases-published.capped-copy.sh"

pass=0
fail=0

work="$(mktemp -d)"
cleanup() { rm -rf "$work"; rm -f "$CAPPED_SCRIPT"; }
trap cleanup EXIT

# CI checks out one ref with no history, so CAPPED_REV may be absent. One
# shallow fetch of that single commit gets it. The shared checkout step every
# other guard self-test runs under stays as it is. This only ever reads;
# nothing here can move a real branch.
git -C "$repo_root" fetch --depth=1 -q origin "$CAPPED_REV" 2>/dev/null || true

# `dirname "$0"` in each script must find a folder with lib/help-header.sh.
# So the capped copy is placed next to the real script, not in the scratch
# dir. It is a sibling file, deleted by the trap above, never staged.
if ! git -C "$repo_root" show "$CAPPED_REV:scripts/check-releases-published.sh" >"$CAPPED_SCRIPT" 2>/dev/null; then
  echo "SKIP — could not read ${CAPPED_REV}'s copy of check-releases-published.sh (no network, or a shallow clone that cannot reach it); the fail-side of the witness cannot run, but this tree's own fetch is still exercised below." >&2
else
  chmod +x "$CAPPED_SCRIPT"
fi

# A grace window wide enough that every real tag in this checkout falls
# inside it and is skipped. So the only thing either script can report is
# its own truncation guard. The fixture never has to fake a real git tag.
grace_secs=315360000 # 10 years
now="$(date -u +%s)"

# The fake `gh`. Each script makes one call:
#   the capped version: `gh release list --limit 1000 ...`
#   this tree:          `gh api --paginate --slurp .../releases?per_page=100`
# Both get 1001 fake releases, one past the old cap. The shape matches real
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

if [ -x "$CAPPED_SCRIPT" ]; then
  capped_out="$(run_with_fake_gh "$CAPPED_SCRIPT")"
  capped_status=$?
  if [ "$capped_status" -ne 0 ] && printf '%s' "$capped_out" | grep -q "page limit"; then
    pass=$((pass + 1)); echo "ok   OLD the capped script hits its 1000-item cap on 1001 releases and errors"
  else
    fail=$((fail + 1))
    echo "FAIL OLD the capped script hits its 1000-item cap on 1001 releases and errors — exit ${capped_status}, got: ${capped_out}"
  fi
fi

new_out="$(run_with_fake_gh "$NEW_SCRIPT")"
new_status=$?
if [ "$new_status" -eq 0 ] && printf '%s' "$new_out" | grep -q "^check-releases-published: OK"; then
  pass=$((pass + 1)); echo "ok   NEW this tree's script pages through all 1001 releases and reports clean"
else
  fail=$((fail + 1))
  echo "FAIL NEW this tree's script pages through all 1001 releases and reports clean — exit ${new_status}, got: ${new_out}"
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
