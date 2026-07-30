//! Review: what was proposed, and Keep / Edit / Ignore over it.
//!
//! # What a reviewer needs to see
//!
//! A proposal is a claim somebody's document made, split out by a model, and
//! checked against the tree. All three of those facts change the decision, so all
//! three are shown: the statement, where in which document it came from, and what
//! the probe found. The refuted ones are listed **first** — a claim the tree already
//! contradicts is the one where keeping it does real harm, and burying it at
//! position nine of a list is how it gets kept by a reviewer working down the page.
//!
//! # What Keep actually does
//!
//! Writes `.stella/rules/<lineage>.toml` — a Git-tracked file, one record, with the
//! provenance that makes it citable — and appends an immutable decision. It
//! **refuses to overwrite** an existing file, matching `stella memory promote`'s
//! shipped behavior: a hand-edited record is somebody's work and a re-run of a
//! review must not silently replace it.
//!
//! A `personal` record goes to `~/.stella/rules/` instead. That is the substrate
//! rule ADR 0011 leans on — records live in files, and `sharing_scope` picks which
//! location — and it is also the privacy boundary: a personal record must never
//! enter a Context PR (§10), which is guaranteed most simply by never writing it
//! into the repository tree at all.
//!
//! # Why Ignore writes anything at all
//!
//! Because otherwise it does nothing. Without a recorded decline, the next
//! `stella ingest AGENTS.md` re-proposes the same claim, review costs what it cost
//! the first time, and the reviewer learns that declining is theatre. The decline
//! is negative evidence plus a deadline, and ingest reads it.

use std::path::Path;

use colored::Colorize;

use stella_core::ingest::record::{ContextFile, Record, SharingScope};
use stella_core::records::{Decision, DecisionEvent, decision};

use super::{FoundProposal, read_proposals, resolve_candidate, verdict_label};
use crate::context_records::{append_decision, now_rfc3339, publication_path, read_decisions};

/// `stella context review`.
pub fn run_review(root: &Path, show_all: bool) -> Result<(), String> {
    let proposals = read_proposals(root);
    if proposals.is_empty() {
        println!("No proposals pending.");
        println!(
            "  {}",
            "stella ingest AGENTS.md   # extract records from a document you already wrote"
                .dimmed()
        );
        return Ok(());
    }
    let states = decision::fold(&read_decisions(root));
    let now = now_rfc3339();

    // Refuted first, then unfalsifiable, then supported — worst news at the top.
    let mut ordered: Vec<&FoundProposal> = proposals.iter().collect();
    ordered.sort_by_key(|found| {
        let rank = match verdict_of(found) {
            Some("refuted") => 0,
            Some("unfalsifiable") | None => 1,
            _ => 2,
        };
        (rank, found.proposal.candidate_id.clone())
    });

    let mut shown = 0;
    let mut dismissed = 0;
    println!();
    for found in ordered {
        let proposal = &found.proposal;
        let decided = states.get(&proposal.candidate_id);
        if proposal.dismissed_reason.is_some() && !show_all {
            dismissed += 1;
            continue;
        }
        shown += 1;
        print_proposal(found, decided, &now);
    }

    if dismissed > 0 {
        println!(
            "\n{}",
            format!(
                "{dismissed} dismissed by the extraction gate (compound claims, quarantined \
                 executable content) — `--all` to see them and why."
            )
            .dimmed()
        );
    }
    if shown > 0 {
        println!("\n{}", "Nothing steers until you keep it:".dimmed());
        println!("  {}", "stella context keep <id>".dimmed());
        println!("  {}", "stella context ignore <id> --reason \"…\"".dimmed());
    }
    Ok(())
}

/// One proposal, with everything that changes the decision.
fn print_proposal(found: &FoundProposal, decided: Option<&decision::CandidateState>, now: &str) {
    let proposal = &found.proposal;
    let record = &proposal.record;
    println!(
        "  {}  {}",
        proposal.candidate_id.bold(),
        format!("{}% confidence", proposal.confidence).dimmed()
    );
    println!("    {}", record.statement);

    let force = record
        .steering
        .as_ref()
        .map(|steering| steering.force.as_str())
        .unwrap_or("info");
    let mode = record
        .enforcement
        .as_ref()
        .map(|enforcement| enforcement.mode.as_str())
        .unwrap_or("none");
    println!(
        "    {}",
        format!(
            "{} · force {force} · enforcement {mode} · {}",
            record.kind.as_str(),
            record.lineage_id
        )
        .dimmed()
    );

    println!(
        "    {}",
        format!("proposed in {}", found.path.display()).dimmed()
    );
    if let Some(provenance) = record
        .provenance
        .as_ref()
        .or(found.defaults.provenance.as_ref())
    {
        let source = provenance.source_uri.as_deref().unwrap_or("(unknown)");
        let lines = provenance
            .source_lines
            .as_ref()
            .filter(|range| range.len() == 2)
            .map(|range| format!(":{}-{}", range[0], range[1]))
            .unwrap_or_default();
        println!("    {}", format!("from {source}{lines}").dimmed());
    }

    if let Some(refutation) = proposal.refutation.as_ref() {
        println!(
            "    probe: {}  {}",
            verdict_label(refutation.verdict.as_str()),
            refutation.detail.dimmed()
        );
        if let Some(recommend) = refutation.recommend.as_deref() {
            println!("    {}", format!("recommended: {recommend}").yellow());
        }
    }
    if let Some(validation) = proposal.validation.as_ref() {
        println!(
            "    {}  {}",
            format!("validation: {}", validation.verdict).yellow(),
            validation.action.dimmed()
        );
    }
    if let Some(quarantine) = proposal.quarantine.as_ref() {
        println!(
            "    {}  {}",
            "quarantined:".yellow(),
            format!("{} in {}", quarantine.reason, quarantine.field).dimmed()
        );
    }
    if let Some(state) = decided {
        let note = match state.decision {
            Decision::Keep | Decision::Edit => {
                format!(
                    "already {} by {}",
                    state.decision.as_str(),
                    state.actor_or_unknown()
                )
            }
            Decision::Ignore => match state.cooldown_until.as_deref() {
                Some(until) if stella_core::records::clock::is_before(now, until) => {
                    format!("declined — will not be re-proposed until {until}")
                }
                Some(_) => "declined — the cooldown has lapsed".to_string(),
                None => "declined permanently".to_string(),
            },
        };
        println!("    {}", note.dimmed());
    }
    println!();
}

/// `stella context keep <id>` and `stella context edit <id> --statement …`.
///
/// `statement` present means Edit: the reviewer's wording supersedes the
/// extractor's, and the record's identity is re-stamped from the new content — an
/// edit is a new revision of the same lineage, which is exactly what a
/// content-derived id expresses.
pub fn run_keep(
    root: &Path,
    needle: &str,
    statement: Option<&str>,
    enforce: bool,
) -> Result<(), String> {
    let proposals = read_proposals(root);
    let found = resolve_candidate(&proposals, needle)?;
    let proposal = &found.proposal;

    if let Some(reason) = proposal.dismissed_reason.as_deref() {
        return Err(format!(
            "{} was dismissed by the extraction gate ({reason}) and cannot be published as it \
             stands. {}",
            proposal.candidate_id,
            match reason {
                "compound_claim" =>
                    "Re-run ingest so the claim is split, or write the atomic records by hand.",
                "quarantined_executable" =>
                    "The quarantined content is preserved in the proposal file for you to read; \
                     honoring it requires re-authoring the record yourself.",
                _ => "See `stella context review --all` for the finding.",
            }
        ));
    }

    let mut record = proposal.record.clone();
    if let Some(statement) = statement {
        record.statement = statement.trim().to_string();
        if record.statement.is_empty() {
            return Err("--statement cannot be empty".to_string());
        }
    }
    // Stamping merges the file defaults and derives identity from the *resolved*
    // record, so the published file's hash verifies on load.
    record
        .stamp(&found.defaults)
        .map_err(|e| format!("cannot canonicalize the record: {e}"))?;

    let scope = record
        .sharing_scope
        .or(found.defaults.sharing_scope)
        .unwrap_or(SharingScope::Repository);
    let path = publication_path(root, scope, &record.lineage_id)
        .ok_or_else(|| "cannot determine where to publish this record".to_string())?;
    if path.exists() {
        return Err(format!(
            "{} already exists — refusing to overwrite it. A published record is somebody's \
             reviewed work; edit the file directly, or delete it first if you meant to replace it.",
            path.display()
        ));
    }

    write_record(&path, &found.set_id, &record)?;

    let decision_kind = if statement.is_some() {
        Decision::Edit
    } else {
        Decision::Keep
    };
    let mut event = DecisionEvent::keep(
        proposal.candidate_id.clone(),
        record.lineage_id.clone(),
        actor(),
        now_rfc3339(),
        path.display().to_string(),
    );
    event.decision = decision_kind;
    event.approved_blocking = enforce;
    append_decision(root, &event)?;

    println!("\n  {}  {}", "published".green(), path.display());
    println!("    {}", record.statement);
    println!(
        "    {}",
        format!("{} · {}", decision_kind.as_str(), record.lineage_id).dimmed()
    );
    if enforce {
        println!(
            "    {}",
            "approved to block matching tool calls — it still needs an evaluable guard and a \
             trusted origin to actually arm"
                .yellow()
        );
    }
    println!(
        "\n{}",
        "It is selected into the next session's context frame (a mid-session save appears next \
         session — the prompt cache is built once)."
            .dimmed()
    );
    Ok(())
}

/// `stella context ignore <id>`.
pub fn run_ignore(
    root: &Path,
    needle: &str,
    reason: Option<String>,
    cooldown: Option<&str>,
) -> Result<(), String> {
    let proposals = read_proposals(root);
    let found = resolve_candidate(&proposals, needle)?;
    let proposal = &found.proposal;

    if let Some(cooldown) = cooldown
        && stella_core::records::clock::duration_seconds(cooldown).is_none()
    {
        return Err(format!(
            "--cooldown \"{cooldown}\" is not an ISO-8601 duration (try P90D, P6M, PT12H)"
        ));
    }

    let event = DecisionEvent::ignore(
        proposal.candidate_id.clone(),
        proposal.record.lineage_id.clone(),
        actor(),
        now_rfc3339(),
        reason.clone(),
        cooldown,
    );
    append_decision(root, &event)?;

    println!("\n  {}  {}", "declined".yellow(), proposal.candidate_id);
    println!("    {}", proposal.record.statement);
    match event.cooldown_until.as_deref() {
        Some(until) => println!(
            "    {}",
            format!("will not be re-proposed before {until}").dimmed()
        ),
        None => println!(
            "    {}",
            "will not be re-proposed (no readable cooldown deadline, so the decline stands)"
                .dimmed()
        ),
    }
    if reason.is_none() {
        println!(
            "    {}",
            "no reason recorded — `--reason` makes this decision usable evidence later".dimmed()
        );
    }
    Ok(())
}

/// Write one record as a published context file.
fn write_record(path: &Path, set_id: &str, record: &Record) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let file = ContextFile {
        schema: stella_core::ingest::record::SCHEMA_TAG.to_string(),
        set_id: set_id.to_string(),
        ingest_run_id: None,
        defaults: None,
        records: vec![record.clone()],
        proposals: Vec::new(),
    };
    let body =
        toml::to_string_pretty(&file).map_err(|e| format!("cannot serialize the record: {e}"))?;
    std::fs::write(path, body).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Who is deciding. A local username is enough for solo mode; team mode carries a
/// real identity through Git authorship instead.
pub(crate) fn actor() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "local".to_string())
}

/// The proposal's probe verdict, when one ran.
fn verdict_of(found: &FoundProposal) -> Option<&str> {
    found
        .proposal
        .refutation
        .as_ref()
        .map(|refutation| refutation.verdict.as_str())
}

/// A displayable actor for a folded decision state.
trait ActorOrUnknown {
    fn actor_or_unknown(&self) -> String;
}

impl ActorOrUnknown for decision::CandidateState {
    fn actor_or_unknown(&self) -> String {
        // The fold keeps the decision, not the actor — the log has it, and a review
        // listing does not need to re-read the log to say "already kept".
        format!("a reviewer at {}", self.at)
    }
}
