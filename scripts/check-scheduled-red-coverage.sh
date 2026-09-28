#!/usr/bin/env bash
#
# Guard: `scheduled-red.yml` listens to every scheduled workflow, and only to
# those.
#
#   scripts/check-scheduled-red-coverage.sh [<workflows dir>]
#
# `scheduled-red.yml` starts on `workflow_run`. That trigger names the
# workflows it listens to, by each one's `name:` field. A new scheduled
# workflow that is not in the list fails in silence, which is the gap the
# reporter closes. A name in the list that no scheduled workflow carries is a
# rename the list missed. Both fail here.
#
# A workflow counts as scheduled when a `schedule:` key sits directly under
# its top-level `on:`. A commented-out schedule does not count. Nor does a
# deeper key that happens to be called `schedule`.
#
# The list must be a block list, one `- name` per line. A flow list such as
# `[a, b]` is refused, so this parser never has to guess.
#
# `scripts/test-scheduled-red.sh` runs this on the real tree and on fixtures.
# It is not a gate step. `guard-self-tests.yml` runs that suite on every pull
# request, with no paths filter.
#
# POSIX tools only, so it runs on a bare CI runner.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

workflows="${1:-.github/workflows}"
reporter="$workflows/scheduled-red.yml"

fail=0
report=""
note() { report="${report}check-scheduled-red-coverage: $1"$'\n'; }

# The verdict is settled before anything is written, so a reader that closes
# the pipe changes neither the report nor the exit status.
emit() {
  trap '' PIPE
  printf '%s' "$report" >&2 || true
}

if [ ! -d "$workflows" ]; then
  note "FAIL — $workflows is not a directory."
  emit
  exit 1
fi

if [ ! -f "$reporter" ]; then
  note "FAIL — $reporter does not exist, so no scheduled run is reported."
  emit
  exit 1
fi

# Exit 0 when a `schedule:` key sits at the first indent under `on:`.
is_scheduled() {
  awk '
    function indent_of(s) { match(s, /[^ ]/); return RSTART - 1 }
    /^(on|"on"|'"'"'on'"'"'):/ { inon = 1; child = -1; next }
    inon && /^[^ #]/ { inon = 0 }
    !inon { next }
    /^[ \t]*#/ || !/[^ \t]/ { next }
    {
      ind = indent_of($0)
      if (child < 0) child = ind
      if (ind == child && $0 ~ /^[ \t]*schedule:/) found = 1
    }
    END { exit found ? 0 : 1 }
  ' "$1"
}

# The top-level `name:`, without quotes or a trailing comment.
workflow_name() {
  awk '
    /^name:/ {
      s = $0
      sub(/^name:[ \t]*/, "", s)
      sub(/[ \t]+#.*$/, "", s)
      sub(/[ \t]*$/, "", s)
      if (s ~ /^".*"$/ || s ~ /^'"'"'.*'"'"'$/) s = substr(s, 2, length(s) - 2)
      print s
      exit
    }
  ' "$1"
}

scheduled=""
for wf in "$workflows"/*.yml "$workflows"/*.yaml; do
  [ -f "$wf" ] || continue
  is_scheduled "$wf" || continue
  name="$(workflow_name "$wf")"
  if [ -z "$name" ]; then
    note "FAIL — $wf runs on a schedule and has no top-level name:."
    note "       workflow_run matches a workflow by that name. Give it one."
    fail=1
    continue
  fi
  scheduled="${scheduled}${name}"$'\n'
done

if [ -z "$scheduled" ] && [ "$fail" -eq 0 ]; then
  note "FAIL — no workflow under $workflows runs on a schedule."
  note "       This guard then watches nothing. Either every schedule is gone"
  note "       and scheduled-red.yml should go too, or this parser has rotted."
  emit
  exit 1
fi

# The names under `workflow_run:` then `workflows:`, one per line.
if grep -Eq '^[[:space:]]+workflows:[[:space:]]*\[' "$reporter"; then
  note "FAIL — $reporter lists its workflows as a flow list."
  note "       Write one '- <name>' per line, so this guard can read it."
  emit
  exit 1
fi

listed="$(awk '
  function indent_of(s) { match(s, /[^ ]/); return RSTART - 1 }
  /^[ \t]*#/ || !/[^ \t]/ { next }
  /^[ \t]+workflow_run:/ { inrun = 1; run_ind = indent_of($0); next }
  inrun && indent_of($0) <= run_ind { inrun = 0 }
  inrun && /^[ \t]+workflows:[ \t]*$/ { inlist = 1; list_ind = indent_of($0); next }
  inlist && indent_of($0) <= list_ind { inlist = 0 }
  inlist && /^[ \t]*-[ \t]/ {
    s = $0
    sub(/^[ \t]*-[ \t]+/, "", s)
    sub(/[ \t]+#.*$/, "", s)
    sub(/[ \t]*$/, "", s)
    if (s ~ /^".*"$/ || s ~ /^'"'"'.*'"'"'$/) s = substr(s, 2, length(s) - 2)
    print s
  }
' "$reporter")"

if [ -z "$listed" ]; then
  note "FAIL — $reporter names no workflow under workflow_run: workflows:."
  fail=1
fi

while IFS= read -r name; do
  [ -n "$name" ] || continue
  if ! grep -Fxq -- "$name" <<EOF
$listed
EOF
  then
    note "FAIL — the scheduled workflow '$name' is missing from $reporter."
    note "       A red scheduled run of it would open no issue. Add it to the"
    note "       workflow_run workflows: list, spelled as its name: field."
    fail=1
  fi
done <<EOF
$scheduled
EOF

while IFS= read -r name; do
  [ -n "$name" ] || continue
  if ! grep -Fxq -- "$name" <<EOF
$scheduled
EOF
  then
    note "FAIL — $reporter listens to '$name', and no scheduled workflow has"
    note "       that name. Drop it, or fix the spelling to match a name: field."
    fail=1
  fi
done <<EOF
$listed
EOF

if [ "$fail" -ne 0 ]; then
  emit
  exit 1
fi

count="$(printf '%s' "$scheduled" | grep -c .)"
echo "check-scheduled-red-coverage: OK — $reporter listens to each of the $count scheduled workflow(s), and to nothing else."
