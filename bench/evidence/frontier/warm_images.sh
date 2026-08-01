#!/bin/bash
# Pull every base image the task builds depend on, with retries, before the
# measured run.
#
# The Terminal-Bench lane calls this prepull, and pulls each task's prebuilt
# `docker_image`. No Frontier-Bench task has one: all 74 ship an
# environment/Dockerfile and build in place. So the thing to warm is the set of
# `FROM` bases (plus any compose service images), which `plan.py` collects.
#
# The reason is unchanged from the Terminal-Bench lane, and worth restating
# because it is the whole point: with --max-retries 0 a registry hiccup is a
# permanent reward-0 row, arithmetically identical to Stella failing the task.
# Image availability is a precondition of the measurement, not a term in it.
set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/env.sh"

# On Modal the builds happen in the cloud, against Modal's own registry path.
# Pulling those bases onto this laptop would warm a cache no trial ever reads —
# minutes and gigabytes spent to change nothing. Skipping is the correct
# behaviour, and saying so beats appearing to have prepared something.
if [ "$FB_ENV" = modal ]; then
  echo "SKIP: FB_ENV=modal builds task images in the cloud; a local pull warms nothing."
  exit 0
fi

LIST="${1:-$TB_ROOT/frontier-images.txt}"
FAILED="$TB_ROOT/frontier_warm_failed.txt"
test -f "$LIST" || { echo "FATAL: no image list at $LIST (run fetch_dataset.sh)"; exit 1; }
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

nfailed=$(grep -c . "$FAILED" || echo 0)
echo "WARM_DONE failed=$nfailed"
# A base image that will not pull now is a task that cannot build later. Say so
# before the run rather than letting it arrive as a reward-0 row.
test "$nfailed" = 0 || {
  echo "WARNING: the tasks depending on these images will fail to build."
  echo "         Resolve or exclude them before starting a measured run."
  exit 1; }
