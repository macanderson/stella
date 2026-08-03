#!/bin/bash
# A Stella-only tuning run over a fixed task list.
#
# Not `primary.sh`. That script measures a preregistered phase of the whole
# frozen dataset and exists to produce a publishable number. This one answers a
# narrower and cheaper question — "did the change help?" — over tasks chosen
# *because* Stella lost them, so its result is a tuning signal and never a
# score. Nothing here is claim-eligible: the task list is selected on prior
# results, which is exactly the selection bias a leaderboard row must not have.
#
# The comparator is historical. Claude Code already passed every task in the
# list, on runs whose artifacts are on disk, so re-running it would spend money
# to re-derive a number we hold. Only the Stella arm runs.
#
# Two arms, one variable each in principle and two in practice — the worker's
# effort tier and the judge's author move together, which is what the operator
# asked for. Read the result as "configuration A vs configuration B", not as an
# attribution to either knob.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

ARM="${1:?usage: tune.sh <A|B> <job-name> [task-file]}"
JOB="${2:?usage: tune.sh <A|B> <job-name> [task-file]}"
TASKFILE="${3:-$TB_ROOT/tune5.tasks}"

test -f "$TASKFILE" || { echo "FATAL: no task file at $TASKFILE"; exit 1; }
test ! -e "$JOBS/$JOB" || { echo "FATAL: $JOBS/$JOB exists — a run never resumes"; exit 1; }

# Every role on one provider. A trial carries exactly one credential over the
# anonymous FD, so a judge or triage author on a second provider authenticates
# against nothing — the adapter refuses such a posture rather than quietly
# running the arm the digest does not describe.
export TB_MODEL="${TB_MODEL:-openrouter/anthropic/claude-sonnet-5}"
export STELLA_TRIAGE_MODEL="${STELLA_TRIAGE_MODEL:-openrouter/anthropic/claude-haiku-4.5}"

case "$ARM" in
  A)
    export STELLA_WORKER_EFFORT=xhigh
    export STELLA_WITNESS_AUTHOR_MODEL=openrouter/anthropic/claude-fable-5
    ;;
  B)
    export STELLA_WORKER_EFFORT=high
    export STELLA_WITNESS_AUTHOR_MODEL=openrouter/moonshotai/kimi-k3
    ;;
  *) echo "FATAL: arm must be A or B"; exit 1 ;;
esac

preflight || exit 1

N=$(grep -c . "$TASKFILE")
test "$N" -gt 0 || { echo "FATAL: $TASKFILE lists no tasks"; exit 1; }

# macOS ships bash 3.2: no mapfile, no arrays from process substitution.
INCLUDES=""
while IFS= read -r t; do
  [ -n "$t" ] || continue
  INCLUDES="$INCLUDES --include-task-name terminal-bench/$t"
done < "$TASKFILE"

# The task list is part of this run's identity: two runs that disagree about
# which tasks they covered are not comparable, and the digest is how a later
# reader checks that without trusting the job name.
TASKSET_SHA=$(sort "$TASKFILE" | shasum -a 256 | awk '{print $1}')

echo "arm=$ARM job=$JOB tasks=$N taskset_sha256=$TASKSET_SHA"
echo "sut=$STELLA_SOURCE_COMMIT worker=$TB_MODEL effort=$STELLA_WORKER_EFFORT"
echo "judge=$STELLA_WITNESS_AUTHOR_MODEL triage=$STELLA_TRIAGE_MODEL"
echo "budget/trial=${STELLA_BUDGET:-<uncapped>} concurrency=${TB_CONCURRENCY:-5}"
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
  --n-attempts 1 --n-concurrent "${TB_CONCURRENCY:-5}" --max-retries 0
rc=$?
echo "harbor_exit=$rc finished=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
exit $rc
