# loop-bench

An inexpensive turn-loop and context-query correctness harness over
Terminal-Bench. The full benchmark measures *pass rate* — expensive, dominated
by model quality. This measures what it hides and a **cheap** model exposes just
as clearly: did the turn execute real work, or abort having done nothing? did it
die without saying why? did `project_overview` / `graph_query` get used at all?

It shells out to the `stella` binary and `harbor` with **no dependency on any
stella crate** (`clap`, `serde`, `serde_json` only), so it compiles in seconds
and never drags the workspace into a bench iteration. It is one file.

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
the report for CI instead of the table.

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
`zero_work && reward != Some(1.0)`; the process exits `1` if any trial matches —
even when others passed, and also when no trial artifacts were found at all.
Exit `2` means the task list resolved to nothing.

## Gotchas

- A trial whose event stream is missing or unreadable is **reported**, not
  skipped: it lands as `zero_work` with a `no event stream` error. Skipping it
  made a launch failure look like a clean run with fewer rows, gate still green.
- `reward` is `Option<f64>`, and `None` is not zero — a trial that never reached
  the verifier must not read as a verifier failure.
- A non-positive or non-finite `--budget` denies the very first model call, so
  every task reports as a loop failure: the harness manufacturing the signal it
  gates on. It warns rather than proceeding silently.
- `$STELLA_BINARY` must be a **Linux** build; the task containers are amd64.
  Unset, it warns and falls back to `target/release/stella`.

## Testing

```bash
cargo test -p loop-bench        # no make target; `make test` covers it via --workspace
```

Seven pure unit tests at the bottom of [`src/main.rs`](src/main.rs) feed JSONL to
`distill_events` and assert the verdict — no Docker, no harbor, no key. Each
pins a real defect: batched `apply_edits` reporting zero writes, an ellipsis
past the column budget that shifted a whole row, a retryable warning read as
terminal, a 177-tool *solved* run called silent. A new signal means a
`TrialReport` field, an arm in `distill_events`, a `print_table` column
(`TABLE_WIDTH`), and a test.

## See also

- [`../README.md`](../README.md) — benchmarking overview and claim-run rules
- [`../harbor_adapter/README.md`](../harbor_adapter/README.md) — the adapter this
  drives, and the `stella-events.jsonl` it writes
- [`../../AGENTS.md`](../../AGENTS.md) — "Essential commands / The gate"
