# stella-observatory

The crate behind `stella observe`: a tiny HTTP server that reads the
workspace's own telemetry out of `.stella/private/*.db` and serves it as a
single embedded HTML page.

Three properties are security boundaries, not conveniences, and each is
enforced by construction rather than policy: the listener binds `127.0.0.1`
only, every database handle is opened `SQLITE_OPEN_READ_ONLY`, and the page is
`include_str!`'d with zero external references. Outside its tests, nothing here
opens an outbound connection, writes a file, or answers a method other than
`GET`. The crate links **no workspace crate that opens anything** — see *Where
it sits* for the two pure ones it does link, and why that is a different rule
than a dependency count.

## Where it sits

Nearly a leaf. [`Cargo.toml`](Cargo.toml) lists `rusqlite`, `serde_json`,
`sha2`, `thiserror`, `tokio` (the `net` feature only), `toml` — and two
`stella-*` dependencies: [`stella-home`](../stella-home), which has no
dependencies of its own, and [`stella-core`](../stella-core), for the
`self_driving` fold and its signal thresholds. Everything heavier is excluded
deliberately: `stella_store::Store::open` creates `.stella/` and runs schema
migrations, and an observer that migrates what it observes is not an observer.

**The rule is about the write path, not about the dependency count.** This
crate re-reads artifacts instead of linking the crates that produce them, which
is why it opens `store.db` with `rusqlite` rather than linking `stella-store`.
Both crates above are the opposite shape and neither opens anything:
`stella-home` is path arithmetic over environment variables, and
`stella-core::self_driving` is pure decision logic over owned data (no I/O, by
invariant 2) with the clock passed in as a parameter.

The price used to be four acknowledged copies. `global::data_dir` was one of
them — a hand-synced mirror of `../stella-store/src/usage.rs` with a comment
asking readers to keep it equal — and is now shared through `stella-home`
instead (#1139). `src/self_driving.rs` was another: a private `fold_runs` and
`self_improvement`, written when the only other implementation was shell, and
the two had already drifted — the dashboard and `stella self-driving metrics`
disagreed about whether the loop was NOISY for every odd cycle count, because
one tested `2 * new < n` and the other `new < n / 2` in integer arithmetic
(#1613). Both now come from `stella-core`. Three copies remain:
`project_id_for` ([`src/global.rs`](src/global.rs)) still mirrors the
store's, `/api/explorations` re-hashes exploration manifests itself, and
[`src/sent_context.rs`](src/sent_context.rs) re-implements the receipt
reconstruction `stella_store::Store::reconstruct_call` performs (#1475). The
last is the largest of the three and the only one with a *byte-level* coupling
— it rebuilds a `tool_call` block's preimage in `stella_protocol::ToolCall`'s
field order — so `tests/schema_conformance.rs` seeds its digests from that
crate's own serializer: a reordered field fails the suite instead of printing
"the journal is torn" on a user's dashboard.

Only [`stella-cli`](../stella-cli) depends on it: `run_observe`
([`../stella-cli/src/storage_cmd.rs:47`](../stella-cli/src/storage_cmd.rs))
preflights the private store paths and calls `serve` (`--port`, default `7787`; `0` picks a
free one). This crate builds no binary —
[`examples/serve.rs`](examples/serve.rs) is the dev harness. It reads two tiers:
`<root>/.stella/private/{store,fleet,codegraph}.db` per workspace, and
`~/.stella/usage.db`, the cross-project hub.

## Layout

| File | What it holds |
|---|---|
| [`src/lib.rs`](src/lib.rs) | The HTTP responder: the route table, the `Host` and head-cap gates, the CSP, `serve`. Open it to add a route or to touch anything security-relevant. |
| [`src/db.rs`](src/db.rs) | Every query against `.stella/private/store.db` and `fleet.db`. Open it when a panel needs a new aggregate; the SQL deliberately mirrors `stella stats` semantics (resolved = outcome `completed`, `off-grid` = provider `local`). |
| [`src/sent_context.rs`](src/sent_context.rs) | `/api/execution-context`: the receipt queries (`step_receipt`, `step_manifest`, `context_blocks`) and the fold that rebuilds the messages one model call was sent, with the digest-verification verdict. Kept out of `src/db.rs` so that file stays clear of the 1500-line ratchet. |
| [`src/global.rs`](src/global.rs) | The user-tier view over `~/.stella/usage.db`: the project switcher (`/api/projects`, `?project=`) and the hub-telemetry drill (org → workspace → repo → project). |
| [`src/fsview.rs`](src/fsview.rs) | Views derived from files rather than SQL — skills, memories, rule files, `reflections.jsonl` lessons, `mcp.toml`, the settings scope chain, exploration maps — plus `redact`, the credential scrubber. |
| [`src/self-driving.rs`](src/self-driving.rs) | The perpetual delivery loop's runs, cycles and controller state, read from `~/.stella/self-driving/<slug>/`. Plain JSONL, no database — see below for why the `crashed` status is computed here rather than read. |
| [`src/codegraph.rs`](src/codegraph.rs) | `codegraph.db` flattened to `{nodes, edges, groups}` for the force-directed canvas, including the Rust module-path resolution the indexer doesn't do. |
| [`src/assets/index.html`](src/assets/index.html) | The entire dashboard — markup, styles and script in one file, embedded at compile time. |
| `src/assets/mark.svg`, `src/assets/wordmark.svg` | Favicon and header lockup, served from `/assets/`. |
| [`examples/serve.rs`](examples/serve.rs) | `cargo run -p stella-observatory --example serve -- <root> <port>` — serve any workspace without building the CLI. |

## God files — do not add lines

This crate has no god files: no file exceeds the gate's 1500-line ratchet
(`scripts/check-file-size.sh`), and none may appear — a new file crossing
1500 lines fails the gate outright, and `scripts/file-size-baseline.txt`
accepts no new entries. When a file here approaches the limit, split it before
it crosses.

## Key concepts

### The three boundaries, and what actually enforces each

**Loopback-only** starts at `TcpListener::bind(("127.0.0.1", port))`
(`src/lib.rs:299`), but the bind is not the boundary: a web page can resolve an
attacker-controlled name to `127.0.0.1` and read this dashboard cross-origin.
Three things close that. `host_is_local` (`src/lib.rs:356`) refuses any `Host`
that does not *parse* as a loopback IP — never a prefix test, because
`127.0.0.1.attacker.example` is a registrable name `starts_with("127.")` waves
through. `handle` refuses an unterminated request head with `431` *before*
routing (`src/lib.rs:385`): padding the request line past the 8 KiB cap used to
push `Host` out of the buffer entirely, and the no-`Host` allowance then waved
the rebound request through to a route that still parsed out of the truncated
path. And `frame-ancestors 'none'` in the CSP stops that page framing it
instead. The no-`Host` allowance itself is deliberate — a browser `fetch`
always sends one, so its absence means raw curl or a test, not the attack.

**Read-only** is `OpenFlags::SQLITE_OPEN_READ_ONLY` at all three open sites —
`db::open_read_only` (`src/db.rs:780`), `global::open_usage`
(`src/global.rs:59`), `codegraph::snapshot` (`src/codegraph.rs:45`) — each with
a 5 s `busy_timeout`, so a checkpoint or a migration's exclusive lock makes a
poll wait rather than 500. `Observatory::new` opens nothing, and each open
returns `None` for a missing file rather than creating it. Outside
`#[cfg(test)]` nothing here writes to the filesystem, and non-`GET` requests get
`405` (`src/lib.rs:454`), so there is no mutation verb to reach.

**Embedded** is three `include_str!`s (`src/lib.rs:51`–`55`).
`dashboard_html_has_no_external_references` fails the suite if `index.html` ever
contains `http://`, `https://`, `//cdn`, `@import` or `integrity=`, and the
`CSP` constant (`src/lib.rs:84`) restates that to the browser (`default-src
'self'`, `connect-src 'self'`, `img-src 'self'`). `'unsafe-inline'` on
`script-src`/`style-src` is unavoidable and load-bearing: the page is one
document with an inline `<script>`, an inline `<style>` and inline `style="…"`
attributes, so dropping it from `style-src` would strip the layout silently.
`csp_admits_everything_the_embedded_dashboard_actually_uses` pins both halves.

### `respond` is pure; there is no server state

`respond(workspace_root, path)` (`src/lib.rs:134`) maps a path to a `Response`
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
`run_id` appears only in `Observatory::fleet` (`src/db.rs:722`), against a
different file (`fleet.db`) and a different hierarchy: a fleet run fans out to
tasks, then attempts, then commits. It is not an execution and not a session,
and no query may join the two. AGENTS.md's glossary is the authority.

### Missing is a state, never an error

A workspace that has never run `stella` renders an empty dashboard, not a 500.
An absent file yields `None` and an empty payload at the call site; an absent
*table* is caught by `is_missing_table` (`src/db.rs:843`), degrading
`collect_rows`/`or_empty` to `[]`/`{}`. `global.rs` goes further — `query_rows`
treats any `prepare` failure as empty, because a `usage.db` predating the hub
replica has no `telemetry` table at all.

### Secrets never reach the browser

`settings.json` and `mcp.toml` carry credentials, and both are served. `redact`
(`src/fsview.rs:493`) replaces every string at or below a *credential scope* —
a key `sensitive_key` matches, or an `env`/`headers` map whose values are
credentials by position. The scope is **inherited, not recomputed per level**:
settings are arbitrary user JSON, so a secret can sit a container below the key
naming it (`{"api_keys": ["sk-live-…"]}`), and redacting only the direct string
child leaked that. Keys ending `_env` are exempt — they name a variable, not
its value. `mcp_servers` never reads env or header *values* at all.

The one hole left in that claim is `mcp_servers`' `target`: it is `cmd` plus
`args` for a stdio server and `url` for an HTTP one, both served verbatim and
neither routed through `redact`. A token passed on the command line
(`--token=…`) or in a query string (`https://mcp.example/sse?key=…`) is a
credential sitting in a field the scrubber does not look at. Anything new that
reaches the browser must be audited the same way before it lands.

### The one input this crate does not own: exploration manifests

Everything else here is read out of a store stella wrote. An exploration record
is different — it travels with the tree and can be *ingested* from another
machine ([`../../docs/spec/exploration-sharing.md`](../../docs/spec/exploration-sharing.md)
§3), so its `path → sha256` manifest is untrusted text.
`fsview::is_workspace_relative` refuses any key that is absolute or contains
`..` before `/api/explorations` opens it, mirroring the `resolve_within_root`
guard the producer (`stella_tools::staleness`) already applies: without it, a
manifest keyed `"../../.ssh/id_rsa"` turns a freshness poll into an out-of-root
read whose verdict reports whether that file exists and whether its bytes hash
to a chosen value. The check is lexical, so a
symlink *inside* the workspace pointing out of it is still followed — a narrower
guarantee than the producer's canonicalising one, and the reason to keep the
manifest a list of paths rather than anything more expressive.

### The palette is a mirror, and one token in it is deliberately unused

The `:root` block at the top of `index.html` mirrors the comet brand kit
(v1.0) — Phosphor Gold `#FFB000` on Ink `#0B0B0C` over warm neutrals, set in
JetBrains Mono. [`../../docs/brand/`](../../docs/brand/README.md) is the normative
source (`css/tokens.css` holds the values); the same tokens live in
`website/src/app/tokens.css`, and the surfaces move together. The block
carries the whole core set even where this page uses only part of it. Do not
prune it to the tokens currently referenced.

One token is applied nowhere on purpose. `#E3B341`, the first categorical data
mark, may not share a chart with the gold `#FFB000`: the two are hue 42° and
hue 41° and measure 1.06:1 against each other — nothing but size tells them
apart. Gold is the signal — the accent, the focus ring, the comet in the
masthead — and never a data series, so `--c1` stands down: every chart on this
page pairs `--c4` (hue 175) with `--c2` (hue 256), 81° apart at 1.99:1, and
both clear the gold accent. Two further collisions the pairing avoids, both
measured on `--surface`: `--c3` against `--bad` is Δhue 18 at 1.30:1, so
magenta never appears in the tool-error chart, and `--c2` against the neutral
mark is Δhue 148 at 1.08:1, so violet never appears in the stacked runs chart.

The same constraint caps the code graph at three crate colours plus a neutral
tail. That is fewer than the eight it wants; the previous eight were invented
rather than drawn from the kit, and two of them read as "active" while
`#008300` measured 2.4:1 on this surface. Widening the ramp needs new
validated values in the brand kit, not new hexes here.

Gold carries no status anywhere. Status is `--ok` / `--warn` / `--bad`, always
paired with a glyph (`✓`, `◌`, `✕`), so hue is never the only carrier — which
matters doubly now that `--warn` sits in gold's hue region (1.05:1 against the
accent): the glyph and the badge context, never the hue, say "warning".

## Gotchas

- **The page is embedded at compile time.** Editing
  [`src/assets/index.html`](src/assets/index.html) does nothing until you
  rebuild — `--example serve` included.
- **The test schema is a hand-written copy, and it has already drifted.**
  `seeded_workspace` in `src/lib.rs` spells out its own DDL for the subset of
  tables the observatory reads, and nothing checks it against
  `../stella-store/src/ddl.rs` — nor does any open here read the
  `PRAGMA user_version` those migrations stamp. The store's shipped
  `executions` carries `session_id`, `usage_complete` and `usage_status`, and
  its `telemetry` carries `call_role` and `usage_complete`; the fixture has
  none of them. That divergence is harmless only because no query selects
  those columns. A column this crate *does* read, renamed in the store, keeps
  this suite green and breaks the dashboard at runtime.
- **Store paths are hardcoded**, `<root>/.stella/private/<name>` — this crate
  can't use `stella-store`'s path resolver (see *Where it sits*; `stella-home`
  answers where `~/.stella` is, not where a workspace's private store is), so a
  workspace still in the pre-`private/` legacy layout renders empty.
  `stella observe` sidesteps that: the CLI's `preflight_observatory_stores`
  resolves and migrates first. `--example serve` does not.
- **There is no server-side time filter.** The `24h`/`7d`/`30d` window is a
  client-side constant. `/api/overview`, `/api/executions`, `/api/activity` and
  `/api/tools` carry no `LIMIT`, and `Observatory::tools` sorts every
  `tool_calls` row ever recorded for an exact p50 (leaderboards cap at 50–100).
  Nothing prunes those tables — `stella-store` runs no retention sweep over
  `executions`/`telemetry`/`tool_calls` — so the payload, the per-request
  allocation and the page's re-render all grow with the workspace's entire
  history, and the tab re-fetches all of it every 5 s.
- **`percent_decode` (`src/lib.rs:258`) parses attacker-reachable bytes.** A
  malformed escape (`%`, `%A`, `%ZZ`) must stay literal — never a panic, never
  a dropped byte — and decoding runs over bytes before UTF-8 validation so a
  multi-byte character split across escapes reassembles.
- **`STELLA_DATA_DIR` is process-wide.** The two tests that set it serialize on
  `DATA_DIR_LOCK`, poison-tolerantly; a third that mutates it without the lock
  will flake the other two.
- **The code graph is capped at 600 nodes** (`MAX_NODES`, `src/codegraph.rs:29`):
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
refusal.

## Extending it

Adding an API route:

1. Write the query or view function in the module that owns the source —
   [`src/db.rs`](src/db.rs) for `store.db`/`fleet.db`,
   [`src/global.rs`](src/global.rs) for `usage.db`,
   [`src/fsview.rs`](src/fsview.rs) for files,
   [`src/codegraph.rs`](src/codegraph.rs) for the graph. Degrade a missing
   file or table to an empty payload; do not return an error.
2. Add the arm to the `match route` in `respond` (`src/lib.rs:148`). Take
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

- [`../../AGENTS.md`](../../AGENTS.md) — "Glossary — the identifiers that look alike"
  (the `execution_id` / `run_id` distinction this crate joins across), "The
  `.stella/` directory (per-workspace state)" for what each store holds, and
  invariant 3, "Zero telemetry egress by default".
- [`../../docs/spec/exploration-sharing.md`](../../docs/spec/exploration-sharing.md)
  §4e — the exploration-map freshness verdict `fsview::explorations` computes.
- [`../../website/content/docs/commands/observe.mdx`](../../website/content/docs/commands/observe.mdx)
  and [`../../website/content/docs/telemetry/dashboard.mdx`](../../website/content/docs/telemetry/dashboard.mdx)
  — the user-facing flags and a tour of each tab.
- [`../stella-store`](../stella-store) writes everything this crate reads;
  [`../stella-graph`](../stella-graph) writes `codegraph.db`, and
  [`../stella-fleet`](../stella-fleet) `fleet.db`.
