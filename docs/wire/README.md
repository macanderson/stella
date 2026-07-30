# The `AgentEvent` wire contract

Generated files. Do not edit them by hand.

| File | What it is |
| --- | --- |
| `agentevent.schema.json` | JSON Schema 2020-12 for `AgentEvent` and its whole payload graph |
| `agentevent.d.ts` | The same contract as TypeScript declarations |

Both are derived from `stella-protocol/src/event.rs` and committed, so a change
to the wire format lands as a reviewable diff instead of as something a
consumer discovers.

## 1. Why this directory exists

`AgentEvent` is the wire format for three surfaces at once:

- the TUI folds it into its render model,
- `stella --output-format stream-json` prints it, one JSON object per line,
- `stella-serve` streams it over SSE.

Three consumers, one enum, and until this existed nothing proved that a change
to it was additive. `docs/design/serve-surface.md` makes the argument against
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
`stella-pipeline/tests/fixtures/from_a_newer_stella.jsonl` pins executably.

`AgentEvent::Unknown` itself therefore has no schema member: it carries no wire
tag of its own, and re-serializes as the foreign object it wrapped. Validating
a recorded stream against this schema will reject exactly those lines, and that
rejection is information about version skew, not about corruption.

Structural invariants that span *several* events — legal stage ordering,
`tool_start`/`tool_result` pairing, a single terminal `complete`, monotonic
budget — cannot be expressed in JSON Schema at all. They live in
`stella_pipeline::replay::validate_stream`, and
`stella-pipeline/tests/stream_conformance.rs` runs them over recorded fixtures.
A stream that validates against this schema line by line can still be an
illegal stream.
