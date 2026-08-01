#!/bin/bash
# The measured run for one resource tier. Publish the preregistration before
# calling this.
set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/env.sh"
fb_preflight || exit 1

TIER="${1:?usage: primary.sh <small|medium|large|xlarge|all> <job-name>}"
JOB="${2:?usage: primary.sh <tier> <job-name>}"
test ! -e "$JOBS/$JOB" || { echo "FATAL: $JOBS/$JOB exists — a run never resumes"; exit 1; }
test -f "$FB_PLAN" || { echo "FATAL: no plan at $FB_PLAN (run fetch_dataset.sh)"; exit 1; }

read -r N CONC <<EOF
$(python3 -c "
import json,sys
p=json.load(open('$FB_PLAN'))
tiers=p['tiers']
if '$TIER'=='all':
    names=sorted(t for v in tiers.values() for t in v['tasks'])
    conc=min((v['concurrency'] for v in tiers.values()), default=1)
else:
    if '$TIER' not in tiers:
        sys.exit('FATAL: no tier \'$TIER\' in the plan (have: %s)' % ', '.join(tiers))
    names=sorted(tiers['$TIER']['tasks']); conc=tiers['$TIER']['concurrency']
open('$TB_ROOT/frontier-$TIER.tasks','w').write('\n'.join(names)+'\n')
print(len(names), conc)
")
EOF
test -n "${N:-}" || exit 1
test "$N" -gt 0 2>/dev/null || { echo "FATAL: no tasks for tier $TIER"; exit 1; }

CONC="${FB_CONCURRENCY:-$CONC}"

# macOS ships bash 3.2: no mapfile, no arrays from process substitution.
INCLUDES=""
while IFS= read -r t; do
  [ -n "$t" ] || continue
  INCLUDES="$INCLUDES --include-task-name $FB_TASK_PREFIX/$t"
done < "$TB_ROOT/frontier-$TIER.tasks"

# Uploading publishes every trajectory. Build the flags once, here, so the
# invocation below cannot half-apply them — `--public` without `--upload` is a
# Harbor error, and `--upload` without meaning to is a disclosure.
UPLOAD_FLAGS=""
if [ "$FB_UPLOAD" = 1 ]; then
  UPLOAD_FLAGS="--upload --$FB_UPLOAD_VISIBILITY"
  if [ "$FB_UPLOAD_VISIBILITY" = public ]; then
    echo "NOTE: this run will be uploaded to Harbor Hub as PUBLIC — every"
    echo "      trajectory and command becomes permanently world-readable."
  fi
fi

echo "tier=$TIER job=$JOB tasks=$N concurrency=$CONC attempts=$FB_ATTEMPTS"
echo "backend=$FB_ENV upload=${FB_UPLOAD}${FB_UPLOAD:+/$FB_UPLOAD_VISIBILITY}"
echo "sut=$STELLA_SOURCE_COMMIT model=$TB_MODEL budget/trial=${STELLA_BUDGET:-uncapped}"
echo "dataset=$FB_DATASET"
echo "harbor=$(harbor --version) started=$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# Say plainly what this run is and is not. A tiered, GPU-excluding, single
# attempt run is a development baseline; the leaderboard wants all 74 tasks at
# five-plus attempts with no subsetting. Both are legitimate — reporting one as
# the other is not, and the distinction is easiest to lose at the moment a
# number finally appears.
# Name every reason this run would be refused, not just the first. An operator
# who fixes one and reruns only to be told about the next has paid for a full
# run to learn something this loop could have said in one line.
EXCLUDED_N=$(python3 -c "import json;print(len(json.load(open('$FB_PLAN'))['excluded']))")
GAPS=""
[ "$TIER" = "all" ] || GAPS="$GAPS tier=$TIER(needs-all)"
[ "$FB_ATTEMPTS" -ge "$FB_SUBMISSION_MIN_ATTEMPTS" ] || GAPS="$GAPS attempts=$FB_ATTEMPTS(needs-$FB_SUBMISSION_MIN_ATTEMPTS)"
[ "$EXCLUDED_N" = 0 ] || GAPS="$GAPS excluded=$EXCLUDED_N(needs-0;-subsetting-is-not-allowed)"
[ "$FB_UPLOAD" = 1 ] || GAPS="$GAPS upload=off"
[ "$FB_UPLOAD_VISIBILITY" = public ] || GAPS="$GAPS visibility=$FB_UPLOAD_VISIBILITY(needs-public)"
if [ -n "$GAPS" ]; then
  echo "posture=development-baseline — not submittable:$GAPS"
  echo "         (see SUBMISSION.md; a baseline is a legitimate thing to run,"
  echo "          it just must not be reported as a leaderboard number)"
else
  echo "posture=submission-shaped"
fi

cd "$TB_REPO" || exit 1
# INCLUDES and UPLOAD_FLAGS are deliberately unquoted: pre-built flag lists,
# one word per token, every task name a fixed [a-z0-9-] slug from the
# digest-pinned dataset and every upload flag a literal from this script.
# shellcheck disable=SC2086
harbor run \
  --env "$FB_ENV" \
  --dataset "$FB_DATASET" \
  $INCLUDES \
  --agent stella_harbor:StellaAgent \
  --model "$TB_MODEL" \
  --job-name "$JOB" \
  --jobs-dir "$JOBS" \
  $UPLOAD_FLAGS \
  --n-attempts "$FB_ATTEMPTS" --n-concurrent "$CONC" --max-retries 0
rc=$?
echo "harbor_exit=$rc finished=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
exit $rc
