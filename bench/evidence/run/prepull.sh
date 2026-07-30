#!/bin/bash
# Pull every task image before the measured run, with retries.
#
# Registry flakiness must not be able to enter the score: with --max-retries 0 a
# failed image pull is a permanent reward-0 row, arithmetically identical to
# Stella failing the task. Image availability is a precondition, not a term in
# the measurement. Pass a task-name list file to prioritise a subset.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

LIST="${1:-$TB_ROOT/images.txt}"
FAILED="$TB_ROOT/prepull_failed.txt"
: > "$FAILED"
total=$(grep -c . "$LIST")
i=0
while IFS= read -r img; do
  [ -n "$img" ] || continue
  i=$((i+1))
  if docker image inspect "$img" >/dev/null 2>&1; then
    echo "[$i/$total] have    $img"; continue
  fi
  ok=0
  for attempt in 1 2 3 4 5; do
    if timeout 1200 docker pull --quiet "$img" >/dev/null 2>&1; then ok=1; break; fi
    sleep $((attempt * 10))
  done
  if [ "$ok" = 1 ]; then echo "[$i/$total] pulled  $img"
  else echo "[$i/$total] FAILED  $img"; echo "$img" >> "$FAILED"; fi
done < "$LIST"
echo "PREPULL_DONE failed=$(grep -c . "$FAILED" || echo 0)"
