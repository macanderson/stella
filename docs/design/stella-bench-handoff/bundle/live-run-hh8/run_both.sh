#!/bin/bash
# Matched head-to-head, preregistration #1013. Arms run back-to-back on this
# host, same 89 tasks, same model, effort max, NO per-trial budget cap.
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$HOME/stella/bench/harbor_adapter/.venv/bin:$PATH"
export TB_REPO=~/stella TB_ROOT=~/tb21
cd ~/stella

echo "=== ARM A: stella ==="
env STELLA_BUDGET= \
  OPENROUTER_API_KEY="$(cat ~/.or_key)" \
  TB_MODEL="openrouter/z-ai/glm-5.2" \
  TB_CONCURRENCY=3 \
  STELLA_DISABLE_REFLECTION=1 \
  bash bench/evidence/run/primary.sh ALL hh8-armA-stella
echo "ARM_A_EXIT=$?"

echo "=== ARM B: claude-code ==="
ANTHROPIC_BASE_URL="https://api.z.ai/api/anthropic" \
ANTHROPIC_AUTH_TOKEN="$(cat ~/.zai_key)" \
ANTHROPIC_MODEL="glm-5.2" \
API_TIMEOUT_MS=3000000 \
CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 \
harbor run --env docker \
  --dataset "terminal-bench/terminal-bench-2-1@sha256:7d7bdc1cbedad549fc1140404bd4dc45e5fd0ea7c4186773687d177ad3a0699a" \
  --agent claude-code --model glm-5.2 \
  --ak reasoning_effort=max --ak thinking=enabled \
  --job-name hh8-armB-claudecode --jobs-dir ~/tb21/jobs \
  --n-attempts 1 --n-concurrent 3 --max-retries 0
echo "ARM_B_EXIT=$?"
echo "BOTH_ARMS_DONE"
