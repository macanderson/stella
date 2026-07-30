#!/bin/bash
# Turn the phase job directories into committed evidence: per-trial rows, the
# score, the per-task table, and the manifest that pins every input.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

RUN_ID="${1:?usage: finalize.sh <run-id> <job-name-prefix>}"
PREFIX="${2:?usage: finalize.sh <run-id> <job-name-prefix>}"
OUT="$TB_REPO/bench/evidence/$RUN_ID"
mkdir -p "$OUT"

# Merge every job directory belonging to this run. There may be more than the
# two resource-class phases: on a slow link the remaining tasks are sometimes
# run in batches as their images finish downloading, so that pulling and running
# overlap instead of serialising.
#
# Batching is scheduling, not measurement. Batch membership follows the
# alphabetical image-download order, never a result, and no batch changes the
# attempt count, the concurrency within a batch, the task set, or the
# denominator. Every job dir found here is merged; a task that never ran simply
# has no row and scores 0 against the fixed denominator.
: > "$OUT/trials.jsonl"
found=0
for d in "$JOBS/${PREFIX}"-*; do
  [ -d "$d" ] || continue
  tmp="$(mktemp)"
  if python3 "$TB_REPO/bench/evidence/score_dev_baseline.py" extract "$d" -o "$tmp" >/dev/null; then
    cat "$tmp" >> "$OUT/trials.jsonl"
    found=$((found+1))
    echo "merged $(basename "$d") ($(grep -c . "$tmp") trials)"
  else
    echo "note: no per-trial results in $(basename "$d")"
  fi
  rm -f "$tmp"
done
test "$found" -gt 0 || { echo "FATAL: no job directories matched ${PREFIX}-*"; exit 1; }

# A task must not appear twice: that would mean two job dirs ran it, which is a
# resume or a rerun, and both are forbidden.
python3 - "$OUT/trials.jsonl" <<'PY'
import collections, json, sys
names = [json.loads(l)["task_name"] for l in open(sys.argv[1]) if l.strip()]
dupes = [n for n, c in collections.Counter(names).items() if c > 1]
if dupes:
    print(f"FATAL: {len(dupes)} task(s) appear in more than one job dir: {dupes[:5]}")
    raise SystemExit(1)
print(f"{len(names)} trials, all distinct tasks")
PY

TASKS="${TB_TASKS:-89}"
python3 "$TB_REPO/bench/evidence/score_dev_baseline.py" score "$OUT/trials.jsonl" \
  --tasks "$TASKS" --markdown "$OUT/results.md" > "$OUT/score.json"
cat "$OUT/score.json"

MANIFEST_JOB="$(ls -d "$JOBS/${PREFIX}"-* 2>/dev/null | head -1)"
python3 "$TB_REPO/bench/evidence/make_manifest.py" \
  --job-dir "$MANIFEST_JOB" \
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
