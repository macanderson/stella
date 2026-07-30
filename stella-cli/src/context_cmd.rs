//! `stella context` — the context-record command family.
//!
//! One verb today: `validate`, the on-demand truth-probe sweep. It re-runs each
//! context record's **ungated** probe against the working tree and reports
//! whether the claim still holds — the manual form of the staleness maintenance
//! that keeps a once-true record from steering the agent wrong after the world
//! moves (the `05` "Node 20 vs `.nvmrc` 22" case).
//!
//! ## Scope
//!
//! This is the buildable half of the maintenance sweep. The *scheduled* sweep
//! and *automatic* demotion-from-selection wait on the record loader and
//! renderer (a published record has to be selected before it can be un-selected);
//! this command delivers the reusable check now. The retention decision
//! (`refuted` + `on_expiry` → keep/warn/drop/block) lives in
//! [`stella_core::ingest::retention_for`], so the future selection path and this
//! command reach the same verdict.
//!
//! ## Safety is inherited, not re-decided
//!
//! Only [`Record::honored_probe`] probes run — gated probes
//! (`command_succeeds`/`http_ok`) are never honored on an imported/inferred
//! record, so validating an ingested document can no more run a command or reach
//! the network than ingesting it could.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use colored::Colorize;
use stella_core::ingest::{ContextFile, Record, Retention, Verdict, retention_for};

use crate::ingest_cmd::probe;

/// Subcommands under `stella context`.
#[derive(Subcommand, Clone)]
pub enum ContextCmd {
    /// Re-run context records' truth probes against the tree and report which
    /// claims have gone stale.
    Validate {
        /// TOML context-record files to validate. With none, scans
        /// `.stella/proposals/` and `.stella/rules/`.
        paths: Vec<PathBuf>,
        /// Exit non-zero if any record is refuted — for use as a CI gate.
        #[arg(long)]
        strict: bool,
    },
}

/// Run a `stella context` subcommand.
pub fn run(cmd: &ContextCmd) -> Result<(), String> {
    match cmd {
        ContextCmd::Validate { paths, strict } => validate(paths, *strict),
    }
}

/// The per-file, per-record tallies a validate run reports.
#[derive(Default)]
struct Tally {
    supported: usize,
    refuted: usize,
    unchecked: usize,
}

impl Tally {
    fn total(&self) -> usize {
        self.supported + self.refuted + self.unchecked
    }
}

/// Longest slice of a statement shown per line.
const MAX_STATEMENT: usize = 88;

fn validate(paths: &[PathBuf], strict: bool) -> Result<(), String> {
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    let files = if paths.is_empty() {
        default_targets(&root)
    } else {
        paths.to_vec()
    };

    if files.is_empty() {
        println!(
            "  {} no context-record files found. Ingest some first: {}",
            "·".dimmed(),
            "stella ingest CLAUDE.md".dimmed()
        );
        return Ok(());
    }

    let checked_at = stella_context::format_rfc3339(crate::memory::unix_now_secs());
    let mut tally = Tally::default();
    println!();

    for file in &files {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .to_string_lossy()
            .to_string();
        match load_context_file(file) {
            Ok(context) => {
                println!("  {}", rel.bold());
                for record in records_of(&context) {
                    check_record(&root, record, &checked_at, &mut tally);
                }
            }
            Err(err) => {
                // A malformed file is reported and skipped, not fatal — one bad
                // file must not hide the staleness of the others.
                eprintln!("  {}  {}", rel.red(), err.dimmed());
            }
        }
    }

    summarize(&tally);
    if strict && tally.refuted > 0 {
        return Err(format!("{} record(s) refuted as stale", tally.refuted));
    }
    Ok(())
}

/// Re-run one record's honored probe and report the verdict.
fn check_record(root: &Path, record: &Record, checked_at: &str, tally: &mut Tally) {
    let statement = truncate(&record.statement);
    let Some(probe) = record.honored_probe() else {
        // No probe, or a gated one that is never honored here: unfalsifiable by
        // absence. Visible, never counted as a failure.
        tally.unchecked += 1;
        println!(
            "    {} {statement}  {}",
            "·".dimmed(),
            "unfalsifiable".dimmed()
        );
        return;
    };
    let refutation = probe::evaluate(root, probe, checked_at);
    match refutation.verdict {
        Verdict::Supported => {
            tally.supported += 1;
            println!("    {} {statement}", "✓".green());
        }
        Verdict::Unfalsifiable => {
            tally.unchecked += 1;
            println!(
                "    {} {statement}  {}",
                "·".dimmed(),
                "unfalsifiable".dimmed()
            );
        }
        Verdict::Refuted => {
            tally.refuted += 1;
            let retention = retention_for(Verdict::Refuted, record.on_expiry());
            println!(
                "    {} {statement}  {}",
                "✗".red(),
                format!("refuted → {}", retention_label(retention)).yellow()
            );
            println!("        {}", refutation.detail.dimmed());
        }
    }
}

/// The one-word consequence of a refuted claim, for the report.
fn retention_label(retention: Retention) -> &'static str {
    match retention {
        Retention::Keep => "keep",
        Retention::Warn => "review (still citing)",
        Retention::Drop => "drop from selection",
        Retention::Block => "block",
    }
}

fn summarize(tally: &Tally) {
    if tally.total() == 0 {
        println!("  {} no records with claims to check.", "·".dimmed());
        return;
    }
    let refuted = if tally.refuted > 0 {
        format!(", {} refuted (stale)", tally.refuted)
            .red()
            .to_string()
    } else {
        String::new()
    };
    println!(
        "\n  {} {} record(s): {} supported, {} unfalsifiable{}",
        "✦".magenta(),
        tally.total(),
        tally.supported,
        tally.unchecked,
        refuted
    );
}

/// Every checkable record in a file: published `[[record]]`s and the record each
/// `[[proposal]]` would create.
fn records_of(context: &ContextFile) -> Vec<&Record> {
    context
        .records
        .iter()
        .chain(context.proposals.iter().map(|p| &p.record))
        .collect()
}

/// The default scan set: every `.toml` under `.stella/proposals/` and
/// `.stella/rules/`.
fn default_targets(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in [
        root.join(".stella").join("proposals"),
        root.join(".stella").join("rules"),
    ] {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let is_toml = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("toml"));
                if is_toml {
                    out.push(path);
                }
            }
        }
    }
    out.sort();
    out
}

/// Load a context-record file, tolerating bare TOML datetimes.
///
/// The surface stores timestamps as strings ([`stella_core::ingest::record`]),
/// but hand-authored files (and the `docs/context-record-examples`) use bare
/// TOML datetime literals, which will not deserialize into a `String` field. So
/// we parse to a [`toml::Value`], rewrite every datetime to its string form, and
/// only then deserialize — this is the "the CLI normalizes bare TOML datetimes
/// on the way in" contract the schema module documents.
fn load_context_file(path: &Path) -> Result<ContextFile, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut value: toml::Value = toml::from_str(&text).map_err(|e| e.to_string())?;
    normalize_datetimes(&mut value);
    // Re-emit the normalized value (datetimes now strings) and parse it as the
    // typed surface. Round-tripping through a string uses only `to_string` +
    // `from_str`, both already exercised elsewhere in the ingest path.
    let normalized = toml::to_string(&value).map_err(|e| e.to_string())?;
    toml::from_str::<ContextFile>(&normalized).map_err(|e| e.to_string())
}

/// Recursively rewrite every TOML datetime to its RFC-3339 string form.
fn normalize_datetimes(value: &mut toml::Value) {
    match value {
        toml::Value::Datetime(dt) => {
            *value = toml::Value::String(dt.to_string());
        }
        toml::Value::Table(table) => {
            for (_key, v) in table.iter_mut() {
                normalize_datetimes(v);
            }
        }
        toml::Value::Array(items) => {
            for v in items.iter_mut() {
                normalize_datetimes(v);
            }
        }
        _ => {}
    }
}

/// Truncate a statement on a char boundary for the one-line report.
fn truncate(statement: &str) -> String {
    if statement.chars().count() <= MAX_STATEMENT {
        return statement.to_string();
    }
    let kept: String = statement.chars().take(MAX_STATEMENT - 1).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests;
