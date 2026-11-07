#!/usr/bin/env bash
#
# Hermetic tests for scripts/lib/anthropic-refusal.sh.
#
#   ./scripts/test-anthropic-refusal.sh    (or: make anthropic-refusal-test)
#
# No network and no API key. Each case writes a body to a temp file and calls
# the report on it, the way the triage sweep does after `curl`. So the suite
# runs the same on a bare runner as on a dev box.
#
# The triage sweep went red on every run once the account ran out of credit.
# Its log read like any other API failure, and a person had to open it to
# learn the cause. Every case below fails on the old commit: the report it
# calls did not exist yet.
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
# shellcheck source=scripts/lib/anthropic-refusal.sh
. "$repo_root/scripts/lib/anthropic-refusal.sh"

# The runner sets both. Left on, each case would put a real error note on
# the run page of the suite that is testing it, and write to its summary.
unset GITHUB_ACTIONS GITHUB_STEP_SUMMARY

pass=0
fail=0

ok() {
  printf '  \033[32m✓\033[0m %s\n' "$*"
  pass=$((pass + 1))
}
bad() {
  printf '  \033[31m✗\033[0m %s\n' "$*"
  fail=$((fail + 1))
}

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

# Run the report on one status and one body. Stderr lands in $scratch/err,
# stdout in $scratch/out, and the summary in $scratch/summary.
run_report() {
  local status="$1" body="$2"
  printf '%s' "$body" >"$scratch/body"
  : >"$scratch/summary"
  (
    set -euo pipefail
    GITHUB_STEP_SUMMARY="$scratch/summary" \
      report_anthropic_refusal "$status" "$scratch/body"
  ) >"$scratch/out" 2>"$scratch/err"
  code=$?
}

# A file must hold one substring.
has() {
  local name="$1" file="$2" needle="$3"
  if grep -qF -- "$needle" "$file"; then
    ok "$name"
  else
    bad "$name — $file did not contain '$needle': $(cat "$file")"
  fi
}

# A file must not hold one substring.
lacks() {
  local name="$1" file="$2" needle="$3"
  if grep -qF -- "$needle" "$file"; then
    bad "$name — $file should NOT have contained '$needle': $(cat "$file")"
  else
    ok "$name"
  fi
}

# The report must return 0 under `set -e`, so the caller's own `exit 1` is
# what fails the run.
returned_zero() {
  if [ "$code" -eq 0 ]; then
    ok "$1"
  else
    bad "$1 — the report exited $code: $(cat "$scratch/err")"
  fi
}

echo "anthropic-refusal:"

# The exact body the API sent when the account ran out of credit.
credit='{"type":"error","error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API. Please go to Plans & Billing to upgrade or purchase credits."},"request_id":"req_011CfMgzfJBmSP8M3kgVEdzK"}'

run_report 400 "$credit"
returned_zero "a credit refusal returns 0 under set -e"
has "a credit refusal names itself on stderr" "$scratch/err" "ANTHROPIC CREDIT REFUSAL"
has "...and says it is billing" "$scratch/err" "This is billing, not code."
has "...with the status" "$scratch/err" "status: 400"
has "...with error.type" "$scratch/err" "error.type: invalid_request_error"
has "...with error.message" "$scratch/err" "error.message: Your credit balance is too low"
has "a credit refusal names itself in the summary" "$scratch/summary" "ANTHROPIC CREDIT REFUSAL"
has "...with error.type in the summary" "$scratch/summary" "error.type: invalid_request_error"
lacks "...and is not the generic line" "$scratch/err" "ANTHROPIC API REFUSAL"
lacks "...and prints no run-page note off Actions" "$scratch/out" "::error"

# An overload is a real API failure, and it is not a billing one.
overloaded='{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}'

run_report 529 "$overloaded"
returned_zero "an overload returns 0 under set -e"
has "an overload gets the generic line" "$scratch/err" "ANTHROPIC API REFUSAL: HTTP 529"
has "...with error.type" "$scratch/err" "error.type: overloaded_error"
has "...with error.message" "$scratch/err" "error.message: Overloaded"
has "...and the generic line in the summary" "$scratch/summary" "ANTHROPIC API REFUSAL: HTTP 529"
lacks "...and never the credit line on stderr" "$scratch/err" "CREDIT REFUSAL"
lacks "...or in the summary" "$scratch/summary" "CREDIT REFUSAL"

# The credit words under some other error type are not a credit refusal.
# The type is half of the shape, so a message alone cannot make one.
lookalike='{"type":"error","error":{"type":"api_error","message":"credit balance is too low"}}'

run_report 500 "$lookalike"
lacks "the credit words under another type are not a credit refusal" "$scratch/err" "CREDIT REFUSAL"
has "...and get the generic line" "$scratch/err" "ANTHROPIC API REFUSAL: HTTP 500"

# A gateway page in HTML. jq fails on it, and the status must still show.
run_report 502 '<html><body>502 Bad Gateway</body></html>'
returned_zero "a body that is not JSON returns 0 under set -e"
has "a body that is not JSON still names the status" "$scratch/err" "ANTHROPIC API REFUSAL: HTTP 502"
has "...and says the body is not the API's error JSON" "$scratch/err" "the body is not the API's error JSON"
has "...and shows the start of the body" "$scratch/err" "502 Bad Gateway"
has "...and names the status in the summary" "$scratch/summary" "ANTHROPIC API REFUSAL: HTTP 502"
lacks "...and never the credit line" "$scratch/err" "CREDIT REFUSAL"

# A body line that starts with "::" would reach the runner as a workflow
# command if the report printed it bare.
run_report 502 $'gateway\n::error::injected\n'
has "a body line starting with :: is still shown" "$scratch/err" "injected"
if grep -q '^::' "$scratch/err"; then
  bad "...but no stderr line may start with :: — $(cat "$scratch/err")"
else
  ok "...but no stderr line starts with ::"
fi

# An empty body. curl writes nothing when the server sends nothing.
run_report 504 ''
returned_zero "an empty body returns 0 under set -e"
has "an empty body still names the status" "$scratch/err" "ANTHROPIC API REFUSAL: HTTP 504"

# JSON that is not the error shape.
run_report 400 '["not", "an", "error"]'
has "JSON of another shape is not the API's error JSON" "$scratch/err" "the body is not the API's error JSON"

# With no summary file set, stderr still carries the report. An empty
# value counts as unset, the way the report reads it.
printf '%s' "$credit" >"$scratch/body"
out="$(GITHUB_STEP_SUMMARY='' report_anthropic_refusal 400 "$scratch/body" 2>&1)"
case "$out" in
*"ANTHROPIC CREDIT REFUSAL"*) ok "with no summary file, stderr still names the credit refusal" ;;
*) bad "with no summary file, stderr did not name the credit refusal: $out" ;;
esac

# Under Actions, a credit refusal also puts a titled note on the run page.
out="$(GITHUB_STEP_SUMMARY='' GITHUB_ACTIONS=true \
  report_anthropic_refusal 400 "$scratch/body" 2>/dev/null)"
case "$out" in
*"::error title=Anthropic credit refusal::ANTHROPIC CREDIT REFUSAL"*) ok "under Actions, a credit refusal writes a titled run-page note" ;;
*) bad "under Actions, the run-page note was missing or untitled: $out" ;;
esac

# Any other refusal gets the other title, and a `%` in the message is
# encoded so the runner does not read it as the start of an escape.
printf '%s' '{"type":"error","error":{"type":"rate_limit_error","message":"100% of the limit used"}}' >"$scratch/body"
out="$(GITHUB_STEP_SUMMARY='' GITHUB_ACTIONS=true \
  report_anthropic_refusal 429 "$scratch/body" 2>/dev/null)"
case "$out" in
*"::error title=Anthropic API refusal::ANTHROPIC API REFUSAL: HTTP 429"*) ok "under Actions, any other refusal writes the API refusal note" ;;
*) bad "under Actions, the API refusal note was missing or mistitled: $out" ;;
esac
case "$out" in
*"100%25 of the limit used"*) ok "...and encodes a % in the message" ;;
*) bad "...and did not encode the % in the message: $out" ;;
esac

echo
echo "anthropic-refusal: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
