#!/bin/bash
# Standing Stella-vs-Claude-Code head-to-head. Both arms run CONCURRENTLY.
#
#   ./race.sh dev  20 mytag     # 20-task dev set,  ~1 task-latency wall-clock
#   ./race.sh all  24 mytag     # full 89 tasks,    both arms at once
#
# Why concurrent arms are safe here: measurement showed the workload is
# API-bound, not CPU-bound (load 0.15 with 3 trials on 8 vCPU; ~40 MiB per
# container). Arms do not contend for anything that changes a trial outcome,
# and each trial is isolated in its own container.
#
# The ONLY difference between the arms is the endpoint:
#   arm A  stella      -> OpenRouter   (openrouter/z-ai/glm-5.2)
#   arm B  claude-code -> z.ai         (glm-5.2)
# Same model, same effort (max), same thinking (on), no budget cap on either,
# same task list, same attempts, same concurrency, same host, same clock.
set -uo pipefail

SET="${1:?usage: race.sh <dev|all> <concurrency-per-arm> <tag>}"
CONC="${2:?usage: race.sh <dev|all> <concurrency-per-arm> <tag>}"
TAG="${3:?usage: race.sh <dev|all> <concurrency-per-arm> <tag>}"

export TB_REPO="$HOME/stella" TB_ROOT="$HOME/tb21"
export PATH="$HOME/.local/bin:$TB_REPO/bench/harbor_adapter/.venv/bin:$PATH"
export PYTHONPATH="$TB_REPO/bench/harbor_adapter"
JOBS="$TB_ROOT/jobs"
DATASET="terminal-bench/terminal-bench-2-1@sha256:7d7bdc1cbedad549fc1140404bd4dc45e5fd0ea7c4186773687d177ad3a0699a"

case "$SET" in
  dev) TASKFILE="$TB_ROOT/devset.tasks" ;;
  all) TASKFILE="$TB_ROOT/all.tasks" ;;
  *)   TASKFILE="$SET"; test -f "$TASKFILE" || { echo "FATAL: no such task set/file: $SET"; exit 1; } ;;
esac
N=$(grep -c . "$TASKFILE")

INCLUDES=""
while IFS= read -r t; do
  [ -n "$t" ] || continue
  INCLUDES="$INCLUDES --include-task-name terminal-bench/$t"
done < "$TASKFILE"

A_JOB="$TAG-armA-stella"
B_JOB="$TAG-armB-claudecode"
for j in "$A_JOB" "$B_JOB"; do
  test ! -e "$JOBS/$j" || { echo "FATAL: $JOBS/$j exists — a run never resumes"; exit 1; }
done

echo "=================================================================="
echo " RACE  set=$SET  tasks=$N  concurrency=$CONC/arm  tag=$TAG"
echo " arm A stella      -> OpenRouter   job=$A_JOB"
echo " arm B claude-code -> z.ai         job=$B_JOB"
echo " effort=max  thinking=on  budget=NONE  attempts=1  retries=0"
echo " started=$(date -u +%FT%TZ)"
echo "=================================================================="

# ---- arm A: stella, via the frozen adapter seam -------------------------
(
  export STELLA_BINARY="$TB_REPO/target/x86_64-unknown-linux-gnu/release/stella"
  export STELLA_BUDGET=""            # empty => adapter omits --budget entirely
  export STELLA_DISABLE_REFLECTION=1
  export OPENROUTER_API_KEY="$(cat "$HOME/.or_key")"
  cd "$TB_REPO"
  # shellcheck disable=SC2086
  harbor run --env docker --dataset "$DATASET" $INCLUDES \
    --agent-import-path stella_harbor:StellaAgent \
    --model "openrouter/z-ai/glm-5.2" \
    --job-name "$A_JOB" --jobs-dir "$JOBS" \
    --n-attempts 1 --n-concurrent "$CONC" --max-retries 0
  echo "ARM_A_EXIT=$? finished=$(date -u +%FT%TZ)"
) > "$HOME/$TAG-armA.log" 2>&1 &
PID_A=$!

# ---- arm B: claude-code, same model over the z.ai endpoint --------------
(
  export ANTHROPIC_BASE_URL="https://api.z.ai/api/anthropic"
  export ANTHROPIC_AUTH_TOKEN="$(cat "$HOME/.zai_key")"
  export ANTHROPIC_MODEL="glm-5.2"
  export API_TIMEOUT_MS=3000000
  export CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1
  cd "$TB_REPO"
  # shellcheck disable=SC2086
  harbor run --env docker --dataset "$DATASET" $INCLUDES \
    --agent claude-code --model glm-5.2 \
    --ak reasoning_effort=max --ak thinking=enabled \
    --job-name "$B_JOB" --jobs-dir "$JOBS" \
    --n-attempts 1 --n-concurrent "$CONC" --max-retries 0
  echo "ARM_B_EXIT=$? finished=$(date -u +%FT%TZ)"
) > "$HOME/$TAG-armB.log" 2>&1 &
PID_B=$!

echo "arm A pid=$PID_A  arm B pid=$PID_B"
echo "watch: python3 ~/live.py $A_JOB $B_JOB"
wait $PID_A; RA=$?
wait $PID_B; RB=$?
echo "BOTH_ARMS_DONE armA=$RA armB=$RB finished=$(date -u +%FT%TZ)"
