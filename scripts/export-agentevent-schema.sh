#!/usr/bin/env bash
#
# Regenerate the committed wire-contract artifacts for `AgentEvent`.
#
#   docs/wire/agentevent.schema.json    JSON Schema 2020-12 for AgentEvent
#   docs/wire/agentevent.d.ts           …and its TypeScript declarations
#   docs/wire/serveframe.schema.json    JSON Schema for the stella-serve frame
#   docs/wire/serveinbound.schema.json  …and the two bodies a host POSTs back
#   docs/wire/serveframe.d.ts           …and their TypeScript declarations
#   docs/wire/wrapper.wire.json         the wrapper socket's messages, as bytes
#   docs/wire/wrapper.schema.json       …and as JSON Schema
#   docs/wire/wrapper.d.ts              …and as TypeScript declarations
#
# Three exporters. `AgentEvent` is the payload (consumed by the TUI,
# by --output-format stream-json, AND by the server); a `ServerFrame` is the
# envelope, and exists only between stella-serve and its host. They are
# separate artifacts because they have separate blast radii — a transport
# change must not read as a change to the CLI's output format — but both print
# through stella_protocol::schema_export, so there is one subset of JSON Schema
# to keep in step rather than two.
#
# The third is the wrapper socket (`doc:wrapper-socket` §3 commitment 2), and it
# alone ships a *corpus* as well: every message in its fullest and emptiest
# legal form, which is what pins the bytes a plugin's parser meets in whatever
# language it was written. Its schema and its declarations print through the
# same two paths the other two contracts take.
# `crates/stella-plugin/src/wire_corpus.rs` states exactly what the corpus
# catches and what only the schema would (#3532).
#
# `AgentEvent` is the wire format for three surfaces at once — the TUI folds
# it, `--output-format stream-json` prints it, and stella-serve streams it over
# SSE — and nothing used to prove that a change to it was additive.
# docs/spec/serve-surface.md says as much about its own hand-maintained route
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
cargo run --quiet -p stella-serve --features schema --bin export-serve-schema -- "$out_dir"
cargo run --quiet -p stella-plugin --features schema --bin export-wrapper-wire -- "$out_dir"

echo "export-agentevent-schema: OK — wire contract written to $out_dir/."
