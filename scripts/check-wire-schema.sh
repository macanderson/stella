#!/usr/bin/env bash
#
# Guard: the committed wire contract still describes the types. See #971.
#
# `AgentEvent` (crates/stella-protocol/src/event.rs) is the wire format for three
# surfaces at once — the TUI folds it, `--output-format stream-json` prints it,
# and stella-serve streams it over SSE — and until docs/wire/ existed nothing
# proved that a change to it was additive. Three consumers, one enum, zero
# machine-checked contract.
#
# docs/spec/serve-surface.md is the cautionary case: it opens with a
# hand-maintained table of "the only routes that exist" and calls that prose
# "the single most dangerous drift in this document". Hand-maintained wire
# documentation drifts, silently, and reads as authority the whole time. So
# docs/wire/ is *generated* from the types and *committed*, and this guard is
# the half that makes committing worth anything: it regenerates into a temp
# directory and fails on any difference.
#
# What it catches is precisely the change that was invisible before — a renamed
# field, a re-tagged variant, a payload type that grew a required member — with
# the failure landing on the author's machine rather than on a consumer parsing
# a stream that no longer matches its types.
#
# What it deliberately does NOT do is judge whether the change is *allowed*.
# The wire contract is additive-only, but "additive" is a review question; this
# script's job is to make sure the diff is on the screen when that question is
# asked. A regeneration is one command, named in the failure below.
#
# Uses portable POSIX tools so it runs on a bare CI runner.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

committed="docs/wire"

if [ ! -d "$committed" ]; then
  echo "check-wire-schema: FAIL — $committed/ does not exist." >&2
  echo "     Generate it: bash scripts/export-agentevent-schema.sh" >&2
  exit 1
fi

tmp="$(mktemp -d "${TMPDIR:-/tmp}/stella-wire-schema.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

# Regenerate from the CURRENT types. Build noise goes to stderr and is only
# shown on failure — a green gate should say one line.
if ! build_log="$(cargo run --quiet -p stella-protocol --features schema \
  --bin export-wire-schema -- "$tmp" 2>&1)"; then
  echo "check-wire-schema: FAIL — the AgentEvent exporter did not run." >&2
  printf '%s\n' "$build_log" >&2
  exit 1
fi
if ! build_log="$(cargo run --quiet -p stella-serve --features schema \
  --bin export-serve-schema -- "$tmp" 2>&1)"; then
  echo "check-wire-schema: FAIL — the serve-frame exporter did not run." >&2
  printf '%s\n' "$build_log" >&2
  exit 1
fi

status=0
for artifact in agentevent.schema.json agentevent.d.ts \
                serveframe.schema.json serveinbound.schema.json serveframe.d.ts; do
  if [ ! -f "$committed/$artifact" ]; then
    echo "check-wire-schema: FAIL — $committed/$artifact is missing." >&2
    status=1
    continue
  fi
  if ! diff -u "$committed/$artifact" "$tmp/$artifact" >"$tmp/$artifact.diff"; then
    echo "check-wire-schema: FAIL — $committed/$artifact is stale." >&2
    echo "" >&2
    # Bounded: a full schema diff can be thousands of lines, and the first
    # screenful already names the drift.
    head -n 60 "$tmp/$artifact.diff" >&2
    lines="$(wc -l <"$tmp/$artifact.diff" | tr -d ' ')"
    if [ "$lines" -gt 60 ]; then
      echo "  … $((lines - 60)) more diff line(s)." >&2
    fi
    status=1
  fi
done

if [ "$status" -ne 0 ]; then
  cat >&2 <<'EOF'

The committed wire contract no longer matches what `AgentEvent` and its payload
types would emit. That means one of two things, and only you know which:

  * You changed the wire format. Regenerate and commit the artifacts with the
    change, so the diff is reviewable:

        bash scripts/export-agentevent-schema.sh

    Then read the diff as a wire change, not as noise. Removing a field,
    renaming one, re-tagging a variant, or making an optional field required
    all BREAK every consumer of `--output-format stream-json`, of the SSE
    stream, and of docs/wire/agentevent.d.ts. The contract is additive-only.

  * You did not mean to. Then the diff above is the bug report: something in
    the event graph changed shape without anyone deciding it should.

EOF
  exit 1
fi

echo "check-wire-schema: OK — docs/wire/ matches the types."
