# stella-graph

The code-graph indexer: tree-sitter symbol and import-edge extraction over a
workspace, persisted in SQLite at `.stella/private/codegraph.db`, and served
back as `ContextFrame`s. It is implemented **as a built-in CGP provider**
(`PROVIDER_ID = "code-graph"`) that recall reaches through the CGP host, and
`stella init` is what builds the index.

The boundary is one-directional: this crate depends on `contextgraph-types`
for the wire shape and **never on `stella-context`**, so the provider can be
consumed without the retrieval plane depending back on it. Two smaller rules
follow from the same discipline. Nothing here hard-codes the database
location — `CodeGraph::open` takes a caller-supplied path, and the
`.stella/private/codegraph.db` convention lives in the callers. And a
tree-sitter parse failure on an arbitrary file is *not* a `GraphError`: it is
skipped-with-record so one unparseable file never aborts an index batch
(L-L1). `GraphError` is reserved for real infrastructure faults — SQLite, I/O,
a malformed built-in query.

## Where it sits

No workspace crate is a dependency of this one; its only non-third-party
dependency is `contextgraph-types`, pinned by git rev. It builds no binary.
`stella-cli` and `stella-tools` both depend on it: the CLI mounts the graph
for a session (`crates/stella-cli/src/agent/graph.rs`) and `stella-tools` uses it for
`read_symbol`, `code_map`, `impact`, `overview`, and the pre-write schema gate
(`crates/stella-tools/src/schema_gate.rs`).

## Boundary — does this change belong here?

The decision rule: a change belongs here if it alters what the index contains
or how it is built and served — a grammar and its S-expression queries,
symbol/import/call-site extraction, the walk and generated-file exclusion,
the `codegraph.db` schema and its convergence migration, the watcher, the
storage-map adapters and manifest merge, or frame assembly for this
provider's own results. A change that alters how those frames rank *against
other sources* — fusion, dedup, MMR, cross-provider budgets — belongs to
[`stella-context`](../stella-context): this crate packs only its own answers,
and recall reaches it through the CGP host, never through a link, which is
the one-directional boundary stated above read in the other direction. A
change to how a *tool* consumes the reads (`read_symbol`, `impact`, the
pre-write gate's policy) belongs in `stella-tools`
(`crates/stella-tools/src/schema_gate.rs`); this crate exposes the pure reads
it calls.

The mistake to head off explicitly: **a new language is not a new crate.** It
is a new tree-sitter grammar crate wired into this one — a workspace
dependency, a queries pair in [`src/queries.rs`](src/queries.rs), a
`Language` variant, a `LangPack` — per the six-step recipe under "Extending
it". Likewise a new storage source is a module under
[`src/storage/`](src/storage) and a new symbol kind is a variant in
[`src/symbol.rs`](src/symbol.rs). Ten languages and four storage families
already live here without a split; the exhaustive matches in
[`src/lang.rs`](src/lang.rs) are what keep that scalable.

A new crate is justified only when functionality (a) sits behind a port/trait
and would otherwise drag heavy new dependencies into a deliberately light
crate — not this crate's situation: grammar crates *are* its approved heavy
dependencies and belong here; (b) needs a dependency direction the current
graph forbids — the reason this crate and `stella-context` share
`contextgraph-types` instead of an edge; or (c) is a genuinely separate
deliverable with its own binary or release cadence. Otherwise extend this
crate: a new crate costs a workspace-table row, an impacted-crates scope, CI
time, and a README — with AGENTS.md's workspace table and the root
`Cargo.toml` members updated in the same PR — and a wrong split is harder to
undo than a wrong merge.

## God files — do not add lines

This crate has no god files: no file here exceeds the gate's 1500-line
ratchet (`scripts/check-file-size.sh`), and none may appear — a new file
crossing the limit fails the gate outright, and
`scripts/file-size-baseline.txt` accepts no new entries. When a file
approaches the limit, split it before it crosses — and
[`src/parse.rs`](src/parse.rs) is approaching it at 1474 lines today, so a
new language's decoder arm and tests may be what pushes it over: plan to
extract per-language decoding into a submodule rather than grow the file.

## Layout

| File | What it holds |
|---|---|
| [`src/lib.rs`](src/lib.rs) | Crate docs, the public re-export surface, and `load_storage_snapshot` — the pure read the gate calls per proposed write. |
| [`src/graph.rs`](src/graph.rs) | `CodeGraph`, the public handle: `open` / `mount` / `index_all` / the query methods. Open this to change mount, shutdown, or connection behavior. |
| [`src/lang.rs`](src/lang.rs) | `Language` and the extension → grammar + query mapping. First stop when adding a language. |
| [`src/queries.rs`](src/queries.rs) | The tree-sitter S-expression queries as `const &str` compile-time data, one symbols/imports pair per language. |
| [`src/parse.rs`](src/parse.rs) | `Grammars` (compiled once, shared by reference) and `parse_file`: tree → `Symbol`s + raw `ImportSpec`s. Pure and synchronous. |
| [`src/symbol.rs`](src/symbol.rs) / [`src/import.rs`](src/import.rs) | `SymbolKind` (the cross-language superset, including SQL schema objects) and `ImportKind` plus the relative-specifier resolution ladder. |
| [`src/store.rs`](src/store.rs) | SQLite: the `MIGRATION` DDL, `index_tree`, `apply_changes`, and every read query. The largest file and the one with the durability contract. |
| [`src/walk.rs`](src/walk.rs) | The directory walk and `DENY_DIRS` — the cheap structural half of generated-file exclusion. |
| [`src/generated.rs`](src/generated.rs) | The per-file half: `.gitattributes linguist-generated`, `*.min.*`, and the minified-content heuristic (issue #272). |
| [`src/watch.rs`](src/watch.rs) | The live re-index pipeline: `notify` event source → debounce → one transactional apply, plus the `WatchInjector` test seam. |
| [`src/frames.rs`](src/frames.rs) | Query → `ContextFrame` assembly: citation labels, provenance, score bands, budget packing. |
| [`src/storage.rs`](src/storage.rs) | The storage map's canonical model (layer / namespace / relation / field) and `StorageExtractor`. |
| [`src/storage/sql.rs`](src/storage/sql.rs), [`prisma.rs`](src/storage/prisma.rs), [`ts.rs`](src/storage/ts.rs), [`py.rs`](src/storage/py.rs) | One adapter per source family: SQL DDL; Prisma; Drizzle/TypeORM/Mongoose/DynamoDB; Django/SQLAlchemy. |
| [`src/manifest.rs`](src/manifest.rs) | `stella.storage.toml` — the committed half of the storage map (layers, intent, redirects, stubs) merged at snapshot time. |
| [`src/error.rs`](src/error.rs) | `GraphError`. |

## Key concepts

**Languages are wired in at compile time, natively.** Ten `Language` variants
— Rust, Python, JavaScript, TypeScript, Tsx, Sql, Go, Java, C, Php — over nine
grammar crates (`tree-sitter-typescript` supplies both `LANGUAGE_TYPESCRIPT`
and `LANGUAGE_TSX`, which is why `Tsx` is a separate variant even though it
shares TypeScript's query strings). Extensions map in
[`Language::from_path`](src/lang.rs): `rs`; `py`/`pyi`; `js`/`jsx`/`mjs`/`cjs`;
`ts`/`mts`/`cts`; `tsx`; `go`; `java`; `c`/`h`; `php`; `sql`. Grammars are
linked in from their own crates, not loaded as WASM, and the queries are
module `const`s rather than `.scm` assets — L-L2: built-in assets that resolve
relative to the binary's install path broke the moment the artifact was
bundled differently.

**Every symbol query follows one capture convention** so `parse.rs` can decode
matches uniformly: `@name` is the identifier whose text becomes the symbol
name, and the *kind capture* (`@fn`, `@method`, `@struct`, `@enum`, `@trait`,
`@class`, `@interface`, `@table`, …) captures the whole definition node — its
line span becomes the symbol span and its capture name encodes the kind. Name
fields use the `(_)` wildcard because `identifier` vs `type_identifier` vs
`property_identifier` differ across grammars. Methods are deliberately
double-captured and deduped by the name node's byte range, higher
`SymbolKind::rank` winning.

**`codegraph.db` is a cache versioned by convergence, and stamped.** Every
statement in `MIGRATION` is `CREATE … IF NOT EXISTS` and the whole batch
replays on every writer `open`, so adding a table or an index *is* the
migration. That is only acceptable because the file is fully rebuildable by
`stella init`. A change that needs a *reshape* — an altered or backfilled
column, which `IF NOT EXISTS` silently skips on an existing store — still needs
versioned machinery, and since #617 the file carries the `PRAGMA user_version`
stamp (`SCHEMA_VERSION`) such machinery would key off: a store already in the
field is stamped rather than rebuilt, and one written by a newer stella is
refused instead of written into blind. The writer stamps; `open_read` does not,
because the pre-write gate must not take a write lock. The `code_graph_` table
prefix is a holdover from when this shared `stella-context`'s `context.db`; it
stays so the two schemas cannot collide.

**Mount is warm and non-blocking.** `CodeGraph::mount` opens the store
synchronously so queries are immediately callable, then runs incremental
catch-up and arms the watcher on a background task (L-C1) — building lazily on
first query added seconds to the first real prompt. Indexing and queries use
**separate connections to the same WAL file**, so a long catch-up transaction
never blocks a read; the reader just sees the last committed snapshot. Every
index batch is one transaction, so a kill mid-index commits nothing.

**Frames are cited by human label, never by id (L-C4).** `title` and the
mandatory `citation_label` are strings like `fn run_turn
(crates/stella-core/src/driver.rs:160)`; every frame's provenance ends in a
derivation attributed to `code-graph`; each declares an exact `token_cost`
and a `content_digest` over exactly the bytes it carries inline, and assembly
respects `max_tokens`/`max_frames` (L-C5).

## Gotchas

- **Custom `.gitignore` patterns are not honored.** `walk.rs` is a deny-list
  approximation of ripgrep semantics (hidden dirs, `target`, `node_modules`,
  `dist`, `dist-standalone`, `build`, `out`, `.next`, `vendor`,
  `__pycache__`, `venv`, …), not the `ignore` crate. The same gap applies to
  `.gitattributes`: only the root-level file is read, per-directory ones are
  not merged.
- **Generated-file exclusion runs *before* the byte-compat skip.** Deliberate:
  the sha256 skip would otherwise keep a file indexed before `generated.rs`
  existed in the store forever, because its bytes never change.
- **The read path must not run DDL.** `load_storage_snapshot` uses
  `store::open_read` (pragmas only, no `MIGRATION`) because the pre-write gate
  opens the store on *every* write an agent proposes; running the whole
  `CREATE …` batch there is never free.
- **`Language::tag()` is persisted** in `code_graph_files.language`. Renaming
  a tag is a schema change the convergence migration cannot express.
- **`Language::Php` uses `LANGUAGE_PHP`, not `LANGUAGE_PHP_ONLY`** — real
  `.php` files open and close `<?php` around markup, which the PHP-only
  grammar cannot parse at all.
- **`.h` indexes as C** even in a C++ tree. Misreading a header still yields
  useful struct/function symbols; skipping it loses every declaration.
- **SQL has no `SQL_IMPORTS`** (the const is `""`), and `parse_file` returns an
  empty import vector for it.
- **`references()` is a linear text scan** over the indexed corpus, capped at
  50 hits — best-effort retrieval, not an index.
- **Import frames cap their listing at 50 edges** because `budget_pack` *skips*
  an over-budget frame rather than truncating it: an uncapped barrel file
  produced one multi-thousand-token frame and the caller silently got no
  import context at all.

## Testing

```bash
cargo test -p stella-graph                 # no crate-specific `make` target exists
cargo test -p stella-graph -- --ignored    # adds the real-FS notify smoke test
```

Per-language extraction and the query-compile guard (`all_queries_compile`)
live in `#[cfg(test)]` modules inside [`src/parse.rs`](src/parse.rs); the
integration tests are fixture-driven over real files in tempdirs:
[`tests/languages.rs`](tests/languages.rs) (end-to-end indexing, Python
relative and TS `index.ts` resolution, byte-compat skip),
[`tests/frames.rs`](tests/frames.rs) (citation labels, provenance, budgets,
content digests), [`tests/generated_exclusion.rs`](tests/generated_exclusion.rs)
(witness tests for issue #272), and
[`tests/live_index.rs`](tests/live_index.rs).

Live-index tests are deterministic by construction: they use
`#[tokio::test(start_paused = true)]` (which needs tokio's `test-util` feature,
already a dev-dependency) and drive the real debounce → `apply_changes`
pipeline through `CodeGraph::watch_pipeline_for_tests`, replacing OS event
*delivery* only. [`tests/watcher.rs`](tests/watcher.rs) is the one test that
goes through `notify` end to end and is `#[ignore]`d — FSEvents/inotify timing
would make it a flaky gate.

## Extending it

To add a language:

1. Add the `tree-sitter-<lang>` crate to the workspace `[workspace.dependencies]`
   in `../Cargo.toml` and to this crate's [`Cargo.toml`](Cargo.toml).
2. Add `<LANG>_SYMBOLS` and `<LANG>_IMPORTS` to [`src/queries.rs`](src/queries.rs),
   following the `@name` + kind-capture convention and matching name fields
   with `(_)`.
3. Add the `Language` variant in [`src/lang.rs`](src/lang.rs) and extend all
   five matches: `from_path`, `tag`, `ts_language`, `symbol_query`,
   `import_query`. They are exhaustive, so the compiler enumerates what is
   missing.
4. In [`src/parse.rs`](src/parse.rs), add a `LangPack` field to `Grammars`,
   load it in `Grammars::load`, map it in `Grammars::pack`, and add an arm to
   the import-decoder match in `parse_file` (each language decodes its own
   specifier shape — see `extract_go_imports`, `extract_c_imports`).
5. If specifiers resolve to files, extend [`src/import.rs`](src/import.rs);
   otherwise record them as `ImportKind::Bare`/`Absolute`, which preserves the
   edge even when the target is outside the tree.
6. Add tests: `all_queries_compile` in `src/parse.rs` fails immediately if a
   query string does not compile; add a symbols-and-imports test beside
   `go_symbols_and_grouped_imports`, and extend `new_extensions_classify`
   (`src/parse.rs`) and `extensions_map_to_languages` (`src/lang.rs`).

To add a storage source, implement extraction in a new
[`src/storage/`](src/storage) module, dispatch it from `StorageExtractor::extract`,
and extend `is_storage_file`. Extraction is shared by the indexer and the
pre-write gate, so the two cannot drift; an unrecognized pattern must yield
nothing rather than garbage.

## See also

- [`../../AGENTS.md`](../../AGENTS.md) — "Workspace layout — where a change goes" for
  the crate boundary, and its "Gotchas" entry on `context.db` vs `codegraph.db`
  (they used to share one file; do not revert the split).
- [`../../docs/design/storage-map.md`](../../docs/design/storage-map.md) — the spec
  the `storage` adapters and `manifest` implement (§3 model, §4a sources).
- [`../../docs/design/schema-graph.md`](../../docs/design/schema-graph.md) — the
  earlier schema-aware design; its Phases 1–2 shipped here, the rest was
  absorbed into `storage-map.md`.
- [`../../website/content/docs/context-engine.mdx`](../../website/content/docs/context-engine.mdx)
  — how the code graph fits the retrieval plane from a user's point of view.
- [`../stella-tools/src/schema_gate.rs`](../stella-tools/src/schema_gate.rs) —
  the pre-write gate that consumes `load_storage_snapshot`.
