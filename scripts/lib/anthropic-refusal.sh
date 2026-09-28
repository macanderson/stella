#!/usr/bin/env bash
#
# What to say when the Anthropic API says no.
#
# `.github/workflows/triage-schedule.yml` calls the API once per issue. When
# the answer is not a 200, the sweep stops. This file says why, in a line a
# person can read on the run page without the job log.
#
# ## The credit shape
#
# When the account runs out of credit, the API sends a 400 with the type
# `invalid_request_error` and a message that says the credit balance is too
# low. Nothing in this tree can fix that. Someone has to add credit. So that
# shape gets its own line, `ANTHROPIC CREDIT REFUSAL`, and it says this is
# billing and not code. Every other refusal gets `ANTHROPIC API REFUSAL` with
# its status, type and message.
#
# ## Bodies that are not JSON
#
# A proxy or a load balancer can answer in HTML. Then jq fails. That must not
# hide the status, so each jq call here has a fallback, and the report still
# names the status and says the body was not the API's error JSON.
#
# ## Output
#
# Each line goes to stderr. When `GITHUB_STEP_SUMMARY` is set, it goes to the
# run summary too. Under Actions, one `::error::` line puts it on the run page
# as well.
#
# The function never exits and always returns 0. The caller keeps its own
# `exit 1`, so a refusal still fails the run.
#
# Sourced, never run. It defines functions and does nothing else.

# What kind of body this is: `credit`, `api` or `other`. `other` means the
# body is not one JSON value of the API's error shape. Slurp mode reads the
# whole file as one list, so a file with two values, or none, is `other` too.
anthropic_refusal_kind() {
  local body="$1" kind
  kind=$(jq -rs '
    if length == 1
      and (.[0] | type) == "object"
      and .[0].type == "error"
      and (.[0].error | type) == "object"
    then
      if .[0].error.type == "invalid_request_error"
        and ((.[0].error.message // "") | tostring | ascii_downcase
             | contains("credit balance is too low"))
      then "credit" else "api" end
    else "other" end
  ' "$body" 2>/dev/null) || kind=other
  case "$kind" in
  credit | api) printf '%s' "$kind" ;;
  *) printf 'other' ;;
  esac
}

# One field of `error`, on one line. Empty when it is not there.
anthropic_refusal_field() {
  local body="$1" field="$2" value
  value=$(jq -rs --arg f "$field" '.[0].error[$f] // "" | tostring' \
    "$body" 2>/dev/null) || value=""
  printf '%s' "$value" | tr '\r\n' '  '
}

# The report. The first argument is the HTTP status. The second is the path
# of the file that holds the body.
report_anthropic_refusal() {
  local status="$1" body="$2" kind err_type message title
  local lines=()

  kind=$(anthropic_refusal_kind "$body")
  err_type=$(anthropic_refusal_field "$body" type)
  message=$(anthropic_refusal_field "$body" message)

  case "$kind" in
  credit)
    title="ANTHROPIC CREDIT REFUSAL: the account is out of credit. This is billing, not code."
    lines+=("status: $status")
    lines+=("error.type: $err_type")
    lines+=("error.message: $message")
    lines+=("fix: add credit under Plans & Billing in the Anthropic console. No code change is needed.")
    ;;
  api)
    title="ANTHROPIC API REFUSAL: HTTP $status"
    lines+=("status: $status")
    lines+=("error.type: $err_type")
    lines+=("error.message: $message")
    ;;
  *)
    title="ANTHROPIC API REFUSAL: HTTP $status"
    lines+=("status: $status")
    lines+=("the body is not the API's error JSON")
    ;;
  esac

  echo "$title" >&2
  local line
  for line in "${lines[@]}"; do
    echo "  $line" >&2
  done
  if [ "$kind" = other ] && [ -s "$body" ]; then
    echo "  first 400 bytes of the body:" >&2
    head -c 400 "$body" >&2 || true
    echo >&2
  fi

  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    {
      echo "### $title"
      echo
      for line in "${lines[@]}"; do
        echo "- $line"
      done
    } >>"$GITHUB_STEP_SUMMARY" || true
  fi

  if [ "${GITHUB_ACTIONS:-}" = true ]; then
    # A workflow command ends at a newline, and `%` starts an escape, so
    # both are encoded the way the runner reads them back.
    local note="$title ${lines[*]}"
    note=${note//'%'/'%25'}
    note=${note//$'\r'/'%0D'}
    note=${note//$'\n'/'%0A'}
    if [ "$kind" = credit ]; then
      echo "::error title=Anthropic credit refusal::$note"
    else
      echo "::error title=Anthropic API refusal::$note"
    fi
  fi

  return 0
}
