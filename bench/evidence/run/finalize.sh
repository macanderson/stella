#!/bin/bash
# Turn the phase job directories into committed evidence: per-trial rows, the
# score, the per-task table, and the manifest that pins every input.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

RUN_ID="${1:?usage: finalize.sh <run-id> <job-name-prefix>}"
PREFIX="${2:?usage: finalize.sh <run-id> <job-name-prefix>}"
OUT="$TB_REPO/bench/evidence/$RUN_ID"
mkdir -p "$OUT"

: > "$OUT/trials.jsonl"
for phase in A B; do
  d="$JOBS/${PREFIX}-phase${phase}"
  [ -d "$d" ] || { echo "note: no job dir for phase $phase ($d)"; continue; }
  tmp="$(mktemp)"
  python3 "$TB_REPO/bench/evidence/score_dev_baseline.py" extract "$d" -o "$tmp"
  cat "$tmp" >> "$OUT/trials.jsonl"
  rm -f "$tmp"
done

TASKS="${TB_TASKS:-89}"
python3 "$TB_REPO/bench/evidence/score_dev_baseline.py" score "$OUT/trials.jsonl" \
  --tasks "$TASKS" --markdown "$OUT/results.md" > "$OUT/score.json"
cat "$OUT/score.json"

python3 "$TB_REPO/bench/evidence/make_manifest.py" \
  --job-dir "$JOBS/${PREFIX}-phaseA" \
  --run-dir "$OUT" \
  --sut-commit "$STELLA_SOURCE_COMMIT" \
  --binary-sha256 "$(cat "$TB_ROOT/binary_sha256.txt")" \
  --model "$TB_MODEL" \
  --dataset "$TB_DATASET" \
  --tasks "$TASKS" --attempts 1 \
  --concurrency "${TB_CONCURRENCY_A:-3}" \
  --budget-per-trial "$STELLA_BUDGET" \
  --prereg-url "${TB_PREREG_URL:-}"
echo "evidence written to $OUT"
