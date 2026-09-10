#!/bin/bash
# Run one arm of the two-build pipeline A/B.
#
# The plan is `bench/evidence/pipeline-ab/preregistration.json`.
#
# Two builds of Stella are compared. The build is what makes an arm that arm.
# A flag can be dropped. A posture can claim a stage that never ran. One
# binary cannot turn into the other one.
#
#   control    a build of f4c24c12b, run as `--pipeline classic`. That is the
#              last commit that still has `crates/stella-pipeline` in it.
#   treatment  a build of main, run as `--pipeline witness-v1`. That is the
#              `plugins/stella-witness` path.
#
# `witness_ab.sh` asks a different question. It runs one binary twice and
# changes who writes the failing test. `primary.sh` scores one phase of the
# whole dataset and pairs nothing.
#
# Build each arm, then run each arm. Both arms read one task file.
#
#   `TB_ARM=control TB_BUILD_REPO=/checkouts/stella-f4c24c12b \`
#   `  bench/evidence/run/build_sut.sh f4c24c12bde5578818f1141ec4e438291ac4db55`
#   `TB_ARM=treatment bench/evidence/run/build_sut.sh`
#   `bench/evidence/run/pipeline_ab.sh control   pab1-control   "$TB_ROOT/pipeline_ab.tasks"`
#   `bench/evidence/run/pipeline_ab.sh treatment pab1-treatment "$TB_ROOT/pipeline_ab.tasks"`
#
# Then compare the arms. The README beside the plan holds that command.
set -uo pipefail

ARM="${1:?usage: pipeline_ab.sh <control|treatment> <job-name> [task-file]}"
JOB="${2:?usage: pipeline_ab.sh <control|treatment> <job-name> [task-file]}"

# The plugin id the plan fixed. It is spelled out here, not read from the
# manifest. A rename must not re-aim the arm on its own. The check below reads
# the manifest and holds it to this value.
WITNESS_PLUGIN_ID="witness-v1"

case "$ARM" in
  control)   PIPELINE_ID="classic" ;;
  treatment) PIPELINE_ID="$WITNESS_PLUGIN_ID" ;;
  *) echo "FATAL: arm must be 'control' or 'treatment'"; exit 1 ;;
esac

# Both are set before `env.sh` runs. `env.sh` picks the binary path and the
# provenance paths from the arm.
export TB_ARM="$ARM"
export STELLA_PIPELINE="$PIPELINE_ID"

# The plan fixes this run as uncapped. In `env.sh` an empty value means no
# cap. An unset one takes the 0.60 USD default.
#
# A cap measures the cap. The control arm makes several model calls per step.
# The treatment arm makes one. A ceiling that binds on one arm and not the
# other tells you who hit it first, and nothing else.
if [ -n "${STELLA_SPEND_LIMIT:-}" ]; then
  echo "FATAL: STELLA_SPEND_LIMIT is '$STELLA_SPEND_LIMIT' and this experiment is uncapped."
  echo "       preregistration.json fixes 'no budget cap and no token cap'. Unset it,"
  echo "       or set it empty, and relaunch. The spend stop rule is per replicate"
  echo "       and is applied by the operator between replicates, never per trial."
  exit 1
fi
export STELLA_SPEND_LIMIT=""

source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

# The manifest must still declare the id above. A rename then stops the launch.
# It does not quietly move the arm to some other path.
MANIFEST="$TB_REPO/plugins/stella-witness/plugin.toml"
if [ "$ARM" = "treatment" ]; then
  grep -qE "^id[[:space:]]*=[[:space:]]*\"$WITNESS_PLUGIN_ID\"" "$MANIFEST" || {
    echo "FATAL: $MANIFEST declares some other id than \"$WITNESS_PLUGIN_ID\"."
    echo "       The preregistration pins that id, and --treatment-fired reads"
    echo "       loop_mode=PLUGIN:$WITNESS_PLUGIN_ID. Settle which is right before running."
    exit 1
  }
fi

# The treatment arm needs the plugin in the task container. Nothing puts it
# there.
#
# The adapter uploads one file per trial. That file is the `stella` binary. A
# wrapper plugin is a folder read off disk. `plugins/stella-witness` runs
# `python3 ${plugin_dir}/main.py`. So the roster in the container is empty,
# and `PipelineChoice::resolve` refuses the id.
#
# That refusal fails closed. `agent::goal` turns it into a `CliFailure`, so
# the trial exits non-zero. It cannot run the raw loop under this arm's name.
# The arm yields no number. Finding that out costs a full arm of spend.
#
# So refuse on the host, before Harbor pulls an image. Delete this block in
# the change that stages a plugin. Say in that PR which images have `python3`.
if [ "$ARM" = "treatment" ]; then
  echo "FATAL: the treatment arm cannot run — nothing installs the plugin in the task container."
  echo "       stella_harbor uploads the stella binary and nothing else, so the"
  echo "       roster inside the container is empty and 'stella run --pipeline"
  echo "       $WITNESS_PLUGIN_ID' is refused at resolve time on every trial."
  echo "       Staging a plugin into the container, and the two questions about"
  echo "       task images that go with it, are the fourth precondition in"
  echo "       bench/evidence/pipeline-ab/preregistration.json, which links it."
  echo
  echo "       The control arm is unaffected: its pipeline is built into its binary."
  exit 1
fi

TASKFILE="${3:-$TB_ROOT/pipeline_ab.tasks}"
test -f "$TASKFILE" || {
  echo "FATAL: no task file at $TASKFILE"
  echo "       Create one first — both arms must read the same file:"
  echo "         python3 -c \"import json;p=json.load(open('\$TB_ROOT/phases.json'));print('\\n'.join(sorted(p['phaseA']+p['phaseB'])))\" > $TASKFILE"
  exit 1
}
test ! -e "$JOBS/$JOB" || { echo "FATAL: $JOBS/$JOB exists — a run never resumes"; exit 1; }

N=$(grep -c . "$TASKFILE")
test "$N" -gt 0 || { echo "FATAL: $TASKFILE lists no tasks"; exit 1; }

# Two arms are paired only if they ran the same task set. The first arm writes
# the digest. The second one refuses if it differs. The pin sits beside both
# arms, because it describes the pair.
TASKSET_SHA=$(sort "$TASKFILE" | python3 -c "import hashlib,sys;print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest())")
PIN="$TB_ROOT/pipeline_ab.taskset_sha256"
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

# Two arms with one binary hash is one arm run twice. The analysis refuses that
# pair, but only after the money is gone. Catch it here. This reads the other
# arm's hash, so it fires on whichever arm runs second.
THIS_HASH_FILE="$TB_ARM_DIR/binary_sha256.txt"
test -f "$THIS_HASH_FILE" || { echo "FATAL: no binary_sha256.txt for arm '$ARM' — run build_sut.sh for it"; exit 1; }
case "$ARM" in
  control) OTHER="treatment" ;;
  *)       OTHER="control" ;;
esac
OTHER_HASH_FILE="$TB_ROOT/arms/$OTHER/binary_sha256.txt"
if [ -f "$OTHER_HASH_FILE" ] && [ "$(cat "$THIS_HASH_FILE")" = "$(cat "$OTHER_HASH_FILE")" ]; then
  echo "FATAL: arm '$ARM' and arm '$OTHER' are the same binary ($(cat "$THIS_HASH_FILE"))."
  echo "       The build is the arm, so one binary is one arm. Rebuild the arm that"
  echo "       is wrong, with TB_BUILD_REPO pointed at its own checkout."
  exit 1
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
echo "sut=$STELLA_SOURCE_COMMIT binary_sha256=$(cat "$THIS_HASH_FILE")"
echo "pipeline=$STELLA_PIPELINE worker=$TB_MODEL"
echo "budget/trial=${STELLA_SPEND_LIMIT:-<uncapped>} concurrency=$CONC"
echo "started=$(date -u +%Y-%m-%dT%H:%M:%SZ)"

cd "$TB_REPO" || exit 1
# INCLUDES is a flag list built above. One word per token. Every task name is a
# fixed [a-z0-9-] slug from the frozen dataset.
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
