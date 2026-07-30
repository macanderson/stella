//! `stella context list` and `stella context validate`.
//!
//! `list` answers *what steers this workspace right now*. `validate` answers *and
//! is any of it wrong* — it re-runs every claim's probe on demand and reports every
//! finding and conflict the loader would otherwise only whisper into a diagnostic.
//!
//! # Why validate re-probes instead of reading the cache
//!
//! The scheduled sweep is cadence-driven: a record with `review_every = "P90D"`
//! checked last week will not be re-checked this session, which is exactly what you
//! want in front of a prompt and exactly what you do not want from a command whose
//! whole job is to tell you the truth about now. So `validate` probes everything and
//! deliberately **does not** write the result to the sweep cache — reporting on
//! demand must not move the scheduled sweep's clock forward, or running `validate`
//! twice would silence a record that is genuinely due.
//!
//! # Why a blocking finding is a non-zero exit
//!
//! So it can be a CI check (§12: validation checks run as PR checks). A forbidden-data
//! leak, an unparseable guard, or a refuted claim means something in the repository
//! must not steer an agent, and a command that reported that on stdout with exit 0
//! would be a check that always passes.

use std::path::Path;

use colored::Colorize;
use serde::Serialize;

use stella_core::records::{Disposition, Registry, Severity};

use crate::context_records::{
    SweepCache, now_rfc3339, probe_everything, registry_with_cache, rule_files,
};

/// `stella context list`.
pub fn run_list(root: &Path, json: bool) -> Result<(), String> {
    let registry = crate::context_records::load_registry(root);
    if json {
        let rows: Vec<RecordRow> = registry.entries.iter().map(RecordRow::from_entry).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    if registry.entries.is_empty() {
        println!("Nothing steers this workspace yet.");
        println!(
            "  {}",
            "stella ingest AGENTS.md   # turn instructions you already wrote into records".dimmed()
        );
        return Ok(());
    }

    println!();
    for entry in &registry.entries {
        let force = force_of(&registry, entry);
        // The channel is what the record actually reaches, not what its force asked
        // for. A dropped record is in no channel at all, and labelling it "cached"
        // because it declared `must` would tell the reader it is in the prompt when
        // the very next line says why it is not.
        let channel = if !entry.disposition.is_selected() {
            "NOT STEERING"
        } else if entry.disposition.forces_volatile() {
            "volatile (demoted)"
        } else if force == "must" || force == "should" {
            "cached"
        } else {
            "volatile"
        };
        println!(
            "  {}  {}",
            format!("^{}", entry.record.handle).bold(),
            format!(
                "{force} · {channel}{}",
                if entry.is_enforced() {
                    " · enforced"
                } else {
                    ""
                }
            )
            .dimmed()
        );
        println!("    {}", entry.record.record.statement);
        println!("    {}", entry.record.source.dimmed());
        if let Some(reason) = entry.disposition.reason() {
            println!("    {}", reason.yellow());
        }
    }
    print_diagnostics(&registry);
    Ok(())
}

/// `stella context validate`.
pub fn run_validate(root: &Path, json: bool) -> Result<(), String> {
    let files = rule_files(root, true, true);
    let now = now_rfc3339();
    let cache = probe_everything(root, &files, &now);
    let registry = registry_with_cache(root, &files, &cache, &now);

    if json {
        let report = Report::build(&registry, &cache);
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
        return if report.blocking > 0 {
            Err(format!("{} blocking finding(s)", report.blocking))
        } else {
            Ok(())
        };
    }

    println!();
    print_probes(&registry, &cache);
    print_findings(&registry);
    print_conflicts(&registry);
    print_diagnostics(&registry);

    let blocked = print_blocked(&registry);
    if blocked > 0 {
        return Err(format!(
            "{blocked} record(s) must not steer anything until this is resolved"
        ));
    }
    println!(
        "{}",
        "Every loaded record is schema-valid and currently believed.".green()
    );
    Ok(())
}

/// The probe results, grouped so `unfalsifiable` stays visible.
///
/// It is listed on its own line rather than folded in with `supported`, because a
/// checker that reports OK for claims it never checked launders unvalidated content
/// with a validated stamp — the count of things nobody can check is a number a
/// reviewer should see.
fn print_probes(registry: &Registry, cache: &SweepCache) {
    let mut supported = 0;
    let mut refuted = Vec::new();
    let mut unfalsifiable = Vec::new();
    let mut unprobed = Vec::new();

    for entry in &registry.entries {
        match cache.checked.get(&entry.record.record.lineage_id) {
            Some(result) => match result.verdict.as_str() {
                "supported" => supported += 1,
                "refuted" => refuted.push((entry, result)),
                _ => unfalsifiable.push((entry, result)),
            },
            None => unprobed.push(entry),
        }
    }

    println!("{}", "Truth probes".bold());
    println!("  {}", format!("{supported} supported").green());
    if !refuted.is_empty() {
        println!("  {}", format!("{} refuted", refuted.len()).red());
        for (entry, result) in &refuted {
            println!(
                "    {}  {}",
                format!("^{}", entry.record.handle).red(),
                result.detail.dimmed()
            );
            println!("      {}", entry.record.record.statement.dimmed());
        }
    }
    if !unfalsifiable.is_empty() {
        println!(
            "  {}",
            format!("{} unfalsifiable", unfalsifiable.len()).yellow()
        );
        for (entry, result) in &unfalsifiable {
            println!(
                "    {}  {}",
                format!("^{}", entry.record.handle).yellow(),
                result.detail.dimmed()
            );
        }
    }
    if !unprobed.is_empty() {
        println!(
            "  {}",
            format!(
                "{} with no probe at all — nothing can tell you if these are still true",
                unprobed.len()
            )
            .dimmed()
        );
    }
    println!();
}

/// Every record that is loaded and not steering, with the reason. Returns the count.
///
/// This is the exit-code signal rather than the finding count, because the two do not
/// coincide: a refuted claim carries no finding at all — the refutation is a
/// *disposition*, produced by measuring the world rather than by inspecting the
/// record — and the very first thing this command must fail on is exactly that case.
/// Counting findings alone reported "everything is believed" while printing
/// "1 refuted" three lines above it.
fn print_blocked(registry: &Registry) -> usize {
    let blocked: Vec<&stella_core::records::Entry> = registry
        .entries
        .iter()
        .filter(|entry| !entry.disposition.is_selected())
        .collect();
    if blocked.is_empty() {
        return 0;
    }
    println!("{}", "Not steering".bold());
    for entry in &blocked {
        println!(
            "  {}  {}",
            format!("^{}", entry.record.handle).red(),
            entry.disposition.reason().unwrap_or("out of selection")
        );
        println!("    {}", entry.record.record.statement.dimmed());
        println!("    {}", entry.record.source.dimmed());
    }
    println!();
    blocked.len()
}

/// Every finding, worst first.
fn print_findings(registry: &Registry) {
    let findings = registry.findings();
    // A stamped identity on a hand-authored record is the designed path, not news.
    let worth_saying: Vec<_> = findings
        .iter()
        .filter(|(_, finding)| finding.severity() > Severity::Note)
        .collect();
    if worth_saying.is_empty() {
        return;
    }
    println!("{}", "Findings".bold());
    for (entry, finding) in worth_saying {
        let label = match finding.severity() {
            Severity::Blocking => "blocking".red(),
            Severity::Warning => "warning".yellow(),
            Severity::Note => "note".dimmed(),
        };
        println!("  {label}  {}", format!("^{}", entry.record.handle).bold());
        println!("    {finding}");
        println!("    {}", entry.record.source.dimmed());
    }
    println!();
}

/// Equal-precedence conflicts, with the §12 resolution advice.
fn print_conflicts(registry: &Registry) {
    if registry.conflicts.is_empty() {
        return;
    }
    println!("{}", "Conflicts".bold());
    for conflict in &registry.conflicts {
        println!(
            "  {}  ^{} vs ^{}",
            "overlap".yellow(),
            conflict.left,
            conflict.right
        );
        println!("    {}", conflict.detail);
    }
    println!();
}

/// Files, or blocking refusals, the loader could not honor.
fn print_diagnostics(registry: &Registry) {
    if registry.diagnostics.is_empty() {
        return;
    }
    println!("{}", "Not loaded, or not armed".bold());
    for diagnostic in &registry.diagnostics {
        println!("  {}  {}", diagnostic.source.dimmed(), diagnostic.detail);
    }
    println!();
}

/// The record's steering force, as a string.
fn force_of(_registry: &Registry, entry: &stella_core::records::Entry) -> &'static str {
    entry
        .record
        .record
        .steering
        .as_ref()
        .map(|steering| steering.force.as_str())
        .unwrap_or("info")
}

/// One record in `list --json`. A struct rather than a `json!` literal so the field
/// order is the order written here — `serde_json` sorts map keys, which makes a
/// hand-built object read alphabetically instead of meaningfully.
#[derive(Debug, Serialize)]
struct RecordRow {
    handle: String,
    lineage_id: String,
    statement: String,
    kind: String,
    force: String,
    enforcement: String,
    enforced: bool,
    disposition: String,
    reason: Option<String>,
    source: String,
    record_hash: Option<String>,
    findings: Vec<String>,
}

impl RecordRow {
    fn from_entry(entry: &stella_core::records::Entry) -> Self {
        let record = &entry.record.record;
        Self {
            handle: entry.record.handle.clone(),
            lineage_id: record.lineage_id.clone(),
            statement: record.statement.clone(),
            kind: record.kind.as_str().to_string(),
            force: record
                .steering
                .as_ref()
                .map(|steering| steering.force.as_str().to_string())
                .unwrap_or_else(|| "info".to_string()),
            enforcement: record
                .enforcement
                .as_ref()
                .map(|enforcement| enforcement.mode.as_str().to_string())
                .unwrap_or_else(|| "none".to_string()),
            enforced: entry.is_enforced(),
            disposition: disposition_label(&entry.disposition).to_string(),
            reason: entry.disposition.reason().map(str::to_string),
            source: entry.record.source.clone(),
            record_hash: record.record_hash.clone(),
            findings: entry
                .record
                .findings
                .iter()
                .map(ToString::to_string)
                .collect(),
        }
    }
}

/// The full `validate --json` report.
#[derive(Debug, Serialize)]
struct Report {
    blocking: usize,
    warnings: usize,
    supported: usize,
    refuted: usize,
    unfalsifiable: usize,
    unprobed: usize,
    records: Vec<RecordRow>,
    conflicts: Vec<ConflictRow>,
    diagnostics: Vec<DiagnosticRow>,
}

#[derive(Debug, Serialize)]
struct ConflictRow {
    left: String,
    right: String,
    suspended: String,
    detail: String,
}

#[derive(Debug, Serialize)]
struct DiagnosticRow {
    source: String,
    detail: String,
}

impl Report {
    fn build(registry: &Registry, cache: &SweepCache) -> Self {
        let findings = registry.findings();
        let count = |verdict: &str| {
            registry
                .entries
                .iter()
                .filter(|entry| {
                    cache
                        .checked
                        .get(&entry.record.record.lineage_id)
                        .is_some_and(|result| result.verdict == verdict)
                })
                .count()
        };
        Self {
            blocking: registry
                .entries
                .iter()
                .filter(|entry| !entry.disposition.is_selected())
                .count(),
            warnings: findings
                .iter()
                .filter(|(_, finding)| finding.severity() == Severity::Warning)
                .count(),
            supported: count("supported"),
            refuted: count("refuted"),
            unfalsifiable: count("unfalsifiable"),
            unprobed: registry
                .entries
                .iter()
                .filter(|entry| !cache.checked.contains_key(&entry.record.record.lineage_id))
                .count(),
            records: registry.entries.iter().map(RecordRow::from_entry).collect(),
            conflicts: registry
                .conflicts
                .iter()
                .map(|conflict| ConflictRow {
                    left: conflict.left.clone(),
                    right: conflict.right.clone(),
                    suspended: conflict.suspended.clone(),
                    detail: conflict.detail.clone(),
                })
                .collect(),
            diagnostics: registry
                .diagnostics
                .iter()
                .map(|diagnostic| DiagnosticRow {
                    source: diagnostic.source.clone(),
                    detail: diagnostic.detail.clone(),
                })
                .collect(),
        }
    }
}

/// The disposition's machine-readable name.
fn disposition_label(disposition: &Disposition) -> &'static str {
    match disposition {
        Disposition::Select => "select",
        Disposition::SelectUnfalsifiable => "select_unfalsifiable",
        Disposition::SelectStale { .. } => "select_stale",
        Disposition::Drop { .. } => "drop",
        Disposition::Block { .. } => "block",
    }
}
