# Stella Telemetry & Data-Plane Specification

**Status:** Draft v1 · **Author:** diagnostic pass · **Date:** 2026-07-17

This spec defines how Stella records developer/agent activity, where that data
lives (project tier vs. user tier), and the data plane for a future local
web-hosted usage dashboard that makes developer-specific recommendations
(skills / MCP servers / agents).

It was motivated by three concrete defects found while investigating "Stella
never uses the code-graph tool" and "turns complete with no output." Those
findings are summarized first because they shape the design.

---

## 0. Diagnosis that motivated this spec

All three were confirmed against live data in `.stella/*.db` on this machine.

### 0.1 — "Stella never calls `graph_query`" → **not a bug in wiring; a selection + duplication problem**

- The code graph **is populated**: `.stella/codegraph.db` holds **207 files,
  4,916 symbols, 1,488 imports**. The startup line
  `code graph: 0 symbols, 0 imports across 207 files (0 parsed, 207 unchanged)`
  reports **per-pass `IndexStats`** (0 files changed this pass ⇒ 0 *new* symbols),
  not the graph total. The wording reads as "empty graph" and should be fixed.
- `graph_query` **is advertised to the model.** The registry emits 26 native
  tools for this workspace and `graph_query` is among them
  (`graph_available()` is gated only on `codegraph.db` existing, which it does;
  `stella-tools/src/registry.rs:156`). The MCP wrapper delegates to the native
  registry **live on every model call** (`stella-mcp/src/toolset.rs:290`), so it
  is genuinely in the payload.
- Therefore the model simply **never selects it.** Contributing causes:
  1. A second, overlapping retrieval surface — the `explorations` tool — is
     *always* present and *is* used, but it is **doc-backed**
     (`.stella/explorations/*.json`, `stella-tools/src/exploration.rs`
     `EXPLORATIONS_DIR`), **not** graph-backed. The model satisfies "map the
     codebase" with `explorations` + `grep`/`read_file` and never reaches the graph.
  2. Worker models (this session: `glm-5.2` via Z.ai) default to generic
     `grep`/`read` over a specialized tool with a 5-op enum.
  3. The deck's system prompt may not carry the "reach for `graph_query` FIRST"
     nudge that the pipeline/one-shot prompts have (to be confirmed — see §9).

  **Net:** Stella *does* consult a code map at runtime, but through the
  doc-backed `explorations` surface, not the live tree-sitter graph. There are
  effectively **two code-map surfaces**, and the good one is invisible to the model.

### 0.2 — "Turns complete with no feedback; feature never built" → **truncated/empty completion recorded as success**

Primary evidence, execution **41** (`store.db`), the exact "what happened to
working to build the feature i asked for" turn from the screenshot:

```
step_usage: input_tokens=61099  output_tokens=8192  tool_calls=0
            duration_ms=133700  retries=0            model=glm-5.2
text events emitted: 0
finalized as: {"type":"complete"}   execution.outcome = "completed"
```

- The model burned its **entire 8,192-token output budget** (2¹³ = the output
  cap) on ~133 s of reasoning, emitted **zero `text` and zero tool calls**, was
  cut off, and the turn was recorded as a **successful `complete`** with no error
  and nothing shown to the user. That is the "no feedback."
- **Context overflow was *not* the cause.** Input was 61k tokens — far under the
  window. Compaction *does* work (fired at `before_tokens≈158k–164k` in exec 38).
  The HUD "936k/200k" is a **display/accounting artifact** (telemetry shows the
  real step input was 61k) and should be reconciled, but it did not cause the
  empty turn.
- The related feature turn, execution **40** ("make sure Stella always reads
  skills/commands/agents…"), ran **15 `read_file` + 11 `bash` + 2 `explorations`
  and zero writes** (`files_touched` ops are all `R`). The worker model
  **over-explored and stopped before implementing**, then closed the turn
  `completed`. Two distinct failure modes, both surface as "nothing happened."

**Fixes (see §9):** detect empty/`finish_reason=length` turns and surface them;
raise/adapt the output-token cap for reasoning models; render reasoning or at
least a "truncated at output limit — retry / /compact" notice; treat a zero-
output turn as a non-success outcome.

### 0.3 — "Why more than one project database?" → **DB sprawl + orphaned duplicate tables**

Four SQLite files can appear under `.stella/` today:

| File | Owner crate | Purpose | Problem |
|---|---|---|---|
| `codegraph.db` | `stella-graph` (via `stella-tools/graph.rs`) | Tree-sitter code index; read by `graph_query` **and** the OCP `GraphProvider` (`stella-cli/src/ocp.rs:161`). **Live.** | Filename deviates from the documented design (see below). |
| `context.db` | `stella-context` | Temporal knowledge graph: `node`/`edge`/`episode`/`memory`/`embedding`/`domain` (reflections & memories). **Live.** | **Also contains orphaned `code_graph_files/symbols/imports` (156 files, stale).** Nothing reads them. |
| `store.db` | `stella-store` | Durable telemetry & state: `executions`, `events`, `telemetry`, `files_touched`, `memory_citations`, `agent_uses`, `skill_usage`, `mcp_usage`, `graph_nodes/edges` (session graph), `rules`, `file_locks`. **Live.** | Overlaps conceptually with context.db (both "durable state"). |
| `fleet.db` | `stella-fleet` | Multi-agent fleet task ledger (`stella-cli/src/fleet_cmd.rs:98`). | Only when the fleet is used. |

The concrete bug is **the code-graph tables exist in two files**:
`codegraph.db` (207 files, live) **and** `context.db` (156 files, stale/orphaned).
`stella-graph/src/store.rs` documents the *intended* design — "`02-architecture.md`
§6 mandates **one** `context.db` file … every table prefixed `code_graph_`" — but
production `graph_db_path()` points at a **separate** `codegraph.db`
(`stella-tools/src/graph.rs:31`). So the graph was split out to its own file, the
old tables in `context.db` were never dropped, and the docs still claim the old
layout. The user's instinct ("there should be one project DB") **matches the
original architecture**, so consolidating is a return to intent, not a new opinion.

---

## 1. What is already recorded (audit — build on this, don't rebuild it)

Stella already captures most of what the dashboard needs. The gaps are
**normalization** and **two new record types**, not raw capture.

| Requirement (user's words) | Already captured? | Where |
|---|---|---|
| "every single tool call recorded" | **Yes**, as an event stream | `store.db.events`: `tool_start` (`call_id`, `name`, full `input`) + `tool_result` (`call_id`, `output`, `duration_ms`). 608/608 in this DB. Not yet a queryable table. |
| MCP tool calls, with reason | **Yes** | `store.db.mcp_usage` (`server`, `tool`, `reason`, `called_at_ms`) |
| Agent invocations, with reason | **Yes** | `store.db.agent_uses` (`agent`, `version`, `reason`) |
| Skill invocations, with reason | **Yes** | `store.db.skill_usage` (`skill`, `version`, `reason`) |
| "the user's original prompt recorded" | **Yes** | `store.db.executions.prompt` (+ `kind`, `provider`, `model`, `outcome`, `cost_usd`, timestamps) |
| "all reflections recorded" | **Partially** | `.stella/reflections.jsonl` (`lesson`, `domains`, `occurred_at`) + `context.db` `memory`/`episode` nodes. Loose, two homes. |
| "the agent's reflection on its performance, inline with the prompt" | **No** — missing record type | — |
| "citations counted and the agent's remarks on whether they are useful" | **Yes, exactly** | `store.db.memory_citations` (`memory_id`, `useful_score`, `truthful`, `remark`) |
| Per-step token/cost/latency | **Yes** | `store.db.telemetry` (input/output/cache tokens, `cost_usd`, `duration_ms`, `retries`, `tool_calls` count) |
| Files touched w/ line deltas | **Yes** | `store.db.files_touched` (`ops`, `lines_added/removed`, per-op `events`) |

**Two genuinely missing record types:**
1. **Per-execution self-reflection** — the agent's own assessment of *this* turn
   ("did I do what was asked? what went well / poorly?"), tied to the prompt.
2. **Normalized `tool_calls`** — one queryable row per call (name, args, ok/err,
   duration, optional model-supplied `reason`), materialized from the event stream.

---

## 2. Design principles

1. **Two tiers, one direction of flow.** Project-tier DBs are the source of
   truth for one repo; the user-tier DB is a **derived aggregate** that only ever
   *reads* from project tiers. Never make the dashboard depend on a project DB
   being present.
2. **Durable vs. rebuildable is the split that matters** — not "one file per
   crate." Durable history (what happened) must survive `rm -rf` of any index.
   Rebuildable indexes (the code graph, embeddings) can be deleted and rebuilt.
3. **Capture once, normalize for query.** The event log is the append-only
   ground truth; normalized tables are materialized views for the dashboard.
4. **Privacy by tier.** Prompts and code content stay project-local by default.
   The user tier stores **metadata and rollups**, not source code, unless the
   developer opts in.
5. **Honor `02-architecture.md` §6.** Collapse the project tier toward one
   durable file; keep only a deliberate, documented exception for the
   rebuildable index.

---

## 3. Target architecture

### 3.1 Project tier (`<repo>/.stella/`)

Consolidate four files → **two**, by the durable/rebuildable split:

- **`stella.db` — the single durable project database.** Merge today's
  `store.db` + `context.db` into one file using table-name prefixes
  (`ctx_`, `mem_`, plus the existing telemetry tables). This is exactly the
  "one file, prefixed tables, separate WAL connections" model `stella-graph`'s
  own docs describe. Holds: executions, events, telemetry, tool_calls (new),
  files_touched, reflections (unified), execution_reflection (new),
  memory_citations, agent/skill/mcp usage, rules, the temporal memory graph
  (`node`/`edge`/`episode`/`memory`/`embedding`/`domain`), and the fleet ledger
  (`fleet_*`).
- **`codegraph.db` — the rebuildable code index.** Kept as a **separate file on
  purpose**: it is a derived tree-sitter index, high-churn (bulk re-index + live
  watcher), and must be `rm`-able without touching durable history. Document
  this exception in `02-architecture.md` §6 and in `stella-graph/src/store.rs`
  (whose doc comment currently lies by saying `context.db`).

> If strict single-file is preferred, `codegraph.db`'s `code_graph_*` tables can
> fold into `stella.db` behind the same WAL-isolated write connection the graph
> already uses. Recommendation: **keep it separate** for the rebuild-in-place
> property; the important fix is eliminating duplication and honesty in docs.

**Immediate corrective actions (project tier):**
- **Drop the orphaned `code_graph_*` tables from `context.db`.** Nothing reads them.
- **Rename `store.db`/`context.db` → `stella.db`** (with a one-time migration),
  or, if a rename is too invasive now, at minimum document the two-durable-file
  reality and fix the `stella-graph` doc comment.
- **Fix the startup log** to print the graph *total* ("code graph: 4,916 symbols
  across 207 files; 0 changed this pass"), not the per-pass delta.

### 3.2 User tier (global, cross-project)

A new **`usage.db`** at a per-user location, aggregating every project the
developer runs Stella in. Location (first that resolves), configurable via
`STELLA_DATA_DIR`:

- `${XDG_DATA_HOME:-~/.local/share}/stella/usage.db` (Linux)
- `~/Library/Application Support/stella/usage.db` (macOS)
- `%APPDATA%\stella\usage.db` (Windows)

> Stella already uses `~/.config/stella/` for user-scope skills/commands/agents/
> rules. `usage.db` is *data*, not *config*, so it belongs in the data dir; keep
> `~/.config/stella/` for the config it already owns. Provide one override env var.

The user tier is the **dashboard's only dependency**. It holds cross-project
rollups, a registry of known projects, and the recommendation outputs.

### 3.3 The boundary rule (what goes where)

| Data | Project `stella.db` | User `usage.db` |
|---|---|---|
| Full prompts, tool args, file contents, code graph | ✅ source of truth | ❌ (metadata/hashes/rollups only, opt-in for text) |
| Per-execution rollup (tokens, cost, outcome, duration, #tools, #writes) | ✅ | ✅ copied on turn end |
| Tool-call histogram (name → count, p50/p95 duration, error rate) | ✅ derivable | ✅ aggregated |
| Skill / agent / MCP usage counts + reasons | ✅ | ✅ aggregated |
| Citation usefulness (score, truthful, remark) | ✅ | ✅ aggregated per memory & per domain |
| Reflections & self-critiques | ✅ full text | ✅ rollup + domain tags (text opt-in) |
| Recommendations (skills/servers/agents) | — | ✅ computed here |
| Project registry (path, name, language mix, last seen) | — | ✅ |

---

## 4. Schema specification

### 4.1 Project tier — additions to the durable DB

Existing tables (`executions`, `events`, `telemetry`, `files_touched`,
`memory_citations`, `agent_uses`, `skill_usage`, `mcp_usage`, `rules`) stay as-is.
Add:

```sql
-- One queryable row per NATIVE tool call, materialized from the event stream
-- (tool_start + tool_result). Large outputs are NOT stored here (they remain in
-- events, or are pruned); we keep shape, timing, and success.
CREATE TABLE tool_calls (
    execution_id  INTEGER NOT NULL,
    seq           INTEGER NOT NULL,          -- monotonic within execution
    call_id       TEXT    NOT NULL,          -- provider call id, joins start↔result
    name          TEXT    NOT NULL,          -- read_file, grep, graph_query, ...
    surface       TEXT    NOT NULL,          -- 'native' | 'mcp' | 'skill' | 'agent'
    args_json     TEXT    NOT NULL DEFAULT '{}',
    args_digest   TEXT,                       -- sha256 of args, for dedup/loops
    reason        TEXT    NOT NULL DEFAULT '',-- model's stated reason (see §5.1)
    ok            INTEGER NOT NULL,           -- 1 success, 0 error
    error         TEXT,
    bytes_out     INTEGER NOT NULL DEFAULT 0, -- size of result, not the result
    duration_ms   INTEGER NOT NULL DEFAULT 0,
    ts            TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (execution_id, seq)
);
CREATE INDEX tool_calls_by_name ON tool_calls(name, execution_id);

-- The agent's reflection ON ITS OWN PERFORMANCE for a specific turn, tied to
-- the prompt. This is the "prompt inline with the agent's self-reflection".
CREATE TABLE execution_reflection (
    execution_id     INTEGER PRIMARY KEY,     -- 1:1 with executions.id
    prompt           TEXT NOT NULL,           -- denormalized copy of the ask
    delivered        INTEGER,                 -- self-assessed: did I do the ask? 1/0/NULL
    self_rating      INTEGER,                 -- 0..5 self score
    what_went_well   TEXT NOT NULL DEFAULT '',
    what_to_improve  TEXT NOT NULL DEFAULT '',
    critique         TEXT NOT NULL DEFAULT '',-- free-form self-critique
    -- objective companions the dashboard pairs with the self-view:
    produced_output  INTEGER NOT NULL DEFAULT 0, -- had text or tool calls
    wrote_files      INTEGER NOT NULL DEFAULT 0,
    truncated        INTEGER NOT NULL DEFAULT 0,  -- finish_reason=length / empty
    recorded_at      TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Unify reflections/lessons into one durable table (superset of reflections.jsonl).
CREATE TABLE reflections (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    execution_id  INTEGER,                    -- NULL for cross-turn lessons
    kind          TEXT NOT NULL,              -- 'lesson' | 'self_critique' | 'preference'
    content       TEXT NOT NULL,
    domains       TEXT NOT NULL DEFAULT '[]', -- json array
    occurred_at   INTEGER NOT NULL
);
CREATE INDEX reflections_by_kind ON reflections(kind);
```

`memory_citations` already satisfies "citations counted + usefulness remark"
verbatim — no change, just surface it.

### 4.2 User tier — `usage.db`

```sql
CREATE TABLE projects (
    project_id    TEXT PRIMARY KEY,           -- stable hash of canonical repo path
    name          TEXT NOT NULL,
    root_path     TEXT NOT NULL,
    languages     TEXT NOT NULL DEFAULT '{}', -- json: {rust: 207, ...} from code graph
    first_seen_at TEXT NOT NULL,
    last_seen_at  TEXT NOT NULL
);

-- One row per turn, copied from project tier on turn end (metadata only).
CREATE TABLE execution_rollup (
    project_id    TEXT NOT NULL,
    execution_id  INTEGER NOT NULL,           -- project-local id
    kind          TEXT NOT NULL,              -- deck | deck-pipeline | one-shot ...
    prompt_digest TEXT NOT NULL,              -- sha256(prompt); full text opt-in
    prompt_preview TEXT,                       -- first N chars, opt-in
    model         TEXT NOT NULL,
    provider      TEXT NOT NULL,
    outcome       TEXT NOT NULL,              -- completed | truncated | empty | error | aborted
    cost_usd      REAL NOT NULL,
    input_tokens  INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    duration_ms   INTEGER NOT NULL,
    tool_calls    INTEGER NOT NULL,
    files_written INTEGER NOT NULL,
    produced_output INTEGER NOT NULL,         -- the §0.2 signal
    self_rating   INTEGER,
    started_at    TEXT NOT NULL,
    PRIMARY KEY (project_id, execution_id)
);

CREATE TABLE tool_usage_rollup (        -- histogram powering "you grep symbols a lot"
    project_id  TEXT NOT NULL, tool TEXT NOT NULL, surface TEXT NOT NULL,
    day TEXT NOT NULL, calls INTEGER NOT NULL, errors INTEGER NOT NULL,
    p50_ms INTEGER, p95_ms INTEGER,
    PRIMARY KEY (project_id, tool, surface, day)
);
CREATE TABLE skill_usage_rollup ( project_id TEXT, skill TEXT, day TEXT, calls INTEGER, PRIMARY KEY (project_id, skill, day) );
CREATE TABLE agent_usage_rollup ( project_id TEXT, agent TEXT, day TEXT, calls INTEGER, PRIMARY KEY (project_id, agent, day) );
CREATE TABLE mcp_usage_rollup   ( project_id TEXT, server TEXT, tool TEXT, day TEXT, calls INTEGER, PRIMARY KEY (project_id, server, tool, day) );

CREATE TABLE citation_rollup (          -- "citations counted + usefulness"
    project_id TEXT NOT NULL, memory_id TEXT NOT NULL,
    citations INTEGER NOT NULL, avg_useful REAL NOT NULL,
    untruthful INTEGER NOT NULL, last_remark TEXT,
    PRIMARY KEY (project_id, memory_id)
);

CREATE TABLE reflection_rollup (        -- domain-tagged lessons across projects
    project_id TEXT, domain TEXT, kind TEXT, count INTEGER,
    PRIMARY KEY (project_id, domain, kind)
);

CREATE TABLE recommendations (          -- output of the engine (§7)
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id   TEXT,                    -- NULL = global/user-wide
    kind         TEXT NOT NULL,           -- 'skill' | 'mcp_server' | 'agent' | 'setting'
    target       TEXT NOT NULL,           -- e.g. 'graph_query', 'web-search MCP'
    rationale    TEXT NOT NULL,           -- human-readable, cites the signal
    signal_json  TEXT NOT NULL,           -- the evidence (counts, ratios)
    confidence   REAL NOT NULL,
    status       TEXT NOT NULL DEFAULT 'new', -- new | shown | accepted | dismissed
    created_at   TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
```

---

## 5. Recording requirements → implementation (explicit)

The user's exact asks, mapped to the plan:

### 5.1 "I need every single tool call recorded"
- Already in `events` (`tool_start`/`tool_result`). **Add a writer** that also
  materializes each pair into `tool_calls` (§4.1) at result time — one row per
  call, joinable by `call_id`, with `ok`, `duration_ms`, `bytes_out`.
- **Add a `reason` field to native tool calls** to match `mcp_usage`/`agent_uses`/
  `skill_usage`, which already carry the model's stated reason. (Today only
  external surfaces record *why*; native calls don't.) The model already narrates
  intent in `text` — capture it structurally per call.
- Do **not** copy large outputs to the user tier; keep `bytes_out` + shape only.

### 5.2 "I need all the reflections recorded"
- Collapse `.stella/reflections.jsonl` + `context.db` memory/episode lessons into
  the durable `reflections` table (§4.1). Keep JSONL export for portability, but
  the DB is the source of truth. Roll up per (domain, kind) to the user tier.

### 5.3 "The user's original prompt … inline with the agent's own reflection on its performance"
- `executions.prompt` already stores the ask. **Add `execution_reflection`**
  (§4.1), written at turn end, 1:1 with the execution. The agent emits a short
  structured self-review (`delivered`, `self_rating`, `what_went_well`,
  `what_to_improve`). Pair it in the dashboard with the **objective** companions
  (`produced_output`, `wrote_files`, `truncated`) so a turn like exec 41
  (self-silent, truncated, zero output) is visibly a failure even if the model
  would rate itself kindly. There is already a `judge_verdict` event and a
  `verify_done` tool to build this on.

### 5.4 "Citations counted and the agent's remarks on whether they are useful"
- **Done today** in `memory_citations(useful_score, truthful, remark)`. Just
  aggregate into `citation_rollup` (§4.2) and surface it. No new capture needed.

---

## 6. Sync / aggregation pipeline (project → user)

- **Trigger:** on every execution finalize (the same hook that writes the
  `complete` event), upsert one `execution_rollup` row and increment the day's
  rollup counters in `usage.db`. Cheap, synchronous, WAL-safe.
- **Backfill / repair:** a `stella usage sync` command re-derives all user-tier
  rollups from a project's `stella.db` (idempotent; keyed by `(project_id,
  execution_id)`). Run on first dashboard launch and to recover from a wiped
  `usage.db`.
- **Project identity:** `project_id = sha256(canonical_root_path)`; also store
  `name` + `root_path` in `projects`. Moves/renames create a new id with a
  `previous_root` breadcrumb (keeps history joinable).
- **Isolation:** user-tier writes use their own connection; never block a turn on
  the aggregate. If `usage.db` is unavailable, the project tier is authoritative
  and sync retries next turn.

---

## 7. Dashboard data plane + recommendation engine

**Dashboard** = a local read-only web server (e.g. `stella dashboard`, binds
`127.0.0.1`) over `usage.db`. Panels: cost/latency over time, model mix,
tool-call histograms, skill/agent/MCP usage, citation usefulness, reflection
themes by domain, and a **Recommendations** feed.

**Recommendation engine** (writes `recommendations`) turns aggregated signals
into developer-specific, actionable suggestions:

| Signal (from rollups) | Recommendation |
|---|---|
| High `grep`/`read_file` for symbol lookup **and** `graph_query` calls ≈ 0 while a code graph exists | "Enable/learn `graph_query` — you search symbols manually N×/day." *(This is exactly §0.1 — the data plane would have surfaced the graph-tool gap.)* |
| Repeated near-identical `bash` sequences (via `args_digest`) | "Codify as a custom command/skill: `<name>`." |
| Repeated failures / low `self_rating` clustered in a domain (`reflection_rollup`) | "Install the `<domain>` skill/agent." |
| Frequent capability-gap patterns (web lookups, doc fetches, unresolved refs) | "Add MCP server: web-search / docs / <X>." |
| Low `avg_useful` or `untruthful>0` in `citation_rollup` | "Prune/repair these memories; they mislead." |
| `truncated`/`produced_output=0` turns recurring on a model | "Raise output cap or switch model for this workload." |
| Expensive worker model where a judge/worker split would help | "Use `<cheap>` for workers, `<strong>` for judging." |

Recommendations cite their evidence (`signal_json`) so they are auditable, and
carry `status` so the UI can mark accepted/dismissed and stop re-nagging.

---

## 8. Privacy, retention, opt-in

- **Default:** user tier stores **metadata, digests, counts** — no prompt text,
  no code, no tool outputs. `prompt_preview`/full-text sync is **opt-in**
  (`STELLA_USAGE_TEXT=1` or a config flag).
- **Local only:** `usage.db` never leaves the machine; the dashboard binds
  loopback. Any future cloud sync is a separate, explicit opt-in.
- **Retention:** raw `events` in project tier are prunable after N days
  (rollups already captured); user tier keeps rollups indefinitely (small).
- **Deletion:** `stella usage forget <project>` removes a project's rows.

---

## 9. Migration plan & immediate fixes

**Data-plane migration (incremental, no big-bang):**
1. Add `tool_calls`, `execution_reflection`, `reflections` tables to the durable
   project DB; start writing them (dual-write from the event stream).
2. Stand up `usage.db` + the finalize-hook sync + `stella usage sync` backfill.
3. Ship `stella dashboard` (read-only) over `usage.db`.
4. Add the recommendation engine (start with the `graph_query`-gap rule — it
   pays for itself by fixing §0.1).
5. Consolidate `store.db`+`context.db` → `stella.db`; keep `codegraph.db`
   separate and **document why**; **drop the orphaned `code_graph_*` tables from
   `context.db`**; fix the `stella-graph` doc comment and the startup log wording.

**The three diagnostic bugs (fix independently, high value):**
- **Empty/truncated turn recorded as success (§0.2).** Inspect `finish_reason`;
  when a step ends with `length`/no-content/no-tool-calls, mark the outcome
  `truncated`/`empty` (not `completed`) and surface a visible message
  ("response hit the output-token limit — retry or /compact"). Raise/adapt the
  8,192 output cap for reasoning models; render reasoning or a placeholder so
  133 s of silent generation can't look like a hang. *(Exact fix locations being
  confirmed in `stella-core/src/driver.rs` run-loop and `stella-model/src/zai.rs`.)*
- **`graph_query` never selected (§0.1).** Decide the two-surface story: either
  make `explorations` graph-backed (unify) or strengthen the deck system prompt
  to prefer `graph_query` for symbol/dependency questions; add a tool-selection
  metric (this spec's histogram) to verify the fix. Fix the misleading startup
  log.
- **DB duplication (§0.3).** Drop orphaned `code_graph_*` from `context.db`;
  reconcile the "one DB" doc with the two-durable-file reality.
```

*(File:line fix targets for the truncation handler and glm-5.2 output cap are
appended in §9 once the code trace completes.)*
