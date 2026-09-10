#!/bin/bash
# One arm of the two-build pipeline A/B, preregistered in
# `bench/evidence/pipeline-ab/preregistration.json`.
#
# The experiment compares the built-in staged pipeline against the plugin path
# that replaced it. Each arm is a different build of Stella, and the build is
# what makes it that arm: a flag can be dropped from a command line, a posture
# can claim a tier that never fired, and neither can turn one binary into the
# other one.
#
#   control    — a build of f4c24c12b, the last commit where
#                `crates/stella-pipeline` exists, run as `--pipeline classic`.
#   treatment  — a build of main with `plugins/stella-witness` installed, run
#                as `--pipeline witness-v1`.
#
# Not `witness_ab.sh`: that is two runs of one binary differing in who authors
# the failing test. Not `primary.sh`: that scores a preregistered phase of the
# whole dataset against a fixed denominator rather than pairing two arms.
#
# Usage, one arm at a time, both reading one task file:
#
#   TB_ARM=control   TB_BUILD_REPO=/checkouts/stella-f4c24c12b \
#     bench/evidence/run/build_sut.sh f4c24c12bde5578818f1141ec4e438291ac4db55
#   TB_ARM=treatment bench/evidence/run/build_sut.sh
#
#   bench/evidence/run/pipeline_ab.sh control   pab1-control   "$TB_ROOT/pipeline_ab.tasks"
#   bench/evidence/run/pipeline_ab.sh treatment pab1-treatment "$TB_ROOT/pipeline_ab.tasks"
#
# then, once both arms are extracted into evidence directories:
#   python3 bench/evidence/compare_arms.py <control>/trials.jsonl <treatment>/trials.jsonl \
#       --tasks 89 --cross-sut <control-sha>:<treatment-sha> \
#       --treatment-fired loop_mode=PLUGIN:witness-v1 --markdown <run>/results.md
set -uo pipefail

ARM="${1:?usage: pipeline_ab.sh <control|treatment> <job-name> [task-file]}"
JOB="${2:?usage: pipeline_ab.sh <control|treatment> <job-name> [task-file]}"

# The plugin id the preregistration fixed. It is written out here rather than
# read from the manifest so that renaming the plugin cannot silently re-aim the
# treatment arm at a different path; the manifest is then checked against this
# value below, and a rename reddens the launch instead of the analysis.
WITNESS_PLUGIN_ID="witness-v1"

case "$ARM" in
  control)   PIPELINE_ID="classic" ;;
  treatment) PIPELINE_ID="$WITNESS_PLUGIN_ID" ;;
  *) echo "FATAL: arm must be 'control' or 'treatment'"; exit 1 ;;
esac

# Both selectors are exported before `env.sh` is sourced, because `env.sh`
# decides `STELLA_BINARY` and the provenance paths from the arm.
export TB_ARM="$ARM"
export STELLA_PIPELINE="$PIPELINE_ID"

# The preregistration fixes this arm as uncapped, and `env.sh` reads an empty
# value as no per-trial cap while an unset one takes the development default of
# 0.60 USD. A cap measures the cap: the control arm spends several model calls
# per step where the treatment arm spends one, so a ceiling low enough to bind
# on one arm and not the other turns a comparison of two paths into a
# comparison of who hit the ceiling first.
if [ -n "${STELLA_SPEND_LIMIT:-}" ]; then
  echo "FATAL: STELLA_SPEND_LIMIT is '$STELLA_SPEND_LIMIT' and this experiment is uncapped."
  echo "       preregistration.json fixes 'no budget cap and no token cap'. Unset it,"
  echo "       or set it empty, and relaunch. The spend stop rule is per replicate"
  echo "       and is applied by the operator between replicates, never per trial."
  exit 1
fi
export STELLA_SPEND_LIMIT=""

source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

# The manifest still has to declare the id this script names, so a rename shows
# up as a refused launch rather than as an arm that measured something else.
MANIFEST="$TB_REPO/plugins/stella-witness/plugin.toml"
if [ "$ARM" = "treatment" ]; then
  grep -qE "^id[[:space:]]*=[[:space:]]*\"$WITNESS_PLUGIN_ID\"" "$MANIFEST" || {
    echo "FATAL: $MANIFEST no longer declares id = \"$WITNESS_PLUGIN_ID\"."
    echo "       The preregistration pins that id, and --treatment-fired reads"
    echo "       loop_mode=PLUGIN:$WITNESS_PLUGIN_ID. Settle which is right before running."
    exit 1
  }
fi

# The treatment arm needs the plugin inside the task container, and nothing
# puts it there.
#
# The adapter uploads one file per trial, the `stella` binary. A wrapper plugin
# is a directory the roster reads off disk — `plugins/stella-witness` declares
# `argv = ["python3", "${plugin_dir}/main.py"]` — so on a container holding only
# the binary, `PluginRoster::load` finds nothing and `PipelineChoice::resolve`
# refuses the id. That refusal is fail-closed: `agent::goal` turns it into a
# `CliFailure`, so the trial exits non-zero rather than running the raw loop
# under a treatment-arm label. The run would produce no number and cost a full
# arm of spend to find out.
#
# Refused here, on the host, before a job tree exists or Harbor pulls an image.
# Delete this block in the change that gives the adapter a way to stage a
# plugin, and say in that PR which task images carry `python3`.
if [ "$ARM" = "treatment" ]; then
  echo "FATAL: the treatment arm cannot run — nothing installs the plugin in the task container."
  echo "       stella_harbor uploads the stella binary and nothing else, so the"
  echo "       roster inside the container is empty and 'stella run --pipeline"
  echo "       $WITNESS_PLUGIN_ID' is refused at resolve time on every trial."
  echo "       Tracked as the fourth precondition in"
  echo "       bench/evidence/pipeline-ab/preregistration.json."
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

# The task set is part of the experiment's identity, and two arms are paired
# only if they covered the same one. The first arm records the digest; the
# second refuses if it differs. The pin lives beside the arms rather than inside
# one, because it describes the pair.
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

# Two arms reporting one binary hash is one arm run twice, and the analysis
# refuses such a pair after the money is gone. Catch it here instead. This
# reads the sibling arm's recorded hash, so it fires whichever arm runs second.
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
# INCLUDES is a pre-built flag list, one word per token, every task name a fixed
# [a-z0-9-] slug from the frozen dataset.
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
