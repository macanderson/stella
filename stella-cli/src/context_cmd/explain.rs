//! `stella context explain <handle>` — why did that rule apply?
//!
//! `docs/design/adaptive-context/context-pr.md` §13 promises this exchange works:
//!
//! ```text
//! > Why did you add an integration test?
//!
//! Applied rule: api-integration-coverage
//! Reason: src/api/refunds.ts matched this rule's scope.
//! Authority: merged Context PR #184 (.stella/rules/api-integration-coverage.md @ 6ee3d4a).
//! Enforcement: advisory.
//! ```
//!
//! Four separate facts, and each one comes from a different place: the statement
//! from the record, the match reason from `applies_to`, the authority from
//! provenance, and the enforcement from whether a guard actually armed. This command
//! is the lookup that assembles them from a `^handle` — the token the agent writes.
//!
//! # Why the handle is the lookup key
//!
//! Because it is the only identifier the model ever sees. `record_id` is content-
//! derived and opaque (`rec_acme_web_pkg_manager_9f3c…`), so an agent asked to cite
//! its sources could not produce one; a lineage id it also never receives. Explain
//! accepts all three, but the handle is the one that makes the exchange above
//! possible without the model having to be told an identifier it has no use for.
//!
//! # Where efficacy numbers come from, and where they do not
//!
//! Selection and citation counts live in the context ledger, which is a separate
//! store with its own retention policy. When it is absent — the common solo case —
//! this command says so rather than printing zeros: "cited 0 times" and "we have no
//! citation data" look identical on screen and mean opposite things, and the first
//! one would make a healthy new record look ignored.

use std::path::Path;

use colored::Colorize;

use stella_core::records::{Disposition, Entry, Registry};

/// `stella context explain <rule>`.
pub fn run_explain(root: &Path, needle: &str) -> Result<(), String> {
    let registry = crate::context_records::load_registry(root);
    let entry = find(&registry, needle).ok_or_else(|| not_found(&registry, needle))?;
    let record = &entry.record.record;

    println!();
    println!(
        "  {}  {}",
        format!("^{}", entry.record.handle).bold(),
        record.kind.as_str().dimmed()
    );
    println!("    {}", record.statement);
    println!();

    // Scope — the "why did it match" half.
    let applies_to = record
        .steering
        .as_ref()
        .and_then(|steering| steering.applies_to.as_ref());
    match applies_to.filter(|applies| !applies.is_empty()) {
        Some(applies) => {
            let mut parts = Vec::new();
            if !applies.paths.is_empty() {
                parts.push(format!("paths {}", applies.paths.join(", ")));
            }
            if !applies.tasks.is_empty() {
                parts.push(format!("tasks {}", applies.tasks.join(", ")));
            }
            if !applies.keywords.is_empty() {
                parts.push(format!("keywords {}", applies.keywords.join(", ")));
            }
            println!("  {}  {}", "Scope".bold(), parts.join(" · "));
        }
        None => println!(
            "  {}  unscoped — it applies to every task in this workspace",
            "Scope".bold()
        ),
    }

    // Authority — provenance, at commit level where it exists.
    println!("  {}", "Authority".bold());
    println!("    {}", entry.record.source);
    if let Some(provenance) = record.provenance.as_ref() {
        if let Some(uri) = provenance.source_uri.as_deref() {
            let lines = provenance
                .source_lines
                .as_ref()
                .filter(|range| range.len() == 2)
                .map(|range| format!(":{}-{}", range[0], range[1]))
                .unwrap_or_default();
            println!("    {}", format!("extracted from {uri}{lines}").dimmed());
        }
        match provenance.commit.as_deref() {
            Some(commit) => println!("    {}", format!("at commit {commit}").dimmed()),
            None => println!(
                "    {}",
                "no source commit recorded — the record cannot be cited with commit-level \
                 provenance"
                    .dimmed()
            ),
        }
        if let Some(run) = provenance.ingest_run_id.as_deref() {
            println!("    {}", format!("ingest run {run}").dimmed());
        }
    }
    if let Some(id) = record.record_id.as_deref() {
        println!("    {}", format!("record {id}").dimmed());
    }

    // Enforcement — what actually happens, not what was asked for.
    println!("  {}", "Enforcement".bold());
    let asked = record
        .enforcement
        .as_ref()
        .map(|enforcement| enforcement.mode.as_str())
        .unwrap_or("none");
    if entry.is_enforced() {
        println!(
            "    {}",
            format!("{asked} — matching tool calls are denied at the tool boundary").red()
        );
    } else if asked == "hard" {
        println!(
            "    {}",
            "asked for hard enforcement and is NOT armed — see the refusal below".yellow()
        );
    } else {
        println!(
            "    {}",
            format!("{asked} — rendered into the prompt, no tool call is blocked").dimmed()
        );
    }
    let force = record
        .steering
        .as_ref()
        .map(|steering| steering.force.as_str())
        .unwrap_or("info");
    println!(
        "    {}",
        format!(
            "force {force} · {} channel",
            if entry.disposition.forces_volatile() {
                "volatile (demoted by the truth sweep)"
            } else if force == "must" || force == "should" {
                "cached"
            } else {
                "volatile"
            }
        )
        .dimmed()
    );

    // Truth — the axis that decides whether it steers at all.
    println!("  {}", "Truth".bold());
    if let Some(truth) = record.truth.as_ref() {
        println!(
            "    {}",
            format!(
                "basis {}{}",
                truth.basis.as_str(),
                truth
                    .verified_by
                    .as_deref()
                    .map(|by| format!(", verified by {by}"))
                    .unwrap_or_default()
            )
            .dimmed()
        );
        match truth.probe.as_ref() {
            Some(probe) => println!(
                "    {}",
                format!(
                    "probe {}{}",
                    probe.kind.as_str(),
                    probe
                        .path
                        .as_deref()
                        .map(|path| format!(" against {path}"))
                        .unwrap_or_default()
                )
                .dimmed()
            ),
            None => println!(
                "    {}",
                "no probe — nothing can tell you whether this is still true".dimmed()
            ),
        }
    } else {
        println!("    {}", "no truth block".dimmed());
    }
    match &entry.disposition {
        Disposition::Select => println!("    {}", "currently believed".green()),
        Disposition::SelectUnfalsifiable => println!(
            "    {}",
            "unfalsifiable — selected, and never counted as supported".yellow()
        ),
        Disposition::SelectStale { reason } => println!("    {}", reason.yellow()),
        Disposition::Drop { reason } | Disposition::Block { reason } => {
            println!("    {}", format!("out of selection: {reason}").red())
        }
    }

    // Findings, including the blocking refusal when there was one.
    if !entry.record.findings.is_empty() {
        println!("  {}", "Findings".bold());
        for finding in &entry.record.findings {
            println!("    {finding}");
        }
    }
    if let Some(refusal) = entry.guard.refusal.as_ref() {
        println!("  {}", "Not armed".bold());
        println!("    {refusal}");
    }

    // Efficacy — or an honest statement that we do not have it.
    println!("  {}", "Efficacy".bold());
    println!("    {}", efficacy_line(root, &entry.record.handle).dimmed());
    println!();
    Ok(())
}

/// Selection and citation counts, when the ledger has them.
///
/// The `selected` → `rendered` → `cited` funnel is recorded by the session that
/// rendered the record; a workspace that has not run a session since the record was
/// published has nothing to report, which is different from a record nobody used.
fn efficacy_line(root: &Path, handle: &str) -> String {
    let ledger = root.join(".stella/private/context.db");
    if !ledger.exists() {
        return "no context ledger in this workspace yet — selection and citation counts appear \
                once a session has run with this record loaded"
            .to_string();
    }
    format!(
        "run `stella proposals` for the behavioral loop's counts; per-record citation history for \
         ^{handle} is recorded in .stella/private/context.db"
    )
}

/// Find a record by handle (with or without the caret), lineage id, or record id.
fn find<'a>(registry: &'a Registry, needle: &str) -> Option<&'a Entry> {
    registry.by_handle(needle).or_else(|| {
        registry.entries.iter().find(|entry| {
            entry.record.record.lineage_id == needle
                || entry.record.record.record_id.as_deref() == Some(needle)
        })
    })
}

/// A not-found message that lists what *is* loaded — the next thing the reader
/// needs, and cheap to include.
fn not_found(registry: &Registry, needle: &str) -> String {
    if registry.entries.is_empty() {
        return format!(
            "no record matches \"{needle}\" — nothing is loaded in this workspace. \
             Run `stella context review` to see what has been proposed."
        );
    }
    let handles: Vec<String> = registry
        .entries
        .iter()
        .map(|entry| format!("^{}", entry.record.handle))
        .collect();
    format!(
        "no record matches \"{needle}\". Loaded: {}",
        handles.join(", ")
    )
}
