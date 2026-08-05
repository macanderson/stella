# stella-protocol

The serde types and port traits every other crate in the workspace speaks:
agent events, tool schemas, multimodal attachments, model roles, provider
request/response envelopes, and the `Provider` trait itself. `stella run
--output-format stream-json` is a `serde_json` serialization of `AgentEvent`,
one line per event, so this crate is the workspace's public wire contract.

**Types only — zero logic, zero I/O.** Nothing here opens a socket, reads a
file, or spawns a task. The only functions that exist are total, allocation-
light helpers over their own data (`AgentEvent::type_tag`,
`ProviderError::is_retryable`, `ContextUsage::is_consistent`,
`classify_media_type`). The crate also depends on **no other workspace crate**,
and must not: `stella-core` depends on `stella-protocol`, so importing a core
type here is a dependency cycle. That is why several fields mirror a
`stella-core` type instead of re-exporting it — `AgentEvent::LoopDetected::kind`
is a `String` mirroring `loop_detect::LoopVerdict`, `BudgetScope` duplicates
`budget::BudgetAxis`, and `AgentEvent::Compaction` is a flat struct rather than
a re-exported `CompactionReport`.

## Where it sits

The bottom of the workspace. Its only dependencies are `serde`, `serde_json`,
`thiserror`, and `async-trait`; eleven of the other fourteen crates depend on
it (every crate except `stella-context`, `stella-graph`, and
`stella-observatory`). It builds no binary. The `Provider` port lives here
rather than in `stella-model` precisely so `stella-core` can drive every model
call through `&dyn Provider` without linking a single vendor adapter.

```
stella-protocol ← stella-core ← stella-cli / stella-pipeline / stella-tui …
       ↑ also: stella-model (implements Provider), stella-tools, stella-store,
         stella-mcp, stella-media, stella-fleet, stella-serve
```

## Boundary — does this change belong here?

A change belongs here exactly when it is a serde type — or a field on one —
that two crates must agree on across a crate or process boundary, plus at most
a total, allocation-light helper over that type's own data in the mold of the
four the intro names. The test an agent can apply before writing: if the diff
needs an `await`, a clock, a filesystem or network touch, a dependency beyond
`serde`/`serde_json`/`thiserror`/`async-trait`, or a `match` that decides what
the program *does* next, it does not belong here. Every type added pays its way
with a byte-for-byte serde round-trip test in its own module (AGENTS.md
invariant #4).

The behavior over these types always has a home elsewhere: decision logic over
events, budgets, and compaction in `stella-core`; wire transport implementing
`Provider` in [`stella-model`](../stella-model); rendering in `stella-tui`;
persistence in `stella-store`; replay and diffing in `stella-pipeline`. And
nothing may be imported back from any of them — the mirrored-field examples in
the intro are the required pattern for wanting a core type on the wire, not a
workaround to clean up.

[`src/event.rs`](src/event.rs) carries an extra obligation: it is the
workspace's public wire contract, and its shape is committed as generated
schema artifacts under `docs/wire/` (JSON Schema plus TypeScript declarations).
The gate's `wire-schema` step (`scripts/check-wire-schema.sh`) regenerates them
and fails on any difference, so a change to the event vocabulary regenerates
`docs/wire/` in the same PR (`make wire-schema-update`) — the guard's job is to
put the wire diff on the reviewer's screen, because "additive" is a review
judgment, not a mechanical one.

From this crate's seat the workspace-wide new-crate rule almost always answers
"extend". A new crate is justified only when functionality (a) sits behind a
port and would drag heavy dependencies into a deliberately light crate — but a
type heavy enough to need such a dependency has no business on the shared
contract at all; (b) needs a dependency direction the current graph forbids —
the reason leaf crates like `stella-home` and `stella-diag` exist, and already
solved for anything expressible as a type here at the bottom; or (c) is a
genuinely separate deliverable with its own binary and release cadence.
Splitting the shared vocabulary across two type crates would cost every
consumer a second dependency and reviewers a second place to look, on top of
the standing price of any new crate — an AGENTS.md workspace-table row, an
impacted-crates scope, CI time, a README — and a wrong split is harder to undo
than a wrong merge. A justified new crate updates AGENTS.md's workspace table
and the root `Cargo.toml` members list in the same PR.

## God files — do not add lines

The gate's `file-size` guard (`scripts/check-file-size.sh`) enforces a
1500-line ratchet — a NEW file over the limit is a hard failure with no
baseline escape — and this crate has exactly one file grandfathered at a
recorded ceiling in `scripts/file-size-baseline.txt`. It is a god file: already
too big, closed to growth. Plan event work so no new line lands in it: new
supporting vocabulary goes in a new module re-exported from
[`src/lib.rs`](src/lib.rs) — the crate's own precedent is
[`src/ladder.rs`](src/ladder.rs), split out of `event.rs` when the ladder rung
joined it (#1043), with the re-export keeping `stella_protocol::LadderSnapshot`
at its old path — and types you touch there are candidates to extract, taking
their inline round-trip tests with them. A genuinely new `AgentEvent` variant
cannot avoid its lines in `event.rs`; offset them by extracting the variant's
supporting types, or move the ceiling honestly as below.

| God file | Ceiling (lines) |
|---|---|
| [`src/event.rs`](src/event.rs) | 2906 |

A ceiling can move only via `make file-size-update`, which lands as a
reviewable baseline diff justified like any other change — treat it as an
escape hatch for an irreducible line (a module declaration in an oversized
`lib.rs`), never as a planning assumption.

## Layout

| File | What it holds |
|---|---|
| [`src/lib.rs`](src/lib.rs) | The crate's flat re-export surface, and the statement of the one-directional wire-compatibility rule. Read it first. |
| [`src/event.rs`](src/event.rs) | `AgentEvent` and its supporting types — the stream-json vocabulary, 34 wire variants plus the tagless `Unknown` fallback. Open it to add or change anything a renderer, the journal, or a receipt consumes. |
| [`src/context_event.rs`](src/context_event.rs) | `LifecycleEventEnvelope` / `LifecycleEvent` — the *other* event channel, the one an older binary can still read. Open it when a new event must survive replay by an older reader. |
| [`src/completion.rs`](src/completion.rs) | `CompletionRequest` / `CompletionResult` / `CompletionUsage`, `GenerationParams`, `FinishReason`. The one envelope every provider adapter translates to and from. |
| [`src/provider.rs`](src/provider.rs) | The `Provider` port and `ToolCallObserver`, the seam speculative tool execution hangs on. |
| [`src/tool.rs`](src/tool.rs) | `ToolSchema`, `ToolCall`, `ToolOutput`, `ToolResult` — the engine's single internal tool dialect. |
| [`src/attachment.rs`](src/attachment.rs) | Multimodal *input* attachments, plus `classify_media_type`, `media_type_for_path`, `human_bytes`. |
| [`src/role.rs`](src/role.rs) | `Role` (worker/triage/plan/verifier/embed/vision/image/video) and `ModelRef`. |
| [`src/error.rs`](src/error.rs) | `ProviderError` and its retry classification. |
| [`src/cache.rs`](src/cache.rs) | `CacheCause` and the one-line hint each carries, so the CLI receipt and the deck panel print identical wording. |

## Key concepts

**Additivity holds in both directions, and the boundary between "newer" and
"broken" is the whole design.** New *fields* ride `#[serde(default)]`, so a
newer binary parses every older stream. New `AgentEvent` *variants* travel
backwards via `AgentEvent::Unknown { event_type, payload }`: an older binary
meets an unrecognized `"type"` by preserving the event whole and moving on, and
the JSONL replay reader keeps the line.

What travels backwards is a **variant**, not a field. An unrecognized key on a
tag this build already knows parses (serde ignores it) and is then dropped when
the event is re-serialized, because nothing captured it — so a proxy or
`replay::to_jsonl` relaying a newer stream passes new *events* through whole
while quietly narrowing new *fields* on old ones. Only `AgentEvent::Unknown`
preserves an object verbatim.

The tolerance is scoped to the **tag alone**. A `"type"` this build knows,
carrying a body that does not fit its variant, is still a hard error — that is
corruption or an encoder bug, not a version skew, and laundering it into
`Unknown` would convert a loud failure into silent data loss. `KNOWN_TYPE_TAGS`
is the exact boundary, and it is generated from the same macro list as
`type_tag()` so the two cannot drift.

**The tolerance covers the tag, not the vocabularies underneath it.** The
enums nested inside a variant — `ModelCallRole`, `StageKind`, `PolicyKind`,
`CiStatus`, `FinishReason`, and their peers — are closed, so a token from a
newer build is a body that does not fit a known tag, i.e. a hard error that
costs the reader the whole event. `ModelCallRole` is the one to watch: it has
grown from four values to fourteen, and its `Unknown` variant is the
`serde(default)` for an *absent* `role`, not a `serde(other)` catch-all for an
unrecognized one. `BlockKind` and `CacheZone` are the only two vocabularies
that do degrade — and they degrade lossily, re-serializing a future token as
`"other"` rather than preserving it the way `AgentEvent::Unknown` preserves an
event. Adding a value to a nested vocabulary is therefore still a
one-directional change; only `AgentEvent` variants travel backwards.

`LifecycleEventEnvelope` remains the right channel for stella-*internal*
lifecycle events — it adds an explicit `schema_version`, and it keeps internal
vocabulary off the public cross-language event contract. It is no longer the
only way to stay readable by an older binary.

**The `agent_event_tags!` list is a compile-time guard, not a convenience.**
It generates both `type_tag()` and `KNOWN_TYPE_TAGS` from one variant→tag
mapping, and the generated match has no wildcard arm — so adding a variant
fails `cargo build -p stella-protocol` with `E0004` right at the invocation.
The comment directly below it carries the full downstream checklist: which
matchers the compiler will also stop you at (`stella-pipeline`
`replay::event_signature`, `stella-tui` `model::Model::apply`,
`textline::event_line`, `deck::trace_of`) and — the dangerous half — which ones
it cannot (`replay::structural_diff`'s volatile keep-set,
`deck::event_intensity`, `deck::status_from_event`). Read that list before you
add a variant; it is maintained there, not here.

Note the guard is now about *this workspace's* renderers staying complete, not
about wire safety: external readers survive a new variant on their own.

**Content-freedom is a type-level property.** `UsageIncompleteReason` is a
closed enum precisely so an error body cannot be represented;
`AgentEvent::PolicyDecision` carries a `subject` and a short `outcome` token,
never a secret value; `ContextUsage` / `ContextProviderUsage` are a provider id
and three numbers. `AgentEvent::BlockRegistered::content` is the one deliberate
exception — bytes for the two block kinds the journal cannot otherwise resolve
(the system prefix, the assembled user/recall message). The export projection
strips it, so content-freedom holds *on export*; it does not hold on the live
event stream, which is the same stream `--output-format stream-json` writes to
stdout. That is the one field on `AgentEvent` that can carry raw prompt text,
and only where the operator points the stream keeps it off a remote sink. See
AGENTS.md's "Zero telemetry egress by default".

**Correctness predicates travel with the data.** A consumer never re-derives a
classification a producer already made: `ProviderError::is_retryable` is the
adapter's own verdict (`AgentEvent::Error` even carries `retryable` on the
wire), `CompletionUsage::reported` proves the adapter saw the provider's
terminal usage frame rather than inferring it from nonzero counters, and
`ContextUsage::is_consistent` / `as_of_is_wellformed` let a metering pipeline
check a receipt before trusting it.

**`ToolCallObserver` is advisory but exact.** An adapter may announce all,
some, or none of the calls it will return, but any call it does announce must
be byte-identical (same `call_id`, `name`, parsed `input`) to the one in the
final `CompletionResult` — consumers match speculated work back by exact
equality, and a mismatch is harvested as `AgentEvent::SpeculationDiscarded`.

## Gotchas

- **`AgentEvent::Text { delta }` is not a delta.** The field name is
  wire-frozen legacy; the value is the step's *full* answer text and is
  authoritative. `AgentEvent::TextDelta { text }` is the live fragment stream.
  Consumers must **replace** any accumulated preview with `Text`, never append
  — a retried model call re-streams its deltas from the start and there is no
  reset marker.
- **`LifecycleEventEnvelope::decode` never fails, which cuts both ways.** A
  *recognized* `event_type` whose payload doesn't deserialize degrades to
  `LifecycleEvent::Unknown` rather than erroring, so a renamed payload field
  looks exactly like a future event type. The `event_type` string constants are
  a wire contract; renaming one silently reroutes the event to `Unknown`.
- **The golden JCS vectors in `context_event.rs` are meant to break.** Renaming,
  retyping, adding, or reordering a field in a lifecycle body breaks exactly one
  golden line, because those canonical bytes are the preimage
  `stella_core::context_record::hash` builds `record_hash` from. The dev-dep
  pins the same canonicalizer crate *and version* core hashes with.
- **`ToolSchema::read_only` defaults to `false`** — the safe direction. An MCP
  tool or an older schema that doesn't know about the field is treated as
  mutating and never speculated on.
- **`CompletionUsage::cache_write_tokens` is not a subset of `input_tokens`.**
  Providers report cache writes separately; folding them in would change cost
  accounting, since `Pricing::cost_usd` carries no cache-write rate.
- **Adding a field to `CompletionMessage` can be a prompt-cache regression.**
  `attachments` carries `skip_serializing_if = "Vec::is_empty"` so a text-only
  message serializes byte-for-byte as it did before the field existed. Anything
  that perturbs the stable prefix breaks cache hits (AGENTS.md, "Byte-stable
  prompts").
- **There is no `"auto"` model slug.** Selection is `Option<ModelRef>`; `None`
  *is* auto. `ModelRef` deliberately has no auto variant — the TS-era bug where
  a pseudo-slug leaked into resolver paths is structurally excluded (L-M3).
- **`ProviderError::RateLimited` interpolates its hint by hand.** The obvious
  `{retry_after_ms:?}` renders "retry after Some(500)ms", or the nonsense
  "retry after Nonems", into a message the user reads on the TUI.
- **`ContextUsage`'s sums saturate rather than wrap** — these predicates run
  over journal bytes the consumer did not write, and an audit check must answer
  `false` on a corrupt report, never panic on a debug-build overflow.
- **`AgentEvent::Compaction::calibration_factor` defaults to `0.0`, which is a
  sentinel, not a factor.** On a pre-receipt journal the field is absent, so
  `serde(default)` supplies zero: recovering the raw budget as `effective *
  factor` gives 0, and dividing by it gives infinity. Read `0.0` the way you
  read `effective_budget_tokens == 0` — "this journal predates calibration" —
  and skip the derivation. The identity factor is `1.0`, and the default cannot
  be moved to it without changing how every already-written journal decodes.
- **Dollar fields must be finite.** `cost_usd`, `spent_usd`, `limit_usd`, and
  `estimated_cost_usd` are `f64` (JSON has no decimal type, and a string-encoded
  decimal would break every existing consumer). Binary rounding is far below a
  billable unit at these magnitudes, so that is fine — but `serde_json` writes
  `NaN`/`±Infinity` as `null`, and `null` is not an `f64`. Because the tag is
  known, the line is then a *hard* parse error, not an `Unknown`: a single
  non-finite cost destroys the whole event. The `Option<f64>` case is quieter
  and worse — `null` parses as `None`, so an infinite `BudgetTick::limit_usd`
  comes back as "no limit set". Keep the arithmetic finite at the emitter;
  the wire cannot tell you it wasn't.
- **Duplicate JSON keys are last-wins, not an error.** `AgentEvent`'s decoder
  buffers through `serde_json::Value` to read the tag, and a `Value` is a map —
  so `{"type":"text","delta":"a","delta":"b"}` decodes as `"b"` where a direct
  struct deserialize would have rejected it as a duplicate field. The same hop
  drops line/column from the resulting error message. Both are the price of the
  forward-compat fallback (#672), and both narrow the "a known tag with a bad
  body stays loud" guarantee slightly.
- **`context_event.rs` has no emitter yet.** Nothing in the workspace builds or
  reads a `LifecycleEventEnvelope`; the types and their golden JCS vectors are
  there to pin the wire shape (and the `record_hash` preimages taken from it)
  ahead of the first producer. It is a published contract, not live traffic.
- **`AgentEvent::UsageIncomplete::retries` serializes as `null` when absent.**
  It is the one `Option` on the event vocabulary without
  `skip_serializing_if`, so it costs a key on every incomplete-usage line while
  its neighbours (`Pr::number`, `Pr::ci`, `ContextFrameRef::uri`, …) omit
  theirs. Changing it now would be a wire change, so it is documented rather
  than fixed; do not copy the pattern onto a new field.

## Testing

```bash
make test-protocol        # = cargo test -p stella-protocol
```

There is no `tests/` directory — every test is an inline `#[cfg(test)] mod
tests` at the bottom of the module it covers, and the suite needs no fixtures,
network, or env vars. The dominant shape is the serde round-trip (AGENTS.md
"Serde-first": every type crossing a crate boundary round-trips byte-for-byte),
paired with a "legacy stream still parses" test for each `serde(default)` field
— e.g. `completion_usage_without_cache_write_tokens_still_parses`,
`step_usage_from_a_pre_drift_stream_still_parses`,
`legacy_compaction_without_identities_still_parses`. The other shape is the
golden-bytes test in `context_event.rs`, which needs the
`serde_json_canonicalizer` dev-dependency.

## Extending it

**Adding a field to an existing type** — give it `#[serde(default)]` (plus
`skip_serializing_if` if its absence should stay off the wire), and add a test
that deserializes a hand-written pre-field JSON literal and asserts the
default. Every `serde(default)` in this crate has one; match the neighborhood.

**Adding an `AgentEvent` variant** — first ask whether it must be readable by an
older binary. If yes, it belongs on `LifecycleEventEnvelope`, not here. If no:

1. Add the variant to `AgentEvent` in [`src/event.rs`](src/event.rs).
2. Add its arm to `type_tag` — `cargo build -p stella-protocol` fails with
   `E0004` until you do.
3. Add a round-trip test asserting the `"type"` tag serializes as you expect.
4. Work the checklist in `type_tag`'s doc comment: the compile-enforced
   matchers in `stella-pipeline` and `stella-tui` will stop you one crate at a
   time, then hand-audit the wildcard matchers the compiler cannot catch. This
   step is not optional bookkeeping — a variant landed on `main` past the
   compile-enforced half and broke `stella-pipeline` (#421) and then
   `stella-tui` (#422) on separate days.

**Adding a lifecycle event** — add the token to `context_event::event_type`,
the payload struct, the `LifecycleEvent` variant, the `decode` arm, and a
golden JCS vector in the same PR. `the_event_type_tokens_are_canonical` and
`golden_jcs_vectors_for_every_event_body` are the tests that fail until you do.

**Adding a generation parameter** — put it on `GenerationParams` as an
`Option`, with include-semantics: `None` leaves the provider default alone.
Each adapter in `stella-model` forwards the subset its dialect supports and
silently drops the rest; a parameter a provider cannot express must never fail
the request. If it is a genuine capability divergence rather than a droppable
knob, it also needs a row in `crates/stella-model/src/provider_parity.rs`.

## See also

- [`../../AGENTS.md`](../../AGENTS.md) — "Architecture: ports, not concretions"
  (invariants 1 and 4), and the "Glossary" table, which disambiguates
  `turn_instance` / `(step, call_seq)` from the store's `execution_id` and the
  fleet's `run_id`.
- [`../../docs/design/session-telemetry-receipts-spec.md`](../../docs/design/session-telemetry-receipts-spec.md)
  — the spec `BlockRegistered`, `StepManifest`, `BlockKind`, and `CacheZone`
  implement (§4, §5, §6.2–6.4).
- [`../../docs/design/adaptive-context/context-reuse.md`](../../docs/design/adaptive-context/context-reuse.md) — §2 defines
  `ContextUsage` / `ContextProviderUsage` and the arithmetic identity
  `is_consistent` checks.
- [`../stella-model`](../stella-model) implements `Provider`;
  [`../stella-core/src/ports.rs`](../stella-core/src/ports.rs) holds the
  sibling `ToolExecutor` port.
