//! Every table and index the store owns, as DDL at the current
//! [`SCHEMA_VERSION`](crate::migrations::SCHEMA_VERSION) — the one place the
//! CURRENT shape of `.stella/private/store.db` is written down. Fresh databases get
//! this whole schema in one shot
//! ([`create_latest_schema`](crate::migrations::create_latest_schema));
//! existing files reach the same shape table by table through
//! [`crate::migrations`] — which is why most statements carry
//! `IF NOT EXISTS` (one batch serves both the fresh path and an additive
//! migration) and three are name-parameterized functions (the
//! lang_altertable §7 table rebuilds create the new shape under a scratch
//! name first).

/// Every table the store owns — the allowlist for [`Store::count`](crate::Store::count) and the
/// fresh-file probe in [`Store::migrate`](crate::Store::migrate).
///
/// "Owns" means *versioned by `PRAGMA user_version`*, not "present in the
/// file". `store.db` also carries the optional `enterprise_export_*` tables,
/// which converge by column probing outside the migration chain
/// ([`crate::enterprise_telemetry`]) — they are deliberately absent here, since
/// the fresh-file probe must answer "has the versioned schema ever been
/// created?" and those tables are created after it runs.
pub(crate) const TABLES: [&str; 21] = [
    "executions",
    "forgotten",
    "events",
    "telemetry",
    "files_touched",
    "memory_citations",
    "rules",
    "mcp_usage",
    "file_locks",
    "agent_uses",
    "skill_usage",
    "tool_calls",
    "execution_reflection",
    "reflections",
    "tasks",
    "pull_requests",
    "context_blocks",
    "step_manifest",
    "step_receipt",
    "foundry_tools",
    "session_turn_diffs",
];

/// `executions` DDL at [`SCHEMA_VERSION`](crate::migrations::SCHEMA_VERSION) — the spine every other table
/// keys off, one row per goal/turn. Note that this "run" is NOT the fleet
/// ledger's `run_id` (one multi-agent fan-out, `stella-fleet/src/ledger.rs`)
/// and NOT a session — see the glossary in `AGENTS.md` for the five
/// look-alike identifiers. `session_id` (v8) is the nullable
/// cross-process session registry id ([`SessionRecord::id`](crate::SessionRecord::id)) stamped by
/// [`Store::set_execution_session`](crate::Store::set_execution_session) right after the row is opened, linking
/// per-turn executions back to their session so
/// [`Store::session_events`](crate::Store::session_events) can reassemble the full journal; NULL for rows
/// persisted before v8 or for runs outside a registered session. The
/// by-session index is that reader's access path (filter on session_id,
/// scan in id order). `IF NOT EXISTS` on both so the batch also tolerates a
/// partial file that already grew them.
///
/// `executions_unfinished` is **partial** (`WHERE finished_at IS NULL`), and
/// that is the whole point: the crash-recovery sweep at
/// [`Store::open`](crate::Store::open) asks "is anything unclosed?" on every
/// single open, and the honest answer is almost always no. A partial index
/// holds only the open rows — usually zero, at most a handful — so the
/// question costs an empty index probe instead of a scan over every
/// execution the workspace has ever run.
///
/// `journal_era` (v22) records which compaction-journaling era wrote this
/// execution's events — see [`JournalEra`](crate::JournalEra). It is stamped
/// by the writer at [`Store::begin_execution`](crate::Store::begin_execution)
/// rather than inferred at read time, because the reader cannot tell "this
/// build journaled no rewrites" from "this build could not" by looking at the
/// events. The `DEFAULT 0` is what every row written before v22 backfills to,
/// and it is deliberately the benign reading: a code this build does not know
/// is treated as the oldest era, so an unfamiliar stamp can only ever
/// under-alarm, never raise a false one.
pub(crate) const EXECUTIONS_DDL: &str = "CREATE TABLE IF NOT EXISTS executions (
       id INTEGER PRIMARY KEY AUTOINCREMENT,
       kind TEXT NOT NULL,
       prompt TEXT NOT NULL,
       provider TEXT NOT NULL,
       model TEXT NOT NULL,
       started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
       finished_at TEXT,
       outcome TEXT,
       cost_usd REAL NOT NULL DEFAULT 0,
       session_id TEXT,
       usage_complete INTEGER NOT NULL DEFAULT 0 CHECK(usage_complete IN (0, 1)),
       usage_status TEXT NOT NULL DEFAULT 'pending'
         CHECK(usage_status IN ('pending', 'complete', 'incomplete')),
       journal_era INTEGER NOT NULL DEFAULT 0
     );
     CREATE INDEX IF NOT EXISTS executions_by_session
       ON executions(session_id, id);
     CREATE INDEX IF NOT EXISTS executions_unfinished
       ON executions(id) WHERE finished_at IS NULL;";

/// Tables whose shape has not changed since v0. `IF NOT EXISTS` keeps one
/// batch usable both for fresh files and for filling gaps in partial legacy
/// files (a v0 file only holds what its era's code created). The v0-era
/// `graph_nodes`/`graph_edges` pair left this batch when v17 dropped them
/// (a seam reserved for a context plane that shipped its own stores
/// instead — nothing ever wrote or read the tables).
pub(crate) const UNCHANGED_TABLES: &str = "CREATE TABLE IF NOT EXISTS file_locks (
       path TEXT PRIMARY KEY,
       holder TEXT NOT NULL,
       acquired_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
     );";

/// `events` DDL at [`SCHEMA_VERSION`](crate::migrations::SCHEMA_VERSION), parameterized over the table name
/// because the v0 → v1 rebuild first creates it under a scratch name.
///
/// UNIQUE (execution_id, seq): one row per position in an execution's event
/// stream. The drain loop owns a monotonically increasing `seq` per
/// execution and replay reads `(execution_id, seq)` back in order, so a
/// duplicate position is a double-write, not data — the constraint turns it
/// into an error instead of a silently corrupted replay. Its implicit index
/// is exactly the replay access path (superseding the pre-v1 non-unique
/// `events_by_execution` index, which is why no separate index exists).
pub(crate) fn events_ddl(table: &str) -> String {
    format!(
        "CREATE TABLE {table} (
           execution_id INTEGER NOT NULL,
           seq INTEGER NOT NULL,
           ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
           event_type TEXT NOT NULL,
           payload TEXT NOT NULL,
           UNIQUE (execution_id, seq)
         );"
    )
}

/// `files_touched` DDL at [`SCHEMA_VERSION`](crate::migrations::SCHEMA_VERSION) — see [`events_ddl`] for why it
/// is name-parameterized.
///
/// UNIQUE (execution_id, path): one session record per normalized path —
/// the ledger aggregates every touch of a file into one record before
/// persisting, so a duplicate path is a double-write, not data. `events` is
/// the ordered JSON audit log (`[{event, reason, lines_added,
/// lines_removed}, …]`); rows persisted before v2 carry the backfill
/// defaults (zero deltas, empty log).
pub(crate) fn files_touched_ddl(table: &str) -> String {
    format!(
        "CREATE TABLE {table} (
           execution_id INTEGER NOT NULL,
           path TEXT NOT NULL,
           ops TEXT NOT NULL,
           lines_added INTEGER NOT NULL DEFAULT 0,
           lines_removed INTEGER NOT NULL DEFAULT 0,
           events TEXT NOT NULL DEFAULT '[]',
           UNIQUE (execution_id, path)
         );"
    )
}

/// `telemetry` DDL at [`SCHEMA_VERSION`](crate::migrations::SCHEMA_VERSION) — see [`events_ddl`] for why it is
/// name-parameterized.
///
/// UNIQUE (execution_id, step): one row per committed model call —
/// `StepUsage` is emitted exactly once per step that lands. `drift_samples`
/// treats `(execution_id, step)` as insertion order and `usage_stats` sums
/// tokens/cost per execution, so a duplicate step double-counts money.
pub(crate) fn telemetry_ddl(table: &str) -> String {
    format!(
        "CREATE TABLE {table} (
           execution_id INTEGER NOT NULL,
           step INTEGER NOT NULL,
           ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
           provider TEXT NOT NULL,
           call_role TEXT NOT NULL DEFAULT 'unknown',
           model TEXT NOT NULL,
           input_tokens INTEGER NOT NULL,
           estimated_input_tokens INTEGER NOT NULL DEFAULT 0,
           output_tokens INTEGER NOT NULL,
           cache_read_tokens INTEGER NOT NULL,
           cache_miss_tokens INTEGER NOT NULL,
           cache_write_tokens INTEGER NOT NULL DEFAULT 0,
           cost_usd REAL NOT NULL,
           duration_ms INTEGER NOT NULL,
           retries INTEGER NOT NULL,
           tool_calls INTEGER NOT NULL,
           usage_complete INTEGER NOT NULL DEFAULT 0 CHECK(usage_complete IN (0, 1)),
           UNIQUE (execution_id, step)
         );"
    )
}

/// `rules` DDL — one row per extension-authored workspace rule, keyed by
/// rule id (the analog of a rule file's filename stem). `contents` is the
/// FULL rule markdown in the `.stella/rules/*.md` authoring format
/// (optional `---` frontmatter — `description:`/`guard-*:` keys — plus the
/// rule statement body); the store never parses it, `stella_core::rules`
/// does. `source` is an opaque label naming the writer (extension/provider
/// id). `IF NOT EXISTS` so one batch serves both the fresh-file schema and
/// the v2 → v3 migration.
pub(crate) const RULES_TABLE: &str = "CREATE TABLE IF NOT EXISTS rules (
       rule_id TEXT PRIMARY KEY,
       contents TEXT NOT NULL,
       source TEXT NOT NULL,
       created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
       updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
     );";

/// `drift_samples` filters (provider, model) and sorts (execution_id DESC,
/// step DESC) at EVERY session start, over a table that grows one row per
/// model call forever — without this index it full-scans. Non-unique on
/// purpose: uniqueness lives on the (execution_id, step) key; this is the
/// query's covering access path.
pub(crate) const TELEMETRY_INDEX: &str = "CREATE INDEX IF NOT EXISTS telemetry_by_model
       ON telemetry(provider, model, execution_id, step);";

/// `memory_citations` DDL at [`SCHEMA_VERSION`](crate::migrations::SCHEMA_VERSION).
///
/// UNIQUE (execution_id, memory_id): one citation per memory per execution —
/// the session ledger keeps only the model's latest judgment of a memory
/// before persisting, so a duplicate pair is a double-write, not data.
/// `truthful` is 0/1. The by-memory index is the access path of
/// [`Store::memory_citation_stats`](crate::Store::memory_citation_stats), which scans per memory in citation
/// order; the UNIQUE key's implicit (execution_id, …) index can't serve it.
pub(crate) const MEMORY_CITATIONS_DDL: &str = "CREATE TABLE IF NOT EXISTS memory_citations (
       execution_id INTEGER NOT NULL,
       memory_id TEXT NOT NULL,
       ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
       useful_score INTEGER NOT NULL,
       truthful INTEGER NOT NULL,
       remark TEXT NOT NULL DEFAULT '',
       UNIQUE (execution_id, memory_id)
     );
     CREATE INDEX IF NOT EXISTS memory_citations_by_memory
       ON memory_citations(memory_id, execution_id);";

/// `forgotten` DDL at [`SCHEMA_VERSION`](crate::migrations::SCHEMA_VERSION) — explicit human
/// tombstones over anything that steers the agent
/// ([`ContextSurface`](crate::ContextSurface)).
///
/// This is a *stored* judgment, unlike quarantine, which
/// [`Store::quarantined_memory_ids`](crate::Store::quarantined_memory_ids)
/// derives from citation counts. A derivation can express "the model kept
/// calling this untruthful"; it cannot express "a person read this and said
/// remove it", which is why the state needs a table of its own rather than
/// another fold over `memory_citations`.
///
/// It lives in `store.db` beside `memory_citations` rather than in
/// `context.db` next to the memories themselves, because context nodes are
/// mutable current-state (`upsert_node` overwrites in place, so there is no
/// point-in-time reader), and a tombstone must outlive edits to the thing it
/// buries — including the row being deleted entirely.
///
/// PRIMARY KEY (surface, item_id): one verdict per item, and re-forgetting
/// is an idempotent upsert rather than a duplicate.
///
/// `content` is the forgotten text, copied in at forget time. It is what
/// makes the tombstone survive re-learning: the reflection recorder and the
/// skill miner compare *candidates* against it
/// ([`forget::is_suppressed`](crate::forget::is_suppressed)), so a
/// re-mined paraphrase with a brand-new id is still caught. Without the copy
/// the check would depend on the original row, which forgetting may remove.
///
/// Restoring is a plain `DELETE` — the row's absence is the "not forgotten"
/// state, so there is no tri-state to keep consistent.
pub(crate) const FORGOTTEN_DDL: &str = "CREATE TABLE IF NOT EXISTS forgotten (
       surface TEXT NOT NULL,
       item_id TEXT NOT NULL,
       content TEXT NOT NULL DEFAULT '',
       reason TEXT NOT NULL DEFAULT '',
       forgotten_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
       PRIMARY KEY (surface, item_id)
     );
     CREATE INDEX IF NOT EXISTS forgotten_by_surface
       ON forgotten(surface, forgotten_at);";

/// `agent_uses` DDL at [`SCHEMA_VERSION`](crate::migrations::SCHEMA_VERSION) — the agent-invocation log
/// ([`AgentUseRow`](crate::AgentUseRow)): one row per invocation of an installed agent
/// definition, attributed to the execution it ran under and to the
/// definition's pinned `version` at invocation time. Deliberately **not**
/// UNIQUE on any key: invoking the same agent-version twice in one execution
/// is two real events, and the drain-per-execution write path never
/// double-writes a drained event. `IF NOT EXISTS` keeps the one DDL usable
/// for both the fresh-file path and the additive v3 → v4 migration. No
/// secondary index: every reader (the JSON export, the observatory) walks
/// the whole log; the v4-era `agent_uses_by_agent` index served no query
/// and was dropped in v17.
pub(crate) const AGENT_USES_DDL: &str = "CREATE TABLE IF NOT EXISTS agent_uses (
       execution_id INTEGER NOT NULL,
       agent TEXT NOT NULL,
       version INTEGER NOT NULL,
       reason TEXT NOT NULL DEFAULT '',
       ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
     );";

/// `skill_usage` DDL at [`SCHEMA_VERSION`](crate::migrations::SCHEMA_VERSION) — per-execution skill-version
/// invocation telemetry (SKILLS tab), the exact analogue of [`AGENT_USES_DDL`].
/// Append-only: one row per skill applied in a turn, no UNIQUE key. The
/// by-skill index serves per-skill/version aggregate queries.
pub(crate) const SKILL_USAGE_DDL: &str = "CREATE TABLE IF NOT EXISTS skill_usage (
       execution_id INTEGER NOT NULL,
       skill TEXT NOT NULL,
       version INTEGER NOT NULL,
       reason TEXT NOT NULL DEFAULT '',
       ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
     );
     CREATE INDEX IF NOT EXISTS skill_usage_by_skill
       ON skill_usage(skill, version, execution_id);";

/// `mcp_usage` DDL at [`SCHEMA_VERSION`](crate::migrations::SCHEMA_VERSION).
///
/// A per-call log (NOT a per-key aggregate like `files_touched`): the same
/// server+tool called twice is two rows. UNIQUE (execution_id, seq) is the
/// house double-write guard (the `events` pattern) — `seq` is the row's index
/// in an execution's drained batch, so re-persisting the same drained batch is
/// an error, not a silent double-count. `called_at_ms` is the call time
/// captured at the tool call (not the drain time). The by-server index is NOT
/// [`Store::mcp_usage_stats`](crate::Store::mcp_usage_stats)'s access path (that reader scans the whole log
/// in `called_at_ms` order and folds in Rust); it serves the observatory's
/// per-(server, tool) aggregate (`stella-observatory`'s MCP panel, `GROUP BY
/// server, tool`), which reads this file directly.
pub(crate) const MCP_USAGE_DDL: &str = "CREATE TABLE IF NOT EXISTS mcp_usage (
       execution_id INTEGER NOT NULL,
       seq INTEGER NOT NULL,
       server TEXT NOT NULL,
       tool TEXT NOT NULL,
       reason TEXT NOT NULL DEFAULT '',
       called_at_ms INTEGER NOT NULL,
       UNIQUE (execution_id, seq)
     );
     CREATE INDEX IF NOT EXISTS mcp_usage_by_server
       ON mcp_usage(server, tool);";

/// `tool_calls` DDL at [`SCHEMA_VERSION`](crate::migrations::SCHEMA_VERSION) — one queryable row per tool call,
/// normalized from the append-only `events` stream (`tool_start` +
/// `tool_result`) so the dashboard can query call histograms without
/// JSON-scanning the event log. `surface` is `'native'` or `'mcp'` (derived
/// from the tool-name prefix — the only surfaces the producer emits; skills
/// and agents have their own tables), and `reason` is currently always empty
/// (the event stream carries none — the column waits for a producer). Large
/// outputs are NOT stored here — only shape, timing, and success
/// (`bytes_out` records the result size, not the result). UNIQUE
/// (execution_id, seq) is the house double-write guard. The by-name index is
/// the access path for usage histograms (e.g. "grep called N times,
/// graph_query zero").
///
/// **This projection is written LIVE** (v18), inside the same transaction as
/// the `tool_start`/`tool_result` event that produces it
/// ([`Store::record_event`](crate::Store::record_event)). Before v18 it was
/// materialized once, at turn end, from the whole event stream — which meant
/// an in-flight turn reported zero tool calls no matter how many it had made,
/// and a turn that never reached its end (SIGKILL, panic, power loss) left
/// its calls in `events` with no row here forever. The end-of-turn fold
/// ([`Store::materialize_tool_calls`](crate::Store::materialize_tool_calls))
/// survives as the idempotent *repair* path, not as the only writer.
///
/// `state` is the lifecycle the live write needs and the end-of-turn fold
/// could not express: `'running'` (announced, no result yet), `'ok'`, or
/// `'error'`. It is strictly richer than `ok`, which is kept in lockstep
/// (`ok = 1` iff `state = 'ok'`) so every pre-v18 reader keeps working.
/// Without it an in-flight call is indistinguishable from a failed one with
/// an empty error message, and a dashboard cannot honestly draw either.
///
/// `ts` is now the moment the call was **announced** rather than the moment
/// the turn ended, so per-day rollups bucket a call on the day it ran.
///
/// The by-state index is the access path for the live views — "what is
/// running right now" and the interrupted-call sweep — both of which filter
/// on `state` before anything else.
///
/// `tool_calls_by_call_id` is the live writer's own access path: a
/// `tool_result` finds the row its `tool_start` opened by `call_id`, which is
/// the only identity the two events share. It is UNIQUE because one `call_id`
/// is one call — the invariant that stops a re-announced start from minting a
/// second row and double-counting the call — but **partial**, excluding
/// `call_id = ''`. Legacy rows predating the column carry the empty default,
/// and a total unique index would fail to build on any file holding two of
/// them, which fails the migration and takes the workspace's whole store with
/// it. The invariant is only meaningful for real ids anyway.
pub(crate) const TOOL_CALLS_DDL: &str = "CREATE TABLE IF NOT EXISTS tool_calls (
       execution_id INTEGER NOT NULL,
       seq INTEGER NOT NULL,
       call_id TEXT NOT NULL DEFAULT '',
       name TEXT NOT NULL,
       surface TEXT NOT NULL DEFAULT 'native',
       args_json TEXT NOT NULL DEFAULT '{}',
       args_digest TEXT NOT NULL DEFAULT '',
       reason TEXT NOT NULL DEFAULT '',
       ok INTEGER NOT NULL DEFAULT 1,
       state TEXT NOT NULL DEFAULT 'ok'
         CHECK(state IN ('running', 'ok', 'error')),
       error TEXT NOT NULL DEFAULT '',
       bytes_out INTEGER NOT NULL DEFAULT 0,
       duration_ms INTEGER NOT NULL DEFAULT 0,
       ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
       UNIQUE (execution_id, seq)
     );
     CREATE INDEX IF NOT EXISTS tool_calls_by_name
       ON tool_calls(name, execution_id);
     CREATE INDEX IF NOT EXISTS tool_calls_by_state
       ON tool_calls(state, execution_id, seq);
     CREATE UNIQUE INDEX IF NOT EXISTS tool_calls_by_call_id
       ON tool_calls(execution_id, call_id) WHERE call_id != '';";

/// `execution_reflection` DDL at [`SCHEMA_VERSION`](crate::migrations::SCHEMA_VERSION) — the agent's own
/// assessment of ONE turn, tied 1:1 to its execution (and thus to
/// `executions.prompt`). Pairs the model's self-view (`delivered`,
/// `self_rating`, `what_went_well`, `what_to_improve`, `critique`) with the
/// objective companions (`produced_output`, `wrote_files`, `truncated`) so a
/// self-silent, zero-output turn is visibly a failure even if the model would
/// rate itself kindly.
pub(crate) const EXECUTION_REFLECTION_DDL: &str =
    "CREATE TABLE IF NOT EXISTS execution_reflection (
       execution_id INTEGER PRIMARY KEY,
       prompt TEXT NOT NULL DEFAULT '',
       delivered INTEGER,
       self_rating INTEGER,
       what_went_well TEXT NOT NULL DEFAULT '',
       what_to_improve TEXT NOT NULL DEFAULT '',
       critique TEXT NOT NULL DEFAULT '',
       produced_output INTEGER NOT NULL DEFAULT 0,
       wrote_files INTEGER NOT NULL DEFAULT 0,
       truncated INTEGER NOT NULL DEFAULT 0,
       recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
     );";

/// `reflections` DDL at [`SCHEMA_VERSION`](crate::migrations::SCHEMA_VERSION) — the durable, unified home for
/// lessons and self-critiques (superset of the loose `.stella/private/reflections.jsonl`
/// and the context.db memory nodes). `execution_id` is NULL for cross-turn
/// lessons; `domains` is a JSON array of domain tags. No secondary index:
/// every reader (the JSON export, the observatory's recency feed) walks the
/// whole table; the v7-era `reflections_by_kind` index served no query and
/// was dropped in v17.
pub(crate) const REFLECTIONS_DDL: &str = "CREATE TABLE IF NOT EXISTS reflections (
       id INTEGER PRIMARY KEY AUTOINCREMENT,
       execution_id INTEGER,
       kind TEXT NOT NULL,
       content TEXT NOT NULL,
       domains TEXT NOT NULL DEFAULT '[]',
       occurred_at INTEGER NOT NULL
     );";

/// `tasks` DDL at [`SCHEMA_VERSION`](crate::migrations::SCHEMA_VERSION) — the latest task-board snapshot per
/// session, one row per (session, task id), mirrored from the protocol's
/// `TaskUpdate` snapshots by [`Store::record_task_board`](crate::Store::record_task_board). UNIQUE
/// (session_id, task_id) is the upsert key: each snapshot REPLACES a task's
/// row (board state, not history — the `events` stream already keeps every
/// snapshot). NOTE: SQL NULLs are pairwise distinct, so rows recorded
/// without a session id never conflict — dedup only holds per session.
/// `status`/`owner` carry the protocol's serde snake_case strings (e.g.
/// `"in_progress"`); `task_id` is the board's per-session ordinal id
/// ("1", "2", …), read back in `CAST(task_id AS INTEGER)` order.
pub(crate) const TASKS_DDL: &str = "CREATE TABLE IF NOT EXISTS tasks (
       id INTEGER PRIMARY KEY AUTOINCREMENT,
       execution_id INTEGER NOT NULL,
       session_id TEXT,
       task_id TEXT NOT NULL,
       subject TEXT NOT NULL,
       description TEXT,
       status TEXT NOT NULL,
       owner TEXT,
       updated_at INTEGER NOT NULL,
       UNIQUE(session_id, task_id)
     );";

/// `pull_requests` DDL at [`SCHEMA_VERSION`](crate::migrations::SCHEMA_VERSION) — one row per tracked pull
/// request, keyed by URL (the one stable identity across forks/renames).
/// UNIQUE (url) is the upsert key for [`Store::upsert_pull_request`](crate::Store::upsert_pull_request): a
/// later observation of the same PR updates its status/CI verdict in place.
/// `session_id` is the producing session's registry id, NULL when unknown;
/// `updated_at` is epoch millis of the latest observation.
pub(crate) const PULL_REQUESTS_DDL: &str = "CREATE TABLE IF NOT EXISTS pull_requests (
       id INTEGER PRIMARY KEY AUTOINCREMENT,
       session_id TEXT,
       url TEXT NOT NULL,
       number INTEGER,
       status TEXT NOT NULL,
       ci_status TEXT,
       updated_at INTEGER NOT NULL,
       UNIQUE(url)
     );";

/// `context_blocks` DDL at [`SCHEMA_VERSION`](crate::migrations::SCHEMA_VERSION)
/// — the context-receipts block registry (v11, spec §4). One row per durable,
/// individually attributable unit that entered a step's prompt, born once and
/// keyed by its content-addressed `block_id` (`blk_…`). Content-free: it stores
/// a `content_digest`, never the payload bytes — the preimage lives in the
/// originating event in the journal. `call_id`/`memory_id` are the join keys
/// back to the tool call or memory node that produced the block.
/// UNIQUE via PRIMARY KEY (execution_id, block_id): a block is registered once
/// per execution; a byte-identical block re-entering context resolves to the
/// same id, so the second registration is an idempotent no-op, not data.
///
/// `token_cost` is nullable as of v19 (#925), meaning "not derivable from any
/// preimage this store still holds" — see
/// [`ContextBlockRow::token_cost`](crate::ContextBlockRow::token_cost). A fresh
/// file will never contain one: the emitter has the content in hand, so it
/// always writes a cost. The column is nullable here anyway so a migrated file
/// and a fresh file have the same shape, which is worth more than a constraint
/// that only history can violate.
pub(crate) const CONTEXT_BLOCKS_DDL: &str = "CREATE TABLE IF NOT EXISTS context_blocks (
       execution_id INTEGER NOT NULL,
       block_id TEXT NOT NULL,
       kind TEXT NOT NULL,
       origin_turn INTEGER NOT NULL,
       origin_step INTEGER NOT NULL,
       call_id TEXT,
       memory_id TEXT,
       token_cost INTEGER,
       content_digest TEXT NOT NULL,
       citation_label TEXT,
       content TEXT,
       first_seen_ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
       PRIMARY KEY (execution_id, block_id)
     );
     CREATE INDEX IF NOT EXISTS context_blocks_by_memory
       ON context_blocks(memory_id, execution_id);";

/// `step_manifest` DDL at [`SCHEMA_VERSION`](crate::migrations::SCHEMA_VERSION)
/// — the per-step request manifest, normalized one row per (step, block) in
/// wire order (v11, spec §5). This is the receipt that makes any past step
/// reconstructable: replaying the fold + this ordering yields exactly what the
/// model saw. PRIMARY KEY (execution_id, turn_instance, step, call_seq, ordinal)
/// is the wire position; `call_seq` separates the several model calls that can
/// share one step (worker 0, overflow summarizer 1, allocated management roles
/// 2+ — v13). `call_id` is the per-occurrence tool-call attribution (v15):
/// block ids are content-addressed, so `context_blocks.call_id` names only the
/// call that first minted a block, and duplicates would otherwise be
/// unattributable. The second index is the reverse lookup (every step a given
/// block was resident) that cost-of-carry and eviction analysis read.
pub(crate) const STEP_MANIFEST_DDL: &str = "CREATE TABLE IF NOT EXISTS step_manifest (
       execution_id INTEGER NOT NULL,
       turn_instance INTEGER NOT NULL,
       step INTEGER NOT NULL,
       call_seq INTEGER NOT NULL DEFAULT 0,
       ordinal INTEGER NOT NULL,
       block_id TEXT NOT NULL,
       cache_zone TEXT NOT NULL,
       resident_since_step INTEGER NOT NULL,
       message_index INTEGER NOT NULL DEFAULT 0,
       call_id TEXT,
       PRIMARY KEY (execution_id, turn_instance, step, call_seq, ordinal)
     );
     CREATE INDEX IF NOT EXISTS step_manifest_by_block
       ON step_manifest(execution_id, block_id);";

/// `step_receipt` DDL at [`SCHEMA_VERSION`](crate::migrations::SCHEMA_VERSION)
/// — the manifest header, one row per committed step (v11, spec §5/§12): which
/// model served it, and the budget the compaction pass actually compared
/// against (raw budget / calibration factor), so the receipt's numbers line up
/// with the decision that was made. This is the increment-1 header only: the
/// per-zone cache columns (spec §7) are NOT built yet, and when they are they
/// arrive as an additive migration on top of this shape.
/// PRIMARY KEY (execution_id, turn_instance, step, call_seq): one receipt per
/// model call, not per step — a step can carry the worker call plus the
/// auxiliary calls that ride it (v13).
pub(crate) const STEP_RECEIPT_DDL: &str = "CREATE TABLE IF NOT EXISTS step_receipt (
       execution_id INTEGER NOT NULL,
       turn_instance INTEGER NOT NULL,
       step INTEGER NOT NULL,
       call_seq INTEGER NOT NULL DEFAULT 0,
       provider TEXT NOT NULL,
       model TEXT NOT NULL,
       call_role TEXT NOT NULL,
       effective_budget_tokens INTEGER NOT NULL,
       calibration_factor REAL NOT NULL,
       estimated_input_tokens INTEGER NOT NULL,
       compiled_frame_id TEXT,
       frame_hash TEXT,
       PRIMARY KEY (execution_id, turn_instance, step, call_seq)
     );";

/// `foundry_tools` DDL at [`SCHEMA_VERSION`](crate::migrations::SCHEMA_VERSION) — the tool-foundry
/// adoption ledger (#830): one row per self-authored tool this workspace has
/// approved, holding the witness that proved it and the human decision that
/// enabled it.
///
/// `name` is the PRIMARY KEY because one tool name is one capability: a second
/// adoption of the same name is a *replacement* (new bytes, new proof), not a
/// second grant, and the writer's upsert clears `enabled` accordingly.
///
/// The digests are the tamper baseline. They pin the exact manifest and script
/// bytes the witness ran against, so an edit after adoption is detectable
/// rather than inherited — the same tamper-exclusion posture the pipeline's
/// witness protocol takes toward its own test artifact.
///
/// `enabled` defaults to 0 and `enabled_at` to NULL, which is the schema-level
/// statement of #830's guardrail: adoption alone grants nothing.
///
/// No index beyond the implicit one on the primary key. The table holds one
/// row per self-authored tool in a workspace — single digits, realistically —
/// and every reader either fetches by name or scans the lot.
pub(crate) const FOUNDRY_TOOLS_DDL: &str = "CREATE TABLE IF NOT EXISTS foundry_tools (
       name TEXT PRIMARY KEY,
       signature TEXT NOT NULL DEFAULT '',
       manifest_digest TEXT NOT NULL,
       script_digest TEXT NOT NULL,
       witness TEXT NOT NULL DEFAULT '',
       witness_input TEXT NOT NULL DEFAULT '{}',
       witness_expect TEXT NOT NULL DEFAULT '',
       enabled INTEGER NOT NULL DEFAULT 0,
       adopted_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
       enabled_at TEXT
     );";

/// One turn's precomputed workspace diff (#1870): the `stella-diff`-shaped
/// hunks between the work journal's `refs/stella/<session>/turn/<n-1>` and
/// `turn/<n>` marks, computed by the session that owns the journal at the
/// moment it marks the turn and persisted here so the observatory can replay
/// a turn's file changes without ever opening the bare repo (the dashboard
/// reads artifacts and spawns nothing — the #1870 boundary ruling, recorded
/// in `stella-cli`'s `turn_diff` module).
///
/// `execution_id` ties the journal's turn ordinal to the store's turn
/// identity (one turn is one execution) — the two numbering schemes have no
/// other persisted correspondence, and the Sessions view joins through it.
/// Nullable: a turn can end without having opened an execution row. The
/// `UNIQUE (session_id, turn)` upsert key makes a re-recorded turn a
/// replacement, matching the ref namespace it mirrors, where a turn mark is
/// a single ref.
pub(crate) const SESSION_TURN_DIFFS_DDL: &str = "CREATE TABLE IF NOT EXISTS session_turn_diffs (
       session_id   TEXT NOT NULL,
       turn         INTEGER NOT NULL,
       execution_id INTEGER,
       recorded_at  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
       files        TEXT NOT NULL,
       UNIQUE (session_id, turn)
     );";
