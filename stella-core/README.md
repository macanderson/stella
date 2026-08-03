# stella-core

The step-driver. `Engine::run_turn` takes a message history, a budget guard and
an event channel, and runs the model/tool loop to an answer: one model call per
step, retry+backoff, context compaction, tool-output eviction, loop detection,
USD metering. Alongside it live the workspace's other decision engines — rules,
skills, routing, the goal loop, the task board, discovery ranking — which the
CLI drives directly rather than through a turn.

**No I/O.** This crate never imports a provider SDK, never touches the
filesystem, never spawns a process, never opens a socket. Anything needing the
outside world is a trait the caller implements: `ToolExecutor`, `Clock`,
`TurnGate`, `TurnSteering` ([`src/ports.rs`](src/ports.rs)), `Sleeper`
([`src/retry.rs`](src/retry.rs)), `HookRunner` ([`src/hooks.rs`](src/hooks.rs)),
`RuleSource` ([`src/rules.rs`](src/rules.rs)), `SkillSource`
([`src/skills.rs`](src/skills.rs)) — plus `Provider` from `stella-protocol`.
Even the working directory is passed in (`EngineConfig::cwd`) rather than read
from `std::env`. That is what makes compaction, eviction, loop detection and
budget arithmetic plain synchronous functions over owned data, testable against
fakes without a runtime. The one documented exception is reading a clock:
[`src/bus.rs`](src/bus.rs) reads `SystemTime` to timestamp events and `Instant`
to enforce the per-handler latency budget.

## Where it sits

Depends on exactly one workspace crate — `stella-protocol` (types) — plus
`serde`, `tokio` (`sync` + `time` only), `async-trait`, `futures-util`, `rand`,
`sha2` and `serde_json_canonicalizer`. Note what is absent: no `regex`, no
HTTP client, no SQLite. `stella-cli`, `stella-tools`, `stella-mcp`,
`stella-pipeline`, `stella-fleet` and `stella-serve` depend on it. Library only
— it builds no binary.

## Layout

| File | What it holds |
|---|---|
| [`src/lib.rs`](src/lib.rs) | Module list and the crate's re-export surface. Read the `pub use` block to see what callers are meant to touch. |
| [`src/driver.rs`](src/driver.rs) | `Engine`, `EngineConfig`, `TurnOutcome`, `run_turn`. The one file that sequences every other module against real I/O. Start here. |
| [`src/driver/settlement.rs`](src/driver/settlement.rs) | The between-steps budget check and `BudgetTick`/warning emission, split out of the step loop. |
| [`src/ports.rs`](src/ports.rs) | The port boundary: `ToolExecutor`, `ReadOnlyTools`, `Clock`, `TurnGate`, `TurnSteering`. |
| [`src/budget.rs`](src/budget.rs) | `BudgetGuard` — USD spend against a turn and/or session cap. Returns `BudgetOutcome`; aborts nothing itself. |
| [`src/compaction.rs`](src/compaction.rs) | `compact()` — dedup, supersession, aging, eviction. Open when the conversation is being rewritten wrongly. |
| [`src/estimator.rs`](src/estimator.rs) | Conservative token estimate plus `Calibration`/`CalibrationMap`, the per-model drift correction fed by reported usage. |
| [`src/loop_detect.rs`](src/loop_detect.rs) | `detect_loop()` — exact repeats and short cycles over `CallRecord`s. |
| [`src/retry.rs`](src/retry.rs) | `RetryPolicy`, backoff computation, `retry_with_backoff*`, and the `Sleeper` port. |
| [`src/speculation.rs`](src/speculation.rs) | Early execution of read-only tool calls announced mid-stream (`pub(crate)`). |
| [`src/receipts.rs`](src/receipts.rs) | `ReceiptLedger` — `BlockRegistered` + `StepManifest` context receipts, content-free (digests, never payloads). |
| [`src/accounted_call.rs`](src/accounted_call.rs), [`src/event_sender.rs`](src/event_sender.rs) | A one-shot accounted provider call for callers that are not the engine, and the channel wrapper that can journal an event durably before admitting it. |
| [`src/bus.rs`](src/bus.rs) | The in-process extension bus: observers (`emit`) and policy hooks (`emit_blocking`) over a dotted event-name catalog. |
| [`src/hooks.rs`](src/hooks.rs) | Settings-declared *shell* hooks — `SessionStart`/`PreToolUse`/`PostToolUse` matching and blocking decisions. |
| [`src/rules.rs`](src/rules.rs), [`src/rules/metadata.rs`](src/rules/metadata.rs) | Rules engine: loading, precedence merge, Tier-1 rendering, Tier-2 `evaluate_guards`, candidate mining, metadata parse/render. |
| [`src/skills.rs`](src/skills.rs) | Skills engine: `SKILL.md` loading, `select_skills`, `render_skills_section`, auto-creation mining, install vocabulary. |
| [`src/glob.rs`](src/glob.rs), [`src/mining.rs`](src/mining.rs), [`src/summarize.rs`](src/summarize.rs) | Non-public shared helpers: the `*`-only glob matcher behind rule guards and hook matchers; the lexical mining primitives the rules and skills miners share (they were once two byte-identical copies); the overflow summarizer's prompt and span rendering. |
| [`src/discovery.rs`](src/discovery.rs) | The ranker behind `tool_search`/`skill_search`/`mcp_search`: `select:` lookups, `+required` terms, field-weighted scoring. |
| [`src/extensions.rs`](src/extensions.rs) | Custom commands and agents parsed from markdown, plus `plan_extension_sync` for adopting `.claude/`/`.agents/` definitions. |
| [`src/subagent.rs`](src/subagent.rs) | `Engine::run_sub_agent` — a bounded child turn with its own carved budget and its own (discarded) transcript, returning only a capped summary. `goal.rs`'s judge is one. |
| [`src/goal.rs`](src/goal.rs) | The goal loop: worker turn → judge verdict → feedback, bounded by round cap, budget and turn abort. The judge runs as a sub-agent. |
| [`src/router.rs`](src/router.rs) | Role → model resolution over a caller-supplied `ProviderProfile`, plus the per-provider circuit breaker. |
| [`src/tasks.rs`](src/tasks.rs) | `TaskBoard` — the transition rules behind the `task_*` tools; records `SpawnRequest`s rather than spawning. |
| [`src/mcp_usage.rs`](src/mcp_usage.rs) | The MCP usage record/ledger types, homed here so `stella-mcp` and `stella-tools` need no edge between them. |
| [`src/context_record.rs`](src/context_record.rs) + [`src/context_record/`](src/context_record) | The adaptive-context Phase 1 value layer: taxonomy enums, scope, temporal, canonical `record_hash`, context-use, contract, outcome, representation. Types and validators only. |

## Key concepts

**The step loop is a fixed phase sequence.** `run_turn`
([`src/driver.rs:673`](src/driver.rs)) iterates up to `EngineConfig::max_steps`,
and each step runs the same phases in the same order: pause gate → drain
steering / check soft stop → budget check → snapshot tool-result identities →
compaction pass → loop detection → model call (wrapped in retry+backoff) →
committed-step bookkeeping → dispatch. Each phase is one sub-method
(`run_compaction_pass`, `check_loop_detection`, `run_model_call`,
`dispatch_completion`). The order is load-bearing, not stylistic: identities are
snapshotted *before* compaction because the compaction pass rewrites tool
results in place and loop detection then runs on the rewritten history in that
same step (#554). The engine holds no conversation state — `messages` is
borrowed `&mut`, so the caller owns persistence and can inspect history after an
abort.

**Everything that ends a turn ends it at a step boundary.** Budget aborts, the
pause gate, the soft stop and loop-detection aborts all fire between steps,
never mid-tool — killing a tool in flight leaves the workspace and the model's
view of it inconsistent, a defect this codebase already shipped once in its
TypeScript era. [`src/budget.rs`](src/budget.rs) is deliberately unable to abort
anything: `BudgetGuard::evaluate`/`record_spend` return a
`BudgetOutcome::AbortTurn` *recommendation*, and `check_budget`
([`src/driver/settlement.rs`](src/driver/settlement.rs)) turns it into
`TurnOutcome::Aborted`. The one place the engine may interrupt a running tool is
`EngineConfig::tool_timeout` (15 minutes by default), and it surfaces as a
`ToolOutput::Error` the model can route around — a tool result, not a turn abort.

**A sub-agent is a child turn whose transcript is thrown away.**
`Engine::run_sub_agent` ([`src/subagent.rs`](src/subagent.rs)) builds a second
`Engine` over the parent's fields, runs one turn against a *local*
`Vec<CompletionMessage>`, and returns a character-capped summary. The value is
context economy, not parallelism (`stella fleet` already has that): work the
parent would otherwise carry — and re-send on every later step for the rest of
the session — is absorbed by a transcript that goes out of scope. The budget is
**carved**, `min(requested, parent headroom)` via `BudgetGuard::carve`, so
`--budget` stays a hard ceiling once turns nest, and the child's spend settles
back into the parent exactly once on every path (`settle_child`) — including a
failure, because a child that aborted still spent what it spent. Failure is a
typed `SubAgentOutcome`, never an `Err` that kills the parent turn, and an
aborted child salvages its last answer rather than losing paid work with the
context it lived in. Tools default to read-only, which is also the *fast*
default: read-only calls engage the speculation pump, so a read-only child hides
its I/O behind generation.

**Exactly-once is a guarantee about mutating tools only.**
`retry_with_backoff_observed` wraps the model call together with that attempt's
speculation pump. A mutating tool call runs *outside* the retried closure, after
a model call has already succeeded, so a retried step structurally cannot
re-execute it — proved by `retry_never_re_executes_a_tool_call`
([`src/driver/tests.rs:1194`](src/driver/tests.rs)), which counts real executions
against a flaky scripted provider. Read-only calls carry no such guarantee: they
run inside the retried closure and inside speculation, so one can execute several
times in a step. Do not assume a `read_only` tool with an observable side channel
(a rate-limited API, a write to `codegraph.db`) runs at most once.

**Speculation is fenced by the first mutating call.** Dispatch runs every
mutating call as its own barrier in call order, so a read-only call appearing
*after* a mutation must observe it. Calls stream in order, so
[`src/speculation.rs`](src/speculation.rs) permanently stops speculating for the
rest of the step at the first non-read-only call it sees — only the all-read-only
prefix is ever run early. A speculative result is harvested only when the
committed call is byte-identical (id, name, input) to what was announced;
anything else is discarded and reported as an
`AgentEvent::SpeculationDiscarded` so the I/O it already ran stays accountable
(#370).

**Compaction's four mechanisms pull in opposite directions on purpose.**
`compact()` ([`src/compaction.rs:203`](src/compaction.rs)) applies, least-lossy
first: dedup of byte-identical tool outputs, supersession of re-run calls, aging
(middle-out truncation of old large outputs), then eviction. Dedup keeps the
**earliest** copy — byte-identical content is position-independent, so stubbing
the later ones keeps the provider's prompt-cache prefix byte-identical (#372).
Supersession keeps the **latest**, because it is about staleness, not
duplication. They look like the same rule with a sign error; they are not. The
system message and the latest user message are never touched.

## Gotchas

- **Two unrelated things are called `HookEvent`.** `hooks::HookEvent` is the
  settings-declared shell-hook lifecycle enum; `bus::HookEvent` is the
  extension-bus envelope. Only the first is re-exported at the crate root; the
  second stays module-qualified on purpose ([`src/lib.rs:41`](src/lib.rs)).
- **Loop detection ignores `ToolCall::call_id`.** `ToolCall` derives `PartialEq`
  over all fields including the id, which providers mint fresh per call — using
  derived equality here would mean the detector silently never fires.
  `same_record` in [`src/loop_detect.rs`](src/loop_detect.rs) is the one place
  that distinction is made, comparing name + input + output.
- **A repeat is only a loop when the *output* repeats too**, and compaction
  rewrites those outputs in place. Identical input with changing output is
  legitimate work (polling a process, re-running a shrinking test suite);
  callers that can preserve what a call really produced pass it as
  `CallRecord::identity` rather than let comparison see rewritten bytes.
- **Bus observers must return `Err`, never panic.** `catch_unwind` contains a
  panic only under an unwinding profile, and the workspace `release` profile sets
  `panic = "abort"` ([`../Cargo.toml`](../Cargo.toml)).
- **`run_session_start_hooks` is not called by `run_turn`.** `SessionStart` is a
  session-level event and `run_turn` runs many times per session; the caller
  invokes it once and folds the output into the system prompt it builds.
- **`Engine::with_sleeper` is the only *public* constructor,** and it cannot
  carry `gate`/`steering`/`hooks` — those are builder-set private fields. A
  nested turn built through it silently drops all three, which is the bug
  `goal.rs::assess` shipped with. Do not hand-roll a child engine: call
  [`src/subagent.rs`](src/subagent.rs)'s `run_sub_agent`, which constructs the
  child in-crate and carries every seam. The crate still exports the `Sleeper`
  port with no production implementation — wiring a real one is the binary's
  job, and tests wire a no-op to run retries at zero wall-clock cost.
- **A sub-agent's steering is filtered, not inherited.** `drain_steering` is
  destructive by contract, so a child that inherited the parent's `TurnSteering`
  would swallow a message the user addressed to the parent. `ChildSteering`
  forwards the (non-destructive, latched) soft stop and returns nothing for the
  drain.
- **`context_record` is not `stella-context`,** and it is not the live rules
  path either: `rules::metadata` still drives that. The two coexist by decision —
  do not merge them, and read the subsumption table in
  [`src/context_record.rs`](src/context_record.rs) before assuming a mapping is
  ratified (two edges are explicitly flagged as unratified).

## Testing

```bash
make test-core           # cargo test -p stella-core
make watch-core          # re-run on save (needs cargo-watch)
```

There is no `tests/` directory: every test is an inline `#[cfg(test)]` module in
the file it covers, which is why the engine's own suite is split across
[`src/driver/tests.rs`](src/driver/tests.rs) and
[`src/driver/tests/`](src/driver/tests) (`audit_fixes.rs` holds the 2026-07
turn-driver audit witnesses; also `budget_boundaries.rs`,
`usage_completeness.rs`). `proptest!` blocks live in
[`src/retry.rs`](src/retry.rs), [`src/loop_detect.rs`](src/loop_detect.rs),
[`src/tasks.rs`](src/tasks.rs) and [`src/skills.rs`](src/skills.rs); past
failing seeds are committed under
[`proptest-regressions/`](proptest-regressions). No feature flag, no env var, no
fixture server and no network — driver tests wire scripted `Provider`s, counting
`ToolExecutor`s and no-op `Sleeper`s, so the suite runs in seconds. Keep it that
way: a test here that needs a file or a socket means the logic under test is in
the wrong crate.

## Extending it

**Adding an engine capability that needs the outside world:**

1. Define the trait next to the decision it serves — [`src/ports.rs`](src/ports.rs)
   for engine-wide seams, otherwise module-local like `rules::RuleSource`.
2. Attach it with a `with_*` builder on `Engine` that stores an `Option`, so an
   engine built without it takes exactly the old path
   (`with_hooks`/`with_calibration`/`with_gate`/`with_steering` are the pattern).
3. Implement it in `stella-cli` or `stella-tools`. Never here.
4. Test it in-crate against a fake implementation.

**Adding a compaction mechanism:** extend `compact()`
([`src/compaction.rs`](src/compaction.rs)) in least-lossy-first order, add the
counters to `CompactionReport`, and add a test beside
`never_evicts_the_most_recent_tool_result` — plus check
`dedups_identical_outputs_keeping_the_earliest` and
`repeated_identical_call_supersedes_older_differing_results` still pass, since
they pin the two opposing retention directions.

**Adding a reason a turn can end:** return `Option<TurnOutcome>` from a check
called at the top of the step loop in `run_turn`, next to `check_budget` and
`check_loop_detection` — never from inside `dispatch_completion`, or you have
built the mid-tool abort this crate exists to prevent.

## See also

- [`../AGENTS.md`](../AGENTS.md) — "Architecture: ports, not concretions"
  (invariants 1, 2 and 6 are this crate's contract) and "Testing approach".
- [`../stella-protocol`](../stella-protocol) — `Provider`, `AgentEvent`,
  `CompletionMessage`, `ToolCall`/`ToolOutput`. [`../stella-tools`](../stella-tools)
  has `ToolRegistry`, the production `ToolExecutor`;
  [`../stella-cli`](../stella-cli) wires the remaining ports.
- [`../website/content/docs/agent-engine-paths.mdx`](../website/content/docs/agent-engine-paths.mdx)
  — every caller that drives `run_turn`;
  [`../website/content/docs/configuration/agent-engine-config.mdx`](../website/content/docs/configuration/agent-engine-config.mdx)
  — the user-facing knobs behind `EngineConfig`.
- [`../docs/design/session-telemetry-receipts-spec.md`](../docs/design/session-telemetry-receipts-spec.md)
  — the receipt schema [`src/receipts.rs`](src/receipts.rs) emits.
