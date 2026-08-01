#!/bin/bash
# Prove EFFORT actually lands, on both arms, before a scored run spends anything.
#
# `preflight-parity` proves both keys reach the same model with thinking on. It
# says nothing about effort, which is the whole variable a pinned-effort run
# exists to fix. The failure being prevented is the one that voided three
# earlier head-to-heads: a flag was passed, assumed effective, and never checked
# on the wire. An unexercised guard is indistinguishable from a broken one.
#
# Three independent checks, because each can pass while another fails:
#
#   1. The API honours `output_config.effort` on this model, for BOTH keys.
#      Probed behaviourally — same prompt at `low` and at the target tier — so
#      the check tests the observable effect rather than that the field was
#      accepted and ignored. Anthropic rejects unknown fields, but "accepted"
#      and "acted on" are different claims and only one of them is the point.
#   2. Stella's frozen posture actually carries the target tier. Asserted by
#      digest, not by reading the constant, so the value the launcher delivers
#      and the value asserted here cannot drift.
#   3. Claude Code's CLI accepts `--effort <tier>`. Deferred to the in-container
#      smoke run: the CLI is installed per trial from `claude.ai/install.sh`, so
#      the host has no copy to interrogate.
#
# Usage: preflight_effort.sh [model] [tier] [expected-posture-digest-prefix]
set -uo pipefail

MODEL="${1:-claude-sonnet-5}"
TIER="${2:-xhigh}"
EXPECT_DIGEST="${3:-5759a7ad}"
ENV_FILE="${TB_ANTHROPIC_ENV_FILE:-$HOME/.env.harbor-anthropic.local}"
REPO="${TB_REPO:-$HOME/stella}"

test -f "$ENV_FILE" || { echo "FATAL: no env file at $ENV_FILE"; exit 1; }
set -a
# shellcheck source=/dev/null
. "$ENV_FILE"
set +a
: "${STELLA_ANTHROPIC_API_KEY:?missing STELLA_ANTHROPIC_API_KEY}"
: "${CLAUDE_CODE_ANTHROPIC_API_KEY:?missing CLAUDE_CODE_ANTHROPIC_API_KEY}"

FAIL=0

# A prompt with genuine reasoning depth, so the tiers have somewhere to differ.
# A trivial prompt collapses every tier onto the same short answer and the
# comparison proves nothing.
read -r -d '' PROMPT <<'PROMPT_END'
A 3x3 grid of switches, each toggling itself and its orthogonal neighbours.
Prove whether every configuration is solvable, and give the full reasoning.
PROMPT_END

# Bounded by the request being non-streaming, not by generosity. Anthropic
# documents that a large `max_tokens` without streaming exceeds HTTP timeouts
# as idle connections drop, and that is not theoretical here: at 64000 both
# xhigh probes died on the 300s read timeout while 32000 completed. Raising
# this further requires switching the probe to streaming first.
MAX_TOKENS=32000

probe() { # $1=key  $2=effort  -> output_tokens | ERROR:...
  # The key goes through the environment, never argv. A process's command line
  # is world-readable via ps/pgrep on a shared host, so passing a credential
  # as an argument publishes it to every user on the box for the life of the
  # call — and this script's calls are the slow ones. The env block of another
  # user's process is not readable the same way.
  PROBE_KEY="$1" python3 - "$2" "$MODEL" "$PROMPT" "$MAX_TOKENS" <<'PY'
import json, os, sys, urllib.request, urllib.error

key = os.environ["PROBE_KEY"]
effort, model, prompt, max_tokens = sys.argv[1:5]
body = json.dumps({
    "model": model,
    "max_tokens": int(max_tokens),
    "thinking": {"type": "adaptive"},
    "output_config": {"effort": effort},
    "messages": [{"role": "user", "content": prompt}],
}).encode()
req = urllib.request.Request(
    "https://api.anthropic.com/v1/messages",
    data=body,
    headers={
        "x-api-key": key,
        "anthropic-version": "2023-06-01",
        "content-type": "application/json",
    },
)
try:
    with urllib.request.urlopen(req, timeout=300) as r:
        print(json.load(r).get("usage", {}).get("output_tokens", 0))
except urllib.error.HTTPError as e:
    # The body carries the actual reason (unknown field, bad tier, bad key);
    # the status alone would not distinguish "effort rejected" from "no quota".
    try:
        msg = json.load(e).get("error", {}).get("message", "")
    except Exception:
        msg = e.reason
    print("ERROR:%s" % str(msg)[:100])
except Exception as e:
    print("ERROR:%s" % str(e)[:100])
PY
}

echo "== effort preflight: model=$MODEL tier=$TIER =="

for arm in stella claude; do
  case "$arm" in
    stella) key="$STELLA_ANTHROPIC_API_KEY" ;;
    claude) key="$CLAUDE_CODE_ANTHROPIC_API_KEY" ;;
  esac
  # Wall-clock is reported alongside tokens because an xhigh turn's duration is
  # the direct input to the truncation risk: Harbor kills a trial at its agent
  # timeout, and a tier that reasons for minutes per step is the most likely
  # way this run degrades. Free to measure here, expensive to discover at 89x2.
  t0=$(date +%s); lo=$(probe "$key" low);     t1=$(date +%s)
  hi=$(probe "$key" "$TIER");                 t2=$(date +%s)
  echo "  $arm: low=$lo ($((t1 - t0))s)  $TIER=$hi ($((t2 - t1))s)"
  case "$lo$hi" in
    *ERROR*) echo "FAIL: $arm effort probe errored"; FAIL=1; continue ;;
  esac
  # Only the LOW tier pinning at the ceiling destroys the comparison. If low
  # pins, both tiers report max_tokens and the check compares the cap to
  # itself — which is how the first version of this script "passed" while
  # proving nothing.
  #
  # The high tier pinning is not the same failure. With low strictly under the
  # ceiling and high at it, low < MAX = high, so `high > low` is established;
  # the tier's true spend is merely unmeasured, not unproven. Failing that
  # case would demand a ceiling high enough to contain xhigh, and this request
  # is non-streaming, so raising the ceiling reintroduces the read timeout.
  # Being unable to measure a bound is not the same as being unable to prove
  # an inequality, and only the inequality is what this check claims.
  if [ "$lo" -ge "$MAX_TOKENS" ] 2>/dev/null; then
    echo "FAIL: $arm low tier pinned at max_tokens=$MAX_TOKENS — both tiers are"
    echo "      at the ceiling, so this compares the cap to itself, not effort"
    FAIL=1
    continue
  fi
  if [ "$hi" -ge "$MAX_TOKENS" ] 2>/dev/null; then
    echo "       note: $TIER clipped at $MAX_TOKENS — spend is >= that, exact value unmeasured"
  fi
  # Strictly greater, not >=: identical counts are the signature of a field
  # that was parsed and discarded, which is the exact defect being hunted.
  if ! [ "$hi" -gt "$lo" ] 2>/dev/null; then
    echo "FAIL: $arm $TIER ($hi) did not exceed low ($lo) — effort may be ignored"
    FAIL=1
  fi
done

# Assert by digest rather than by reading the constant: the digest is what the
# trusted launcher delivers and what the manifest registers as the SUT, so a
# match here proves the run and this check agree on one posture.
# Loaded by file path, not as `stella_harbor.posture`: the package __init__
# imports `harbor`, so the package route only works inside the adapter venv and
# would make this check silently venv-dependent. posture.py is a pure function
# of the model and arm with no Harbor, Docker, or credential dependency —
# which is exactly why it was split into its own module — so importing the
# file directly is honest rather than a workaround.
posture=$(cd "$REPO" && python3 - "$MODEL" <<'PY' 2>&1
import importlib.util, sys

spec = importlib.util.spec_from_file_location(
    "_posture", "bench/harbor_adapter/stella_harbor/posture.py"
)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
p, _, d = mod._benchmark_engine_posture("anthropic/%s" % sys.argv[1])
print("%s %s" % (d[:8], p["agents"]["worker"]["effort"]))
PY
)
echo "  posture: $posture"
if [ "$posture" != "$EXPECT_DIGEST $TIER" ]; then
  echo "FAIL: posture is not the $TIER arm (expected '$EXPECT_DIGEST $TIER')"
  FAIL=1
fi

if [ $FAIL -eq 0 ]; then
  echo "EFFORT PREFLIGHT PASS: $TIER honoured on both keys; Stella posture pinned $TIER"
  echo "NOTE: Claude Code's own --effort flag is verified in-container by the smoke run."
else
  echo "EFFORT PREFLIGHT FAIL — do not launch"
fi
exit $FAIL
