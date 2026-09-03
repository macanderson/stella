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
# One question is asked twice. Does the script page past 1000 and report no
# truncation? The shipped script must pass. A stand-in that stops at a fixed
# cap must fail. The second arm is what gives the first its meaning. A check
# that passes a capped fetcher too would measure nothing.
#
# An earlier version put main's copy of the script in that second arm. It
# asserted main still had the cap. The fix then merged to main, so the arm
# was wrong, and every new pull request went red on it. A self-test that
# compares against main lasts exactly one merge. This one builds its own
# stand-in, so it does not expire.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
NEW_SCRIPT="$repo_root/scripts/check-releases-published.sh"

pass=0
fail=0

work="$(mktemp -d)"
cleanup() { rm -rf "$work"; }
trap cleanup EXIT

# A grace window wide enough that every real tag in this checkout falls
# inside it and is skipped. So the only thing either script can report is
# its own truncation guard. The fixture never has to fake a real git tag.
grace_secs=315360000 # 10 years
now="$(date -u +%s)"

# The fake `gh`. Two call shapes are served:
#   the shipped script: `gh api --paginate --slurp .../releases?per_page=100`
#   the capped stand-in: `gh release list --limit N ...`
# Both get 1001 fake releases, one past the old cap. The `api` shape matches
# real `gh --slurp` output: 11 pages of up to 100 each, the last holding 1.
# The `release list` shape ignores the `--limit` and serves all 1001. So a
# caller that trusts a fixed cap gets more than it asked for and trips its
# own ceiling. That trip is what the capped arm below has to observe.
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

# The stand-in for the shape this guard rejects: one fetch behind a fixed
# cap, plus the ceiling check that fetch needs. It is written here, not read
# from a branch. So nothing outside this file can change what the capped arm
# means.
cat >"$work/capped-fetch.sh" <<'CAPPED'
#!/usr/bin/env bash
set -uo pipefail
limit=1000
count="$(gh release list --limit "$limit" | jq 'length')"
if [ "$count" -ge "$limit" ]; then
  echo "::error::the release list hit its ${limit}-item page limit, so this run cannot see every release." >&2
  exit 1
fi
echo "check-releases-published: OK — nothing to report (${count} release(s) seen)."
CAPPED
chmod +x "$work/capped-fetch.sh"

# The one assertion, applied to both arms. A script passes when it exits 0,
# says OK, and names no truncation or sanity ceiling. It returns a status
# instead of tallying. That lets the capped arm assert the same check must
# fail, which is how the check is shown to tell the two apart.
pages_cleanly() {
  local out status
  out="$(PATH="$work:$PATH" "$1" --now "$now" --grace-secs "$grace_secs" 2>&1)"
  status=$?
  LAST_OUT="$out"
  [ "$status" -eq 0 ] || return 1
  printf '%s' "$out" | grep -q "^check-releases-published: OK" || return 1
  ! printf '%s' "$out" | grep -qi "page limit\|sanity ceiling"
}

if pages_cleanly "$NEW_SCRIPT"; then
  pass=$((pass + 1))
  echo "ok   NEW the shipped script pages through all 1001 releases with no truncation"
else
  fail=$((fail + 1))
  echo "FAIL NEW the shipped script pages through all 1001 releases with no truncation — got: ${LAST_OUT}"
fi

if pages_cleanly "$work/capped-fetch.sh"; then
  fail=$((fail + 1))
  echo "FAIL CAPPED a fixed-cap fetcher must be caught, but the check above passed it — the NEW arm is vacuous"
else
  pass=$((pass + 1))
  echo "ok   CAPPED a fixed-cap fetcher is caught, so the NEW arm is measuring something"
fi

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
