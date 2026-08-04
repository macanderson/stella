#!/bin/bash
# The #1295 measurement, run HOST-side instead of inside the task container.
#
# Why this exists rather than `run.sh`: the container half of Harbor needs
# outbound TLS from inside the task image, and an environment whose only egress
# is a host-loopback proxy cannot give it that — the adapter blanks proxy
# variables on purpose (`_LAUNCHER_ENV_OVERRIDES`, `provider_proxy: disabled`),
# and defeating that control to make a benchmark run would be trading a
# security property for a number. So the container is used for what it is
# actually needed for — providing the task's real starting workspace — and the
# agent runs on the host, where egress works.
#
# What is preserved: the task's real files and instruction, the #1225 git
# baseline, the same pipeline, and the same two arms.
# What is NOT: Harbor's verifier. No reward is produced, so this measures the
# RATE the issue asks for and the ask's behaviour — never a pass rate. Anything
# claiming a solve rate from these artifacts is misreading them.
#
#   TB_ROOT=/abs/scratch STELLA_BINARY=/abs/stella ANTHROPIC_API_KEY=... \
#   bench/evidence/judge-evidence-demand-1295/host_run.sh
set -uo pipefail

: "${TB_REPO:=$(git rev-parse --show-toplevel)}"
: "${TB_ROOT:?set TB_ROOT to an absolute scratch directory}"
: "${STELLA_BINARY:?set STELLA_BINARY}"
: "${ANTHROPIC_API_KEY:?set ANTHROPIC_API_KEY}"

DATASET_DIR="${DATASET_DIR:-$TB_ROOT/dataset/terminal-bench-2-1}"
RUNS="${RUNS:-$TB_ROOT/hostruns}"
TASKS="${TASKS:-fix-ocaml-gc modernize-scientific-stack adaptive-rejection-sampler circuit-fibsqrt dna-assembly write-compressor}"
BUDGET="${BUDGET:-0.60}"
WORKER="${WORKER:-anthropic/claude-sonnet-5}"
AUTHOR="${AUTHOR:-anthropic/claude-fable-5}"
TURN_BUDGET="${TURN_BUDGET:-600}"
mkdir -p "$RUNS"

# The same posture the adapter builds, delivered through the same trusted
# launcher seam. Pinned per role so neither arm can drift into auto-selection,
# and `pipeline_judge_model` is what the authored-witness tier resolves its
# independent author from — without it the witness rung cannot fire and the
# #1295 ask has no channel that could ever answer it.
posture() {
  cat <<JSON
{"default_model":"$WORKER","allowed_models":["$WORKER","$AUTHOR"],
 "pipeline_judge_model":"$AUTHOR","auto_mode":"off","effort_auto":"off",
 "reasoning_auto":"off","headless_scope_bypass":"on",
 "pipeline_judge_evidence_demand":"$1"}
JSON
}

# Extract the task's real starting workspace out of its image. `docker cp` from
# a created-but-never-started container: no task process runs, so nothing the
# task would do at runtime has happened yet — the tree is exactly what the
# agent is meant to meet.
extract_workspace() {
  local task="$1" dest="$2"
  local image
  image="$(grep -oP 'docker_image\s*=\s*"\K[^"]+' "$DATASET_DIR/$task/task.toml" | head -1)"
  local cid
  cid="$(docker create --entrypoint sh "$image" 2>/dev/null)" || return 1
  rm -rf "$dest"; mkdir -p "$dest"
  docker cp -q "$cid:/app/." "$dest/" 2>/dev/null || docker cp "$cid:/app/." "$dest/" >/dev/null 2>&1
  local rc=$?
  docker rm -f "$cid" >/dev/null 2>&1
  return $rc
}

# #1225's baseline, restated for the host. Same guards, same identity, same
# `.stella/` exclusion — a workspace that already has a repository is left
# alone, because committing over task-owned history changes the task.
git_baseline() {
  local dir="$1"
  ( cd "$dir" || exit 0
    if [ -e .git ] || git rev-parse --git-dir >/dev/null 2>&1; then
      echo "git baseline skipped: a repository is already present"; exit 0
    fi
    git init -q . >/dev/null 2>&1 || exit 0
    mkdir -p .git/info && echo ".stella/" >> .git/info/exclude
    git -c user.name=stella-baseline -c user.email=baseline@stella.invalid add -A >/dev/null 2>&1
    git -c user.name=stella-baseline -c user.email=baseline@stella.invalid \
        commit -q -m "stella-harbor: task workspace baseline" >/dev/null 2>&1
    echo "git baseline committed" )
}

run_arm() {
  local arm="$1" demand="$2"
  for task in $TASKS; do
    local dir="$RUNS/$arm/$task"
    echo "=== [$arm] $task"
    extract_workspace "$task" "$dir/workspace" || { echo "  extract failed"; continue; }
    git_baseline "$dir/workspace" | sed 's/^/  /'
    mkdir -p "$dir"
    # `-I`-style isolation of the agent's own environment: no repository env
    # file, no project settings/hooks, no catalog refresh — the same controls
    # the benchmark launcher pins, so the host run and a container run differ
    # in where they execute and nothing else.
    ( cd "$dir/workspace" && \
      STELLA_ENGINE_CONFIG_JSON="$(posture "$demand")" \
      STELLA_NO_ENV_FILE=1 STELLA_NO_SETTINGS=1 STELLA_TRUST_PROJECT=0 \
      STELLA_PROJECT_HOOKS=0 STELLA_CATALOG_AUTO_REFRESH=0 \
      ANTHROPIC_API_KEY="$ANTHROPIC_API_KEY" \
      timeout 1800 "$STELLA_BINARY" \
        --model "$WORKER" --budget "$BUDGET" --turn-budget "$TURN_BUDGET" \
        --output-format stream-json \
        run -- "$(cat "$DATASET_DIR/$task/instruction.md")" \
        > "$dir/stella-events.jsonl" 2> "$dir/stderr.txt" )
    # `--output-format stream-json` puts the event stream on stdout, so stdout
    # IS the stream. The durable-path env is not an option here: it accepts
    # only Harbor's fixed mounted destination by design, so that a repository
    # `.env` cannot turn stream rendering into an append primitive.
    echo "  exit $? -> $dir"
  done
}

run_arm on on
run_arm off off

OUT="${OUT:-$TB_ROOT/analysis}"
mkdir -p "$OUT"
python3 "$TB_REPO/bench/evidence/judge-evidence-demand-1295/analyze.py" \
  extract "$RUNS/on" --arm on --layout host -o "$OUT/on.turns.jsonl"
python3 "$TB_REPO/bench/evidence/judge-evidence-demand-1295/analyze.py" \
  extract "$RUNS/off" --arm off --layout host -o "$OUT/off.turns.jsonl"
python3 "$TB_REPO/bench/evidence/judge-evidence-demand-1295/analyze.py" \
  report "$OUT/on.turns.jsonl" "$OUT/off.turns.jsonl" | tee "$OUT/report.txt"
