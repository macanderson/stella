---
id: replay-golden-trajectories
title: "Golden replay trajectories"
status: archived
---

# Golden replay trajectories

> **Status (2026-08-19):** this document describes machinery that lived in
> `crates/stella-pipeline`, which was removed from the workspace (#3865)
> along with `make record-golden`. It is kept as the design record for the
> replay-fixture approach; **none of the commands, paths or types below exist
> in the current tree**, and the whole document is written in the past tense
> for that reason. A verification plugin that wants trajectory fixtures
> implements this on its own side of the wrapper socket
> (`doc:pipeline-as-plugins` §8); the surviving per-PR wire gate is
> `doc:verification-gate` Layer 2.

How the golden-trajectory fixtures were recorded and refreshed, and what a
**reference** engine had to do before its runs could join them.

Machinery: `crates/stella-pipeline/src/replay.rs` (`validate_stream`,
`structural_diff`, `streams_equivalent`) and
`crates/stella-pipeline/src/replay/golden.rs` (the fixture format and its load
gates) — both deleted in #3865.

---

## Two kinds of recording, deliberately not interchangeable

A golden's manifest named its `source`, and the distinction was decisive:

| Source | What it is | What it proves |
|---|---|---|
| `rust_stack` | Recorded from this workspace's own pipeline | **Drift baseline.** Catches a stage that stopped being emitted, a tool that changed name, an event that moved or vanished. Both sides are the same code, so it is not independent evidence. |
| `reference` | Recorded from another engine through an adapter that emits this protocol | **Reference trajectory.** The only kind that answers "does the Rust stack agree with the reference implementation?" |

Encoding this in the fixture rather than in prose is what stopped a baseline
from being cited later as a reference — a rule worth carrying into any
successor. `RecordingSource::is_reference()` was asserted in
`crates/stella-pipeline/src/pipeline/tests/golden.rs`.

Every committed golden was a `rust_stack` baseline. No `reference` recording was
ever made — see [The reference-engine gap](#the-reference-engine-gap).

---

## The fixture format

Two files per task lived under `crates/stella-pipeline/tests/fixtures/golden/`:

- `<task_id>.jsonl` — one `AgentEvent` per line, the same wire format
  `stella run --output-format stream-json` emits.
- `<task_id>.manifest.json` — `task_id`, a one-line `description`, the
  `source`, and `event_count`.

`event_count` was not decoration. `parse_jsonl` deliberately tolerates a torn
final line (L-T1) — correct for a live reader recovering from a crashed writer,
and exactly wrong for a committed fixture, where it would silently hand back a
recording one event short and weaken every assertion made against it. The count
turned that into a loud `GoldenError::Truncated`.

`GoldenTrajectory::load` additionally refused:

- a recording that is not parseable as an `AgentEvent` stream
  (`GoldenError::Recording`) — this is what a foreign wire format produces;
- a manifest whose `task_id` disagrees with the file it was loaded as
  (`GoldenError::TaskIdMismatch`);
- a recording that violates the protocol's own structural invariants
  (`GoldenError::NotWellFormed`). A golden is the yardstick other runs are
  measured against; an ill-formed one would license ill-formed runs.

---

## Recording and refreshing

> **Neither command exists.** `make record-golden` is not a Makefile target and
> `-p stella-pipeline` names no crate. They are shown because the *shape* of the
> refresh loop is the part a successor should copy.

```bash
make record-golden                                       # (removed, #3865)
STELLA_REFRESH_GOLDEN=1 cargo test -p stella-pipeline --lib golden   # (removed)
```

The recorders lived in `crates/stella-pipeline/src/pipeline/tests/golden.rs`.
Each drove the real `Pipeline` over scripted model/test ports and recorded the
stream it actually emitted — deterministic, no API key, runnable in CI, which
was the only reason a recording could be asserted on every `cargo test` instead
of refreshed by hand and hoped over. **Any successor that needs a live model to
refresh its fixtures has already lost this property.**

**A non-empty fixture diff was a change to the observable event contract**, and
was reviewed as one. If the change was intended, the refreshed fixture landed
with the change that caused it; if not, it was a regression.

### Adding a task

The loop was:

1. Add a `#[tokio::test]` to `crates/stella-pipeline/src/pipeline/tests/golden.rs`
   driving the pipeline over scripted ports and ending in `check_golden`.
2. Run `make record-golden` to write the fixture.
3. **Read the recorded `.jsonl`.** It is evidence — confirm it is the flow you
   meant to capture. (A golden blessed without looking is a changelog, not a
   test; the same rule still governs the TUI's deck snapshots today.)
4. Keep the set discriminating: `the_recorded_flows_are_structurally_distinct`
   failed if two goldens collapsed into the same trajectory, because a golden
   set whose members are indistinguishable passes no matter what the pipeline
   does.

---

## The reference-engine gap

Issue #462 asks for reference trajectories recorded from the TS engine. The
engine is reachable. **It does not emit this protocol**, and that — not access
— is the blocker.

Its `--output-format stream-json` emits five untyped envelope kinds:

```jsonc
{"type":"stage","label":"evaluating the prompt","detail":"single"}
{"type":"text","delta":"..."}
{"type":"reasoning","delta":"..."}
{"type":"tool","phase":"start","name":"read_file"}
{"type":"tool","phase":"end","name":"read_file","ok":true,"durationMs":12}
{"type":"result","ok":true,"text":"done","steps":3,"model":"..."}
```

`AgentEvent` is a tagged enum of ~34 variants. The two do not line up, and the
overlap is inverted from what a golden replay needs:

| Reference event | Deserializes as `AgentEvent`? | Gap |
|---|---|---|
| `stage` | No | Carries a free-text `label`; the stage *kind* is dropped entirely. `AgentEvent::Stage` needs a typed `StageKind`. The two vocabularies also differ: the reference has `evaluate`/`enhance`/`route`/`revise`, this protocol has `ScopeReview`/`Witness`/`Verify`/`Reflect`/`ContextWrite`. |
| `text` | **Yes** | — |
| `reasoning` | **Yes** | — |
| `tool` (start/end) | No | No `call_id` on either phase, so `validate_stream`'s tool-pairing invariant is not merely unmet — it is *unrepresentable*. |
| `result` | No | The stream terminates with `result`; this protocol terminates with `complete`, which `validate_stream` requires to be last. |

The two kinds that already parse (`text`, `reasoning`) are exactly the ones
`event_signature` treats as structurally inert, and every event carrying
structural identity fails. A half-imported reference recording would therefore
be **all volatile content and no structure**: it would parse into something
shaped like a trajectory that asserts nothing. That is why the loader gated
instead of trusting a recording, and why
`crates/stella-pipeline/tests/reference_conformance.rs` pinned the gap
executably rather than leaving it as a comment that can rot. That test went with
the crate, so **the gap is now recorded here in prose only** — exactly the shape
it was written to avoid.

Note also that `structural_diff` is *positional* by design. Even with the wire
format fixed, a reference stream would have to align stage-for-stage with the
Rust stack's — and the reference emits no `Witness`, `ScopeReview`, `Reflect`,
or `ContextWrite` stage at all.

### The adapter

The adapter was `crates/stella-pipeline/src/replay/reference_adapter.rs`
(`adapt_reference_stream`, manifest id `replay::reference_adapter@v1`). It
discharged the obligations `reference_conformance.rs` stated — a typed
`StageKind` per known stage label, synthesized `ref-N` call ids with FIFO
pairing (the only order the id-less wire can support), and a terminal
`Complete` translated from `result` — and it **fails closed** on everything
else: an unmapped stage label, an unknown event kind, or an unpaired tool
phase is a typed error naming the line, never a guess. Extending the label
table is a reviewed change; that is where the fidelity of a reference
trajectory actually lives, which is why the manifest names the adapter and
its version.

The reference transmits no tool arguments, so `ToolCall::input` is an empty
object in adapted streams: a reference golden asserts stage order, tool
identity/pairing, and termination — not arguments. `structural_diff` is also
*positional*, and the reference emits no `Witness`, `ScopeReview`, `Reflect`,
or `ContextWrite` stage at all, so a reference golden was only comparable
against runs whose configuration skipped those stages (e.g. a run with a
supplied test command that never authored a witness — `--test-command` is
refused outright today, #3867).

### Provenance of reference recordings — the settled policy

The reference engine lives in a **private** repository and its one-shot path
routes model calls through a private platform (or a gateway API key), so a
public OSS repo's CI can never regenerate a reference fixture. The policy
(#462):

- **Reference recordings are committed opaque artifacts.** They are recorded
  by whoever can run the reference engine, adapted through the versioned
  adapter above, and checked in like any other fixture.
- **CI re-validates, never regenerates.** The loader's gates (manifest count,
  structural invariants, provenance kind) run on every load, so a committed
  reference golden is continuously proven well-formed even though it cannot
  be reproduced downstream.
- **Refresh is attributed, not automated.** A refreshed recording lands as an
  ordinary reviewed change whose manifest names the engine and adapter
  version that produced it; drift between adapter versions is visible in the
  manifest diff.

`STELLA_REFRESH_GOLDEN=1` and `make record-golden` were the *rust-stack* refresh
path only — a reference fixture could never be refreshed from this repo, by
construction. Both are gone; the policy above is the part that outlives them,
and applies unchanged to any verification plugin that commits opaque fixtures it
cannot regenerate downstream.
