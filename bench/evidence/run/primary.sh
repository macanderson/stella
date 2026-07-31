#!/bin/bash
# The measured run for one resource-class phase. Publish the preregistration
# before calling this.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"
preflight || exit 1

PHASE="${1:?usage: primary.sh <A|B|ALL> <job-name>}"
JOB="${2:?usage: primary.sh <A|B|ALL> <job-name>}"
test ! -e "$JOBS/$JOB" || { echo "FATAL: $JOBS/$JOB exists — a run never resumes"; exit 1; }

# ALL runs every task in one job. The A/B split exists only because a VM with
# less memory than the largest task cannot let that task share the host; on a
# host that fits the largest task plus its neighbours the split just serialises
# work for no reason. Membership of A and B is a property of the host, never of
# the measurement — the task set, the attempt count and the denominator are
# identical either way.
case "$PHASE" in
  A)   KEY=phaseA; CONC="${TB_CONCURRENCY_A:-3}" ;;
  B)   KEY=phaseB; CONC=1 ;;
  ALL) KEY=all;    CONC="${TB_CONCURRENCY:-4}" ;;
  *)   echo "FATAL: phase must be A, B or ALL"; exit 1 ;;
esac

python3 -c "
import json
p=json.load(open('$TB_ROOT/phases.json'))
names = p['phaseA'] + p['phaseB'] if '$KEY' == 'all' else p['$KEY']
print('\n'.join(sorted(names)))
" > "$TB_ROOT/$KEY.tasks"
N=$(grep -c . "$TB_ROOT/$KEY.tasks")
test "$N" -gt 0 || { echo "FATAL: no tasks for phase $PHASE"; exit 1; }

# macOS ships bash 3.2: no mapfile, no arrays from process substitution.
INCLUDES=""
while IFS= read -r t; do
  [ -n "$t" ] || continue
  INCLUDES="$INCLUDES --include-task-name terminal-bench/$t"
done < "$TB_ROOT/$KEY.tasks"

echo "phase=$PHASE job=$JOB tasks=$N concurrency=$CONC"
echo "sut=$STELLA_SOURCE_COMMIT model=$TB_MODEL budget/trial=$STELLA_BUDGET"
echo "started=$(date -u +%Y-%m-%dT%H:%M:%SZ)"

cd "$TB_REPO"
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
