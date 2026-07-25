# stella-observatory

The crate behind `stella observe`: a tiny HTTP server that reads the
workspace's own telemetry out of `.stella/private/*.db` and serves it as a
single embedded HTML page.

Three properties are security boundaries, not conveniences, and each is
enforced by construction rather than policy: the listener binds `127.0.0.1`
only, every database handle is opened `SQLITE_OPEN_READ_ONLY`, and the page is
`include_str!`'d with zero external references. Outside its tests, nothing here
opens an outbound connection, writes a file, or answers a method other than
`GET`. The crate also links **no other workspace crate**, and must not.

## Where it sits

A leaf. [`Cargo.toml`](Cargo.toml) lists `rusqlite`, `serde`, `serde_json`,
`sha2`, `thiserror`, `tokio` (the `net` feature only) and `toml` — no `stella-*`
dependency at all. That is deliberate: `stella_store::Store::open` creates
`.stella/` and runs schema migrations, and an observer that migrates what it
observes is not an observer. The price is two acknowledged copies —
`global::data_dir` and `project_id_for` ([`src/global.rs:27`](src/global.rs),
`:48`) mirror `../stella-store/src/usage.rs`, and `/api/explorations` re-hashes
exploration manifests itself.

Only [`stella-cli`](../stella-cli) depends on it: `run_observe`
([`../stella-cli/src/main.rs:847`](../stella-cli/src/main.rs)) preflights the
private store paths and calls `serve` (`--port`, default `7787`; `0` picks a
free one). This crate builds no binary —
[`examples/serve.rs`](examples/serve.rs) is the dev harness. It reads two tiers:
`<root>/.stella/private/{store,fleet,codegraph}.db` per workspace, and
`~/.stella/usage.db`, the cross-project hub.

## Layout

| File | What it holds |
|---|---|
| [`src/lib.rs`](src/lib.rs) | The HTTP responder: the route table, the `Host` and head-cap gates, the CSP, `serve`. Open it to add a route or to touch anything security-relevant. |
| [`src/db.rs`](src/db.rs) | Every query against `.stella/private/store.db` and `fleet.db`. Open it when a panel needs a new aggregate; the SQL deliberately mirrors `stella stats` semantics (resolved = outcome `completed`, `off-grid` = provider `local`). |
| [`src/global.rs`](src/global.rs) | The user-tier view over `~/.stella/usage.db`: the project switcher (`/api/projects`, `?project=`) and the hub-telemetry drill (org → workspace → repo → project). |
| [`src/fsview.rs`](src/fsview.rs) | Views derived from files rather than SQL — skills, memories, rule files, `reflections.jsonl` lessons, `mcp.toml`, the settings scope chain, exploration maps — plus `redact`, the credential scrubber. |
| [`src/codegraph.rs`](src/codegraph.rs) | `codegraph.db` flattened to `{nodes, edges, groups}` for the force-directed canvas, including the Rust module-path resolution the indexer doesn't do. |
| [`src/assets/index.html`](src/assets/index.html) | The entire dashboard — markup, styles and script in one file, embedded at compile time. |
| `src/assets/mark.svg`, `src/assets/wordmark.svg` | Favicon and header lockup, served from `/assets/`. |
| [`examples/serve.rs`](examples/serve.rs) | `cargo run -p stella-observatory --example serve -- <root> <port>` — serve any workspace without building the CLI. |

## Key concepts

### The three boundaries, and what actually enforces each

**Loopback-only** starts at `TcpListener::bind(("127.0.0.1", port))`
(`src/lib.rs:270`), but the bind is not the boundary: a web page can resolve an
attacker-controlled name to `127.0.0.1` and read this dashboard cross-origin.
Three things close that. `host_is_local` (`src/lib.rs:291`) refuses any `Host`
that does not *parse* as a loopback IP — never a prefix test, because
`127.0.0.1.attacker.example` is a registrable name `starts_with("127.")` waves
through. `handle` refuses an unterminated request head with `431` *before*
routing (`src/lib.rs:342`): padding the request line past the 8 KiB cap used to
push `Host` out of the buffer entirely, and the no-`Host` allowance then waved
the rebound request through to a route that still parsed out of the truncated
path. And `frame-ancestors 'none'` in the CSP stops that page framing it
instead. The no-`Host` allowance itself is deliberate — a browser `fetch`
always sends one, so its absence means raw curl or a test, not the attack.

**Read-only** is `OpenFlags::SQLITE_OPEN_READ_ONLY` at all three open sites —
`db::open_read_only` (`src/db.rs:716`), `global::open_usage`
(`src/global.rs:59`), `codegraph::snapshot` (`src/codegraph.rs:38`) — each with
a 5 s `busy_timeout`, so a checkpoint or a migration's exclusive lock makes a
poll wait rather than 500. `Observatory::new` opens nothing, and each open
returns `None` for a missing file rather than creating it. Outside
`#[cfg(test)]` nothing here writes to the filesystem, and non-`GET` requests get
`405` (`src/lib.rs:362`), so there is no mutation verb to reach.

**Embedded** is three `include_str!`s (`src/lib.rs:47`–`51`).
`dashboard_html_has_no_external_references` fails the suite if `index.html` ever
contains `http://`, `https://`, `//cdn`, `@import` or `integrity=`, and the
`CSP` constant (`src/lib.rs:63`) restates that to the browser (`default-src
'self'`, `connect-src 'self'`, `img-src 'self'`). `'unsafe-inline'` on
`script-src`/`style-src` is unavoidable and load-bearing: the page is one
document with an inline `<script>`, an inline `<style>` and inline `style="…"`
attributes, so dropping it from `style-src` would strip the layout silently.
`csp_admits_everything_the_embedded_dashboard_actually_uses` pins both halves.

### `respond` is pure; there is no server state

`respond(workspace_root, path)` (`src/lib.rs:113`) maps a path to a `Response`
and is what the unit tests drive — no sockets involved. Every request opens its
own connections and drops them; the page re-polls every 5 s while its tab is
visible. The one twist is `?project=<id>`, accepted by every `/api/*` route: the
id is resolved by `global::resolve_project_root` (`src/global.rs:138`) against
the rollup's own `projects` table — never a path supplied by the client — and
that root replaces the serving root for the request. Unknown ids fall back to
the serving workspace rather than erroring, so a stale dropdown cannot break the
page. `/api/projects` and `/api/hub-telemetry` are cross-project by nature and
keep the original root.

### `execution_id` and `run_id` are different entities

[`src/db.rs`](src/db.rs) joins both. `execution_id` is the store's unit of work
— one row in `executions`, the foreign key `telemetry`, `tool_calls`,
`files_touched` and `execution_reflection` hang off; every join in
`Observatory::executions`, `execution`, `models` and `activity` is on it.
`run_id` appears only in `Observatory::fleet` (`src/db.rs:658`), against a
different file (`fleet.db`) and a different hierarchy: a fleet run fans out to
tasks, then attempts, then commits. It is not an execution and not a session,
and no query may join the two. AGENTS.md's glossary is the authority.

### Missing is a state, never an error

A workspace that has never run `stella` renders an empty dashboard, not a 500.
An absent file yields `None` and an empty payload at the call site; an absent
*table* is caught by `is_missing_table` (`src/db.rs:779`), degrading
`collect_rows`/`or_empty` to `[]`/`{}`. `global.rs` goes further — `query_rows`
treats any `prepare` failure as empty, because a `usage.db` predating the hub
replica has no `telemetry` table at all.

### Secrets never reach the browser

`settings.json` and `mcp.toml` carry credentials, and both are served. `redact`
(`src/fsview.rs:376`) replaces every string at or below a *credential scope* —
a key `sensitive_key` matches, or an `env`/`headers` map whose values are
credentials by position. The scope is **inherited, not recomputed per level**:
settings are arbitrary user JSON, so a secret can sit a container below the key
naming it (`{"api_keys": ["sk-live-…"]}`), and redacting only the direct string
child leaked that. Keys ending `_env` are exempt — they name a variable, not
its value. `mcp_servers` never reads env or header *values* at all.

## Gotchas

- **The page is embedded at compile time.** Editing
  [`src/assets/index.html`](src/assets/index.html) does nothing until you
  rebuild — `--example serve` included.
- **The test schema is a hand-written copy.** `seeded_workspace` in `src/lib.rs`
  spells out its own DDL for the subset of tables the observatory reads, and
  nothing checks it against `../stella-store/src/ddl.rs`. A column renamed in
  the store keeps this suite green and breaks the dashboard at runtime.
- **Store paths are hardcoded**, `<root>/.stella/private/<name>` — this crate
  can't use `stella-store`'s path resolver (see *Where it sits*), so a
  workspace still in the pre-`private/` legacy layout renders empty.
  `stella observe` sidesteps that: the CLI's `preflight_observatory_stores`
  resolves and migrates first. `--example serve` does not.
- **There is no server-side time filter.** The `24h`/`7d`/`30d` window is a
  client-side constant. `/api/overview`, `/api/executions`, `/api/activity` and
  `/api/tools` carry no `LIMIT`, and `Observatory::tools` sorts every
  `tool_calls` row ever recorded for an exact p50 (leaderboards cap at 50–100).
- **`percent_decode` (`src/lib.rs:229`) parses attacker-reachable bytes.** A
  malformed escape (`%`, `%A`, `%ZZ`) must stay literal — never a panic, never
  a dropped byte — and decoding runs over bytes before UTF-8 validation so a
  multi-byte character split across escapes reassembles.
- **`STELLA_DATA_DIR` is process-wide.** The two tests that set it serialize on
  `DATA_DIR_LOCK`, poison-tolerantly; a third that mutates it without the lock
  will flake the other two.
- **The code graph is capped at 600 nodes** (`MAX_NODES`, `src/codegraph.rs:22`):
  the highest-degree files are kept and the rest reported as `truncated`, so a
  large workspace's graph is a sample, not the whole index.

## Testing

```bash
cargo test -p stella-observatory
```

There is no crate-specific `make` target — the root `Makefile` has one only for
core/model/tools/cli/protocol — so `make test` picks this up with the workspace.
Every test is an inline `#[cfg(test)] mod tests`: no `tests/` directory, no
fixture dir, feature flag or env var.

The dominant shape: `seeded_workspace()` builds a `TempDir` with a `store.db`
seeded from hand-written DDL, `seed_fs_surfaces()` layers on the file-backed
surfaces (skills, memories, rules, `reflections.jsonl`, `mcp.toml`,
`settings.json`, `codegraph.db`), then the test calls `respond()` and asserts on
the parsed JSON. Two `#[tokio::test]`s use a real socket on port 0 for what
`respond` cannot cover: the response head (CSP, `nosniff`) and the head-cap
refusal. `empty_workspace_degrades_to_empty_payloads_not_errors` asserts
`200 OK` for every route against a bare `TempDir` — a new route belongs in that
list.

## Extending it

Adding an API route:

1. Write the query or view function in the module that owns the source —
   [`src/db.rs`](src/db.rs) for `store.db`/`fleet.db`,
   [`src/global.rs`](src/global.rs) for `usage.db`,
   [`src/fsview.rs`](src/fsview.rs) for files,
   [`src/codegraph.rs`](src/codegraph.rs) for the graph. Degrade a missing
   file or table to an empty payload; do not return an error.
2. Add the arm to the `match route` in `respond` (`src/lib.rs:127`). Take
   filters via `query_param`, never by splitting the query string yourself.
3. Add the path to `empty_workspace_degrades_to_empty_payloads_not_errors`'s
   list — it fails until step 1's degradation is right — then seed the table or
   file in `seeded_workspace`/`seed_fs_surfaces` and assert the payload.
4. Render it in [`src/assets/index.html`](src/assets/index.html) and add the
   route to the refresh list. Everything you add to the page must be inline:
   `dashboard_html_has_no_external_references` fails on the first external URL,
   and the CSP would block it in the browser anyway.

A new file or store also has to be audited for credentials first — route it
through `redact`, or emit key names only, the way `mcp_servers` does.

## See also

- [`../AGENTS.md`](../AGENTS.md) — "Glossary — the identifiers that look alike"
  (the `execution_id` / `run_id` distinction this crate joins across), "The
  `.stella/` directory (per-workspace state)" for what each store holds, and
  invariant 3, "Zero telemetry egress by default".
- [`../docs/design/exploration-sharing.md`](../docs/design/exploration-sharing.md)
  §4e — the exploration-map freshness verdict `fsview::explorations` computes.
- [`../website/content/docs/commands/observe.mdx`](../website/content/docs/commands/observe.mdx)
  and [`../website/content/docs/telemetry/dashboard.mdx`](../website/content/docs/telemetry/dashboard.mdx)
  — the user-facing flags and a tour of each tab.
- [`../stella-store`](../stella-store) writes everything this crate reads
  (`src/ddl.rs`, `src/usage.rs`); [`../stella-graph`](../stella-graph) writes
  `codegraph.db`; [`../stella-fleet`](../stella-fleet) writes `fleet.db`.
