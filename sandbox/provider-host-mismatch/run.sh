#!/usr/bin/env bash
#
# Provider / host mismatch sandbox runner.
#
# For each row in scenarios.tsv this script:
#   1. starts the zero-dep mock provider on a fresh port, told which fake key
#      that "host" accepts;
#   2. runs REAL `stella config` in a pristine, isolated HOME with fake keys in
#      the env and the mock as --base-url, and reads back which PROVIDER + key
#      + base URL stella resolved (this is the deterministic, network-free proof
#      of the mismatch);
#   3. replays the resolved provider's key against the mock over HTTP to observe
#      the host's real 200/401 (the consequence);
#   4. compares to the expected result and prints a verdict row.
#
# Nothing here uses a real API key or reaches a real provider. Safe to run
# offline, repeatedly, on any machine with python3 + a `stella` binary on PATH.
#
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$HERE"

# --- locate a stella binary -------------------------------------------------
STELLA="${STELLA_BIN:-}"
if [[ -z "$STELLA" ]]; then
  if [[ -x "../../target/release/stella" ]]; then
    STELLA="../../target/release/stella"
  elif command -v stella >/dev/null 2>&1; then
    STELLA="$(command -v stella)"
  else
    echo "error: no stella binary found. Build one (cargo build --release) or set STELLA_BIN." >&2
    exit 1
  fi
fi
echo "stella: $STELLA ($("$STELLA" --version 2>/dev/null | head -1))"

OR_KEY="sk-or-FAKE-openrouter"
ZAI_KEY="zai-FAKE-key"
PORT_BASE=8830
TMP="$(mktemp -d)"
trap 'pkill -f "mock_provider.py" 2>/dev/null; rm -rf "$TMP"' EXIT

strip_ansi() { sed $'s/\x1b\\[[0-9;]*m//g'; }

# Every provider credential stella might auto-detect. We strip ALL of them from
# the inherited environment (the machine running the sandbox may legitimately
# have real keys set) and add back only the scenario's fake ones, so detection
# is decided by the scenario, not by ambient state.
PROVIDER_VARS=(ZAI_API_KEY ANTHROPIC_API_KEY OPENAI_API_KEY XAI_API_KEY \
  DEEPSEEK_API_KEY GEMINI_API_KEY GOOGLE_API_KEY OPENROUTER_API_KEY \
  VERTEX_ACCESS_TOKEN AWS_ACCESS_KEY_ID LOCAL_API_KEY ZAI_GLM_CODING_PLAN)
unset_args=(); for v in "${PROVIDER_VARS[@]}"; do unset_args+=(-u "$v"); done

pass=0; fail=0; i=0
printf '\n%-24s | %-10s | %-9s | %-6s | %s\n' "scenario" "resolved" "host" "expect" "verdict"
printf -- '-------------------------|------------|-----------|--------|--------\n'

while IFS=$'\t' read -r name model_arg host_key mangle expect note; do
  [[ -z "${name:-}" || "$name" == \#* ]] && continue
  i=$((i+1)); port=$((PORT_BASE + i))
  [[ "$model_arg" == "(auto)" ]] && model_arg=""

  # fake keys, with optional corruption of the OpenRouter key
  or_val="$OR_KEY"
  case "$mangle" in
    trailing_space) or_val="${OR_KEY} " ;;
    quoted)         or_val="\"${OR_KEY}\"" ;;
  esac

  # which key does this host accept?
  host_accept="$OR_KEY"; [[ "$host_key" == "zai" ]] && host_accept="$ZAI_KEY"

  # start the mock for this host
  python3 mock_provider.py --port "$port" --accept-key "$host_accept" \
      --provider-label "${host_key}-host" >"$TMP/mock.$i.log" 2>&1 &
  mock_pid=$!
  # wait for it to bind
  for _ in $(seq 1 20); do curl -s "http://127.0.0.1:$port/whoami" >/dev/null 2>&1 && break; sleep 0.1; done

  # pristine HOME so no real credentials.toml / settings.json interfere
  fakehome="$TMP/home.$i"; mkdir -p "$fakehome"

  # Auto-detect (no --model) is order-and-presence sensitive: to keep the
  # scenario deterministic and portable we expose ONLY the OpenRouter key so
  # detection lands on OpenRouter regardless of provider order. When a provider
  # is pinned via --model, both keys are present (the pin decides).
  env_args=(HOME="$fakehome" NO_COLOR=1 OPENROUTER_API_KEY="$or_val")
  [[ -n "$model_arg" ]] && env_args+=(ZAI_API_KEY="$ZAI_KEY")

  # (2) what does stella resolve?  (env -u strips ambient real keys first)
  cfg="$(env "${unset_args[@]}" "${env_args[@]}" \
        "$STELLA" --base-url "http://127.0.0.1:$port/v1" $model_arg config 2>&1 | strip_ansi)"
  resolved_provider="$(printf '%s\n' "$cfg" | sed -n 's/.*Provider:[[:space:]]*//p' | head -1)"

  # (3) replay the resolved provider's key against the host
  case "$resolved_provider" in
    *Z.ai*|*zai*) sent="$ZAI_KEY" ;;
    *)            sent="$or_val" ;;   # OpenRouter (and default) -> the OR key (mangled if any)
  esac
  code="$(curl -s -o /dev/null -w '%{http_code}' -X POST \
      "http://127.0.0.1:$port/v1/chat/completions" \
      -H "Authorization: Bearer ${sent}" -H "Content-Type: application/json" \
      -d '{"model":"x","messages":[]}')"

  kill "$mock_pid" 2>/dev/null; wait "$mock_pid" 2>/dev/null

  verdict="PASS"; [[ "$code" == "$expect" ]] || verdict="FAIL"
  [[ "$verdict" == "PASS" ]] && pass=$((pass+1)) || fail=$((fail+1))
  short_prov="$(printf '%s' "$resolved_provider" | cut -c1-10)"
  printf '%-24s | %-10s | %-9s | %-6s | %s (got %s)\n' \
      "$name" "$short_prov" "$host_key" "$expect" "$verdict" "$code"
done < scenarios.tsv

printf -- '-------------------------|------------|-----------|--------|--------\n'
printf 'total: %d  pass: %d  fail: %d\n\n' "$i" "$pass" "$fail"
[[ "$fail" -eq 0 ]] || exit 1
