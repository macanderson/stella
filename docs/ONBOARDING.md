---
id: onboarding
title: "Start here — Stella for new engineers"
status: living
---

# Start here

This is the one document to read before your first PR. It has three parts:

1. **Principles** — the rules every change follows, in plain language.
2. **Schema** — the shapes of the data Stella keeps: Rust types and SQL tables.
3. **ADR index** — the decisions that got us here, and why.

Everything here is a summary. The code is always the real answer — this
document just tells you where to look and what to expect when you get there.

---

## Part 1 — Principles

### The one-sentence version

Stella is a coding agent written in Rust that is trying to be the best-built
open-source agent there is. "Best-built" means: correct, boring, and it
never claims a proof it doesn't have.

### Six rules that shape every PR

1. **No shortcuts, or say so out loud.** An `unwrap` on real data, a silenced
   warning with no reason, a `TODO` instead of a real design — each of these
   is a bug, not a style choice. If the right fix is too big for right now,
   file a GitHub issue and say so in the PR.

2. **Claims need proof.** "This works" is not a sentence you get to write
   without a test that shows it — one that fails before your change and
   passes after (a **witness test**). If you cannot test something (like a
   piece of terminal UI), say exactly how you checked it by hand instead.

3. **Engines don't touch the outside world.** The core decision-making code
   (`stella-core`) never opens a file, calls a network, or spawns a process
   directly. It talks through small interfaces ("ports") like
   `Provider` (talk to a model) and `ToolExecutor` (run a tool). This is
   what makes the core logic testable without touching a network.

4. **Nothing leaves this computer unless a person chose that.** By default
   Stella sends zero telemetry anywhere except the model provider the user
   picked. This is enforced by a checked allowlist in code, not just a
   promise in a doc.

5. **Every tool does one job.** `bash` runs shell. `edit_file` edits. There
   is no tool with a hidden "mode" flag that turns it into a different tool
   in disguise (like a `delete=true` flag on an edit tool). If a capability
   needs a second verb, it gets a second tool.

6. **Nothing gets lost.** If you find a bug, a missing test, or a loose end
   while working and you don't fix it right now, you must file a GitHub
   issue for it before you're done. A problem that only lives in your head
   or a chat log doesn't count as handled.

### How a change gets checked

- You write code and a **test that proves it** (the witness test).
- `make gate` is the full checklist — formatting, lint, tests, and dozens of
  small rule-checkers ("guards") that catch things like an unpinned GitHub
  Action, a file that got too big, or a doc link that points nowhere.
- **CI runs the gate, not your laptop.** Push your branch and let the GitHub
  Actions workflow build and test it. Only small, targeted local checks
  (one crate, one test) are fine to run yourself.
- A red gate means "not ready yet," full stop — it is never overridden by
  skipping the check.

### The shape of the codebase, in one paragraph

Stella is a Cargo **workspace**: one repository, many small crates, each with
one job (see the Schema section below for the full map). The engine
(`stella-core`) plans and runs turns. Providers (`stella-model`) talk to
Anthropic, OpenAI, and friends. Tools (`stella-tools`) are things the agent
can do, like editing a file. Everything is glued together by the CLI
(`stella-cli`), which is the program a user actually runs.

### Where to read more

- `AGENTS.md` (repo root) — the full, detailed version of everything above:
  every gate step, every workspace crate, every architectural rule, spelled
  out with the history of why each rule exists.
- `CLAUDE.md` (repo root) — the same spirit, aimed at an AI coding agent
  working in this repo.
- Each crate's own `README.md` — what that one crate owns and how to extend
  it safely.

---

## Part 2 — Schema: what Stella actually stores

Stella's data comes in two shapes: **Rust types** (what a running program
passes around in memory) and **SQL tables** (what gets saved to disk in
SQLite files). Both are summarized here. **The code is the source of
truth** — these are landmarks, not the full definition.

### 2.1 The shared vocabulary — six things that sound alike

These six words are used constantly and mean different things. Mixing them
up is the single most common confusion for a new engineer.

| Term | What it actually is | Lives in |
|---|---|---|
| **session** | One run of the CLI program, start to finish. | `stella-store` (`SessionRecord`) |
| **execution** | One row in the `executions` table: one goal/turn, its cost, its outcome. | `stella-store` |
| **turn** | One trip through the model+tools loop, inside one execution. | `stella-protocol` (`turn_instance`) |
| **step** | One iteration inside a turn: one model call plus the tools it asked for. | `stella-protocol` (`step`, `call_seq`) |
| **fleet run** | One multi-agent fan-out (several agents working at once). | `stella-fleet` (`run_id`) |
| **task** | Either: one job dispatched inside a fleet run, or one row on the agent's own visible to-do list. | `stella-fleet` / `stella-store` |

### 2.2 The Rust type layer (`stella-protocol`)

`stella-protocol` is the crate that defines the *shapes* every other crate
agrees on — no logic, no file or network access, just types that convert
cleanly to and from JSON. A few of the ones you'll see constantly:

| Type | What it represents |
|---|---|
| `AgentEvent` (in `event/kind.rs`) | Every kind of thing that can happen during a run — a stage starting, a file changing, a tool finishing. About 40 options; every one is required to declare **who reads it** (see `event/consumers.rs`) so nothing is emitted into the void. |
| `ToolCall` / `ToolResult` / `ToolOutput` (`tool.rs`) | A request to run a tool, and what came back — success, or a typed `ErrorClass` (never a raw string). |
| `Role` / `ModelRef` (`role.rs`) | Who is "speaking" (user, assistant, tool) and which model answered. |
| `ModelCallRole` (`event/call_role.rs`) | Which job a particular model call was doing (e.g. the worker, a reflection pass, a triage classification). |
| `HookEvent` (`hook.rs`) | The points in a turn where a hook (user-defined automation) is allowed to run. |
| `LadderRung` / `ProofTree` / `OracleObservation` (`ladder.rs`) | The shape of a verification decision — was a claim proven, and how. |
| `ContextUsage` / `ManifestEntry` (`receipt.rs`) | What went into the model's context window this turn, so it can be audited later. |

To see every type: `crates/stella-protocol/src/`. To see how a wire format
turns into a document, `docs/wire/` holds the generated schema pages.

### 2.3 The SQL layer — four separate SQLite databases

Stella never uses one giant database. Each concern gets its own file under
`.stella/private/`, and they never share foreign keys with each other:

| Database | Crate that owns it | What it holds |
|---|---|---|
| `store.db` | `stella-store` | Executions, events, telemetry, tasks, memories — the main operational record of what Stella did. |
| `context.db` | `stella-context` | Recallable memories, embeddings, episodes — what the agent can remember and search. |
| `codegraph.db` | `stella-graph` | The tree-sitter code index built by `stella init`. |
| `fleet.db` | `stella-fleet` | Multi-agent fan-out runs: which task went to which worker, and what it cost. |

Plus one **global** database outside any single project:
`~/.stella/usage.db` — a cross-project rollup of billing/usage data, synced
from every project's own `store.db`.

#### `store.db` — the main tables (`crates/stella-store/src/ddl.rs`)

| Table | One-line purpose |
|---|---|
| `executions` | The spine: one row per goal/turn — prompt, provider, model, cost, outcome. Every other table below hangs off `executions.id`. |
| `events` | The full event stream (see `AgentEvent` above), persisted. |
| `telemetry` | Token counts and cost, per model call. |
| `files_touched` | Which files an execution edited. |
| `memory_citations` | Which saved memories were read into a given turn. |
| `rules` | This repo's own steering policy records (`.stella/rules/*.toml`), mirrored here. |
| `mcp_usage`, `skill_usage`, `agent_uses` | Usage counters for MCP servers, skills, and sub-agents. |
| `tool_calls` | Every tool call made, with its result. |
| `reflections`, `execution_reflection` | Post-turn lessons the agent wrote about its own run. |
| `tasks` | The agent's own visible to-do list (what the terminal UI's task panel shows). |
| `pull_requests` | GitHub PRs opened or touched by a run. |
| `context_blocks` | What was assembled into the model's context this turn (an audit trail). |
| `step_manifest`, `step_receipt` | Per-step bookkeeping: what was asked for, what came back. |
| `foundry_tools` | Tools generated or registered dynamically. |
| `session_turn_diffs` | The diff produced by each turn, for replay/review. |

#### `context.db` — retrieval tables (`crates/stella-context/src/store/schema.rs`)

| Table | One-line purpose |
|---|---|
| `node`, `edge` | The code/knowledge graph — things and the relationships between them. |
| `embedding`, `embedder_fingerprint` | Vector embeddings and which embedding model produced them. |
| `episode` | Episodic memory — things that happened, in order. |
| `memory` | Durable saved memories, promoted from reflections. |
| `domain`, `node_domains`, `edge_domains` | The domain taxonomy used to tag and filter recall. |
| `context_records` | Published context-steering records (see ADR 0011/0012 below). |
| `ann_centroid`, `ann_assignment`, `ann_index_state` | The approximate-nearest-neighbor index over embeddings. |

#### `fleet.db` — multi-agent ledger (`crates/stella-fleet/src/ledger.rs`)

| Table | One-line purpose |
|---|---|
| `runs` | One row per fan-out (`run_id`). |
| `tasks` | One row per unit of work dispatched inside a run. |
| `attempts` | One row per try at a task (a task can be retried). |
| `commits` | Git commits produced by an attempt. |
| `lineage` | Which attempt/task produced which other one. |
| `spend` | Cost tracking per run/task/attempt. |
| `dispatch_claims` | A lease table so two fleet loops can't claim the same work twice. |

Full column-level DDL for every table above lives in the `ddl.rs` /
`schema.rs` files cited in each heading — that is the one place the *current*
shape is written down, and migrations keep it in sync automatically.

---

## Part 3 — ADR index

An **ADR** (Architecture Decision Record) is a short, permanent note that
says: here is a decision we made, here is why, and here is what it rules
out. ADRs don't get edited to "fix" an old decision — a new one supersedes
it instead, so the history never gets rewritten.

Stella's ADRs live in `docs/adr/` and cover the adaptive-context /
context-record system (how durable steering and memory work). Read
[`adr/README.md`](adr/README.md) for the full ADR process; this table is the
index.

| # | Title | Status | What it decided |
|---|---|---|---|
| [0001](adr/0001-semantic-taxonomy.md) | Semantic Taxonomy | living | The vocabulary for classifying context records. |
| [0002](adr/0002-scope-vs-sharing.md) | Scope vs. Sharing | — | How a record's visibility (who can see it) is separated from its scope (what it applies to). |
| [0003](adr/0003-bitemporal-semantics.md) | Bitemporal Semantics | — | Records track both "when it was true" and "when we recorded it," separately. |
| [0004](adr/0004-record-revision-identity.md) | Record Revision Identity | — | How a record keeps a stable identity across edits/revisions. |
| [0005](adr/0005-storage-authority.md) | Storage Authority | — | Which store is the authoritative copy when the same fact could live in more than one place. |
| [0006](adr/0006-contextframe-vs-compiledcontextframe.md) | ContextFrame vs. CompiledContextFrame | implemented | Splits the *proposed* context shape from the *compiled, ready-to-send* one. |
| [0007](adr/0007-immutable-promotion-history.md) | Immutable Promotion History | living | A record's promotion history is append-only — never rewritten. |
| [0008](adr/0008-markdown-canonical-rules.md) | Markdown Repository Rules Remain Canonical | — | Superseded in part by 0011; kept for the historical reasoning. |
| [0009](adr/0009-enum-freeze-resolutions.md) | Enum-Freeze Resolutions for Flagged Phase-1 Decisions | implemented | Locks down a set of enum values that early design work had left open. |
| [0010](adr/0010-incremental-authority-transfer.md) | Incremental Authority Transfer | living | How authority over a record moves between systems gradually, not in one jump. |
| [0011](adr/0011-context-records-are-toml.md) | Context Records Are TOML | implemented | Context records are TOML files in Git, not database-only rows — supersedes 0008's surface decision. |
| [0012](adr/0012-context-record-field-schema.md) | The Context-Record Field Schema, and Records-Live-in-Files | implemented | The exact field schema for a context record, and that files (not just DB rows) are canonical. |
| [0013](adr/0013-session-artifact-boundary.md) | The Session Artifact Boundary | proposed | Where a session's own artifacts stop and shared/durable state begins. |
| [0014](adr/0014-memories-join-the-record-control-plane.md) | Memories Join the Context-Record Control Plane | proposed | Memories (`.stella/memories/*.md`) become part of the same governed record system as context records. |

**Reading order for a newcomer:** 0001 → 0011 → 0012 gives you the current
shape fastest; the rest fill in *why* it ended up this way.

---

## Keeping this document accurate

This file is a map, not the territory. When code changes the schema or a
principle, update this file in the same PR — a stale map is worse than no
map. If you're deleting or renaming something this file points at, run
`make doc-links` to make sure nothing else silently broke.
