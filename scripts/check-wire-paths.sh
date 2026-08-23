#!/usr/bin/env bash
#
# Guard: `.githooks/pre-push` decides "could this diff have moved the wire
# contract?" by reading .github/workflows/wire-schema.yml, not by restating it.
# See #3836, and #2847 for the guard this copies.
#
# The defect is not a wrong pattern — it is one question with two
# hand-maintained answers. The hook chooses between `make guards-fast`
# (compiles nothing) and `make guards` (runs the two schema exporters) on a
# regex it used to carry itself, under a comment that named the hazard out loud
# and left it standing: "Keep this list in sync with that workflow's `paths:`
# filter — both answer the same question, and neither is derived from the
# other."
#
# For the bench suites the same shape cost a red `main` (#2847). Here it would
# be quieter and in the direction that misleads: the hook drops to the cheap
# rung for a diff the workflow WOULD have considered wire-touching, so a stale
# generated schema leaves the machine believing it was checked — the hole #1439
# made this rung conditional in order to close.
#
# So the copy is deleted, scripts/wire-schema-paths.sh parses the workflow, and
# this guard holds that arrangement in place: sections 1-3 run the parse on
# every gate so a workflow edit meets its error immediately, and section 4 fails
# when a hand-written copy creeps back into the hook.
#
# Uses portable POSIX tools so it runs on a bare CI runner.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

workflow=".github/workflows/wire-schema.yml"
derive="./scripts/wire-schema-paths.sh"
hook=".githooks/pre-push"

fail=0

# The verdict is decided before anything is written, and emission is
# best-effort: a reader that closed the pipe (`| head -1`) must be able to
# change neither the report nor the exit status (#1815).
report=""
note() { report="${report}check-wire-paths: $1"$'\n'; }
emit() {
  trap '' PIPE
  printf '%s' "$report" >&2 || true
}

for required in "$workflow" "$derive" "$hook"; do
  if [ ! -f "$required" ]; then
    note "FAIL — $required does not exist."
    emit
    exit 1
  fi
done

# ── 1. The parse is total ────────────────────────────────────────────────────

if ! entries="$("$derive" list 2>&1)"; then
  note "FAIL — $derive could not read the path set out of $workflow:"
  note ""
  while IFS= read -r line; do
    note "     $line"
  done <<EOF
$entries
EOF
  note ""
  note "     A \`paths:\` entry the parse cannot translate is a hard error, not"
  note "     a skip: a skipped entry narrows the hook below the workflow."
  emit
  exit 1
fi

count="$(printf '%s\n' "$entries" | grep -c . || true)"
if [ "$count" -eq 0 ]; then
  note "FAIL — $derive found no \`paths:\` entries in $workflow."
  note "     Either the workflow stopped filtering, or the parse stopped"
  note "     recognising the shape it is written in."
  emit
  exit 1
fi

# ── 2. Every entry names something that exists ───────────────────────────────
#
# A `paths:` entry for a path that was deleted or renamed is a filter arm that
# can never fire — the workflow keeps watching a file nobody will edit again,
# and the hook inherits the same blind spot.

while IFS= read -r entry; do
  [ -n "$entry" ] || continue
  target="${entry%\*\*}"
  if [ ! -e "$target" ] && [ ! -d "$target" ]; then
    note "FAIL — $workflow watches '$entry', which does not exist."
    note "     A filter arm over a path nothing can touch never fires."
    fail=1
  fi
done <<EOF
$entries
EOF

# ── 3. The filter selects what it claims to ──────────────────────────────────

if ! filter="$("$derive" filter 2>&1)"; then
  note "FAIL — $derive could not build the filter from $workflow:"
  note "     $filter"
  fail=1
  filter=""
elif [ "${filter#^\(}" = "$filter" ]; then
  note "FAIL — the filter built from $workflow does not look like one:"
  note "     $filter"
  note "     $hook greps the pushed diff with it; an unanchored pattern would"
  note "     match paths the workflow does not consider wire-touching."
  fail=1
else
  # Shape proves nothing on its own. Behaviour is the question, and it fails in
  # two directions that cost differently: a filter matching nothing drops every
  # push to `guards-fast` and lets a stale committed schema through, while one
  # matching everything makes every website-only push pay a cargo build — the
  # cost #1439 made this rung conditional to avoid.
  for probe in docs/wire/agentevent.schema.json crates/stella-protocol/src/event.rs \
    scripts/check-wire-schema.sh; do
    if ! printf '%s\n' "$probe" | grep -Eq "$filter"; then
      note "FAIL — the filter does not match $probe, which IS wire-touching:"
      note "     $filter"
      note "     $hook would take the cheap rung for a push that touches it."
      fail=1
    fi
  done
  for probe in website/src/app/page.tsx README.md crates/stella-core/src/driver.rs; do
    if printf '%s\n' "$probe" | grep -Eq "$filter"; then
      note "FAIL — the filter matches $probe, which is NOT wire-touching:"
      note "     $filter"
      note "     $hook would run the schema exporters on unrelated pushes."
      fail=1
    fi
  done
fi

# ── 4. The hook does not carry a second copy ─────────────────────────────────

if ! grep -qF 'wire-schema-paths.sh filter' "$hook"; then
  note "FAIL — $hook does not read the path filter from $derive."
  note "     It has to select the same paths $workflow does; a pattern typed"
  note "     out here is the same duplication in a different file."
  fail=1
fi

# Any grep in the hook whose pattern names a watched path is a copy returning.
# Comment lines are dropped first — the reasoning above the call cites
# `docs/wire/` in prose, and citing a path is not filtering on one.
if hand_written="$(grep -v '^[[:space:]]*#' "$hook" | grep -n 'grep -E.*docs/wire' || true)"; then
  if [ -n "$hand_written" ]; then
    note "FAIL — $hook greps for a wire path itself:"
    while IFS= read -r line; do
      note "     $line"
    done <<EOF
$hand_written
EOF
    note "     Read the filter with \`$derive filter\` instead of restating it."
    fail=1
  fi
fi

if [ -n "$filter" ] && grep -qF -- "$filter" "$hook"; then
  note "FAIL — $hook contains $workflow's path filter literally:"
  note "     $filter"
  note "     Read it with \`$derive filter\` instead of copying it."
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  note ""
  note "The wire-contract path set lives in $workflow, and nothing else may"
  note "restate it."
  emit
  exit 1
fi

emit
printf 'check-wire-paths: OK — %s path(s) in %s, and the hook derives its filter from them.\n' \
  "$count" "$workflow" || true
