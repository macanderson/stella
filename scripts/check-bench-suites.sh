#!/usr/bin/env bash
#
# Guard: `make bench-test` runs every Python suite .github/workflows/bench.yml
# runs, and does so by reading that workflow rather than by restating it.
# See #2847.
#
# The defect this exists to prevent is not "a suite is missing" — it is "the
# list is written twice". Before #2847 the workflow ran seven pytest suites and
# `make bench-test` ran three, so the four the workflow added over time had no
# local command at all. On 2026-08-11 that cost a red `main`: #2844 merged with
# its bench check already reporting `failure`, on a DETERMINISTIC
# `arenabench/tests/test_runner_netns.py` failure that any local run of that one
# file would have caught. The pre-push hook is this repository's stated answer
# to `enforce_admins` being off, and for those four suites it had nothing to run.
#
# The fix was to delete the second copy: scripts/bench-suites.sh parses the
# workflow, `make bench-test` runs what it finds, and .githooks/pre-push selects
# on the workflow's own scope filter. This guard holds that arrangement in
# place — a hand-written suite list creeping back into the Makefile, or a hand
# copied regex creeping back into the hook, is the original defect returning and
# is what sections 4 and 5 below fail on.
#
# It also runs the parse itself on every gate (sections 1-3), so a workflow edit
# that this repository cannot read meets its error immediately, rather than at
# whichever later push happens to run the suites.
#
# What it deliberately does NOT do: run the suites. They take minutes and need
# uv; `bench-test` is not a gate step for exactly that reason (#2847 weighed it
# and left it out), and .githooks/pre-push runs it only for a push that touches
# the bench tooling.
#
# Uses portable POSIX tools so it runs on a bare CI runner.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

workflow=".github/workflows/bench.yml"
derive="./scripts/bench-suites.sh"
makefile="Makefile"
hook=".githooks/pre-push"

fail=0

# The verdict is decided before anything is written, and emission is
# best-effort: a reader that closed the pipe (`| head -1`) must be able to
# change neither the report nor the exit status (#1815).
# scripts/check-gate-parity.sh is the shape being copied.
report=""
note() { report="${report}check-bench-suites: $1"$'\n'; }
emit() {
  trap '' PIPE
  printf '%s' "$report" >&2 || true
}

for required in "$workflow" "$derive" "$makefile" "$hook"; do
  if [ ! -f "$required" ]; then
    note "FAIL — $required does not exist."
    emit
    exit 1
  fi
done

# ── 1. The parse is total ────────────────────────────────────────────────────

if ! suites="$("$derive" list 2>&1)"; then
  note "FAIL — $derive could not read the suite list out of $workflow:"
  note ""
  while IFS= read -r line; do
    note "     $line"
  done <<EOF
$suites
EOF
  note ""
  note "     A pytest invocation the parse cannot classify is a hard error, not"
  note "     a skip: a silently unrecognised suite is a suite nothing runs."
  emit
  exit 1
fi

count="$(printf '%s\n' "$suites" | grep -c . || true)"
if [ "$count" -eq 0 ]; then
  note "FAIL — $derive found no pytest suites in $workflow."
  note "     Either the workflow stopped gating them, or the parse stopped"
  note "     recognising the shape it is written in."
  emit
  exit 1
fi

# ── 2. Every suite names something that exists ───────────────────────────────

while IFS="$(printf '\t')" read -r mode workdir args; do
  [ -n "$mode" ] || continue
  if [ ! -d "$workdir" ]; then
    note "FAIL — $workflow runs a suite in '$workdir', which is not a directory."
    fail=1
  fi
  for arg in $args; do
    case "$arg" in
    -*) continue ;;
    esac
    if [ ! -e "$workdir/$arg" ] && [ ! -e "$arg" ]; then
      note "FAIL — $workflow runs pytest against '$arg', which does not exist."
      fail=1
    fi
  done
done <<EOF
$suites
EOF

# ── 3. The scope filter is readable ──────────────────────────────────────────

if ! filter="$("$derive" filter 2>&1)"; then
  note "FAIL — $derive could not read the scope filter out of $workflow:"
  note "     $filter"
  fail=1
  filter=""
elif [ "${filter#^\(}" = "$filter" ]; then
  note "FAIL — the scope filter read from $workflow does not look like one:"
  note "     $filter"
  note "     $hook greps the pushed diff with it; an unanchored pattern would"
  note "     match paths the workflow does not consider bench tooling."
  fail=1
fi

# ── 4. The Makefile does not carry a second copy of the list ─────────────────

recipe="$(awk '
  /^bench-test:/ { inside = 1; next }
  inside && /^\t/ { print; next }
  inside { exit }
' "$makefile")"

if [ -z "$recipe" ]; then
  note "FAIL — $makefile has no bench-test recipe."
  fail=1
else
  if ! printf '%s\n' "$recipe" | grep -qF 'bench-suites.sh run'; then
    note "FAIL — $makefile's bench-test does not run \`$derive run\`."
    note "     That delegation is what keeps one suite list instead of two."
    fail=1
  fi
  if printf '%s\n' "$recipe" | grep -qF 'pytest'; then
    note "FAIL — $makefile's bench-test names pytest itself:"
    while IFS= read -r line; do
      note "     $line"
    done <<EOF
$(printf '%s\n' "$recipe" | grep -F 'pytest')
EOF
    note "     A suite spelled out here is a second hand-maintained copy of"
    note "     $workflow's list, which is the defect #2847 records. Add the"
    note "     suite to the workflow — the one that decides whether main is"
    note "     green — and it is run locally by construction."
    fail=1
  fi
fi

# ── 5. The hook does not carry a second copy of the filter ───────────────────

if ! grep -qF 'bench-suites.sh filter' "$hook"; then
  note "FAIL — $hook does not read the scope filter from $derive."
  note "     It has to select the same paths $workflow does; a pattern typed"
  note "     out here is the same duplication in a different file."
  fail=1
fi
if [ -n "$filter" ] && grep -qF -- "$filter" "$hook"; then
  note "FAIL — $hook contains $workflow's scope filter literally:"
  note "     $filter"
  note "     Read it with \`$derive filter\` instead of copying it."
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  note ""
  note "The suite list lives in $workflow — the required check that decides"
  note "whether main is green — and nothing else may restate it."
  emit
  exit 1
fi

emit
printf 'check-bench-suites: OK — %s suites in %s, all run by make bench-test.\n' \
  "$count" "$workflow" || true
