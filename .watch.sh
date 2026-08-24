#!/bin/sh
# Emit one line per settled CI check on PR 4670, plus a line when Sourcery's
# review lands. Exits once both are done. Deleted after use.
export NO_COLOR=1
unset CLICOLOR_FORCE
prev=""
sourcery=0
i=0
while [ "$i" -lt 110 ]; do
  i=$((i + 1))
  s=$(gh pr checks 4670 --json name,bucket 2>/dev/null || echo '[]')
  cur=$(printf '%s' "$s" | jq -r '.[] | select(.bucket!="pending") | "\(.name): \(.bucket)"' 2>/dev/null | sort)
  printf '%s\n' "$cur" > /tmp/watch_cur.txt
  printf '%s\n' "$prev" > /tmp/watch_prev.txt
  comm -13 /tmp/watch_prev.txt /tmp/watch_cur.txt 2>/dev/null
  prev="$cur"
  if [ "$sourcery" = "0" ]; then
    n=$(gh pr view 4670 --json comments --jq '[.comments[] | select(.author.login == "sourcery-ai")] | length' 2>/dev/null)
    if [ "${n:-0}" -gt 0 ]; then
      echo "SOURCERY: review comment posted"
      sourcery=1
    fi
  fi
  if printf '%s' "$s" | jq -e 'length > 0 and all(.bucket!="pending")' >/dev/null 2>&1; then
    if [ "$sourcery" = "1" ]; then
      echo "ALL CHECKS SETTLED"
      exit 0
    fi
  fi
  sleep 30
done
echo "WATCH TIMED OUT"
