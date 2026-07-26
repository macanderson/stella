# stella-fleet

The multi-agent fleet layer: it takes a DAG of tasks, dispatches them wave by
wave to worker agents running in one shared repository tree (a dedicated git
worktree only where a plan opts in), and records every attempt, commit and
dollar in an embedded SQLite ledger. It is the engine behind `stella fleet`;
the command wiring, the real worker, and the `--budget` split live in
[`../stella-cli/src/fleet_cmd.rs`](../stella-cli/src/fleet_cmd.rs).

The boundary *is* the seam: subagent fan-out goes through exactly one API,
[`Fleet::dispatch`](src/fleet.rs) (L-E9). Nothing here spawns a process for an
agent ad hoc — hand-rolled per-call-site fan-out is what lost lineage and left
budgets uncounted in the TS era, so a caller gets claims, ledger rows, lineage
and budget metering or it does not get a worker. Everything external is a port
trait (`FleetWorker`, `GitCli`, `GhCli`, `Sleeper`, plus `stella_core::Clock`)
so every test runs against fakes; `git`/`gh` are shelled out to through
`tokio::process` rather than linking libgit2 (a deliberately-avoided heavy
native build); and there is no `unwrap`/`panic` outside tests.

## Where it sits

Depends on `stella-protocol` (`AgentEvent`, `PrStatus`, `CiStatus`),
`stella-core` (`BudgetGuard`, `Clock`), `stella-store` (the `file_locks` table
that backs task claims) and `stella-tools` (`subprocess_env` scrubbing for the
`git`/`gh` subprocesses). `stella-cli` is the only crate that depends on it,
and this crate builds no binary of its own. It deliberately does **not** depend
on `stella-model`: prompt-cache pricing and TTL *policy* stay on that side of
the boundary, and only the scheduling *heuristic* lives here
([`src/cache_schedule.rs:1`](src/cache_schedule.rs)).

## Layout

| File | What it holds |
|---|---|
| [`src/lib.rs`](src/lib.rs) | The five-pieces-one-seam overview and the crate's re-exports. Read it first. |
| [`src/plan.rs`](src/plan.rs) | `Plan`/`Task`/`Isolation` and the pure DAG scheduling: `validate`, `topological_order`, `ready_tasks`, cycle detection. No I/O, no async — open it to change how waves are formed or what a plan may declare. |
| [`src/fleet.rs`](src/fleet.rs) | `Fleet::dispatch` (the seam), `run_wave`, `run_plan`, the claim/control RAII guards, and the per-task pause/resume/stop verbs. The biggest file and the one that orders everything else. |
| [`src/ledger.rs`](src/ledger.rs) | `fleet.db`: schema, migrations, and every read/write of runs, tasks, attempts, commits, lineage and spend. |
| [`src/git.rs`](src/git.rs) | The `GitCli` port, `SystemGitCli`, and `WorktreeManager` — worktree create/remove/list plus the pathspec-only commit helper. |
| [`src/monitor.rs`](src/monitor.rs) | The `GhCli` port and `Monitor`: live PR reconciliation, the CI poll loop, the pure `decide` cap state machine, and the `AgentEvent` emit-shape helpers. |
| [`src/cache_schedule.rs`](src/cache_schedule.rs) | `warmest_first` — the pure, no-I/O ordering heuristic for equal-priority runnable sessions. |

## Key concepts

**The ledger hierarchy: run → task → attempt → commits/spend.** One
`.stella/private/fleet.db` per workspace holds `runs`, `tasks`, `attempts`,
`commits`, `spend` and `lineage`. An attempt row is opened *before* the worker
runs ([`ledger.rs:178`](src/ledger.rs)) so a crash mid-attempt still leaves a
row naming what was in flight; the closing half — outcome, commits and the
spend row — is written all-or-nothing in one transaction
([`Ledger::finish_attempt`, ledger.rs:197](src/ledger.rs)). Retries are extra
`attempts` rows, never extra lineage edges: `record_lineage` is idempotent per
`(parent_run_id, child_task_id)`. This is not a rebuildable cache like
`codegraph.db` — it is the authoritative record of a subagent's commits and of
real money spent, and nothing can reconstruct it once written. The in-memory
`BudgetGuard` is the *gate*; the ledger is the *record*, and dispatch writes
both.

**A fleet `run_id` is not an `execution_id` and not a session id.** It is the
top of the hierarchy above — one multi-agent fan-out — and it is owned by this
crate. `execution_id` is one row of `stella-store`'s `executions` table (one
goal/turn), and a session is one run of the CLI tracked under
`~/.stella/sessions/`. The three join correctly today, so this is a naming
hazard rather than a bug; see the *Glossary — the identifiers that look alike*
table in [`../AGENTS.md`](../AGENTS.md) before assuming two of them mean the
same thing. Note `task` is doubly loaded too: a fleet `TaskId` is a unit of
work in a run, while a `tasks` row in `stella-store` is the agent's own
task-board snapshot.

**What `dispatch` does, in order** ([`fleet.rs:463`](src/fleet.rs)): check the
aggregate parent budget; claim the task's declared paths; allocate the
workspace (a worktree only for `Isolation::Isolated`, otherwise the repo root);
record task + lineage + attempt-open; register the worker's control lines and
run it; meter the child's cost into the parent `BudgetGuard`; stamp the outcome
atomically. Metering precedes the stamp so that a ledger write which fails
cannot *also* drop a spend the worker has already made from the in-memory
gate — over-counting a lost row is the safe direction. The claim rows and the control registration are held by
`Drop`-scoped guards (`ClaimGuard`, [`fleet.rs:117`](src/fleet.rs);
`ControlGuard`, [`fleet.rs:146`](src/fleet.rs)) rather than released at a
statement, because a panicking worker or a dropped dispatch future skips the
statement — and `file_locks` rows are durable, so a missed release outlives the
process.

**Share by default, isolate on purpose.** `Isolation::SharedTree` is the
default: every worker runs in the one repository root, coordinated by
cooperative claims, so conflicts surface at write time with the rival named and
the build cache stays warm. `Isolation::Isolated` is the explicit opt-in for
genuinely divergent work (best-of-N, checkout-state mutation) and costs a cold
build cache plus conflicts deferred to integration time.

**Long waits are capped, not global.** `Monitor::watch_ci`
([`monitor.rs:606`](src/monitor.rs)) polls CI and extends the wait *only* on
fresh evidence (a changed run-set fingerprint, or a job actively in progress).
The arithmetic is a pure function, `decide` ([`monitor.rs:428`](src/monitor.rs)),
so the 2h cumulative cap, the 20m stall window and the 10m startup grace are
table-tested with an injected `Clock` instead of a real wait (L-E4).

## Gotchas

- **Fleet commits always name explicit pathspecs.** `WorktreeManager::commit_paths`
  ([`git.rs:413`](src/git.rs)) runs `git add -- <paths>` then
  `git commit -m <msg> -- <paths>`, never `-A`/`.`/`-a` and never a
  pathspec-less commit — a blanket add in a shared tree sweeps another worker's
  staged files, which is how work was lost in the TS monorepo. An empty
  pathspec is `WorktreeError::EmptyPathspec`; a pathspec-less commit is not
  expressible through this API.
- **`SystemGitCli` strips `GIT_DIR` and friends on every invocation**
  ([`git.rs:87`](src/git.rs)). Inside a git hook (the pre-push gate running
  `cargo test`) those exported variables silently override `-C` — it once
  rewrote the host repo's identity and committed test fixtures onto a real
  branch.
- **The pre-worker budget check is a snapshot, not a reservation.** Up to
  `max_concurrency` workers can pass it before any of them records spend, so
  worst-case overshoot is one in-flight window's cost. `fleet_cmd` divides the
  cap by the concurrency width before handing each child its own guard; a
  caller wiring this crate directly must do the same or accept the window.
- **`run_plan` never cancels in flight.** When a child trips an enforced
  parent budget the *remaining waves* are not launched, but running siblings
  settle — same "safe boundaries only" contract as the engine.
- **A failed task does not unblock its dependents**, and a dispatch error
  (worktree creation, ledger I/O) is recorded as that task's failure rather
  than an early return that would discard settled siblings' handles.
- **Two `stella fleet` runs in one workspace open the same `fleet.db`.** WAL
  plus `busy_timeout=5000` are set on every open
  ([`ledger.rs:140`](src/ledger.rs)) — without the timeout a second writer's
  `finish_attempt` fails after its worker already spent real money.
- **`Ledger` is not `Sync`.** The fleet holds it behind a `Mutex` and
  `finish_attempt` uses `unchecked_transaction`, which is sound *only* because
  that mutex is the borrow rusqlite would otherwise enforce and this is the one
  place a transaction is opened. A lock is never held across an `.await`.
- **An additive table or index is free; a *reshape* is not.** The base DDL is
  convergent (`CREATE … IF NOT EXISTS`, replayed on every open), so a new table
  reaches an existing ledger the next time it is opened. Altering or
  backfilling a column does not — the `IF NOT EXISTS` guard silently skips it
  on an existing file, which is exactly how a schema change becomes a runtime
  `INSERT` failure. That change must land as a numbered `MIGRATION_V<n>` with a
  matching `version < n` arm; `migrate` ([`ledger.rs:376`](src/ledger.rs))
  stamps `PRAGMA user_version` in the same transaction as the DDL it applies,
  the way `MIGRATION_V2` rebuilt `lineage` to add its uniqueness constraint.
- **Nothing removes worktrees or branches.** `WorktreeManager::remove` exists
  but neither `Fleet` nor `fleet_cmd` calls it; isolated worktrees under
  `.stella/worktrees/<slug>` and their `fleet/<slug>-<hash>` branches are left
  for review. The slug hashes the run scope *and* the task id, so re-running a
  plan with the same task ids does not collide with what the last run kept.
- **Some of this crate's surface has no product caller yet.**
  `WorktreeManager::remove`/`list`/`commit_paths` and the whole warmest-first
  path (`Fleet::with_cache_warmth`, `cache_schedule`) are exercised only by
  this crate's own tests — `fleet_cmd` never calls them. They are API and
  tests, not shipped behavior; treat their coverage as a contract for the
  wiring still to come, not as evidence the feature is live.
- **`WatchConfig::run_list_limit` (default 50) is a truncation point.** A
  branch with more CI runs than the limit can report `AllCompleted` while older
  runs go unobserved.

## Testing

```bash
cargo test -p stella-fleet
```

There is no `tests/` directory and no `make` target for this crate — every test
is an inline `#[cfg(test)]` module next to the code it covers. `plan.rs` is
proptested (acyclic-plan strategies assert that `topological_order` respects
every edge, that wave scheduling drains the plan, and that injected 2-cycles
are always detected). `fleet.rs`, `git.rs` and `monitor.rs` drive fakes through
the ports: a fake `FleetWorker`, a `GitCli`/`GhCli` that records argv and
returns canned `GitOutput`/`GhOutput`, and a `Sleeper` that advances an
injected `Clock` instead of sleeping — which is how the 2h cap is proven in
microseconds. `git.rs` also has real-`git` integration tests (worktree
isolation, branch-preserving removal) that seed a throwaway repo in a tempdir
and skip with a printed note when `git` is not on `PATH`.

## Extending it

Adding a ledger column, table or constraint — the case with a real footgun:

1. Add a `MIGRATION_V<n>` const in [`src/ledger.rs`](src/ledger.rs) holding
   only the new steps. Reshaping an existing table (a new column, a new
   constraint) needs the full rebuild dance `MIGRATION_V2` shows —
   `CREATE`/`INSERT … SELECT`/`DROP`/`RENAME`, plus recreating the indexes the
   drop took with it.
2. Add the matching `if version < n { … }` arm in `migrate` and bump
   `SCHEMA_VERSION` in the same commit.
3. Write the witness pair the existing tests model: a fresh ledger is stamped
   at the current version, and a ledger seeded with the *old* schema migrates
   in place without losing rows — `an_unversioned_ledger_migrates_in_place_without_losing_data`
   is the template. Downgrades are unguarded by design: a file stamped by a
   newer binary is opened as-is.
4. Keep the step **replay-safe** (`IF NOT EXISTS`, or a rebuild like
   `MIGRATION_V2`). `migrate` reads `PRAGMA user_version` *before* it opens its
   transaction, so two processes first-opening the same `fleet.db` can both
   decide to apply the same step. A step that would fail on a second
   application — `ALTER TABLE … ADD COLUMN` is the obvious one — needs that
   read moved inside an IMMEDIATE transaction first.

A new `git`/`gh` interaction goes on `WorktreeManager`/`Monitor` and through
the existing `GitCli`/`GhCli::run`, never as a fresh `Command` — that is what
keeps the env scrubbing, `-C` pinning and fake-driven tests in force. A new
kind of worker implements `FleetWorker` and is handed to `Fleet::new`; it must
honor its `WorkerControls` (park on the pause watch at a safe step boundary
only, treat a closed channel as "resumed"/"no stop coming").

## See also

- [`../AGENTS.md`](../AGENTS.md) — *Glossary — the identifiers that look alike*
  (why `run_id` is none of the other four ids) and *The `.stella/` directory*
  (where `fleet.db` sits and what else shares that directory).
- [`../stella-cli/src/fleet_cmd.rs`](../stella-cli/src/fleet_cmd.rs) — the only
  consumer: plan parsing, the real worker, the per-child budget split, and the
  `gh`-backed PR/CI pass.
- [`../website/content/docs/commands/fleet.mdx`](../website/content/docs/commands/fleet.mdx)
  — the user-facing `stella fleet` contract (flags, cleanup behavior).
- [`../website/content/docs/agent-fleets.mdx`](../website/content/docs/agent-fleets.mdx)
  — the fleet concept guide, including the plan-file schema.
- [`../docs/design/fleet.plan.toml`](../docs/design/fleet.plan.toml) — a real
  plan file that deserializes straight into `Plan`.
