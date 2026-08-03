# loop-bench

An inexpensive turn-loop and context-query correctness harness over
Terminal-Bench. The full benchmark measures *pass rate* — expensive, dominated
by model quality. This measures what it hides and a **cheap** model exposes just
as clearly: did the turn execute real work, or abort having done nothing? did it
die without saying why? did `project_overview` / `graph_query` get used at all?

It shells out to the `stella` binary and `harbor` with **no dependency on any
stella crate** (`clap`, `serde`, `serde_json` only), so it compiles in seconds
and never drags the workspace into a bench iteration.

> Wider benchmarking context — the Harbor adapter, the standalone SWE-bench
> harness, the zero-cost smoke test, claim-run rules — lives in
> **[`../README.md`](../README.md)**. Start there.

## Running it

Run it **from the workspace root**: harbor loads `stella_harbor:StellaAgent` by
import path via the relative `PYTHONPATH` entry `bench/harbor_adapter` — wrong
cwd gives a warning here, an opaque per-trial `ImportError` otherwise.

```bash
cargo run -p loop-bench -- --n 4                    # first 4 tasks of the default pool
cargo run -p loop-bench -- --tasks fix-git,prove-plus-comm -m openrouter/z-ai/glm-5.2
cargo run -p loop-bench -- --analyze-only --jobs-dir <dir> --job-name <name>   # free
```

Defaults: 4 tasks from `DEFAULT_POOL`, `openrouter/z-ai/glm-4.7-flash`,
`--budget 0.20` USD/task (passed on as `STELLA_BUDGET`), `--concurrent 4`,
`--dataset terminal-bench`, output under `loop-bench-jobs/loop-bench/`.
`--stella-binary` / `$STELLA_BINARY` names the uploaded binary; `--json` emits
the report for CI instead of the table, and `--json-out <path>` writes that same
report to a file while the table still goes to stdout.

## Key concepts

For each trial dir `<jobs-dir>/<job-name>/<task>__<id>/`, `distill_events` reads
`agent/stella-events.jsonl`: `step_usage` counts model calls, `tool_start` tool
calls (`write_file` / `edit_file` / `apply_edits` are writes — never
`delete_file`, or a destructive loop would look productive; `project_overview` /
`graph_query` are context queries), and a terminal event is `complete` or an
`error` with `retryable: false`. The reward comes from `verifier/reward.txt`.

`TrialReport::loop_verdict` collapses that into one word, in this order:

| Verdict | Meaning |
|---|---|
| `solved` | reward `1.0` — a solved task did the work by definition, so reward beats every other signal |
| `SILENT-DEATH` | zero tool calls *and* no terminal event — it vanished with no explanation, the worst mode |
| `ZERO-WORK` | zero tool calls, but it said why |
| `ran (unsolved)` | real work happened, the verifier said no — not a loop failure |

**The gate is loop health, not pass rate.** `loop_broken()` is
`zero_work && reward != Some(1.0)`; the process exits `1` if any trial matches,
even when others passed.

| Exit | Meaning |
|---|---|
| `0` | every trial that reported did real work (or passed) |
| `1` | the loop misbehaved on at least one trial |
| `2` | bad invocation: the task list resolved to nothing, or `--json-out` names an unwritable path (checked before anything is spent) |
| `3` | no trial artifacts at all — infrastructure, not a loop regression |
| `4` | the loop was healthy but fewer than `--min-pass` trials passed |
| `5` | the run finished and the `--json-out` report could not be written (the JSON is dumped on stdout instead) |

`1` outranks `4`: when both fire, the loop failure is the actionable one, and a
broken loop explains the missing passes anyway.

## The nightly CI gate (#873)

[`.github/workflows/nightly-bench.yml`](../../.github/workflows/nightly-bench.yml)
is the only CI job that runs Stella itself against real tasks. It cross-builds
the SUT to the glibc 2.17 floor (a runner-glibc binary cannot exec in the
`bullseye` task images, and harbor scores that as the agent failing the task),
asserts portability, then runs a **pinned** task list on a pinned flash-tier
model — the harness has no RNG, so that list *is* the seed.

`--min-pass` is the pass-rate floor, and the workflow ships it at `0`
(disabled). It is a real gate, not a placeholder: raise it once a few nights of
the uploaded report agree on what normal is. Set by intuition instead, on a
flash-tier model under a $0.20 cap, it is red every night for a reason no loop
fix can address — and the loop-health gate above runs unconditionally either
way.

## Gotchas

- A trial whose event stream is missing or unreadable is **reported**, not
  skipped: it lands as `zero_work` with a `no event stream` error. Skipping it
  made a launch failure look like a clean run with fewer rows, gate still green.
  A stream that exists but holds no parseable event (stella's plain-text
  startup complaint, a truncated upload) says so on its row rather than looking
  like an unexplained silent death.
- `reward` is `Option<f64>`, and `None` is not zero — a trial that never reached
  the verifier must not read as a verifier failure. Only harbor's
  `verifier/reward.txt` is read, not its `reward.json` alternative; a
  `--dataset` whose verifier writes the JSON form under-credits every trial.
- A `--budget` that is non-finite, non-positive, **or smaller than the `0.0001`
  the cap is transmitted at** denies the very first model call, so every task
  reports as a loop failure: the harness manufacturing the signal it gates on.
  It warns rather than proceeding silently.
- `$STELLA_BINARY` must be a **Linux amd64** build; that is what the task
  containers run. Unset, it warns, and the adapter then resolves `stella` on
  `PATH` before `target/release/stella` — on a dev machine the `PATH` hit is a
  host build the container cannot execute.
- Requested tasks are **not reconciled** against reported rows. Harbor globs
  `-i` names and only errors when *nothing* matched, so a run that asked for
  eight tasks and matched six shows six healthy rows and exits `0`. Check the
  row count against `--n`. Multi-step datasets are likewise unsupported: their
  logs move to `steps/<name>/agent/` and every trial reads as "no event stream".
- A crash *mid*-work is not a verdict of its own: `SILENT-DEATH` requires zero
  tool calls, so a turn that did work and then vanished reports as
  `ran (unsolved)`.

## Testing

```bash
cargo test -p loop-bench        # no make target; `make test` covers it via --workspace
```

Seventeen pure unit tests in [`src/tests.rs`](src/tests.rs) (pulled in by
`src/lib.rs` as `#[cfg(test)] mod tests;`) feed JSONL to `distill_events` and
assert the verdict — no Docker, no harbor, no key. Each pins a real defect:
batched `apply_edits` reporting zero writes, an ellipsis past the column
budget that shifted a whole row, a retryable warning read as terminal, a
*solved* run with no `complete` event called silent, a stream of non-JSON
stella output passing for an unexplained silent death. A new signal means a
`TrialReport` field, an arm in `distill_events`, a `print_table` column
(`TABLE_WIDTH`), and a test.

## See also

- [`../README.md`](../README.md) — benchmarking overview and claim-run rules
- [`../harbor_adapter/README.md`](../harbor_adapter/README.md) — the adapter this
  drives, and the `stella-events.jsonl` it writes
- [`../../AGENTS.md`](../../AGENTS.md) — "Essential commands / The gate"
