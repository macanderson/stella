# loop-bench

An inexpensive turn-loop and context-query correctness harness over
Terminal-Bench. The full benchmark measures *pass rate* — expensive, dominated
by model quality. This measures what it hides and a **cheap** model exposes just
as clearly: did the turn execute real work, or abort having done nothing? did it
die without saying why? did `project_overview` / `graph_query` get used at all?

It shells out to the `stella` binary and `harbor`. Its only workspace dependency
is `stella-core`, for the A/B report shape `--compare` emits (see below); the
distillation half still needs nothing but `clap`, `serde`, and `serde_json`, so
a bench iteration stays cheap.

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
cargo run -p loop-bench -- --compare model-a,model-b --tasks fix-git --trials 6  # A/B
cargo run -p loop-bench -- --analyze-only --jobs-dir <dir> --job-name <name>   # free
```

Defaults: 4 tasks from `DEFAULT_POOL`, `openrouter/z-ai/glm-4.7-flash`,
`--budget 0.20` USD/task (passed on as `STELLA_BUDGET`), `--concurrent 4`,
`--dataset terminal-bench`, output under `loop-bench-jobs/loop-bench/`.
`--stella-binary` / `$STELLA_BINARY` names the uploaded binary; `--json` emits
the report for CI instead of the table, and `--json-out <path>` writes that same
report to a file while the table still goes to stdout.

A single run's JSON is an object — `{"trials": [...], "tally": {...},
"reconciliation": {...}}` — where it used to be the bare `trials` array
(#1299). The rows alone are not interpretable: `6 solved` means one thing over
eight requested trials and another over six, and an artifact carrying the rows
without the reconciliation is exactly the artifact that publishes the flattering
number. `--compare` still emits `ComparisonReport` verbatim (see below).

`stella tune effort` reads these files, and reads **both** shapes — A/Bing this
month's arm against an artifact from last month is the ordinary use of that
command, not an edge case. It also prints a note when the report's tally
reports crashes, because those trials count as failures in the A/B and a
promotion decided by them is a decision about the machine.

## Key concepts

For each trial dir `<jobs-dir>/<job-name>/<task>__<id>/`, `distill_events` reads
`agent/stella-events.jsonl`: `step_usage` counts model calls, `tool_start` tool
calls (`write_file` / `edit_file` / `apply_edits` are writes — never
`delete_file`, or a destructive loop would look productive; `project_overview` /
`graph_query` are context queries), and a terminal event is `complete` or an
`error` with `retryable: false`. The reward comes from `verifier/reward.txt`.

Four more files in the same directory are read, each closing a hole where
harbor knew something the harness did not (#1299) — see
[`src/artifacts.rs`](src/artifacts.rs):

| File | What it settles |
|---|---|
| `result.json` | `exception_info` — harbor's own record that the trial *raised*, which is the crash signal. Also `task_name` (the untruncated one) and, for a multi-step trial, the aggregated reward |
| `exception.txt` | the same message, for a trial that died before writing a structured record |
| `agent/stella-exit-cause.json` | the adapter's SIGKILL post-mortem (#1178): `oom-kill` vs `external-teardown`. Both exit `137`, and which one it was decides whether the fix is a memory limit or a timeout |
| `steps/<name>/agent/…` | a multi-step trial's per-step streams, folded into one row |

`TrialReport::loop_verdict` collapses all of that into one word, in this order:

| Verdict | Meaning | Gates red |
|---|---|---|
| `solved` | reward `1.0` — a solved task did the work by definition, so reward beats every other signal | no |
| `NOT-RUN` | requested, but harbor produced no trial dir for it | **yes** |
| `UNREADABLE` | the stream had lines, none of them an event — plumbing, not loop evidence | no |
| `STUCK-LOOP` | the engine's own `loop_detected` fired on a non-pass | **yes** |
| `BUDGET-CAP` | `STELLA_BUDGET` denied the turn — a cost decision, not a loop defect | no |
| `SILENT-DEATH` | zero tool calls *and* no terminal event — it vanished with no explanation, the worst mode | **yes** |
| `ZERO-WORK` | zero tool calls, but it said why | **yes** |
| `CRASHED` | real work happened, then harbor recorded the trial as having raised | no |
| `ran (unsolved)` | real work happened, the verifier said no — not a loop failure | no |

**The gate is loop health, not pass rate.** The process exits `1` if any trial's
`loop_broken()` matches, even when others passed.

`CRASHED` is the lowest-precedence verdict above `ran (unsolved)`, and that is
the whole design. It never displaces a red verdict: a trial that did nothing and
then died is still `SILENT-DEATH` and still red, with the crash printed on its
row as the explanation it previously lacked. So `CRASHED` only ever replaces
`ran (unsolved)` — which did not gate either, meaning the verdict costs the gate
nothing and buys the table a true statement. Naming a failure better must not
make the gate weaker.

It is also read off harbor's `exception_info`, never inferred from the stream's
shape: a turn that did its work and ended without a clean `complete` may simply
have exited on a step cap, and treating that as a death would invent crashes.

| Exit | Meaning |
|---|---|
| `0` | every trial that reported did real work (or passed) |
| `1` | the loop misbehaved on at least one trial |
| `2` | bad invocation: the task list resolved to nothing, or `--json-out` names an unwritable path (checked before anything is spent) |
| `3` | no trial artifacts at all — infrastructure, not a loop regression. Also fires when *every* row is `NOT-RUN`: harbor launching nothing is that same infrastructure failure, and it used to report as `1` |
| `4` | the loop was healthy but fewer than `--min-pass` trials passed |
| `5` | the run finished and the `--json-out` report could not be written (the JSON is dumped on stdout instead) |
| `6` | `--compare --require-winner` and no arm cleared both bars |
| `7` | the job dir holds trials this run did not ask for, so every figure covers a task set that is not the requested one |

`1` outranks `4`: when both fire, the loop failure is the actionable one, and a
broken loop explains the missing passes anyway. `1` outranks `6` and `7`
likewise — a broken loop makes an arm's numbers untrustworthy as a comparison.

A crash is deliberately **not** an exit code. The gate is loop health, and a
trial the machine killed is not evidence about the loop — the same reason
`UNREADABLE` and `BUDGET-CAP` do not gate. What it must never again be is
silent, or dressed as `ran (unsolved)`.

## Requested vs reported (#1299)

Harbor globs `-i` names and errors only when *nothing* matched, so a run that
asked for eight tasks and matched six used to produce six healthy rows and exit
`0`. Six of eight solved reads as 75% when it is 60%, and nothing on the page
said which number you were looking at.

Every run now reconciles what it asked harbor for against what came back, and
prints the disagreement under the table
([`src/reconcile.rs`](src/reconcile.rs)):

- **Missing trials** become `NOT-RUN` rows, at *trial* granularity — three dirs
  for a `--trials 5` task yields two `NOT-RUN` rows. The denominator is the
  sample that was asked for, and the gate goes red (`1`).
- **Surplus trials** — a second run's results sitting in the same job directory,
  since harbor writes to `<jobs-dir>/<job-name>/` verbatim — exit `7`. They
  render as ordinary healthy rows, so the reconciliation block is the only place
  a reader would ever learn the figures cover two runs.
- **Trial dirs no requested task claims** are still skipped, so a stale
  directory cannot contaminate this run's gate — but the skip is now *counted
  and named*, because a mistyped `--tasks` otherwise looks exactly like a run
  harbor launched nothing for.
- **A task name longer than 32 characters** reconciles. Harbor names a trial dir
  `<task_name[:32] rstripped of _->__<shortuuid>`, so comparing the untruncated
  name produced two wrong answers at once: every trial skipped as another run's,
  and the task that really ran reported `NOT-RUN`. Harbor's own `task_name` off
  `result.json` is preferred where it landed; the truncated prefix is the
  fallback.

Nothing here *adjusts* a count to make it consistent. It says what was asked
for, what came back, and where the two disagree.

`--analyze-only` reconciles nothing: it asked for nothing in particular, and
reads whatever a finished jobs dir holds.

## `--compare`: A/B two configs over one task set (#876)

`--compare model-a,model-b` runs the same tasks under each config and emits one
comparison instead of two tables. The **first** config named is the baseline;
`-m/--model` is ignored. Each arm gets its own job directory
(`<job-name>-arm<i>-<slug>`) so two runs' `<task>__<id>` dirs can never be
folded into the wrong arm.

```
task                            model-a      model-b   leader
────────────────────────────────────────────────────────────────
fix-git                             0/6          6/6   model-b
prove-plus-comm                     0/6          6/6   model-b
────────────────────────────────────────────────────────────────
model-a (model-a)        pass   0.0%  $   1.20      48000 tok   10.0 turns  n=12
model-b (model-b)        pass 100.0%  $   0.60      24000 tok    4.0 turns  n=12

WINNER: model-b — pass_rate lift +1.000 over `model-a` (z ∞, n 12/12)
  ✓ cost_usd      0.1000 →     0.0500  (+0.0500 vs tolerance 0.0000)
  ✓ tokens     4000.0000 →  2000.0000  (+2000.0000 vs tolerance 0.0000)
  ✓ turns        10.0000 →     4.0000  (+6.0000 vs tolerance 0.0000)
```

Three things about that output are load-bearing:

- **The report shape is `stella_core::comparison::ComparisonReport`, not a
  harness-local type.** The promotion gates that read it — an adapter (#836), a
  tuned knob (#831/#1065), an eval-gated skill (#1067) — have to be reading the
  same aggregates, the same guard set, and the same significance test that were
  applied here. Two report types would be two standards. `--json` emits that
  struct verbatim.
- **A winner clears two independent bars.** A confident lift on the primary
  metric (`--primary`, default `pass-rate`), decided by the same Welch-style
  test `stella tune` uses — *and* no regression on the guard metrics
  (`cost_usd`, `tokens`, `turns`, all at zero tolerance here). A candidate that
  wins pass rate by spending four times as much is reported as `BLOCKED`, with
  the lift and the price both on the record. The harness reports the refusal; a
  human decides whether to accept a price.
- **Only tasks every arm ran are counted.** A candidate that crashed on the two
  hardest tasks would otherwise raise its own pass rate by having its trials
  vanish. Unpaired tasks are named, and each arm reports how many trials the
  exclusion cost it.

`--trials N` (harbor's `-k`) is usually what a comparison needs: the default
sample floor is five trials per arm, so a two-task run at one trial each cannot
promote however cleanly the arms separate — it reports
`arm ... has 2 trial(s), 5 required`. `--require-winner` turns the comparison
into a gate (exit `6`); it is off by default because a comparison is a
measurement and "no winner" is a legitimate answer to it.

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
- A multi-step trial's rows are **folded**, not listed. Harbor relocates each
  step's logs into `steps/<name>/agent/` and removes the trial-root `agent/`,
  and the harness sums the counters into one row per trial. Two fields do not
  sum, and both choices are about not letting an early step vouch for a late
  one: `terminal_event` is the *last* step's (a trial that completed step one
  and vanished in step two ended by vanishing), and `zero_work` is recomputed
  from the summed tool calls (a step that only reads is normal). Step order
  comes from harbor's own `step_results`, not from sorting directory names.
- A multi-step trial's reward is harbor's **aggregate** off `result.json`, since
  there is no trial-root `verifier/` at all. Harbor folds the per-step rewards
  by the task's `multi_step_reward_strategy` (`last` or `mean`); taking its
  answer rather than inventing one here is what keeps the harness and the
  dataset agreeing on what the task scored.
- A crash is read from harbor's record, so a trial that died in a way harbor
  never noticed still reports as whatever its stream showed. The harness reports
  observations: "harbor recorded that this raised" is one, "the stream looks
  short so it probably died" is not.

## Testing

```bash
cargo test -p loop-bench        # no make target; `make test` covers it via --workspace
```

Pure unit tests in [`src/tests.rs`](src/tests.rs) (pulled in by `src/lib.rs` as
`#[cfg(test)] mod tests;`) feed JSONL to `distill_events` and assert the
verdict — no Docker, no harbor, no key. Each pins a real defect: batched
`apply_edits` reporting zero writes, an ellipsis past the column budget that
shifted a whole row, a retryable warning read as terminal, a *solved* run with
no `complete` event called silent, a stream of non-JSON stella output passing
for an unexplained silent death. A new signal means a `TrialReport` field, an
arm in `distill_events`, a `print_table` column (`TABLE_WIDTH`), and a test.

The #1299 tests build real trial directories under a `tempfile::tempdir` and run
`analyze` over them, because the defects they pin are about *files harbor wrote
that nothing read* — a fixture that hands `distill_events` a string could not
have caught any of them. They pin: a crash after real work reported as
`ran (unsolved)`; a crash before any work quietly downgrading a red
`SILENT-DEATH` to a green verdict (the regression the precedence order exists to
prevent); a task measured over the trials that survived rather than the ones
requested; a second run's trials folded into the first's figures; a 32-character
task name reconciling as both `NOT-RUN` and somebody else's directory; and a
multi-step trial read as empty.

[`src/compare/tests.rs`](src/compare/tests.rs) covers the A/B fold at the same
level: known outcomes producing the expected winner, a spend-bought win blocked
by the guard set, a broken-loop trial counted as a failure rather than dropped,
and a `NOT-RUN` row contributing nothing. The arithmetic underneath is tested in
`stella-core` (`src/comparison/tests.rs` and `props.rs`), which is where it
lives.

## See also

- [`../README.md`](../README.md) — benchmarking overview and claim-run rules
- [`../harbor_adapter/README.md`](../harbor_adapter/README.md) — the adapter this
  drives, and the `stella-events.jsonl` it writes
- [`../../AGENTS.md`](../../AGENTS.md) — "Essential commands / The gate"
