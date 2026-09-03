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
# It runs two scripts against that same list: the one `main` ships today,
# and the one in this working tree. Main's script hits its old cap and
# errors. This branch's script pages through all 1001 and reports clean.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
NEW_SCRIPT="$repo_root/scripts/check-releases-published.sh"
MAIN_SCRIPT="$repo_root/scripts/.check-releases-published.main-copy.sh"

pass=0
fail=0

work="$(mktemp -d)"
cleanup() { rm -rf "$work"; rm -f "$MAIN_SCRIPT"; }
trap cleanup EXIT

# CI checks out one ref with no history, so `origin/main` may not exist yet.
# One shallow, on-demand fetch of just that branch tip fixes that without
# touching the shared checkout step every other guard self-test also runs
# under. It only ever reads; nothing here can move a real branch.
git -C "$repo_root" fetch --depth=1 -q origin main 2>/dev/null || true

# `dirname "$0"` in each script must find a folder with lib/help-header.sh.
# So main's copy is placed next to the real script, not in the scratch dir.
# It is a sibling file, deleted by the trap above, never staged or committed.
if ! git -C "$repo_root" show origin/main:scripts/check-releases-published.sh >"$MAIN_SCRIPT" 2>/dev/null; then
  echo "SKIP — could not read origin/main's copy of check-releases-published.sh (no network or no origin/main ref here); the fail-side of the witness cannot run, but this branch's own fetch is still exercised below." >&2
else
  chmod +x "$MAIN_SCRIPT"
fi

# A grace window wide enough that every real tag in this checkout falls
# inside it and is skipped. So the only thing either script can report is
# its own truncation guard. The fixture never has to fake a real git tag.
grace_secs=315360000 # 10 years
now="$(date -u +%s)"

# The fake `gh`. Each script makes one call:
#   main's version: `gh release list --limit 1000 ...`
#   this branch:    `gh api --paginate --slurp .../releases?per_page=100`
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

if [ -x "$MAIN_SCRIPT" ]; then
  main_out="$(run_with_fake_gh "$MAIN_SCRIPT")"
  main_status=$?
  if [ "$main_status" -ne 0 ] && printf '%s' "$main_out" | grep -q "page limit"; then
    pass=$((pass + 1)); echo "ok   MAIN main's script hits its 1000-item cap on 1001 releases and errors"
  else
    fail=$((fail + 1))
    echo "FAIL MAIN main's script hits its 1000-item cap on 1001 releases and errors — exit ${main_status}, got: ${main_out}"
  fi
fi

new_out="$(run_with_fake_gh "$NEW_SCRIPT")"
new_status=$?
if [ "$new_status" -eq 0 ] && printf '%s' "$new_out" | grep -q "^check-releases-published: OK"; then
  pass=$((pass + 1)); echo "ok   NEW this branch's script pages through all 1001 releases and reports clean"
else
  fail=$((fail + 1))
  echo "FAIL NEW this branch's script pages through all 1001 releases and reports clean — exit ${new_status}, got: ${new_out}"
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
