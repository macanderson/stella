//! `stella stats` — cost/$-per-resolved-task analytics straight from the
//! workspace's local SQLite telemetry (`.stella/private/store.db`).
//!
//! Reads what `stella-store` recorded — nothing else. Works with zero API
//! keys configured (no provider is ever resolved), never writes: if the
//! database file doesn't exist yet it says so instead of creating one.
//!
//! Formats: an aligned table for humans (with a TOTAL row), and json/csv
//! for machine-readable receipts. Field order in json/csv follows
//! [`StatsRow`]'s declaration order — a stable contract.
//!
//! ## Reading the cache columns (issues #267/#269)
//!
//! `HIT%`, `SAVED ($)`, and `TTL REWRITES` are derived at display time, not
//! stored — `stella-store` records only the raw token counts and per-call
//! timing (`Store::usage_stats`, `Store::cache_call_gaps`); this module is
//! where the model catalog and the TTL/pricing policy (both `stella-model`
//! concerns the store deliberately doesn't depend on) turn those into
//! dollars and a rewrite count, via `stella_model::cache_economics` — the
//! exact formulas the deck's CACHE/SAVED/WARMTH statline uses, so the two
//! surfaces never drift:
//!
//! - **`HIT%`** — cache-read tokens over total input tokens for the row.
//! - **`SAVED ($)`** — signed estimated savings at TODAY's catalog list
//!   pricing (the store doesn't persist a per-call savings figure, so this
//!   re-derives it from the summed token counts against current, not
//!   historical, pricing — `-` when the model isn't in the running catalog).
//!   Negative means the cache write premium outran the reads it bought —
//!   the low-hit-rate incident worth investigating, never hidden.
//! - **`TTL REWRITES`** — calls whose session-relative gap since the
//!   previous call exceeded the provider's prompt-cache TTL, so the prefix
//!   had already expired and this call re-wrote it instead of reading it
//!   back (`cache_expired_rewrite`, from [`Store::cache_call_gaps`]) — the
//!   TTL-blind tax cache-aware scheduling (#269) exists to cut. `0` for a
//!   provider with no documented TTL (nothing can expire).
//!
//! Table format also appends a "recent sessions" trend
//! ([`Store::session_cache_trend`]), each carrying a probable-cause
//! diagnosis line when its own turn count and hit rate warrant one
//! ([`diagnose_cache`] against [`LOW_HIT_RATE_THRESHOLD`], the same
//! selection logic and [`stella_protocol::CacheCause::hint`] wording the deck would use).
//! Deliberately **per-session**, not per-(provider, model): `StatsRow::runs`
//! aggregates executions across every session sharing a provider/model, so
//! twenty independent one-shot `stella run`s (each turn 1 — nothing to
//! reread yet) would read as "20 turns, 0% hit" and misfire a diagnosis a
//! real multi-turn session never earned. `SessionCacheTrendRow::turns` is
//! the session's own turn count, so the `> 3`-turns gate means what the
//! acceptance criteria says: turns *within one session*.

use std::collections::HashMap;
use std::path::Path;

use clap::{Subcommand, ValueEnum};
use colored::Colorize as _;
use serde::Serialize;
use stella_model::cache_economics::{
    diagnose_cache, hit_rate, is_cache_expired_rewrite, provider_cache_ttl_secs,
};
use stella_model::catalog::Catalog;
use stella_protocol::CompletionUsage;
use stella_store::cache_trend::SessionCacheTrendRow;
use stella_store::usage::{UsageStore, project_id_for};
use stella_store::{CacheCallGap, Store, StorePrunePolicy, StorePruneReport, UsageStatsRow};

use crate::usage_cmd::parse_age_window;

/// Below this hit rate (with enough turns to have established a cache to
/// hit) a row earns a diagnosis line — matches
/// `stella_model::cache_economics::diagnose_cache`'s own "~20%" acceptance
/// bar.
const LOW_HIT_RATE_THRESHOLD: f64 = 0.20;

/// Subcommands under `stella stats`. Bare `stella stats` (no subcommand) is
/// still the report — this only adds verbs that *write*.
#[derive(Subcommand, Clone)]
pub enum StatsCmd {
    /// Bound this workspace's store (.stella/private/store.db) by dropping
    /// old executions and everything keyed to them: events, telemetry, tool
    /// calls, context blocks, receipts. Replication-safe by default — never
    /// drops an execution whose telemetry has not reached the usage hub
    Prune(PruneArgs),
}

/// Flags for `stella stats prune` and its alias `stella storage prune`.
///
/// One `Args` struct with two mount points, rather than the flags declared
/// twice: `store.db` retention is discoverable both from the store's *stats*
/// (where you notice it is too big) and from `storage` (where the rest of the
/// store verbs live), and two copies of five flags is how the two verbs drift
/// into accepting different spellings of the same knob. #709 removed a rival
/// second implementation that had done exactly that — `--max-executions` on one
/// verb, `--max-rows` on the other, against two different engines.
#[derive(clap::Args, Debug, Clone)]
pub struct PruneArgs {
    /// Drop executions older than this window: N followed by d, w, h, mo,
    /// or y (e.g. 90d, 12w, 720h, 3mo). A bare number means days
    #[arg(long, value_name = "AGE")]
    pub older_than: Option<String>,

    /// Hard ceiling on retained executions; evict the oldest prunable
    /// ones until at/under it
    #[arg(long, value_name = "N")]
    pub max_rows: Option<i64>,

    /// Also drop executions whose telemetry has not replicated into the
    /// usage hub — permanently loses that cost data. Off by default
    #[arg(long)]
    pub force: bool,

    /// VACUUM afterwards to hand freed pages back to the filesystem (also
    /// automatic for a large prune)
    #[arg(long)]
    pub vacuum: bool,

    /// Report what would be pruned without deleting anything
    #[arg(long)]
    pub dry_run: bool,
}

/// Output format for `stella stats`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum StatsFormat {
    /// Aligned human-readable columns with a TOTAL row (default).
    Table,
    /// Pretty-printed JSON array of per-(provider, model) rows.
    Json,
    /// RFC-4180-style CSV with a header row.
    Csv,
}

/// One display row: [`UsageStatsRow`] — the store's raw per-(provider,
/// model) aggregate — plus the cache economics derived here (see the module
/// docs). `stella-store` stays free of the model catalog and TTL policy;
/// this is where they meet the raw counts.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatsRow {
    pub provider: String,
    pub model: String,
    pub division: String,
    pub runs: i64,
    pub resolved: i64,
    pub resolve_rate: f64,
    pub total_cost_usd: f64,
    pub cost_per_resolved_usd: Option<f64>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    /// Cache-read tokens over total input tokens for this row —
    /// [`hit_rate`], the same formula the deck's CACHE cell uses.
    pub cache_hit_rate: f64,
    /// Estimated USD saved by prompt caching at today's catalog list
    /// pricing; `None` when `(provider, model)` isn't in the running
    /// catalog (nothing to price against).
    pub cache_savings_usd: Option<f64>,
    /// Calls whose prefix went cold past the provider TTL before this call
    /// and got rewritten instead of read back — see [`Store::cache_call_gaps`].
    pub cache_expired_rewrites: i64,
    /// Declared last so json (serde, struct order), csv, and the table all
    /// place this column in the same spot — the stable field-order contract
    /// in the module docs.
    pub avg_duration_ms: f64,
}

/// Look up `(provider, model)` in the running model catalog and price this
/// row's summed token counts — [`stella_model::Pricing::cache_savings_usd_for`], the same
/// formula `Store::cache_call_gaps`' caller-side economics and the deck's
/// `CacheInsight` producer both use. `None` when the catalog has no entry
/// for the pair (a retired or custom model) — never a guessed number.
fn cache_savings_for(row: &UsageStatsRow) -> Option<f64> {
    let catalog = Catalog::current();
    let entry = catalog.resolve_for(&row.provider, &row.model).ok()?;
    let usage = CompletionUsage {
        reported: true,
        input_tokens: row.input_tokens.max(0) as u64,
        output_tokens: row.output_tokens.max(0) as u64,
        cached_input_tokens: row.cache_read_tokens.max(0) as u64,
        cache_write_tokens: row.cache_write_tokens.max(0) as u64,
    };
    Some(entry.pricing.cache_savings_usd_for(&row.provider, &usage))
}

/// Fold raw [`CacheCallGap`]s into a `cache_expired_rewrite` count per
/// `(provider, model)`, applying [`is_cache_expired_rewrite`] against each
/// row's provider TTL ([`provider_cache_ttl_secs`]) — the store carries no
/// TTL policy, so that pairing happens here. A gap with no predecessor
/// (`None`) or a provider with no documented TTL contributes nothing; there
/// is nothing to have expired.
fn cache_expired_rewrite_counts(gaps: &[CacheCallGap]) -> HashMap<(String, String), i64> {
    let mut counts: HashMap<(String, String), i64> = HashMap::new();
    for gap in gaps {
        let Some(gap_secs) = gap.gap_secs.filter(|g| *g >= 0) else {
            continue;
        };
        let Some(ttl_secs) = provider_cache_ttl_secs(&gap.provider) else {
            continue;
        };
        if is_cache_expired_rewrite(
            gap_secs as u64,
            gap.cache_write_tokens.max(0) as u64,
            ttl_secs,
        ) {
            *counts
                .entry((gap.provider.clone(), gap.model.clone()))
                .or_insert(0) += 1;
        }
    }
    counts
}

/// Enrich the store's raw aggregates into display rows: hit rate (pure
/// arithmetic over the row's own counts), catalog-priced savings, and the
/// `cache_expired_rewrite` count keyed off the matching `(provider, model)`.
fn to_stats_rows(rows: Vec<UsageStatsRow>, gaps: &[CacheCallGap]) -> Vec<StatsRow> {
    let expired = cache_expired_rewrite_counts(gaps);
    rows.into_iter()
        .map(|r| {
            let cache_hit_rate = hit_rate(
                r.input_tokens.max(0) as u64,
                r.cache_read_tokens.max(0) as u64,
            );
            let cache_savings_usd = cache_savings_for(&r);
            let key = (r.provider.clone(), r.model.clone());
            let cache_expired_rewrites = expired.get(&key).copied().unwrap_or(0);
            StatsRow {
                provider: r.provider,
                model: r.model,
                division: r.division,
                runs: r.runs,
                resolved: r.resolved,
                resolve_rate: r.resolve_rate,
                total_cost_usd: r.total_cost_usd,
                cost_per_resolved_usd: r.cost_per_resolved_usd,
                input_tokens: r.input_tokens,
                output_tokens: r.output_tokens,
                cache_read_tokens: r.cache_read_tokens,
                cache_write_tokens: r.cache_write_tokens,
                avg_duration_ms: r.avg_duration_ms,
                cache_hit_rate,
                cache_savings_usd,
                cache_expired_rewrites,
            }
        })
        .collect()
}

/// One line per session carrying a diagnosis, `session <id>: <hint>` —
/// [`diagnose_cache`] against the session's OWN turn count and hit rate
/// (see the module docs for why this must be per-session, not the
/// per-provider/model `StatsRow` aggregate).
fn session_diagnosis_lines(sessions: &[SessionCacheTrendRow]) -> Vec<String> {
    sessions
        .iter()
        .filter_map(|s| {
            diagnose_cache(
                &s.provider,
                s.turns.max(0) as u64,
                s.input_tokens.max(0) as u64,
                s.cache_read_tokens.max(0) as u64,
                s.cache_write_tokens.max(0) as u64,
                LOW_HIT_RATE_THRESHOLD,
            )
            .map(|cause| format!("session {}: {}", s.session_id, cause.hint()))
        })
        .collect()
}

/// Entry point for `stella stats prune` — retention for `store.db` (#616).
/// `stella storage prune` is an alias onto this same function; see
/// [`PruneArgs`] for why the flags are defined once.
///
/// The store engine ([`Store::prune`]) owns the deletion and the
/// "is this telemetry safe to destroy" invariant; this function owns the two
/// things that cross a database boundary and so cannot live there:
///
/// 1. **Reading the watermark in.** The replication cursor lives in the usage
///    hub (`~/.stella/usage.db`), not in `store.db`. An unreachable hub reads
///    as cursor `0`, which protects every execution that produced a model
///    call — the safe default, announced rather than silently applied.
/// 2. **Writing the corrected watermark back.** Pruning frees (and may
///    renumber) the `telemetry.rowid`s the hub's cursor addresses; the report
///    carries the re-measured value and it is persisted here. Skipping this
///    would strand un-replicated rows above a stale cursor forever.
pub fn run_stats_prune(args: &PruneArgs) -> Result<(), String> {
    let PruneArgs {
        older_than,
        max_rows,
        force,
        vacuum,
        dry_run,
    } = args;
    let (older_than, max_rows, force, vacuum, dry_run) =
        (older_than.as_deref(), *max_rows, *force, *vacuum, *dry_run);
    if older_than.is_none() && max_rows.is_none() {
        return Err("nothing to prune — pass at least one of --older-than or --max-rows".into());
    }
    let older_than = older_than.map(parse_age_window).transpose()?;
    if max_rows.is_some_and(|n| n < 0) {
        return Err("--max-rows must not be negative".into());
    }

    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    // Same courtesy as the report: never create `.stella/` just to prune it.
    if stella_store::existing_workspace_private_sqlite_path(&workspace_root, "store.db")
        .map_err(|e| format!("cannot resolve local store: {e}"))?
        .is_none()
    {
        println!("no local store in this workspace — nothing to prune");
        return Ok(());
    }
    let store =
        Store::open(&workspace_root).map_err(|e| format!("cannot open local store: {e}"))?;

    let project_id = project_id_for(&workspace_root);
    let hub = UsageStore::open_default().ok();
    let telemetry_cursor = match &hub {
        Some(hub) => hub.telemetry_cursor(&project_id).unwrap_or(0),
        None => 0,
    };
    if hub.is_none() {
        println!(
            "{}",
            "the usage hub is unreachable, so no telemetry counts as replicated — \
             only executions with no model calls can be pruned"
                .yellow()
        );
    }

    let report = store
        .prune(&StorePrunePolicy {
            older_than,
            max_rows,
            telemetry_cursor,
            force,
            vacuum,
            dry_run,
        })
        .map_err(|e| format!("prune failed: {e}"))?;

    // Persist the re-measured watermark before reporting success: a prune that
    // deleted rows and then failed to move the cursor has silently broken
    // replication, and the user must not read "removed N rows" as all-clear.
    if let (Some(hub), Some(cursor)) = (&hub, report.telemetry_cursor_after) {
        hub.rewind_telemetry_cursor(&project_id, cursor)
            .map_err(|e| format!("pruned, but could not re-anchor the usage hub cursor: {e}"))?;
    }

    print_prune_report(&report, dry_run, &workspace_root);
    Ok(())
}

fn print_prune_report(report: &StorePruneReport, dry_run: bool, workspace_root: &Path) {
    let verb = if dry_run { "would remove" } else { "removed" };
    if report.aged_out > 0 {
        println!("{verb} {} aged-out execution(s)", report.aged_out);
    }
    if report.ceiling_evicted > 0 {
        println!(
            "{verb} {} execution(s) to meet the --max-rows ceiling",
            report.ceiling_evicted
        );
    }
    if report.dependent_rows > 0 {
        println!(
            "{verb} {} dependent row(s) across {} table(s), including {} telemetry row(s)",
            report.dependent_rows,
            stella_store::DEPENDENT_TABLES.len(),
            report.telemetry_pruned,
        );
    }
    if report.protected_unreplicated > 0 {
        println!(
            "{}",
            format!(
                "kept {} execution(s) whose telemetry has not reached the usage hub — \
                 run `stella usage sync`, then re-run; or pass --force to drop them anyway",
                report.protected_unreplicated
            )
            .yellow()
        );
    }
    if report.still_over_ceiling > 0 {
        println!(
            "{}",
            format!(
                "still {} execution(s) over --max-rows (un-replicated telemetry) — \
                 sync first, or re-run with --force",
                report.still_over_ceiling
            )
            .yellow()
        );
    }
    if report.vacuumed {
        println!(
            "vacuumed {}",
            workspace_root.join(".stella/private/store.db").display()
        );
    }
    if let Some(cursor) = report.telemetry_cursor_after {
        println!("re-anchored the usage hub replication cursor to {cursor}");
    }
    if report.total_rows() == 0 {
        println!("nothing to prune — the store is already within policy");
    }
}

/// Entry point for `stella stats`. `provider` filters rows to one provider
/// id (e.g. `zai`, `anthropic`, `local`); `None` shows everything.
pub fn run_stats(format: StatsFormat, provider: Option<&str>) -> Result<(), String> {
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;

    // Stats is read-only: opening the store would create `.stella/` as a
    // side effect, so bail out politely when there's nothing to read.
    let db_path = stella_store::existing_workspace_private_sqlite_path(&workspace_root, "store.db")
        .map_err(|e| format!("cannot resolve local store: {e}"))?;
    let (rows, sessions) = if db_path.is_some() {
        let store =
            Store::open(&workspace_root).map_err(|e| format!("cannot open local store: {e}"))?;
        let mut rows = store
            .usage_stats()
            .map_err(|e| format!("cannot read usage stats: {e}"))?;
        if let Some(p) = provider {
            rows.retain(|r| r.provider == p);
        }
        let gaps = store
            .cache_call_gaps()
            .map_err(|e| format!("cannot read cache-call gaps: {e}"))?;
        let sessions = store
            .session_cache_trend()
            .map_err(|e| format!("cannot read session cache trend: {e}"))?;
        (to_stats_rows(rows, &gaps), sessions)
    } else {
        (Vec::new(), Vec::new())
    };

    if rows.is_empty() {
        // An empty store is a state, not an error — but keep stdout valid
        // for the machine formats. If the SQLite store has no rows yet a
        // pre-migration DuckDB file is present, say so explicitly: "no
        // executions" would otherwise imply the workspace has no telemetry
        // when in fact the old telemetry simply isn't read by this version.
        let legacy_duckdb = workspace_root.join(".stella").join("stella.duckdb");
        let message = if legacy_duckdb.exists() {
            legacy_duckdb_message(provider)
        } else {
            empty_message(provider)
        };
        match format {
            StatsFormat::Table => println!("{message}"),
            StatsFormat::Json => {
                eprintln!("{message}");
                println!("[]");
            }
            StatsFormat::Csv => {
                eprintln!("{message}");
                println!("{}", csv_header());
            }
        }
        return Ok(());
    }

    match format {
        // The session trend is table-only: json/csv keep the stable
        // array-of-StatsRow contract (`Store::session_cache_trend`'s own
        // shape has no natural row to fold it into).
        StatsFormat::Table => {
            print!("{}", render_table(&rows));
            print!("{}", render_session_trend(&sessions, 10));
        }
        StatsFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&rows).map_err(|e| format!("serialize: {e}"))?
        ),
        StatsFormat::Csv => print!("{}", render_csv(&rows)),
    }
    Ok(())
}

fn empty_message(provider: Option<&str>) -> String {
    match provider {
        Some(p) => format!(
            "No executions recorded for provider `{p}` yet — run `stella run \"...\"` (or \
             chat/goal) to generate local telemetry."
        ),
        None => "No executions recorded yet — run `stella run \"...\"` (or chat/goal) to \
                 generate local telemetry in .stella/private/store.db."
            .to_string(),
    }
}

/// Shown when the current SQLite store is empty/absent but a pre-migration
/// `.stella/stella.duckdb` is present: the telemetry exists, this version just
/// doesn't read the old format. Explicit so a migrated workspace is never
/// silently reported as having no history.
fn legacy_duckdb_message(provider: Option<&str>) -> String {
    let scope = match provider {
        Some(p) => format!(" for provider `{p}`"),
        None => String::new(),
    };
    format!(
        "No executions in the SQLite store{scope}, but a legacy .stella/stella.duckdb was \
         found. Stella migrated its telemetry store from DuckDB to SQLite (.stella/private/store.db); \
         the old DuckDB file is not read by this version and is not migrated automatically. \
         New runs record to .stella/private/store.db; the historical DuckDB data is preserved on disk \
         but not shown here."
    )
}

/// The TOTAL row across every displayed row, computed in Rust (weighted,
/// not an average of averages): rates and $/resolved are re-derived from
/// the summed counts, and $/resolved stays `-` when nothing resolved.
/// `cache_hit_rate` is likewise re-derived from the summed token counts
/// (never an average of per-row percentages); `cache_savings_usd` sums only
/// the rows the catalog could price, `None` when none could be; and
/// `cache_expired_rewrites` is a plain sum — a real count, not a rate.
fn totals(rows: &[StatsRow]) -> StatsRow {
    let runs: i64 = rows.iter().map(|r| r.runs).sum();
    let resolved: i64 = rows.iter().map(|r| r.resolved).sum();
    let total_cost_usd: f64 = rows.iter().map(|r| r.total_cost_usd).sum();
    let total_duration_ms: f64 = rows.iter().map(|r| r.avg_duration_ms * r.runs as f64).sum();
    let input_tokens: i64 = rows.iter().map(|r| r.input_tokens).sum();
    let cache_read_tokens: i64 = rows.iter().map(|r| r.cache_read_tokens).sum();
    let known_savings: Vec<f64> = rows.iter().filter_map(|r| r.cache_savings_usd).collect();
    StatsRow {
        provider: "TOTAL".into(),
        model: "-".into(),
        division: "-".into(),
        runs,
        resolved,
        resolve_rate: if runs > 0 {
            resolved as f64 / runs as f64
        } else {
            0.0
        },
        total_cost_usd,
        cost_per_resolved_usd: (resolved > 0).then(|| total_cost_usd / resolved as f64),
        input_tokens,
        output_tokens: rows.iter().map(|r| r.output_tokens).sum(),
        cache_read_tokens,
        cache_write_tokens: rows.iter().map(|r| r.cache_write_tokens).sum(),
        avg_duration_ms: if runs > 0 {
            total_duration_ms / runs as f64
        } else {
            0.0
        },
        cache_hit_rate: hit_rate(input_tokens.max(0) as u64, cache_read_tokens.max(0) as u64),
        cache_savings_usd: (!known_savings.is_empty()).then(|| known_savings.iter().sum()),
        cache_expired_rewrites: rows.iter().map(|r| r.cache_expired_rewrites).sum(),
    }
}

/// Column headers, in `StatsRow` field order.
const TABLE_HEADERS: [&str; 16] = [
    "PROVIDER",
    "MODEL",
    "DIVISION",
    "RUNS",
    "RESOLVED",
    "RATE",
    "COST ($)",
    "$/RESOLVED",
    "IN TOK",
    "OUT TOK",
    "CACHE RD",
    "CACHE WR",
    "HIT%",
    "SAVED ($)",
    "TTL REWRITES",
    "AVG MS",
];

/// The first three columns are strings (left-aligned); the rest are
/// numbers (right-aligned).
const STRING_COLS: usize = 3;

fn table_cells(row: &StatsRow) -> [String; 16] {
    [
        row.provider.clone(),
        row.model.clone(),
        row.division.clone(),
        row.runs.to_string(),
        row.resolved.to_string(),
        format!("{:.1}%", row.resolve_rate * 100.0),
        format!("{:.4}", row.total_cost_usd),
        match row.cost_per_resolved_usd {
            Some(v) => format!("{v:.4}"),
            None => "-".into(),
        },
        row.input_tokens.to_string(),
        row.output_tokens.to_string(),
        row.cache_read_tokens.to_string(),
        row.cache_write_tokens.to_string(),
        format!("{:.0}%", row.cache_hit_rate * 100.0),
        match row.cache_savings_usd {
            Some(v) => format!("{v:.4}"),
            None => "-".into(),
        },
        row.cache_expired_rewrites.to_string(),
        format!("{:.0}", row.avg_duration_ms),
    ]
}

/// Render the aligned table with a separator line before the TOTAL row, plus
/// a low-hit-rate diagnosis section when any row earned one.
fn render_table(rows: &[StatsRow]) -> String {
    let body: Vec<[String; 16]> = rows.iter().map(table_cells).collect();
    let total = table_cells(&totals(rows));

    // Widths are measured in CHARACTERS, not bytes: `format!("{cell:<width$}")`
    // pads by character count, so a byte-length measurement would over-pad any
    // non-ASCII provider/model id (a custom or locally-named model) and skew
    // every column to its right.
    let mut widths: Vec<usize> = TABLE_HEADERS.iter().map(|h| h.chars().count()).collect();
    for cells in body.iter().chain(std::iter::once(&total)) {
        for (w, cell) in widths.iter_mut().zip(cells.iter()) {
            *w = (*w).max(cell.chars().count());
        }
    }

    let render_line = |cells: &[String]| -> String {
        let cols: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                if i < STRING_COLS {
                    format!("{cell:<width$}", width = widths[i])
                } else {
                    format!("{cell:>width$}", width = widths[i])
                }
            })
            .collect();
        // Two-space gutter; trim so left-aligned last cells never leave
        // trailing whitespace.
        cols.join("  ").trim_end().to_string()
    };

    let mut out = String::new();
    let headers: Vec<String> = TABLE_HEADERS.iter().map(|h| h.to_string()).collect();
    out.push_str(&render_line(&headers));
    out.push('\n');
    for cells in &body {
        out.push_str(&render_line(cells));
        out.push('\n');
    }
    let rule_width = widths.iter().sum::<usize>() + 2 * (widths.len() - 1);
    out.push_str(&"-".repeat(rule_width));
    out.push('\n');
    out.push_str(&render_line(&total));
    out.push('\n');
    out
}

/// The "recent sessions" cache-trend block `run_stats` appends in table
/// format — [`Store::session_cache_trend`]'s persisted per-session facts,
/// most-recent-first, capped to `limit` rows so a long-lived workspace's
/// receipt stays readable, plus a low-hit-rate diagnosis line for any of
/// those sessions that earns one ([`session_diagnosis_lines`] — per-session,
/// using the session's own turn count, see the module docs). Empty input
/// renders nothing (no session has ever been registered — nothing to trend).
fn render_session_trend(sessions: &[SessionCacheTrendRow], limit: usize) -> String {
    if sessions.is_empty() {
        return String::new();
    }
    let shown = &sessions[..sessions.len().min(limit)];
    let mut out = String::from("\nCache trend, recent sessions (most recent first):\n");
    out.push_str("  SESSION           STARTED              TURNS  HIT%\n");
    for s in shown {
        let pct = hit_rate(
            s.input_tokens.max(0) as u64,
            s.cache_read_tokens.max(0) as u64,
        ) * 100.0;
        out.push_str(&format!(
            "  {:<16}  {:<19}  {:>5}  {:>3.0}%\n",
            truncate(&s.session_id, 16),
            s.started_at,
            s.turns,
            pct
        ));
    }
    let hints = session_diagnosis_lines(shown);
    if !hints.is_empty() {
        out.push_str("\n  Low-hit-rate diagnosis:\n");
        for hint in hints {
            out.push_str("    ! ");
            out.push_str(&hint);
            out.push('\n');
        }
    }
    out
}

/// Truncate a display string to `max` chars, marking the cut with `…` — used
/// so a long session id doesn't blow out the trend table's column width.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// CSV header, matching `StatsRow`'s serialized field order exactly.
fn csv_header() -> &'static str {
    "provider,model,division,runs,resolved,resolve_rate,total_cost_usd,cost_per_resolved_usd,\
     input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,cache_hit_rate,\
     cache_savings_usd,cache_expired_rewrites,avg_duration_ms"
}

/// Quote a CSV field when it contains a comma, quote, or line break;
/// embedded quotes are doubled (RFC 4180). Shared with `stella usage report
/// --format csv` (`crate::usage_cmd`): provider/model/org ids come from
/// config and from provider catalogs, so every CSV surface must escape them
/// the same way or one of them silently emits a corrupt row.
pub(crate) fn csv_escape(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

fn csv_row(row: &StatsRow) -> String {
    format!(
        "{},{},{},{},{},{:.4},{:.6},{},{},{},{},{},{:.4},{},{},{:.1}",
        csv_escape(&row.provider),
        csv_escape(&row.model),
        csv_escape(&row.division),
        row.runs,
        row.resolved,
        row.resolve_rate,
        row.total_cost_usd,
        match row.cost_per_resolved_usd {
            Some(v) => format!("{v:.6}"),
            None => String::new(),
        },
        row.input_tokens,
        row.output_tokens,
        row.cache_read_tokens,
        row.cache_write_tokens,
        row.cache_hit_rate,
        match row.cache_savings_usd {
            Some(v) => format!("{v:.6}"),
            None => String::new(),
        },
        row.cache_expired_rewrites,
        row.avg_duration_ms,
    )
}

fn render_csv(rows: &[StatsRow]) -> String {
    let mut out = String::from(csv_header());
    out.push('\n');
    for row in rows {
        out.push_str(&csv_row(row));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(provider: &str, model: &str) -> StatsRow {
        StatsRow {
            provider: provider.into(),
            model: model.into(),
            division: UsageStatsRow::division_for_provider(provider).into(),
            runs: 4,
            resolved: 2,
            resolve_rate: 0.5,
            total_cost_usd: 0.03,
            cost_per_resolved_usd: Some(0.015),
            input_tokens: 6000,
            output_tokens: 600,
            cache_read_tokens: 2000,
            cache_write_tokens: 10,
            avg_duration_ms: 750.0,
            cache_hit_rate: 2000.0 / 6000.0,
            cache_savings_usd: Some(0.5),
            cache_expired_rewrites: 3,
        }
    }

    #[test]
    fn csv_escapes_quotes_and_commas() {
        assert_eq!(csv_escape("plain"), "plain");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_escape("line\nbreak"), "\"line\nbreak\"");
    }

    #[test]
    fn csv_row_matches_header_order_and_blanks_unresolved() {
        let mut r = row("zai", "glm-5.2");
        assert_eq!(
            csv_header().split(',').count(),
            csv_row(&r).split(',').count()
        );
        assert_eq!(
            csv_row(&r),
            "zai,glm-5.2,-,4,2,0.5000,0.030000,0.015000,6000,600,2000,10,0.3333,0.500000,3,750.0"
        );

        // resolved = 0 → the $/resolved field is EMPTY, never 0 or NaN.
        r.resolved = 0;
        r.resolve_rate = 0.0;
        r.cost_per_resolved_usd = None;
        assert_eq!(
            csv_row(&r),
            "zai,glm-5.2,-,4,0,0.0000,0.030000,,6000,600,2000,10,0.3333,0.500000,3,750.0"
        );

        // Fields with commas round-trip quoted.
        r.model = "weird,model".into();
        assert!(csv_row(&r).starts_with("zai,\"weird,model\",-,"));
    }

    #[test]
    fn totals_row_is_weighted_not_average_of_averages() {
        let mut a = row("zai", "glm-5.2");
        let mut b = row("anthropic", "claude-fable-5");
        b.runs = 1;
        b.resolved = 0;
        b.resolve_rate = 0.0;
        b.total_cost_usd = 0.05;
        b.cost_per_resolved_usd = None;
        b.avg_duration_ms = 100.0;
        a.avg_duration_ms = 750.0;

        let t = totals(&[a, b]);
        assert_eq!(t.provider, "TOTAL");
        assert_eq!(t.runs, 5);
        assert_eq!(t.resolved, 2);
        assert!((t.resolve_rate - 0.4).abs() < 1e-12);
        assert!((t.total_cost_usd - 0.08).abs() < 1e-12);
        assert!((t.cost_per_resolved_usd.unwrap() - 0.04).abs() < 1e-12);
        // (750*4 + 100*1) / 5 = 620 — weighted by runs.
        assert!((t.avg_duration_ms - 620.0).abs() < 1e-12);
        // cache_hit_rate is re-derived from the SUMMED token counts (never
        // an average of per-row percentages); cache_savings_usd and
        // cache_expired_rewrites are plain sums across both rows.
        assert!((t.cache_hit_rate - 2000.0 * 2.0 / (6000.0 * 2.0)).abs() < 1e-12);
        assert!((t.cache_savings_usd.unwrap() - 1.0).abs() < 1e-12);
        assert_eq!(t.cache_expired_rewrites, 6);
    }

    #[test]
    fn totals_cache_savings_sums_only_the_catalog_priced_rows() {
        // A row the catalog couldn't price (cache_savings_usd: None) must
        // not zero out or poison the total — it's simply left out of the sum.
        let mut priced = row("zai", "glm-5.2");
        priced.cache_savings_usd = Some(0.30);
        let mut unpriced = row("anthropic", "claude-fable-5");
        unpriced.cache_savings_usd = None;

        let t = totals(&[priced, unpriced]);
        assert!((t.cache_savings_usd.unwrap() - 0.30).abs() < 1e-12);

        // Every row unpriced → the total is honestly None, not a fake $0.00.
        let mut a = row("zai", "glm-5.2");
        a.cache_savings_usd = None;
        let mut b = row("anthropic", "claude-fable-5");
        b.cache_savings_usd = None;
        assert_eq!(totals(&[a, b]).cache_savings_usd, None);
    }

    #[test]
    fn totals_with_nothing_resolved_has_no_cost_per_resolved() {
        let mut a = row("anthropic", "claude-fable-5");
        a.resolved = 0;
        a.cost_per_resolved_usd = None;
        let t = totals(&[a]);
        assert_eq!(t.cost_per_resolved_usd, None);
    }

    #[test]
    fn table_aligns_columns_and_appends_total() {
        let out = render_table(&[row("zai", "glm-5.2"), row("local", "llama-3.3")]);
        let lines: Vec<&str> = out.lines().collect();
        // header + 2 rows + rule + TOTAL
        assert_eq!(lines.len(), 5);
        assert!(lines[0].starts_with("PROVIDER"));
        assert!(lines[1].contains("50.0%"));
        assert!(lines[1].contains("0.0300"));
        assert!(lines[1].contains("0.0150"));
        assert!(lines[2].contains("off-grid"));
        assert!(lines[3].chars().all(|c| c == '-'));
        assert!(lines[4].starts_with("TOTAL"));
        // Every RATE cell ends at the same column — alignment witness.
        let col = lines[0].find("RATE").unwrap() + "RATE".len();
        assert!(lines[1][..col].ends_with("50.0%"));
        assert!(lines[2][..col].ends_with("50.0%"));
        // Cache-economics columns (#267/#269) are present and populated.
        for header in ["HIT%", "SAVED ($)", "TTL REWRITES"] {
            assert!(lines[0].contains(header), "{header} header:\n{out}");
        }
        assert!(lines[1].contains("33%"), "hit rate rendered:\n{out}");
        assert!(lines[1].contains("0.5000"), "savings rendered:\n{out}");
    }

    #[test]
    fn table_shows_a_dash_for_savings_the_catalog_cannot_price() {
        let mut r = row("zai", "glm-5.2");
        r.cache_savings_usd = None;
        let out = render_table(&[r]);
        let lines: Vec<&str> = out.lines().collect();
        // The SAVED ($) column, specifically, right-aligns to a bare dash —
        // never a fake $0.00 for a model the catalog can't price.
        let col = lines[0].find("SAVED ($)").unwrap() + "SAVED ($)".len();
        assert!(
            lines[1][..col].ends_with('-'),
            "unpriced savings is a dash:\n{out}"
        );
    }

    fn trend_row(
        session_id: &str,
        provider: &str,
        turns: i64,
        input: i64,
        cache_read: i64,
        cache_write: i64,
    ) -> SessionCacheTrendRow {
        SessionCacheTrendRow {
            session_id: session_id.into(),
            started_at: "2026-07-21 12:00:00".into(),
            turns,
            provider: provider.into(),
            input_tokens: input,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
        }
    }

    #[test]
    fn diagnosis_fires_on_a_synthetic_zero_hit_multi_turn_session_and_names_opt_in_absent() {
        // The acceptance case: N>3 turns, 0% hit, nothing ever written to the
        // cache on an opt-in provider (a SINGLE session's own turn count,
        // not runs aggregated across sessions) — the marker never engaged.
        let sessions = vec![trend_row("s1", "anthropic", 6, 120_000, 0, 0)];
        let lines = session_diagnosis_lines(&sessions);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("session s1: "), "{}", lines[0]);
        assert!(
            lines[0].contains("cache opt-in never engaged"),
            "{}",
            lines[0]
        );
    }

    #[test]
    fn diagnosis_is_per_session_not_per_provider_model_aggregate() {
        // The false-positive this guards against: twenty independent
        // one-shot runs on the same (provider, model) would aggregate to
        // "20 turns, 0% hit" at the StatsRow level (turn 1 always writes,
        // never rereads) and wrongly earn a diagnosis no single session
        // deserves. Each SESSION here has only 1 turn -> diagnose_cache's
        // own MIN_TURNS gate stays quiet, correctly, per session.
        let many_one_shots: Vec<SessionCacheTrendRow> = (0..20)
            .map(|i| trend_row(&format!("s{i}"), "anthropic", 1, 6_000, 0, 0))
            .collect();
        assert!(session_diagnosis_lines(&many_one_shots).is_empty());

        // A genuine multi-turn session with the same symptoms DOES fire.
        let real_session = vec![trend_row("s-real", "anthropic", 6, 120_000, 0, 0)];
        assert_eq!(session_diagnosis_lines(&real_session).len(), 1);
    }

    #[test]
    fn diagnosis_is_quiet_for_a_healthy_hit_rate_and_appears_in_the_trend_block() {
        // Healthy hit rate: no diagnosis, no section in the rendered trend.
        let healthy = vec![trend_row("s1", "anthropic", 10, 100_000, 50_000, 10_000)];
        assert!(session_diagnosis_lines(&healthy).is_empty());
        assert!(!render_session_trend(&healthy, 10).contains("Low-hit-rate diagnosis"));

        // A diagnosed session's hint reaches the rendered trend block too.
        let sick = vec![trend_row("s-sick", "anthropic", 6, 120_000, 0, 0)];
        let out = render_session_trend(&sick, 10);
        assert!(out.contains("Low-hit-rate diagnosis:"), "{out}");
        assert!(out.contains("session s-sick:"), "{out}");
    }

    #[test]
    fn session_trend_renders_most_recent_first_with_hit_rate() {
        let sessions = vec![
            trend_row("s2", "anthropic", 3, 1_000, 500, 0),
            trend_row("s1", "anthropic", 6, 2_000, 200, 0),
        ];
        let out = render_session_trend(&sessions, 10);
        let s2_pos = out.find("s2").unwrap();
        let s1_pos = out.find("s1").unwrap();
        assert!(
            s2_pos < s1_pos,
            "input order (most-recent-first) preserved:\n{out}"
        );
        assert!(out.contains("50%"), "s2's 500/1000 hit rate:\n{out}");
        assert!(out.contains("10%"), "s1's 200/2000 hit rate:\n{out}");
    }

    #[test]
    fn session_trend_is_capped_and_empty_input_renders_nothing() {
        assert_eq!(render_session_trend(&[], 10), "");
        let many: Vec<SessionCacheTrendRow> = (0..15)
            .map(|i| trend_row(&format!("s{i}"), "anthropic", 1, 100, 10, 0))
            .collect();
        let out = render_session_trend(&many, 5);
        // Leading blank line (the block's own `\n` separator) + title +
        // column header + 5 capped rows (each 1-turn -> no diagnosis lines).
        assert_eq!(out.lines().count(), 3 + 5, "header lines + capped rows");
    }

    #[test]
    fn truncate_marks_a_cut_id_and_leaves_a_short_one_alone() {
        assert_eq!(truncate("short", 16), "short");
        assert_eq!(truncate("a-very-long-session-id-string", 10), "a-very-lo…");
    }
}
