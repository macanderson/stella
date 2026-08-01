#!/bin/bash
# Fetch the pinned dataset and build the resource-aware run plan.
set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/env.sh"

test "$(command -v harbor)" = "$VENV/bin/harbor" || {
  echo "FATAL: run setup_venv.sh first"; exit 1; }

harbor download "$FB_DATASET" --export -o "$DATASET_DIR"

# Shell glob, not fd/find: these scripts run on whatever bench host is handy,
# and the only tool they may assume is the shell itself.
got=0
for f in "$DATASET_DIR"/*/*/task.toml; do [ -f "$f" ] && got=$((got+1)); done
test "$got" = "$FB_TASK_COUNT" || {
  echo "FATAL: dataset exported $got tasks, expected $FB_TASK_COUNT."
  echo "       The digest pin and FB_TASK_COUNT disagree — one of them is stale."
  exit 1; }

python3 "$(dirname "${BASH_SOURCE[0]}")/plan.py" "$DATASET_DIR" \
  -o "$FB_PLAN" --images-out "$TB_ROOT/frontier-images.txt" \
  --backend "$FB_ENV" ${FB_CONCURRENCY:+--concurrency "$FB_CONCURRENCY"}
