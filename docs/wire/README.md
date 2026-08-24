# The wire contract

Generated files. Do not edit them by hand.

| File | What it is |
| --- | --- |
| `agentevent.schema.json` | JSON Schema 2020-12 for `AgentEvent` and its whole payload graph |
| `agentevent.d.ts` | The same contract as TypeScript declarations |
| `serveframe.schema.json` | JSON Schema for `ServerFrame`, the `stella-serve` transport envelope |
| `serveinbound.schema.json` | The bodies a host POSTs to the transport — three that answer a reverse request, plus the optional `engine` object on a turn — and every payload type they reference |
| `serveframe.d.ts` | Both of the above as TypeScript, plus the `seq` envelope |
| `wrapper.wire.json` | Every message the wrapper socket carries, in both its fullest and its emptiest legal form |
| `wrapper.schema.json` | JSON Schema for the socket's two point messages, `WrapperRequest` and `WrapperResponse` |
| `wrapper.d.ts` | The same contract as TypeScript declarations, plus the `WrapperPoint` union |

All are derived from the Rust types and committed, so a change to the wire
format lands as a reviewable diff instead of as something a consumer discovers.

**Three contracts, deliberately separate.** `AgentEvent` is the *payload*, and it
has three consumers at once — the TUI, `--output-format stream-json`, and the
server. `ServerFrame` is the *envelope*, and exists only between `stella-serve`
and its host. Publishing them as one artifact would imply a coupling that is
not there and make a transport change read as a change to the CLI's output
format. They share one TypeScript printer
(`stella_protocol::schema_export`), so there is one subset of JSON Schema to
keep in step rather than two. The *wrapper socket* is the third and is separate
again: it is spoken between the host and an out-of-process plugin in whatever
language its author chose, and neither of the other two artifacts describes a
byte of it.

### The wrapper socket ships a corpus as well as a schema, and neither subsumes the other

`wrapper.wire.json` is a **corpus**: every message serialized through the same
`Serialize` impls the transport uses, twice each — once with every optional
field populated (`full`) and once with every omissible field omitted
(`minimal`). It catches a renamed field, a re-tagged variant, an added or
removed field, and a field moving between required and optional. Totality is
the compiler's: `crates/stella-plugin/src/wire_corpus.rs` enumerates every
closed enum with a successor `match` and builds every struct with an exhaustive
literal, so a new variant or field fails to compile there before it can go
unpublished here.

What it cannot show is a **widened scalar** (`u32` → `u64`) or a string field
gaining a format or pattern constraint: neither changes a byte of an example.
`wrapper.schema.json` is what states those — JSON Schema 2020-12 derived by
`schemars` from the types, exactly as the two `AgentEvent`/`ServerFrame`
artifacts are. It covers `WrapperRequest` and `WrapperResponse`, which is the
point exchange; the host-call and driver channels stay corpus-only, because
`HostCallOk` is an untagged union whose contract is that its variants are
discriminable by their required keys — a property JSON Schema can state as
`oneOf` and cannot check, so a schema there would read as more authority than
it has.

The corpus is not superseded and is not going away: a schema describes shapes,
and the corpus pins the bytes a plugin's parser actually meets, which a schema
cannot be run backwards to recover. `crates/stella-plugin/src/wire_schema.rs`
argues the split where a reader meets it.

`wrapper.d.ts` prints that schema through the same
`stella_protocol::schema_export` the other two `.d.ts` artifacts use. It could
not, until #4535: the printer assumed a union tagged on `type`, and this
socket's envelope is tagged on `point`, so the one contract whose audience
writes TypeScript was the one contract with no declarations to import. Printing
it also flattened the composite document — `schemars` numbers every `$ref` from
the root of the document it generates, so nesting two derived schemas whole left
seventeen references pointing at definitions the composite root did not have.
They resolve now, and each payload type is declared once instead of twice.

## The one attached property, and why it is not derived

Every `AgentEvent` variant in `agentevent.schema.json` carries an optional
`ts`: the wall-clock instant, in Unix-epoch milliseconds, at which the sink
wrote that line (#2111). It is not a member of any variant — it is applied at
the write boundary by `stella_protocol::journal::stamped_line`, because a stamp
is a fact about a *write*, the same event reaches more than one sink, and the
engine that produces events owns no clock.

`schemars` therefore cannot derive it, and `schema_export::attach_journal_stamp`
adds it in one uniform pass instead, with its prose taken from
`journal::TS_DESCRIPTION` so the published text and the Rust doc are one string.
The alternative — flattening an envelope struct into the schema root — would
have cost the discriminated union, and with it `KnownTypeTag`, which is the one
thing a forward-compatible consumer most needs. Optional on every surface is the
only claim true of all three: a line recorded before the field existed has none,
and `stella-serve` frames the event in an envelope that stamps its own.

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

Each exporter lives behind its crate's optional `schema` feature
(`stella-protocol`, `stella-serve`, `stella-plugin`), so a default `cargo build`
never compiles `schemars` or the exporters.

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
on — which is what `AgentEvent::Unknown` does on the Rust side.

`AgentEvent::Unknown` itself therefore has no schema member: it carries no wire
tag of its own, and re-serializes as the foreign object it wrapped. Validating
a recorded stream against this schema will reject exactly those lines, and that
rejection is information about version skew, not about corruption.

## `turn_complete` vs `complete` — two endings, one meaning each

A run can be several turns: a staged run with a revise loop drives the engine
more than once. Those are two different facts, and since #3379 they are two
different events, so a reader never has to guess which contract a line is
holding.

- **`turn_complete`** — the engine's. *One turn is over.* It appears once per
  successful turn, so a run of three turns carries three of them, and it says
  nothing about whether more work is coming.
- **`complete`** — the run owner's, and unchanged: still exactly once, still
  last, still only on success. A consumer that stops at `complete` stops at the
  end of the run, exactly as before.

The engine no longer emits `complete` at all, which is what makes the promise
above true rather than merely usual. Before, a wrapper running several turns
had to reach into the engine's stream and suppress its per-turn `complete` to
keep this invariant — and a consumer reading a raw engine stream saw a word
that meant something different there than it does here.

A consumer that already ignores unknown tags needs no change: `turn_complete`
is simply a new line it did not previously see. One that wants per-turn
boundaries — cost per turn, where a revise round began — should read it rather
than counting `complete`.

Structural invariants that span *several* events — legal stage ordering,
`tool_start`/`tool_result` pairing, a single terminal `complete`, monotonic
budget — cannot be expressed in JSON Schema at all, so a stream that validates
against this schema line by line can still be an illegal stream. The validator
that used to check them (`stella_pipeline::replay`) went with the crate #3865
deleted from this workspace; nothing in this tree checks them today.
