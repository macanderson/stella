# Golden replay trajectories

How the golden-trajectory fixtures are recorded and refreshed, and what a
**reference** engine has to do before its runs can join them.

Machinery: `stella-pipeline/src/replay.rs` (`validate_stream`,
`structural_diff`, `streams_equivalent`) and
`stella-pipeline/src/replay/golden.rs` (the fixture format and its load gates).

---

## Two kinds of recording, deliberately not interchangeable

A golden's manifest names its `source`, and the distinction is load-bearing:

| Source | What it is | What it proves |
|---|---|---|
| `rust_stack` | Recorded from this workspace's own pipeline | **Drift baseline.** Catches a stage that stopped being emitted, a tool that changed name, an event that moved or vanished. Both sides are the same code, so it is not independent evidence. |
| `reference` | Recorded from another engine through an adapter that emits this protocol | **Reference trajectory.** The only kind that answers "does the Rust stack agree with the reference implementation?" |

Encoding this in the fixture rather than in prose is what stops a baseline from
being cited later as a reference. `RecordingSource::is_reference()` is asserted
in `stella-pipeline/src/pipeline/tests/golden.rs`.

Today every committed golden is a `rust_stack` baseline. No `reference`
recording exists — see [The reference-engine gap](#the-reference-engine-gap).

---

## The fixture format

Two files per task under `stella-pipeline/tests/fixtures/golden/`:

- `<task_id>.jsonl` — one `AgentEvent` per line, the same wire format
  `stella run --output-format stream-json` emits.
- `<task_id>.manifest.json` — `task_id`, a one-line `description`, the
  `source`, and `event_count`.

`event_count` is not decoration. `parse_jsonl` deliberately tolerates a torn
final line (L-T1) — correct for a live reader recovering from a crashed writer,
and exactly wrong for a committed fixture, where it would silently hand back a
recording one event short and weaken every assertion made against it. The count
turns that into a loud `GoldenError::Truncated`.

`GoldenTrajectory::load` additionally refuses:

- a recording that is not parseable as an `AgentEvent` stream
  (`GoldenError::Recording`) — this is what a foreign wire format produces;
- a manifest whose `task_id` disagrees with the file it was loaded as
  (`GoldenError::TaskIdMismatch`);
- a recording that violates the protocol's own structural invariants
  (`GoldenError::NotWellFormed`). A golden is the yardstick other runs are
  measured against; an ill-formed one would license ill-formed runs.

---

## Recording and refreshing

```bash
make record-golden        # re-record every golden from the current code
```

or directly:

```bash
STELLA_REFRESH_GOLDEN=1 cargo test -p stella-pipeline --lib golden
```

The recorders live in `stella-pipeline/src/pipeline/tests/golden.rs`. Each
drives the real `Pipeline` over scripted model/test ports and records the
stream it actually emits — deterministic, no API key, runnable in CI, which is
the only reason a recording can be asserted on every `cargo test` instead of
refreshed by hand and hoped over.

**A non-empty fixture diff is a change to the observable event contract.**
Review it as one. If the change is intended, commit the refreshed fixture with
the change that caused it; if it is not, you have found a regression.

### Adding a task

1. Add a `#[tokio::test]` to `stella-pipeline/src/pipeline/tests/golden.rs`
   that drives the pipeline over scripted ports and ends in `check_golden`.
2. Run `make record-golden` to write the fixture.
3. Read the recorded `.jsonl`. It is evidence — confirm it is the flow you
   meant to capture.
4. Keep the set discriminating: `the_recorded_flows_are_structurally_distinct`
   fails if two goldens collapse into the same trajectory, because a golden set
   whose members are indistinguishable passes no matter what the pipeline does.

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
shaped like a trajectory that asserts nothing. That is why the loader gates
instead of trusting a recording, and why
`stella-pipeline/tests/reference_conformance.rs` pins the gap executably rather
than leaving it as a comment that can rot.

Note also that `structural_diff` is *positional* by design. Even with the wire
format fixed, a reference stream would have to align stage-for-stage with the
Rust stack's — and the reference emits no `Witness`, `ScopeReview`, `Reflect`,
or `ContextWrite` stage at all.

### The adapter

The adapter is `stella-pipeline/src/replay/reference_adapter.rs`
(`adapt_reference_stream`, manifest id `replay::reference_adapter@v1`). It
discharges the obligations `reference_conformance.rs` states — a typed
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
or `ContextWrite` stage at all, so a reference golden is only comparable
against runs whose configuration skips those stages (e.g. a `--test-command`
run that never authors a witness).

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

`STELLA_REFRESH_GOLDEN=1` and `make record-golden` remain the *rust-stack*
refresh path only — a reference fixture can never be refreshed from this
repo, by construction.
