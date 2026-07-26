# stella-store

Local SQLite persistence for everything a session produces: one row per
execution, the COMPLETE `AgentEvent` stream (reasoning deltas included), per-call
telemetry, context receipts, task boards, and cooperative file claims — all on
the user's disk, no server and no account.

The crate's hard boundary is **facts, not policy**. It deliberately does not
depend on `stella-model`: no pricing table, no cache TTL, no diagnosis lives here
— [`cache_gaps.rs`](src/cache_gaps.rs) and [`cache_trend.rs`](src/cache_trend.rs)
surface the raw rows and the caller applies the policy. It does not parse the
rule markdown it stores (`stella_core::rules` does). And it never takes a turn
down: every method returns `Result` and the CLI treats a failed store as
observability loss — warn once, keep running. A panic here would be a work
stoppage, which is why `Store::migrate` rejects a negative `user_version` with an
error rather than indexing `MIGRATIONS` with a wrapped `usize`.

## Where it sits

A leaf: its only workspace dependency is `stella-protocol` (`AgentEvent`,
`TaskItem`, `ToolOutput`), plus `rusqlite` (bundled), serde, `base64`, `sha2`,
`rand`, and `libc` on unix for the session registry's `kill(pid, 0)` liveness
check. No binary. `stella-cli` is the main consumer (`Store::open`, the session
registry, notifications, the journal); `stella-fleet` uses `Store` for its file
claims; `stella-tools` uses only the private-path helpers, to reach `codegraph.db`.

## Layout

| File | What it holds |
|---|---|
| [`src/lib.rs`](src/lib.rs) | The `Store` handle, `open`/`in_memory`/`migrate`, the row types, and most of the query surface. Start here. |
| [`src/ddl.rs`](src/ddl.rs) | Every table and index as DDL at the **current** `SCHEMA_VERSION`, plus the `TABLES` allowlist. The one place today's shape is written down. |
| [`src/migrations.rs`](src/migrations.rs) | The ordered `MIGRATIONS` list, the fresh-file bootstrap, and the transactional runner. Open it to add a version. |
| [`src/content_free.rs`](src/content_free.rs) | The zero-egress guard: the reviewed hub-column allowlist and the sentinel harness every egress encoder registers with. |
| [`src/usage.rs`](src/usage.rs) | `~/.stella/usage.db` — the cross-project hub: replication cursor, `prune`, and the cloud-drain staging/ack/quarantine surface. |
| [`src/drain.rs`](src/drain.rs) | The versioned drain wire contract (`DrainBatch`/`DrainRow`), the `CloudIntake` seam, and `drain_org`'s poison-row bisection. |
| [`src/enterprise_telemetry.rs`](src/enterprise_telemetry.rs) + [`src/enterprise_telemetry/`](src/enterprise_telemetry) | The closed-schema `StellaOperationalEventV1` and its bounded, at-least-once host-owned spool. Delivery is the CLI's job. |
| [`src/catalog.rs`](src/catalog.rs) | `catalog.db` — model cards, append-only pricing versions, and the alias join table telemetry is resolved through. |
| [`src/sessions.rs`](src/sessions.rs), [`src/journal.rs`](src/journal.rs), [`src/notify.rs`](src/notify.rs) | The file-backed stores: one JSON file per session / per notification, and the per-session sidecar (`journal.jsonl`, `history.json`, `queue.json`) that makes a session resumable. |
| [`src/durable.rs`](src/durable.rs) | **The** durability contract (#617): `write_atomic` (temp + fsync + rename + parent fsync, the one implementation every crate calls) and `ensure_converged_schema_version`, the `PRAGMA user_version` policy for the convergence-schema sidecars. Read its header before adding any write path. |
| [`src/private.rs`](src/private.rs), [`src/home.rs`](src/home.rs), [`src/identity.rs`](src/identity.rs) | Where state is allowed to live (`.stella/private/`: 0700 dirs, 0600 files, writes through [`durable`](src/durable.rs); `~/.stella` resolution) and the `org_id`/`workspace_id`/`repo_id`/`project_id` scoping that is infallible by design. |
| [`src/receipts.rs`](src/receipts.rs), [`src/reconstruct.rs`](src/reconstruct.rs) | Context-receipt writes (block registry, per-step manifest) and the byte-exact reconstruction of what a model actually saw. |
| [`src/telemetry.rs`](src/telemetry.rs) | `TelemetryRow`, the per-call write path, and the execution-level accounting boundary. |
| `src/tests.rs`, `src/private_state_tests.rs`, `src/quarantine_tests.rs`, `src/usage_completeness_tests.rs` | `#[cfg(test)]` modules; the last three are named witnesses for private-state permissions, quarantine behaviour, and fail-closed accounting. |

## Key concepts

### The schema lives in two files, and they answer different questions

[`ddl.rs`](src/ddl.rs) says what the shape *is*; [`migrations.rs`](src/migrations.rs)
says how an existing file *gets there*. A fresh database gets `create_latest_schema`
in one shot and is stamped at `SCHEMA_VERSION` (`= MIGRATIONS.len()`, 13 today);
an existing one runs each pending step. `PRAGMA user_version` 0 is ambiguous — it
is both "fresh empty file" and "legacy pre-versioning file" — so `Store::migrate`
disambiguates by probing `TABLES` via `any_store_table_exists`. A file stamped
**newer** than this build knows is refused outright: older code writing into a
newer shape would silently violate whatever that schema added.

`apply_migration` opens the transaction, runs the step, runs
`pragma_foreign_key_check`, stamps the new `user_version` *inside that same
transaction*, and commits — so a crash mid-migration rolls back to the old
version **and** the old shape, never a mix. It also suspends `PRAGMA foreign_keys`
outside the transaction and restores it after commit-or-rollback, because SQLite
silently ignores that pragma inside one: the full `lang_altertable` §7 procedure,
followed even though no store table declares a foreign key today, so a future one
cannot be corrupted by a rebuild.

Two rules follow. **A shipped migration must keep producing its own era's shape**:
`migrate_v0_to_v1` re-creates `executions` and `files_touched` with their v1
columns *inline*, not from `ddl.rs`, because the v2 rebuild and the v8 `ALTER`
run against them afterwards — editing an old migration to match today's DDL
breaks every old file on disk. And **most DDL carries `IF NOT EXISTS`** so one
constant serves both the fresh path and an additive migration; the three
name-parameterized functions (`events_ddl`, `telemetry_ddl`, `files_touched_ddl`)
exist because a §7 rebuild creates the new shape under a scratch name first.

### `content_free.rs` — the egress allowlist is a privacy review

AGENTS.md invariant #3 says prompts, paths, tool payloads/results, reasoning,
errors, git state, memories, rules, and local identifiers are never exportable.
That used to hold *by construction*; nothing prevented a future column or struct
field from quietly carrying content. This module makes it a red gate, in two
halves:

1. **Schema allowlist.** `HUB_TELEMETRY_COLUMNS` is the reviewed column set of
   the hub `telemetry` table — the only table a cloud drain reads.
   `hub_telemetry_schema_matches_the_reviewed_allowlist` compares it against the
   live `PRAGMA table_info`, so adding a column fails the build until the
   allowlist is edited in the same PR. That edit *is* the forcing function.
   `SUSPICIOUS_KEY_SUBSTRINGS` ("prompt", "path", "diff", "stderr", …) is the
   second opinion: naming a column `prompt_preview` trips
   `no_allowlisted_hub_column_reads_as_content` even after the allowlist edit landed.
2. **Encoder sentinel harness.** Every encoder that puts bytes on the network
   implements `ContentFreeEncoder` and joins `registered_encoders()` — today
   `NativeDrainGuard` and `EnterpriseOperationalGuard`. `audit_encoder` feeds it
   `poisoned_cloud_event()` / `poisoned_execution_rollup()`, whose content-bearing
   fields carry `CONTENT_SENTINEL`, `PATH_SENTINEL`, and two UUID sentinels, then
   substring-searches the encoded bytes. Those fixtures are **exhaustive struct
   literals**, so adding a field to `CloudTelemetryEvent` or `ExecutionRollupRow`
   fails to *compile* here until someone decides what the fixture puts in it.

Two details keep the guard from passing vacuously. `PASSTHROUGH_MARKER` is an allowlisted
value the encoder must echo — without it an encoder could pass every sentinel
check by quietly encoding a clean hand-rolled fixture (`FixtureNotUsed`).
`DRAIN_FORMATS` lists every drain `format` the epic names, so an unbuilt encoder
is a `NotYetBuilt` **declared gap** — building it without registering a guard
fails `every_built_drain_format_has_a_registered_encoder`. And the leak message
is deliberate: *remove the field from the encoding — do not widen the allowlist*.
A leak here is a privacy incident, not a test failure.

### Which databases this crate owns

| Path | Opened by | Versioning |
|---|---|---|
| `.stella/private/store.db` | `Store::open(workspace_root)` | `PRAGMA user_version` + the ordered `MIGRATIONS` list |
| `~/.stella/usage.db` | `UsageStore::open_default()` | **Convergence**, not migrations — every table in `USAGE_SCHEMA` is `CREATE … IF NOT EXISTS` and the whole batch replays on open |
| `~/.stella/catalog.db` | `CatalogStore::open_default()` | `CATALOG_SCHEMA` batch on open plus its own `migrate` |
| The enterprise operational spool | `EnterpriseTelemetrySpool`, at an **already policy-checked path** the caller supplies (`stella-cli` picks `~/.stella/enterprise-telemetry.db`) | Its own `migrate_spool_schema` |

Plus three file-backed stores under `~/.stella/`: `sessions/` (one JSON record
per session, one writer per file), `sessions/<id>/` (the resume sidecar), and
`notifications/`. [`home.rs`](src/home.rs) resolves `~/.stella` on every platform
— no OS data-dir guessing — with `STELLA_HOME` moving the whole home and
`STELLA_DATA_DIR` / `STELLA_CONFIG_DIR` as narrower overrides.

Flow between the two SQLite tiers is **one-way**: `store.db` → `usage.db` via a
durable per-project cursor; nothing writes back, and a missing or unopenable hub
never blocks a turn. Not owned here: `context.db`, `codegraph.db`, and `fleet.db`
belong to `stella-context`, `stella-graph`, and `stella-fleet` — but they reach
their files through this crate's `workspace_private_sqlite_path`, so the
`.stella/private/` hardening applies to them too.

## Gotchas

- Accounting is **fail-closed**: `executions.usage_complete`/`usage_status`
  (v9/v10) make only an execution finalized via
  `finish_execution_accounted(.., true)` rollupable — otherwise
  `Store::execution_rollup` returns `None`, so a turn whose accounting never
  closed out cannot become an operational event, and v8 → v9 backfills every
  legacy row to *not* complete. Witnesses: `src/usage_completeness_tests.rs`.
- The `UNIQUE (execution_id, seq)` / `(execution_id, step)` keys on `events`,
  `telemetry`, `mcp_usage`, and `tool_calls` are **double-write guards, not
  dedup**. The writers own monotonic counters, so a collision means one logical
  record was persisted twice — for `telemetry` that double-counts money. Let it
  error; do not turn it into an upsert.
- `tasks` is keyed `UNIQUE(session_id, task_id)` and `session_id` is nullable.
  SQL NULLs are pairwise distinct, so rows recorded without a session id **never
  conflict** — the upsert dedup only holds per session.
- `record_event` reads an event's `type` tag by *deserializing* a one-field
  struct, never by string-scanning for the first `"type":"` — a scan silently
  yields the wrong tag if serialization is ever pretty-printed or reordered.
- Failures in `harden_workspace_dir` are **not** swallowed: if `.stella/` cannot
  be made 0700, or given a `.gitignore` covering `private/`, `Store::open`
  refuses to open — such a directory is one commit away from publishing a
  session's transcripts. That ignore is deliberately not `*`; `settings.json`,
  `mcp.toml`, `tools/`, and `skills/` are meant to be committable.
- `acquire_file_lock` never refreshes `acquired_at`, so the age-based
  `prune_stale_file_locks` sweep eventually sees a long healthy run's own live
  claims as stale. `release_file_locks_of_dead_holders` is the exact check
  (holder identities end their owner prefix in the minting pid), and an identity
  that does not parse is assumed **alive**.

## Testing

```bash
cargo test -p stella-store
```

No crate-specific `make` target exists; `make gate` runs `cargo test --workspace`.
Most tests are `#[cfg(test)]` modules inside `src/` (`src/tests.rs` is the bulk),
built on `Store::in_memory()` and `tempfile` — no fixtures, flags, or env vars
needed. The one integration test,
[`tests/enterprise_telemetry.rs`](tests/enterprise_telemetry.rs), exercises the
spool's lease/quarantine/clock behaviour against a real file. The content-free
gate's tests sit inside `src/content_free.rs` and include negative witnesses
(`harness_catches_a_leaking_encoder`, `harness_catches_an_unreviewed_key`) that
fail if the harness itself stops catching leaks.

## Extending it

**Add a migration** (any change to `store.db`'s shape):

1. Add the new or changed DDL to [`src/ddl.rs`](src/ddl.rs) at the current shape,
   wire it into `create_latest_schema` if it is a new table, and add the table
   name to `TABLES` — that list gates `Store::count` and the fresh-file probe.
2. Append `migrate_vN_to_vN+1` to `MIGRATIONS` in
   [`src/migrations.rs`](src/migrations.rs) with a one-line comment saying what it
   does. `SCHEMA_VERSION` follows automatically. Do not touch earlier entries.
3. Additive changes are `CREATE TABLE IF NOT EXISTS` / column-guarded
   `ADD COLUMN`. A constraint or primary-key change is a §7 rebuild (scratch name
   → `INSERT SELECT` → `DROP` → `RENAME`) and needs a keep-rule for pre-existing
   duplicates — see `migrate_v0_to_v1`.
4. If the column reaches `usage.db`'s `telemetry` table, `HUB_TELEMETRY_COLUMNS`
   in [`src/content_free.rs`](src/content_free.rs) must be edited in the same PR
   or the build fails. That is the point.

**Add an egress encoder:** implement `ContentFreeEncoder` (id, optional drain
`format`, `allowed_keys`, and `encode_poisoned_sample` built from the harness
fixtures), add it to `registered_encoders()`, and flip its `DRAIN_FORMATS` entry
from `NotYetBuilt` to `Guarded`. `every_registered_encoder_is_content_free` and
`every_built_drain_format_has_a_registered_encoder` fail until both are done.

## See also

- [`../AGENTS.md`](../AGENTS.md) — "Architecture: ports, not concretions"
  (invariant #3), "The `.stella/` directory", and the glossary of look-alike
  identifiers (session vs execution vs run vs task).
- [`../docs/design/session-telemetry-receipts-spec.md`](../docs/design/session-telemetry-receipts-spec.md)
  — the spec behind `context_blocks` / `step_manifest` / `step_receipt`.
- [`../docs/design/enterprise-authority-telemetry.md`](../docs/design/enterprise-authority-telemetry.md)
  — the enrolled-seat operational rollup this crate spools.
- [`../stella-protocol`](../stella-protocol) — `AgentEvent`, whose full stream
  this crate persists.
