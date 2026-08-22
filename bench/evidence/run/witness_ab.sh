#!/bin/bash
# One arm of the authored-witness A/B (#1284).
#
# The experiment is two runs of the *same* task set on the *same* SUT, differing
# in exactly one thing: whether a second model, independent of the worker,
# authors the failing test that proves the work.
#
#   off — the control arm. Every role inherits TB_MODEL, so no author is
#         independent of the worker and the authored-witness tier cannot run on
#         any task. This is the arm every published Stella number came from.
#   on  — the treatment arm. STELLA_WITNESS_AUTHOR_MODEL names a second model on
#         the worker's provider; it reaches Stella only as `pipeline_judge_model`
#         inside the hashed posture.
#
# Not `primary.sh`: that scores a preregistered phase of the whole frozen
# dataset against a fixed denominator. This runs one arm of a paired experiment
# over a task list the operator fixes once and uses for both arms. Not `tune.sh`
# either: that moves two knobs at once by design and its task list is selected
# on prior results, so nothing it produces is a measurement of anything.
#
# Usage:
#   export STELLA_WITNESS_AUTHOR_MODEL=openrouter/deepseek/deepseek-v4-pro
#   bench/evidence/run/witness_ab.sh off wab1-off  "$TB_ROOT/witness_ab.tasks"
#   bench/evidence/run/witness_ab.sh on  wab1-on   "$TB_ROOT/witness_ab.tasks"
#
# then, once both arms are extracted into evidence directories:
#   python3 bench/evidence/compare_arms.py <off>/trials.jsonl <on>/trials.jsonl \
#       --tasks <denominator> --markdown <run>/results.md
set -uo pipefail

ARM="${1:?usage: witness_ab.sh <off|on> <job-name> [task-file]}"
JOB="${2:?usage: witness_ab.sh <off|on> <job-name> [task-file]}"

# The author is read here, before env.sh unsets an empty one, because the
# control arm has to run with the variable *absent* while the operator keeps it
# exported in their shell for the other arm. Selecting the arm by argument
# rather than by what happens to be in the environment is the whole point: an
# arm chosen by ambient state is an arm nobody can reproduce.
AUTHOR="${STELLA_WITNESS_AUTHOR_MODEL:-}"
case "$ARM" in
  off) unset STELLA_WITNESS_AUTHOR_MODEL ;;
  on)
    # #4103: refused before the "did you export an author?" check below, and
    # deliberately so — that check tells an operator to go and set a variable
    # that would not help. The engine has one role, so the author reaches no
    # model call, and this arm would execute the CONTROL arm under a
    # treatment-arm digest (#1147). Refusing here rather than letting the
    # adapter's own gate do it: by then env.sh has sourced, a job tree exists
    # and Harbor is pulling task images, all for a run that cannot produce a
    # number.
    echo "FATAL: arm 'on' cannot run on this workspace's Stella (#4103)."
    echo "       AgentEngineConfig::model_for resolves agents.default.model >"
    echo "       default_model with no role argument, and pipeline_verifier_model"
    echo "       is a retired key the launcher recognizes and ignores, so no"
    echo "       author${AUTHOR:+ (including '$AUTHOR')} reaches a model call."
    echo
    echo "       The control arm still runs: witness_ab.sh off <job> [tasks]"
    echo "       An independent author needs a verification plugin over the"
    echo "       wrapper socket (doc:pipeline-as-plugins §8), not an engine key."
    echo "       See bench/evidence/witness-ab/README.md."
    exit 1
    ;;
  *) echo "FATAL: arm must be 'off' or 'on'"; exit 1 ;;
esac

source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

TASKFILE="${3:-$TB_ROOT/witness_ab.tasks}"
test -f "$TASKFILE" || {
  echo "FATAL: no task file at $TASKFILE"
  echo "       Create one first — both arms must read the same file:"
  echo "         python3 -c \"import json;p=json.load(open('\$TB_ROOT/phases.json'));print('\\n'.join(sorted(p['phaseA']+p['phaseB'])))\" > $TASKFILE"
  exit 1
}
test ! -e "$JOBS/$JOB" || { echo "FATAL: $JOBS/$JOB exists — a run never resumes"; exit 1; }

N=$(grep -c . "$TASKFILE")
test "$N" -gt 0 || { echo "FATAL: $TASKFILE lists no tasks"; exit 1; }

# The task set is part of the experiment's identity, and the two arms are only
# paired if they covered the same one. The first arm records the digest; the
# second refuses if it differs. python3 rather than shasum/sha256sum because
# preflight already requires python3 and the two commands are not both present
# on both host platforms this runbook supports.
TASKSET_SHA=$(sort "$TASKFILE" | python3 -c "import hashlib,sys;print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest())")
PIN="$TB_ROOT/witness_ab.taskset_sha256"
if [ -f "$PIN" ]; then
  test "$(cat "$PIN")" = "$TASKSET_SHA" || {
    echo "FATAL: this experiment's first arm ran taskset $(cat "$PIN"),"
    echo "       this one is $TASKSET_SHA — two arms over different task sets"
    echo "       are not a paired comparison. Use a new TB_ROOT to start over."
    exit 1
  }
else
  printf '%s\n' "$TASKSET_SHA" > "$PIN"
fi

# Refuse an unusable author on the host, before any container is created. The
# adapter validates it fail-closed anyway (#1147: an author Stella's offline
# seed catalog does not carry drops the judge pin and runs the control arm), but
# discovering that per trial costs the run instead of one message.
if [ "$ARM" = "on" ]; then
  python3 - "$TB_MODEL" "$AUTHOR" <<'PY' || exit 1
import sys
from stella_harbor.posture import _validated_verifier

try:
    author = _validated_verifier(sys.argv[1], sys.argv[2])
except Exception as exc:  # noqa: BLE001 - the message is the whole output
    print(f"FATAL: {exc}")
    raise SystemExit(1)
# `_validated_verifier` raises on an empty spec rather than returning None, so
# this is belt-and-braces against a future loosening — never a silent pass.
if not author:
    print("FATAL: the witness author resolved to nothing — this is the control arm")
    raise SystemExit(1)
print(f"witness author accepted: {author}")
PY
fi

preflight || exit 1

# macOS ships bash 3.2: no mapfile, no arrays from process substitution.
INCLUDES=""
while IFS= read -r t; do
  [ -n "$t" ] || continue
  INCLUDES="$INCLUDES --include-task-name terminal-bench/$t"
done < "$TASKFILE"

CONC="${TB_CONCURRENCY:-3}"
echo "arm=$ARM job=$JOB tasks=$N taskset_sha256=$TASKSET_SHA"
echo "sut=$STELLA_SOURCE_COMMIT worker=$TB_MODEL"
echo "witness_author=${STELLA_WITNESS_AUTHOR_MODEL:-<none: control arm>}"
echo "budget/trial=${STELLA_SPEND_LIMIT:-<uncapped>} concurrency=$CONC"
echo "started=$(date -u +%Y-%m-%dT%H:%M:%SZ)"

cd "$TB_REPO" || exit 1
# INCLUDES is deliberately unquoted: a pre-built flag list, one word per token,
# every task name a fixed [a-z0-9-] slug from the frozen dataset.
# shellcheck disable=SC2086
harbor run \
  --env docker \
  --dataset "$TB_DATASET" \
  $INCLUDES \
  --agent-import-path stella_harbor:StellaAgent \
  --model "$TB_MODEL" \
  --job-name "$JOB" \
  --jobs-dir "$JOBS" \
  --n-attempts 1 --n-concurrent "$CONC" --max-retries 0
rc=$?
echo "harbor_exit=$rc finished=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
exit $rc
