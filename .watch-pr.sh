#!/bin/sh
# Wait until no check on PR $1 is pending, then print the settled table.
# `gh --json` colourises here whatever NO_COLOR says, so strip the escapes.
i=0
while [ "$i" -lt 120 ]; do
  i=$((i + 1))
  s=$(gh pr checks "$1" -R macanderson/stella --json name,bucket 2>/dev/null |
    sed 's/\x1b\[[0-9;]*m//g')
  if [ -n "$s" ] && printf '%s' "$s" | jq -e 'length>0 and (all(.[]; .bucket!="pending"))' >/dev/null 2>&1; then
    printf '%s' "$s" | jq -r '.[] | "\(.bucket)\t\(.name)"' | sort
    exit 0
  fi
  sleep 30
done
echo "TIMED OUT waiting for checks"
exit 1
