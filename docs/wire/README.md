# The wire contract

Generated files. Do not edit them by hand.

| File | What it is |
| --- | --- |
| `agentevent.schema.json` | JSON Schema 2020-12 for `AgentEvent` and its whole payload graph |
| `agentevent.d.ts` | The same contract as TypeScript declarations |
| `serveframe.schema.json` | JSON Schema for `ServerFrame`, the `stella-serve` transport envelope |
| `serveinbound.schema.json` | The two bodies a host POSTs back to answer a reverse request |
| `serveframe.d.ts` | Both of the above as TypeScript, plus the `seq` envelope |

All are derived from the Rust types and committed, so a change to the wire
format lands as a reviewable diff instead of as something a consumer discovers.

**Two contracts, deliberately separate.** `AgentEvent` is the *payload*, and it
has three consumers at once — the TUI, `--output-format stream-json`, and the
server. `ServerFrame` is the *envelope*, and exists only between `stella-serve`
and its host. Publishing them as one artifact would imply a coupling that is
not there and make a transport change read as a change to the CLI's output
format. They share one TypeScript printer
(`stella_protocol::schema_export`), so there is one subset of JSON Schema to
keep in step rather than two.

## The one hand-written line, and why it is safe

`seq` is added by the transport at delivery time, not by the engine, so no
derive on `ServerFrame` describes it — which makes
`StellaWireFrame = ServerFrame & { seq: number }` the single hand-written type
in these artifacts. It is pinned by `envelope_pin` in
`crates/stella-serve/src/history.rs`, which serializes a real frame through the real
encoder and asserts the wire object is exactly the frame's own keys plus
`seq`. If that ever stops being true, the test fails rather than the artifact
quietly lying.

## 1. Why this directory exists

`AgentEvent` is the wire format for three surfaces at once:

- the TUI folds it into its render model,
- `stella --output-format stream-json` prints it, one JSON object per line,
- `stella-serve` streams it over SSE.

Three consumers, one enum, and until this existed nothing proved that a change
to it was additive. `docs/spec/serve-surface.md` makes the argument against
the alternative better than this page can: it opens with a hand-maintained
table of "the only routes that exist" and calls its own prose "the single most
dangerous drift in this document". A hand-written schema would be a second copy
of exactly that failure — authoritative-looking, unchecked, and wrong at some
point nobody can name.

So these artifacts are generated from the types. A derive cannot describe a
shape the code does not have.

## 2. Regenerating

```sh
bash scripts/export-agentevent-schema.sh
```

Run it after any change to `AgentEvent` or a type it carries, and commit the
result with the change. `make wire-schema` (part of `make gate`) regenerates
into a temp directory and fails if the committed files differ, so a forgotten
regeneration is caught before review rather than after release.

The exporter lives behind `stella-protocol`'s optional `schema` feature, so a
default `cargo build` never compiles `schemars`.

## 3. Reading the contract

The schema is a `oneOf` over every `"type"` tag this build emits — the same
list as `stella_protocol::KNOWN_TYPE_TAGS`, and as the `KnownTypeTag` union at
the bottom of the `.d.ts`. Each member is a closed object shape.

The contract is **additive-only**. New fields arrive optional; new event types
arrive as new members of the union. Renaming a field, re-tagging a variant, or
making an optional field required breaks every consumer at once.

## 4. What is deliberately not described

A `"type"` this schema does not list is **not** an error. It is an event from a
newer stella, and a forward-compatible consumer keeps the line intact and moves
on — which is what `AgentEvent::Unknown` does on the Rust side, and what
`crates/stella-pipeline/tests/fixtures/from_a_newer_stella.jsonl` pins executably.

`AgentEvent::Unknown` itself therefore has no schema member: it carries no wire
tag of its own, and re-serializes as the foreign object it wrapped. Validating
a recorded stream against this schema will reject exactly those lines, and that
rejection is information about version skew, not about corruption.

Structural invariants that span *several* events — legal stage ordering,
`tool_start`/`tool_result` pairing, a single terminal `complete`, monotonic
budget — cannot be expressed in JSON Schema at all. They live in
`stella_pipeline::replay::validate_stream`, and
`crates/stella-pipeline/tests/stream_conformance.rs` runs them over recorded fixtures.
A stream that validates against this schema line by line can still be an
illegal stream.
