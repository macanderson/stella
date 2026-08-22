---
id: oxagen-trace-drain
title: "Draining Stella traces into Oxagen — the full-fidelity egress surface"
status: proposed
---

# Draining Stella traces into Oxagen

**Status:** proposed, 2026-08-22. Nothing in this document is built. It maps a
surface that exists (Stella's local trace) onto a surface that exists
(Oxagen's three storage planes) and names the one piece that does not exist
between them (a content-bearing drain).

**How this document was produced, and what that is worth.** Every claim below
about Stella is read from this workspace's source at the commit this branch
forked from, and cites the file. Every claim about Oxagen is read from
`~/Projects/oxagen-platform` at its then-current checkout, and cites the file.
**No drain was run, no intake was exercised, and no schema was migrated** —
this is a reading of two codebases, not an observation of a working pipe. The
mapping tables are therefore *proposals with citations*, and the column-level
mismatches in §6–§8 are the parts most likely to be wrong in the direction of
optimism. They are called out individually rather than smoothed over.

---

## 1. The question, answered

> Does Stella have a way for Oxagen to be fed the full trace for every agent
> execution?

**No — and the absence is deliberate, enforced, and load-bearing.**

Three egress paths exist today. All three are either **content-free by
construction** or **local-only**:

| Path | Ships | Content? | Built? |
|---|---|---|---|
| Cloud drain (`format = "stella"` / `"otel"`) | one row per *model call*: identity + tokens + cost + duration | **No** — enumerated field set, guarded | Yes — `crates/stella-cli/src/cloud_drain.rs` |
| Enterprise operational rollup | one row per *execution*: outcome + tokens + cost + counts | **No** — narrower still, catalog-collapsed | Yes — `crates/stella-store/src/enterprise_telemetry.rs` |
| `/export` session archive | prompts, tool arguments, touched files — the real thing | **Yes** | Yes, but **local only**: a `0600` file on disk, no transport |

The full trace **does** exist, completely, on every machine that runs Stella.
It has two representations (§2). What does not exist is a pipe that carries it
off the box, because `AGENTS.md` invariant #3 ("zero telemetry egress by
default") makes building one an explicit, reviewed decision rather than a
feature increment.

So the work is **not** "instrument Stella". The event model, the published
wire schema, the ordered durable sink, the batching/cursor/poison-row loop and
the HTTPS transport are all already built and tested. The work is a **fourth
drain**: a content-bearing, consent-gated trace format that reuses that
machinery, plus the intake and mapping on Oxagen's side that §6–§8 specify.

---

## 2. What Stella already produces

### 2.1 The event journal — the trace proper

`AgentEvent` (`crates/stella-protocol/src/event.rs`) is the single
representation of everything an execution does. Its variant table lives in
`crates/stella-protocol/src/event/tags.rs`, which generates three things from
one list — the wire tag, the known-tag set, and the signal-consumer ledger —
so a variant cannot exist without declaring its tag.

The tags, in table order:

`stage` · `text` · `text_delta` · `reasoning` · `tool_start` · `tool_result` ·
`speculation_discarded` · `retry` · `steered` · `turn_parked` · `turn_woken` ·
`loop_detected` · `budget_denied` · `retries_exhausted` · `policy_decision` ·
`compaction` · `budget_tick` · `step_usage` · `usage_incomplete` ·
`goal_verdict` · `provider_fallback` · `file_change` · `context_recall` ·
`context_write` · `block_registered` · `step_manifest` · `proof` · `verdict` ·
`scope_review` · `hunk_review` · `ask_user` · `media_progress` ·
`media_complete` · `commit` · `pr` · `task_update` · `sub_agent` ·
`candidate_delivery` · `error` · `turn_complete` · `run_complete`

Plus `AgentEvent::Unknown`, which preserves an unrecognized tag verbatim so an
older reader never silently drops a newer producer's event.

**The contract is published, generated, and committed.** `docs/wire/` holds
`agentevent.schema.json` (JSON Schema 2020-12 over the whole payload graph) and
`agentevent.d.ts` (the same contract as TypeScript declarations), both derived
from the Rust types by `stella_protocol::schema_export` and gate-enforced by
`make wire-schema`. **Oxagen should generate its intake types from these
artifacts rather than hand-writing them** — that is what they are for, and it
is the difference between a wire break landing as a reviewable diff and landing
as a production decode failure.

> **Cardinality warning, from the source.** `AgentEvent::type_tag` returns the
> *preserved original* tag for `Unknown`, which means its return value is not
> drawn from a closed set — it is arbitrary, externally authored text. Any
> ClickHouse `LowCardinality(String)` column or metric label fed from it must
> be validated against `KNOWN_TYPE_TAGS` and bucket everything else as a single
> `unknown` cohort. Letting a foreign stream drive cardinality on
> `events.event_type` would degrade the whole partition.

### 2.2 The two taps

**Tap A — the durable event stream (live, per-process).**
`--output-format stream-json` emits every `AgentEvent` as newline-delimited
JSON. Setting `STELLA_DURABLE_STREAM_JSON_PATH` additionally routes the stream
through an ordered durable writer to a file
(`crates/stella-cli/src/agent/output.rs`). It is strict in both directions: the
env var without `--output-format stream-json` is a startup error, the path is
preflighted before the run begins, and a write failure exits `74` rather than
letting a run proceed unrecorded. The Harbor bench rig already uses this at
`/logs/agent/stella-events.jsonl`.

This is the highest-fidelity tap and the cheapest to consume — a tail of one
file, already in the published schema.

**Tap B — the store (durable, queryable, post-hoc).**
`.stella/private/store.db` (SQLite, `crates/stella-store/src/ddl.rs`). The
`events` table is the ordered append-only journal —
`(execution_id, seq, ts, event_type, payload)`, `UNIQUE (execution_id, seq)` so
a double-write is an error rather than a corrupted replay. Beside it sit the
normalized projections a dashboard can query without JSON-scanning the log:

| Table | Grain | Notes |
|---|---|---|
| `executions` | one goal/turn | the spine; `kind` = door, `pipeline_variant` = wrapper, `role` = system-call role |
| `telemetry` | one committed model call | `UNIQUE (execution_id, step)`; tokens, cache triple, cost, duration, retries |
| `tool_calls` | one tool call | written **live**, in the same transaction as the event; `state ∈ running/ok/error/abandoned` |
| `step_manifest` | one (step, block) in wire order | what the model actually saw — makes any past step reconstructable |
| `step_receipt` | one model call | which model served it, and the budget the compaction pass compared against |
| `context_blocks` | one attributable context unit | content-addressed; stores a digest, not the bytes |
| `files_touched` | one path per execution | `lines_added` / `lines_removed` / ordered op log |
| `mcp_usage`, `skill_usage`, `agent_uses` | one call / load / invocation | per-surface attribution |
| `tasks`, `pull_requests`, `session_turn_diffs` | board state, PR state, per-turn diff | |
| `reflections`, `execution_reflection`, `memory_citations` | the agent's own assessment | |
| `foundry_tools` | self-authored tool adoptions | carries the witness that proved it |

Tap B is richer than Tap A for anything the engine *derived* (the live
`tool_calls` lifecycle, the reconstructable manifest); Tap A is richer for
ordering and for events that never reach a projection. **A serious ingest wants
both**, and §9 recommends starting with A.

---

## 3. What egresses today, and why none of it is a trace

### 3.1 The cloud drain

`~/.stella/usage.db` (the cross-project telemetry hub) stages org-scoped rows;
`drain_org` (`crates/stella-store/src/drain.rs`) pages them, and
`HttpCloudIntake` (`crates/stella-cli/src/cloud_drain.rs`) POSTs each batch to
the endpoint in `~/.stella/cloud.json`'s `drain` block. It is opt-in by
construction — no `org_id` **and** no `drain` block means zero network I/O.

The wire row, `DrainRow`, is the entire payload, and this is its complete field
set:

```
org_id · workspace_id · repo_id · project_id          (identity)
source_rowid · recorded_at                            (addressing / idempotency)
provider · model
input_tokens · output_tokens
cache_read_tokens · cache_miss_tokens · cache_write_tokens
cost_usd · duration_ms · retries · tool_calls · usage_complete
```

No prompt. No tool argument. No path. No event. The batch envelope carries an
integer `schema_version` and the intake accepts a *range*; an unknown version
is a terminal non-poison reject, distinguished from a poison *row* so one bad
row dead-letters instead of wedging an org's cursor forever.

`project_id` deserves a note the source already makes: it is an FNV-1a/64
digest of the canonical workspace path, and it is **a pseudonym, not an
anonymization** — 64 bits of non-cryptographic hash over a guessable input is
dictionary-attackable by a determined intake operator. Oxagen is that operator.
Treat it as identifying.

### 3.2 The enterprise operational rollup

`StellaOperationalEventV1` is narrower again — one row per *execution*:
`schema`, `event_class`, `event_id`, `enrollment_id`, `organization_id`,
`workspace_id`, `provider`, `model`, `outcome`, `duration_ms`, `input_tokens`,
`output_tokens`, `cost_microusd`, `tool_call_count`, `changed_file_count`,
`produced_output`. Provider and model collapse to the literal `"other"` when
the pair is outside the managed catalog, and a rollup with incomplete paid-call
accounting is refused rather than exported. It requires a signed org-managed
document and an active enrollment.

### 3.3 The guard that makes both of those true

`crates/stella-store/src/content_free.rs` is why §3.1 and §3.2 can be stated as
fact rather than intent. Two halves:

1. **A schema allowlist.** `HUB_TELEMETRY_COLUMNS` is compared against the live
   `PRAGMA table_info`, so adding a hub column fails the build until a human
   edits the allowlist and answers "is this content?".
2. **A sentinel harness.** Every encoder that puts bytes on the network
   implements `ContentFreeEncoder`. The harness builds poisoned fixtures —
   every content-bearing or local-only source field stamped with a sentinel
   (`CONTENT_SENTINEL`, `PATH_SENTINEL`, and the two local-identity UUIDs) —
   and fails the encoder if a sentinel reaches the wire, if an unreviewed key
   appears, or if the encoder did not actually consume the poisoned fixture
   (`PASSTHROUGH_MARKER` proves it did, so no encoder can pass vacuously).

`DRAIN_FORMATS` maps every drain format discriminator to its guard; a format
marked `NotYetBuilt` is a declared gap visible in source, and building it
without registering a guard fails the gate.

> ### The consequence for this project, stated plainly
>
> **A full-trace drain cannot be added to the existing encoder registry.** It
> would fail `every_registered_encoder_is_content_free` by design, because it
> ships exactly what the sentinels forbid. Widening the allowlist to make the
> gate green is precisely the expedient `CLAUDE.md` names as a defect.
>
> The correct shape is a **separate, declared egress class** —
> content-bearing, consent-gated — with its own guard asserting its own
> invariants (that it never runs without explicit consent, that redaction ran,
> that the endpoint is the enrolled one). That is an architecture decision for
> a maintainer, not an implementation detail, and §12 records it as an open
> question rather than deciding it here.

---

## 4. Identity — the hard part, and the part most likely to break

Stella's identifiers and Oxagen's do not line up, and the mismatch is not
cosmetic. `AGENTS.md`'s glossary already warns that six Stella ids can each be
read as "one thing the agent did" and are genuinely distinct entities.

| Stella | Type | Scope of uniqueness | Oxagen wants |
|---|---|---|---|
| `executions.id` | `INTEGER AUTOINCREMENT` | **per workspace store only** | `uuid` globally unique |
| `session_id` | `TEXT` (session registry id) | per machine | `uuid` |
| `turn_instance` | `u32` | per session | — |
| `(step, call_seq)` | `(usize, u32)` | per turn | `uuid` step id |
| `org_id` / `workspace_id` | `TEXT` from `cloud.json` / `workspace.json` | org-assigned | `uuid` |
| `repo_id` | `TEXT` | org-assigned | — |
| `project_id` | FNV-1a/64 digest of canonical path | global, collision-prone, reversible | — |
| fleet `run_id` | `TEXT` | per fleet ledger | — |

**`executions.id` is the trap.** It is a per-file autoincrement. Two workspaces
on two machines both have an `execution_id = 41`, and they are unrelated. An
intake that keys on it directly will silently merge distinct executions the
first time a second workspace reports.

**Proposal — deterministic UUIDv5 derivation at the drain boundary.** Mint an
Oxagen-shaped id from a namespace UUID plus a canonical string, so the same
local row always derives the same global id and re-sends are idempotent without
a server-side lookup:

```
execution_uuid = uuidv5(NS_STELLA_EXECUTION, "{org_id}/{workspace_id}/{repo_id}/{project_id}/{execution_id}")
step_uuid      = uuidv5(NS_STELLA_STEP,      "{execution_uuid}/{turn_instance}/{step}/{call_seq}")
tool_call_uuid = uuidv5(NS_STELLA_TOOLCALL,  "{execution_uuid}/{event_seq}")
session_uuid   = uuidv5(NS_STELLA_SESSION,   "{org_id}/{workspace_id}/{session_id}")
```

Three notes on that, each a real constraint rather than a preference:

- **`event_seq`, not `call_id`, keys a tool call.** `call_id` is *not unique
  within an execution* — several providers mint it positionally, so
  `read_file:0` names the first read of every response in a turn. Stella's own
  store was keyed on it until v28 and one observed execution projected 4 rows
  from 176 calls. `tool_calls.event_seq` (the position of the announcing
  `tool_start` in the events stream) is the identity; `-1` means unknown, which
  must map to a minted-per-row id, never to a shared one.
- **Derivation must happen in Stella, not Oxagen.** The drain's idempotency
  contract (`(workspace_id, source_rowid)` today) depends on the client being
  able to re-send after a lost ack and land in the same place. Deriving on the
  intake works too, but only if the derivation is byte-identical on both sides
  forever — one more shared cell to drift.
- **The identity fields must be present at all.** `org_id` and `workspace_id`
  are `NULL` until `stella cloud register` runs. A trace drain therefore has
  the same registration precondition as the existing one, and the same honest
  failure mode: unregistered installs drain nothing.

---

## 5. The three planes, and what belongs in each

Oxagen already stores agent executions three ways, and the split is principled:

| Plane | Store | Grain | Retention | Role |
|---|---|---|---|---|
| **Postgres** | `agent.*` schema (Drizzle, RLS, `orgScopeMixin`) | durable spine, one row per execution / step / tool call | indefinite | system of record, audit, joins |
| **ClickHouse** | `packages/telemetry/src/schema.sql` | high-volume append-only | 90 / 180 / 365 day TTLs | analytics, dashboards, cost |
| **Neo4j** | `packages/ontology` (`neo4j-driver`) | entities + relationships | indefinite | lineage, provenance, semantic recall |

Stella's trace splits along the same seam almost exactly, which is the reason
this mapping is cheap rather than a translation layer:

- `events` (the raw journal) → **ClickHouse**. High volume, append-only,
  bounded retention, queried in aggregate.
- `executions` / `step_receipt` / `tool_calls` → **Postgres**. Low volume, one
  row per real thing, needs RLS, needs joins, needs to outlive a TTL.
- `files_touched` / `commit` / `pr` / `sub_agent` / `skill_usage` /
  `memory_citations` → **Neo4j**. These are edges, and Oxagen already has the
  edge types for most of them.

---

## 6. Mapping — ClickHouse (`@oxagen/telemetry`)

Existing tables read from `packages/telemetry/src/schema.sql` and its numbered
migrations.

### 6.1 The event journal → `events`

| Stella (`store.db.events` / stream-json line) | ClickHouse `events` | Note |
|---|---|---|
| — | `event_id UUID` | mint `uuidv5(NS, "{execution_uuid}/{seq}")` — idempotent re-send |
| derived (§4) | `org_id UUID`, `workspace_id UUID` | |
| `event_type` (`AgentEvent::type_tag`) | `event_type LowCardinality(String)` | **validate against `KNOWN_TYPE_TAGS`**; bucket the rest as `unknown` |
| — | `source_system LowCardinality(String)` | constant `'stella'` — this column is why the table can host a second producer |
| `(execution_id, seq)` | `stream_offset Nullable(String)` | `"{execution_uuid}:{seq}"` — the replay cursor, already a nullable string |
| `payload` (the event's JSON) | `payload String CODEC(ZSTD(3))` | **this is the content-bearing column** |
| `ts` | `emitted_at DateTime64(3)` | |

The fit is exact and needs no migration. Two decisions ride on it:

- **The 90-day TTL is almost certainly wrong for the stated purpose.** The ask
  is "aggregation for reporting *and audit*". `events` drops rows after
  `INTERVAL 90 DAY`. Either the audit-grade subset lives in Postgres/Neo4j
  (§7, §8) with the ClickHouse copy as the analytics mirror, or this table
  needs a longer TTL for `source_system = 'stella'`. **Decide before ingest,
  not after** — a TTL cannot retroactively un-drop a partition.
- **`payload` is where redaction has to happen**, because it is where the
  prompt text, tool arguments and file diffs actually live. §10.

### 6.2 Per-call usage → `token_usage`

| Stella (`telemetry` row / `step_usage`) | ClickHouse `token_usage` | Note |
|---|---|---|
| `step_uuid` (§4) | `execution_step_id UUID` | **non-nullable and in the sort key** — a standalone role call (`reflection`, `skill_author`) with no step must use `NIL_UUID`, per the column's own comment and migration 0012 |
| derived | `org_id`, `workspace_id` | |
| `model`, `provider` | `model`, `provider` | |
| `input_tokens`, `output_tokens` | same | |
| `cache_read_tokens` | `cached_tokens UInt64` | **lossy — see below** |
| `cache_miss_tokens` | *no column* | ⚠️ |
| `cache_write_tokens` | *no column* | ⚠️ |
| `cost_usd REAL` | `cost_usd_micros UInt64` | multiply by 1e6; Stella's enterprise encoder already does this conversion (`finite_nonnegative_microusd`) — reuse its rounding, do not invent a second one |
| `duration_ms` | `duration_ms UInt32` | |
| `call_role` | `capability_name LowCardinality(String)` | or a new column — see §12 |
| — | `surface` | constant `'stella'` |
| `retries` | *no column* | ⚠️ |
| `estimated_input_tokens` | *no column* | deliberately local — the drift-analysis estimate, explicitly excluded from the existing drain contract |

> **⚠️ Migration required.** Stella measures the prompt cache as a *triple*
> (`read` / `miss` / `write`) and ClickHouse stores one `cached_tokens`.
> Collapsing to `cache_read_tokens` loses the write cost, which is the half
> that is actually *billed* on Anthropic and the half a cache-efficacy
> dashboard needs. Stella treats provider cache posture as a declared parity
> axis for exactly this reason (`AGENTS.md` invariant #8, `CachePosture`).
> **Add `cache_write_tokens` and `cache_miss_tokens` columns** rather than
> flattening. `retries` likewise has no home and is a genuine reliability
> signal.

### 6.3 Tool calls → `tool_invocations`

| Stella (`tool_calls` / `tool_start`+`tool_result`) | ClickHouse `tool_invocations` | Note |
|---|---|---|
| `tool_call_uuid` (§4) | `invocation_id UUID` | |
| `name` | `capability_name LowCardinality(String)` | |
| `step_uuid` | `execution_step_id Nullable(UUID)` | nullable here, unlike `token_usage` |
| `state` | `status LowCardinality(String)` | ⚠️ `abandoned` has no counterpart |
| `args_json` length | `input_size_bytes UInt32` | shape, not content |
| `bytes_out` | `output_size_bytes UInt32` | |
| `duration_ms` | `latency_ms UInt32` | |
| `error_class` | `error_class Nullable(String)` | Stella's `snake_case` `ErrorClass` token maps directly; `''` means *unaudited*, which is **not** a class and must not be read as "our bug" |
| `surface` (`native`/`mcp`) | `surface LowCardinality(String)` | |
| MCP server id | `external_server_id Nullable(UUID)`, `external_provider` | from `mcp_usage.server` |
| — | `risk_level`, `required_approval` | Oxagen policy fields; Stella has `policy_decision` events that could feed them |

> **⚠️ `abandoned` is a real state and dropping it makes error rates
> dishonest.** Stella split it from `error` in #3146 precisely because every
> error-rate reader groups on this column, and charging an interrupt as an
> "error" against whatever tool was in flight made those rates lie. Oxagen's
> `agent_tool_calls_status_check` allows `pending/running/completed/failed`;
> the ClickHouse column is unconstrained but its dashboards assume the same
> four. **Add `abandoned` on both sides**, or Stella's traces will import a
> defect Stella already fixed.

### 6.4 The rest

| Stella | ClickHouse | Note |
|---|---|---|
| `text`, `reasoning`, `error` events | `execution_logs` | `log_level` from the variant (`error` → `error`, else `info`); `message` is content-bearing |
| `skill_usage` | `skill_loads` | direct fit; `skill_version` is already `UInt32` |
| `error` events / `tool_calls.error_class` | `error_events` (migration 0020/0022) | already carries `execution_id` |
| `provider_fallback`, `retry`, `retries_exhausted` | `router_outcomes` (migration 0025) | Stella's fallback events are exactly this table's subject |
| **`step_manifest` / `context_blocks`** | **no table** | ⚠️ new table needed — see below |

> **⚠️ The context receipts have no home, and they are the most differentiated
> thing Stella produces.** `step_manifest` records, in wire order, every block
> the model saw on every call, with its cache zone and residency; `step_receipt`
> records the budget the compaction pass compared against. Together they make
> any past step reconstructable — "what did the model actually have in context
> when it made this decision" — which is a question no other agent runtime in
> Oxagen can answer. Landing the trace without them throws away the part worth
> having. Proposed: a `context_manifests` MergeTree ordered by
> `(org_id, execution_id, step, call_seq, ordinal)`, 365-day TTL to match
> `token_usage`.

---

## 7. Mapping — Postgres (`agent.*`)

Read from `packages/database/src/schema/agent.ts`. Every table carries
`idMixin`, `auditMixin`, `orgScopeMixin` (org + workspace, RLS-enforced).

### 7.1 `executions` → `agent.agent_executions`

| Stella | Oxagen | Note |
|---|---|---|
| `execution_uuid` (§4) | `id` (`aex` prefix) | |
| — | `org_id`, `workspace_id` | `orgScopeMixin` |
| `kind` (`run`/`deck`/`goal`/`chat`/`fleet`) | `origin_type` | ⚠️ **CHECK constraint blocks this** — see below |
| `session_uuid` | `origin_id` | the session is the origin for a CLI run |
| `outcome` | `status` | map to `planning/running/completed/failed/cancelled` |
| `prompt` | `input_payload jsonb` | **content-bearing** |
| final assistant text | `output_payload jsonb` | **content-bearing** |
| `Error.message` | `failure_reason` | |
| `started_at`, `finished_at` | `started_at`, `completed_at` | |
| derived | `latency_ms` | |
| `Σ telemetry.input_tokens` | `input_tokens` | |
| `Σ telemetry.output_tokens` | `output_tokens` | |
| `cost_usd` | `estimated_cost_usd numeric(10,6)` | |
| parent (sub-agent) | `parent_execution_id` | Stella's `sub_agent` events carry the lineage |
| `pipeline_variant`, `role`, `journal_era`, `repo_id`, `project_id` | `state jsonb` | no dedicated columns; `state` is the declared extension point |
| — | `synced_to_graph_at` | stamped by the §8 sync |

> **⚠️ Migration required: `agent_executions_origin_type_check`.** The
> constraint allows exactly
> `chat | event_trigger | scheduled_job | mcp_request | workflow_run | fanout | a2a`.
> A Stella CLI run is none of those. Add `cli` (or `stella`), and — per the
> schema file's own warning comment — **keep it in sync with
> `AGENT_EXECUTION_ORIGIN_TYPES` in
> `packages/oxagen/src/contracts/agent.execution.record.ts`**, which is the
> drift that previously let `workflow.run` pass typecheck and fail at runtime.

### 7.2 `step_receipt` + `telemetry` → `agent.agent_execution_steps`

| Stella | Oxagen |
|---|---|
| `step_uuid` | `id` (`aes`) |
| `execution_uuid` | `execution_id` (FK) |
| `step` | `step_number` |
| `call_role` (worker / summarizer / triage / witness_author / …) | `step_type` |
| derived from events | `status` |
| the step's manifest digest, or the prompt | `input_payload jsonb` |
| the step's completion | `output_payload jsonb` |
| `duration_ms`, `input_tokens`, `output_tokens` | same |
| `retries` | `attempts` |

`claimed_by` / `lease_expires_at` are Oxagen's durable-worker lease and have no
Stella counterpart — leave null. Note that `call_seq` (which separates the
several model calls that can share one step — worker 0, overflow summarizer 1,
allocated roles 2+) has **no column**; it must ride `step_type` or a new
column, or two calls in one step will collide on `(execution_id, step_number)`.

### 7.3 `tool_calls` → `agent.agent_tool_calls`

Direct: `tool_name` ← `name`, `tool_type` ← `surface`, `request_payload` ←
`args_json`, `response_payload` ← the result (**content-bearing**), `status` ←
`state`, `latency_ms` ← `duration_ms`. Same `abandoned` gap as §6.3.

### 7.4 Nothing yet has a home for

| Stella | Suggested |
|---|---|
| `files_touched` | Neo4j `TOUCHED_FILE` (§8) — already exists there |
| `commit`, `pr` events | `EntityNode` in Neo4j; Oxagen's GitHub connector already models commits/PRs |
| `tasks` (task board) | `agent.agent_plans` — closest existing shape |
| `reflections`, `execution_reflection` | `:AgentMemory` in Neo4j (§8) |
| `memory_citations` | `REMEMBERS` edge with a usefulness property |
| `foundry_tools` | `agent.skills`-adjacent, or a new table; carries the adoption witness |
| `proof`, `verdict` | ⚠️ no home — see §8 |

---

## 8. Mapping — the knowledge graph (Neo4j, `@oxagen/ontology`)

Oxagen already mirrors completed executions into Neo4j:
`packages/inngest-functions/src/functions/agent.sync-execution-to-graph.ts`
fires on `agent/execution.sync`, calls `recordExecutionInGraph`, and stamps
`synced_to_graph_at`. It is idempotent (MERGE), retried 17 times over ~24h, and
concurrency-limited per org. **A Stella trace should reuse this path verbatim
rather than writing a second graph writer.**

The shape it already writes
(`packages/ontology/src/mutations/record-execution.ts`):

```
(e:Execution:GraphNode {id, orgId, workspaceId, label:'Execution', status, startedAt, …})
  -[:INVOKED]->        (a:Agent {id, orgId})
  -[:ORIGINATED_FROM]->(o:Conversation|WorkflowRun|Fanout {id, orgId})
  -[:CALLED_TOOL]->    (t:Tool {name, type, orgId})
  -[:TOUCHED_FILE]->   (f:SourceFile {naturalKey, orgId})
```

Every node carries the `:GraphNode` anchor label and an optional `embedding`,
which is what puts it in the universal vector index — so a Stella execution
becomes semantically searchable for free.

### 8.1 What maps onto existing edges

| Stella | Edge | Note |
|---|---|---|
| `executions` row | `(:Execution)` | `summary` from the reflection, `displayName` from the prompt's first line |
| `agent_uses` | `-[:INVOKED]->(:Agent)` | |
| session | `-[:ORIGINATED_FROM]->(…)` | needs a `:Session` label, or reuse `:Conversation` |
| `tool_calls` | `-[:CALLED_TOOL]->(:Tool)` | Stella can populate call counts and error rates as edge properties |
| `files_touched` | `-[:TOUCHED_FILE]->(:SourceFile)` | **Stella is richer here** — it has `lines_added`/`lines_removed` and an ordered op log per file, which the current writer does not carry |
| `skill_usage` | `-[:LOADED_SKILL]->(:Skill)` | |
| `sub_agent` events | `-[:BRANCHED_TO_SUBAGENT]->(:Execution)` | |
| `reflections` | `-[:REMEMBERS]->(:AgentMemory)` | |
| `commit`, `pr` | `(:EntityNode)` + `IMPLEMENTS` / `PART_OF` / `AUTHORED_BY` | the GitHub connector's existing vocabulary |
| `file_locks` | `-[:LOCKED]->(:SourceFile)` | already a declared *lineage-only* projection — explicitly **not** load-bearing for mutual exclusion (ADR-021 §5) |

### 8.2 What Stella justifies adding

Three edge types with no counterpart, each carrying something Oxagen cannot
currently express:

- **`VERIFIED_BY`** — `(:Execution)-[:VERIFIED_BY]->(:Verification)`, from
  `proof` / `verdict` events. This is the "verified done, not claimed done"
  chain: which check ran, what it found, whether the fail→pass flip was
  credited. Note that in this workspace these events have **no production
  emitter** — the built-in pipeline was deleted (#3865) and the only remaining
  producer would be an installed verification plugin (Vera). So the edge type
  is worth defining and the ingest is worth writing, but **it will be empty
  until a verification plugin is installed**, and a dashboard must not read
  empty as "unverified".
- **`SAW_CONTEXT`** — `(:Execution)-[:SAW_CONTEXT]->(:ContextBlock)`, from
  `step_manifest` / `block_registered`. The provenance answer: *which retrieved
  block was in context when the model wrote this line*. This is the pairing
  that makes an audit trail causal rather than merely chronological.
- **`CITED_MEMORY`** — from `memory_citations`, carrying `useful_score` and
  `truthful`. Oxagen has `REMEMBERS`; this is the *feedback* on it, which is
  what would let Engram's reinforcement/decay machinery learn from real agent
  usage rather than from retrieval counts alone.

---

## 9. Transport — three options, ranked

### v0 — a `Stop` hook (days, no Stella change at all)

Stella's hook surface (`crates/stella-protocol/src/hook.rs`) fires
`SessionStart` / `PreToolUse` / `PostToolUse` / `Stop` / `PreCompact` /
`PreIssueWork` / `PostIssueWork`. A `Stop` hook configured in
`.stella/settings.json` can read the durable stream-json file and POST it.

- **Pro:** ships today, zero Rust, per-workspace opt-in by editing a settings
  file, and it exercises the whole Oxagen intake against real traces before any
  wire contract is frozen.
- **Con:** per-turn rather than batched, no cursor, no retry, no poison-row
  handling, no ack. Everything `drain_org` already solved is re-solved badly.
- **Verdict: build this first, as a spike to validate §6–§8, and throw it
  away.** Do not let it become the product.

### v1 — a trace drain format (the recommendation)

Add a content-bearing format alongside the existing two, reusing
`drain_org`'s cursor / batching / bisection / ack loop and `HttpCloudIntake`'s
transport, with its own consent gate and its own guard (§3.3).

- **Pro:** every hard part — idempotency, poison rows, terminal-vs-transient
  classification, schema versioning — is already built and tested. The
  additional surface is an encoder, a config block, and a consent gate.
- **Con:** requires the architectural decision in §3.3 to be made first.
- **Shape:** a `trace` block in `~/.stella/cloud.json` beside `drain`, not a
  `format` value inside it. They are different pipes with different consent
  postures, and the existing code already models "different pipe" that way —
  `cloud_drain.rs` is an adapter over the drain port rather than a second use
  of the enterprise spool, for exactly this reason.

### v2 — `stella serve` (already built, different use case)

`crates/stella-serve` is a headless engine a host process drives over the wire,
already documented as "the headless engine for Oxagen"
(`doc:serve-surface`). Every model and tool call is remoted back to the host,
so **Oxagen sees the whole trace live, by construction** — no drain needed,
because Oxagen is running the engine.

This is the right answer for *Oxagen-hosted* agents and the wrong answer for
*developer-workstation* agents, which is what the question is about. Both will
exist. The trace schema in §6–§8 should be the same for both, so a
`stella serve` session and a drained CLI session land in one table and compare.

---

## 10. Privacy, redaction, and the audit posture

The whole point of the drain is to ship what invariant #3 currently forbids, so
the compensating controls are not optional decoration.

1. **Consent is explicit, per-workspace, and revocable.** The existing drain's
   posture — no `org_id` *and* no config block means zero network I/O — is the
   floor, not the ceiling. A content-bearing drain should additionally require
   an affirmative per-workspace grant, and should say so at session start.
2. **Redaction runs before the encoder, not inside the intake.** `/export`
   already masks credentials on its way to a local archive
   (`crates/stella-cli/src/export/`, #817) — that masking is the reusable
   piece, and it must run on the trace path too. A secret that reaches
   ClickHouse has egressed regardless of what the intake does with it.
3. **The guard is the deliverable, not the encoder.** §3.3's harness exists
   because a privacy regression here is an incident. The trace class needs the
   equivalent: a test that fails the build if the encoder runs without a
   consent grant, if redaction is bypassed, or if the endpoint is not the
   enrolled one.
4. **Retention must be decided before the first row lands.** ClickHouse TTLs
   (90d events, 180d tool invocations, 365d token usage) are analytics
   retention. "Audit" in the sense the request means — reconstructing what an
   agent did and why, months later — is a Postgres/Neo4j property. §6.1.
5. **`project_id` is identifying** (§3.1). Do not present it as anonymized in
   any customer-facing description of what Oxagen collects.

---

## 11. The other direction — Oxagen tools for Stella agents

Four mechanisms exist, and they are genuinely different rather than three
flavours of one thing. Picking the wrong one is the common mistake.

| Mechanism | Where it lives | Good for |
|---|---|---|
| **MCP server** | `.stella/mcp.toml` (workspace) — merged into the tool registry at session start | Oxagen capabilities the *model* should call |
| **Custom script tool** | `.stella/tools/*.toml` — a manifest plus a script, auto-discovered | workspace-local glue; no server to run |
| **Plugin (wrapper socket)** | `stella plugin install`, `--pipeline <id>` | wrapping a whole turn — verification, policy, evidence |
| **Hook** | `.stella/settings.json` | reacting to lifecycle points; **`PreToolUse` can veto** |

### 11.1 MCP is the answer for capabilities — but not all 334 of them

Oxagen's `apps/mcp` already ships an enormous tool surface (a-to-z:
`agent`, `graph`, `ontology`, `schema`, `skill`, `sandbox`, `eval`, `billing`,
`iam`, …). **Do not point Stella at all of it.**

Stella's tool doctrine (`CLAUDE.md`; `AGENTS.md` invariant #9) is that the
model chooses a tool from its `description` alone, and that the built-in
surface is deliberately tiny — one shell, the file CRUD quartet, one unified
search, plus a coordination half. A few hundred additional tool descriptions in
the prompt would dominate the context budget and degrade tool choice on every
turn, and it would do so *silently* — the failure mode is worse decisions, not
an error.

**Recommendation: a curated `stella` MCP profile** — a named subset Oxagen
exposes specifically for coding agents, single-purpose per invariant #9 (a
parameter may scope an operation, never select one).

A defensible starting set:

| Tool | One job | Backed by |
|---|---|---|
| `oxagen_graph_search` | semantic search over the org knowledge graph | `graph.*` / the universal vector index |
| `oxagen_node_read` | fetch one node's content by id | `graph.*` |
| `oxagen_memory_write` | persist one durable lesson as `:AgentMemory` | `@oxagen/engram` |
| `oxagen_entity_link` | link this execution to a ticket / PR / entity | `INITIATED_FROM`, `IMPLEMENTS` |
| `oxagen_policy_check` | ask whether an action is permitted | `packages/iam`, pairs with a `PreToolUse` hook |
| `oxagen_spend_check` | remaining budget for this workspace | `workspace_budget_policy` |
| `oxagen_artifact_publish` | store one generated asset | `content.generated_assets` |
| `oxagen_eval_submit` | record one run into the eval tables | `eval_runs` / `eval_results` |

Three design notes that will otherwise be discovered the hard way:

- **Single-purpose is enforced at review.** `update_task(delete=true)` is two
  tools wearing one schema and gets split. An `oxagen_graph(op="search"|"write")`
  would be rejected for the same reason — and it also cannot declare
  `read_only` honestly, which corrupts the engine's concurrency contract
  (Stella dispatches consecutive read-only calls concurrently).
- **`ask_question` already exists** (#4212) for asking whoever is driving. An
  Oxagen approval flow (`agent.approval_requests`) should ride the hook/plugin
  planes, not a tool — a tool the model can decline to call is not an approval
  gate.
- **MCP auth failures are cached.** `.stella/private/mcp_auth_probes.json`
  caches connect-time 401s with a 15-minute TTL and fails open. A token refresh
  on Oxagen's side is not instantly visible to a running session.

### 11.2 Where a plugin beats a tool

Oxagen's most valuable integration may not be a tool at all. The wrapper socket
(`doc:wrapper-socket`, `crates/stella-runtime/src/wrapper/`) hands a plugin the
whole turn via `before_turn` / `after_turn` and lets it report an `EvidenceSet`
judged against a declared `VerdictRule`. That is the natural home for:

- **an org policy wrapper** — refuse a turn that violates a governance rule
  before any model call is paid for;
- **the trace drain itself** — `after_turn` is a natural, already-authorized
  place to ship a completed turn, and it composes with the plugin consent flow
  (`stella_plugin::consent_text`, the project-tier trust gate) instead of
  needing a new one.

That last point is worth weighing against §9's v1 seriously: it may be that the
right answer is *an Oxagen plugin*, not a new drain format in Stella's tree —
which would keep the content-bearing egress out of the reference implementation
entirely, and leave invariant #3 intact and unqualified. §12.

---

## 12. Open questions — decisions a maintainer owns

1. **Does the content-bearing egress live in Stella's tree at all?** §11.2's
   plugin route keeps invariant #3 unqualified and puts the trace drain in
   Oxagen's repository where its consent story already exists. §9's v1 route
   makes it a first-class Stella feature with better reliability machinery.
   These are genuinely different products, and this is the decision everything
   else hangs off.
2. **What is the trace's consent unit** — org enrollment (like enterprise
   telemetry), per-workspace grant, or per-session flag?
3. **Where does `call_role` land in ClickHouse** — reuse `capability_name`, or
   a dedicated `call_role` column? Reuse is cheap and makes per-role cost
   queries collide with per-capability ones.
4. **Retention for `source_system = 'stella'`** (§6.1, §10.4).
5. **Do the three ⚠️ migrations land together?** `origin_type` CHECK (§7.1),
   cache-token columns (§6.2), `abandoned` status (§6.3). They are
   independent, but all three are shared-cell edits of the kind that compose
   badly across parallel PRs.
6. **Is `Unknown` an ingest error or an ingest row?** Recommendation: a row,
   tagged `unknown`, with the original tag preserved in `payload` — matching
   Stella's own forward-compat posture rather than dropping data a newer
   producer emitted.

---

## References

**Stella** — `crates/stella-protocol/src/event.rs`,
`crates/stella-protocol/src/event/tags.rs`,
`crates/stella-protocol/src/hook.rs`,
`crates/stella-store/src/ddl.rs`, `crates/stella-store/src/drain.rs`,
`crates/stella-store/src/content_free.rs`,
`crates/stella-store/src/enterprise_telemetry.rs`,
`crates/stella-store/src/export.rs`,
`crates/stella-store/src/identity.rs`,
`crates/stella-cli/src/cloud_drain.rs`,
`crates/stella-cli/src/agent/output.rs`,
`docs/wire/agentevent.schema.json`, `docs/wire/agentevent.d.ts`.
Related: `doc:serve-surface`, `doc:pipeline-as-plugins`.

**Oxagen** (`~/Projects/oxagen-platform`) —
`packages/database/src/schema/agent.ts`,
`packages/telemetry/src/schema.sql` and `packages/telemetry/src/migrations/`,
`packages/ontology/src/types.ts`,
`packages/ontology/src/mutations/record-execution.ts`,
`packages/inngest-functions/src/functions/agent.sync-execution-to-graph.ts`,
`apps/mcp/src/tools/`.
