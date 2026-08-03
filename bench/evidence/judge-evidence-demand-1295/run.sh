#!/bin/bash
# Two-arm development baseline for #1295: does asking for corroboration when
# only the judge approved the work help, hurt, or do nothing?
#
# NOT a claim run. There is no intent ledger, no host attestation and no
# comparator here, and nothing this produces is publishable as a Stella row —
# see `bench/RUNBOOK.md` for the procedure that is. What this *is* is the
# measurement the issue asks for, run the cheapest way that can answer it: the
# same task list under two arms that differ by exactly one posture key.
#
#   TB_ROOT=/abs/scratch \
#   STELLA_BINARY=/abs/target/x86_64-unknown-linux-gnu/release/stella \
#   ANTHROPIC_API_KEY=... TB_MODEL=anthropic/claude-sonnet-5 \
#   bench/evidence/judge-evidence-demand-1295/run.sh
#
# Both arms run from ONE binary. The arm is `STELLA_JUDGE_EVIDENCE_DEMAND`,
# which the adapter turns into the `pipeline_judge_evidence_demand` posture key
# — so which arm a trial ran is a property of its recorded posture digest, not
# of which shell ran it.
set -uo pipefail

: "${TB_REPO:=$(git rev-parse --show-toplevel)}"
: "${TB_ROOT:?set TB_ROOT to an absolute scratch directory}"
: "${STELLA_BINARY:?set STELLA_BINARY to the portable SUT}"
: "${ANTHROPIC_API_KEY:?set ANTHROPIC_API_KEY}"

export VENV="$TB_REPO/bench/harbor_adapter/.venv"
export PATH="$VENV/bin:$PATH"
export PYTHONPATH="$TB_REPO/bench/harbor_adapter"
export DOCKER_DEFAULT_PLATFORM=linux/amd64

export TB_MODEL="${TB_MODEL:-anthropic/claude-sonnet-5}"
# The treatment arm of #1007. Without an author independent of the worker the
# authored-witness rung cannot fire, no tracked command is ever resolved, and
# #1295's ask is structurally unreachable — so the control arm would measure
# the precondition rather than the feature.
export STELLA_WITNESS_AUTHOR_MODEL="${STELLA_WITNESS_AUTHOR_MODEL:-anthropic/claude-opus-5}"
export STELLA_BUDGET="${STELLA_BUDGET:-0.60}"
export STELLA_DISABLE_REFLECTION=1
export STELLA_SOURCE_COMMIT="${STELLA_SOURCE_COMMIT:-$(git -C "$TB_REPO" rev-parse HEAD)}"

DATASET_DIR="${DATASET_DIR:-$TB_ROOT/dataset/terminal-bench-2-1}"
JOBS="${JOBS:-$TB_ROOT/jobs}"
TASKS="${TASKS:-adaptive-rejection-sampler circuit-fibsqrt dna-assembly fix-ocaml-gc nginx-request-logging write-compressor}"
CONCURRENT="${CONCURRENT:-2}"
ATTEMPTS="${ATTEMPTS:-1}"
mkdir -p "$JOBS"

# The adapter refuses to start when the host carries STELLA_* knobs it does not
# recognize, which is the correct behaviour and inconvenient exactly once: a
# development shell that has role-model variables set for its own use would
# fail every trial. Drop them for the launch rather than teaching the adapter
# to ignore names it does not know.
scrubbed() {
  env -u STELLA_JUDGE_API -u STELLA_JUDGE_API_KEY -u STELLA_JUDGE_EFFORT \
      -u STELLA_JUDGE_MODEL -u STELLA_JUDGE_THINKING_ON -u STELLA_PIPELINE_ON \
      -u STELLA_TRIAGE_API -u STELLA_TRIAGE_API_KEY -u STELLA_TRIAGE_EFFORT \
      -u STELLA_TRIAGE_MODEL -u STELLA_TRIAGE_THINKING_ON \
      "$@"
}

run_arm() {
  local arm="$1" demand="$2"
  local job="jed1295-$arm"
  echo "=== arm $arm (STELLA_JUDGE_EVIDENCE_DEMAND=$demand) -> $JOBS/$job"
  local includes=()
  for task in $TASKS; do includes+=(--path "$DATASET_DIR/$task"); done
  STELLA_JUDGE_EVIDENCE_DEMAND="$demand" scrubbed harbor run \
    --env docker \
    "${includes[@]}" \
    --agent-import-path stella_harbor:StellaAgent \
    --model "$TB_MODEL" \
    --job-name "$job" \
    --jobs-dir "$JOBS" \
    --n-attempts "$ATTEMPTS" \
    --n-concurrent "$CONCURRENT" \
    --max-retries 0
  echo "=== arm $arm exit $?"
}

run_arm on on
run_arm off off

OUT="${OUT:-$TB_ROOT/analysis}"
mkdir -p "$OUT"
python3 "$TB_REPO/bench/evidence/judge-evidence-demand-1295/analyze.py" \
  extract "$JOBS/jed1295-on" --arm on -o "$OUT/on.turns.jsonl"
python3 "$TB_REPO/bench/evidence/judge-evidence-demand-1295/analyze.py" \
  extract "$JOBS/jed1295-off" --arm off -o "$OUT/off.turns.jsonl"
python3 "$TB_REPO/bench/evidence/judge-evidence-demand-1295/analyze.py" \
  report "$OUT/on.turns.jsonl" "$OUT/off.turns.jsonl" | tee "$OUT/report.txt"
