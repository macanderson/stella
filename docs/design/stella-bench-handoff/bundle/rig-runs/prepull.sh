set -uo pipefail
: > ~/tb21/prepull_failed.txt
pull(){ docker image inspect "$1" >/dev/null 2>&1 && return
  for a in 1 2 3 4 5; do timeout 900 docker pull --quiet "$1" >/dev/null 2>&1 && return; sleep $((a*5)); done
  echo "$1" >> ~/tb21/prepull_failed.txt; }
export -f pull
xargs -a ~/tb21/images.txt -P 12 -I{} bash -c 'pull "$@"' _ {}
echo "PREPULL_DONE failed=$(grep -c . ~/tb21/prepull_failed.txt 2>/dev/null || echo 0)"
