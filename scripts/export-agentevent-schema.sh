#!/usr/bin/env bash
#
# Regenerate the committed wire-contract artifacts for `AgentEvent`.
#
#   docs/wire/agentevent.schema.json   JSON Schema 2020-12
#   docs/wire/agentevent.d.ts          TypeScript declarations
#
# `AgentEvent` is the wire format for three surfaces at once — the TUI folds
# it, `--output-format stream-json` prints it, and stella-serve streams it over
# SSE — and nothing used to prove that a change to it was additive.
# docs/design/serve-surface.md says as much about its own hand-maintained route
# table: "the single most dangerous drift in this document". A hand-written
# schema would be a second copy of that problem, so these are DERIVED from the
# types (schemars, behind stella-protocol's optional `schema` feature) and then
# COMMITTED, which is what makes drift a reviewable line in a PR diff instead
# of something a consumer discovers.
#
# Deterministic by construction: serde_json's map is a BTreeMap here, so object
# keys sort, and every array schemars emits is in declaration order. Running
# this twice produces no diff the second time — scripts/check-wire-schema.sh
# depends on exactly that.
#
# Run after ANY change to `AgentEvent` or a type it carries, then commit the
# result alongside the change. `make wire-schema` fails the gate if you forget.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

out_dir="${1:-docs/wire}"

cargo run --quiet -p stella-protocol --features schema --bin export-wire-schema -- "$out_dir"

echo "export-agentevent-schema: OK — wire contract written to $out_dir/."
