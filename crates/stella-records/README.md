# stella-records

The context-record plane. It answers one question: what has this workspace
said about itself, and what steers this turn?

Three module trees do that work.

- `context_record` — the typed record model. Kinds, scopes, time spans,
  the lifecycle types, and the checks that hold a record together.
- `ingest` — the boundary. It stamps a proposal, gates it, and tracks
  where each claim came from.
- `records` — the registry. It reads rule files and record files, merges
  them under one order, and renders the block the prompt sees.

A fourth module, `adapt`, maps what the record channel picked into the
steering candidates `stella-core` packs to a budget.

## Boundary

No I/O. Every entry point is a plain function. The caller passes in the
clock and the file text; nothing here opens a file or a socket.

This crate lived inside `stella-core` once. It left because the
engine did not use it. The engine reached the whole plane through one hash
call, and that call now goes to `stella_protocol::hash::record_hash`. The
engine names no record type at all.

It depends on `stella-protocol` for that hash and for the token estimate
the render budget spends. It also depends on `stella-core`, for two things:

- `rules` — the markdown rule parser. The registry merges markdown rules
  and TOML records under one order, so it has to read a rule file.
- `steering` — the candidate types `adapt` maps onto.

That second edge points the wrong way, and the re-layering epic tracks
the fix. `rules`
could not come along: it reads `glob` and `mining`, and `mining` is shared
with `skills`, which is engine code. The fix is to lift the rule parser
into a crate both sides can take.

## Where a change goes

| You want to… | File |
|---|---|
| Add or change a record field | `src/ingest/record.rs` |
| Change what a record kind means | `src/context_record/kind.rs` |
| Change how a proposal is stamped or gated | `src/ingest/gate.rs` |
| Change which records a turn selects | `src/records/select.rs` |
| Change the rendered block | `src/records/render.rs` |
| Change how two sources merge | `src/records/registry.rs` |
| Change when a guard may block | `src/records/bridge.rs` |

## Hashing

A record's `record_hash` is a `sha256:` string over its canonical bytes.
The rule is ADR 0004, and the code is `stella_protocol::hash`. Two crates
hash against it, so it lives one layer down. `context_record` re-exports it
under the name callers here already use.

The bytes never name a crate. That is why the move changed no digest, and
a test in `stella-protocol` pins the old value to say so.

## God files — do not add lines

This crate has no god files. Keep it that way: see AGENTS.md's
"God files — plan around them, never into them".
