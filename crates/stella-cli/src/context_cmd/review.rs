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
//! **The probe is re-run here, not read off the file** (#4261). A reviewer reads
//! the verdict as a statement about the tree they are looking at, and a proposal
//! can sit in the queue for weeks: this repository's own queue held four claims
//! rendering `supported` against paths a later PR had deleted, one of them naming
//! a file `stella-tools` no longer has. `ingest_cmd::probe::evaluate` is a
//! filesystem read for every kind that survives the sweep's gate, so re-asking
//! costs a `stat` per proposal and buys the difference between a measurement and
//! a memory. Where nothing can re-ask — a `manual` cadence, `none`, a gated probe
//! on an unaudited record — the stored verdict is shown dimmed and dated instead
//! of borrowing a fresh one's colour.
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

use stella_core::ingest::record::SharingScope;
use stella_core::ingest::refresh::same_statement;
use stella_core::records::{Decision, DecisionEvent, decision};

use super::{FoundProposal, read_proposals, resolve_candidate, verdict_label};
use crate::context_records::{
    append_decision, now_rfc3339, publication_path, read_decisions, replace_record, write_record,
};

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

    // Re-run every runnable probe against the tree the reviewer is looking at,
    // once, before anything is ordered or printed (#4261). The stored verdict
    // is the one `stella ingest` recorded, and a proposal can sit in the queue
    // for weeks: four in this repository's own queue rendered `supported` for
    // paths a later PR had deleted. Ordering reads the fresh verdict too —
    // "refuted first" is worth nothing if it sorts on a stale answer.
    let mut ordered: Vec<(&FoundProposal, Probed)> = proposals
        .iter()
        .map(|found| {
            let probed = reprobe(root, found, &now);
            (found, probed)
        })
        .collect();
    ordered.sort_by(|(a, a_probe), (b, b_probe)| {
        rank(a_probe.verdict())
            .cmp(&rank(b_probe.verdict()))
            .then_with(|| a.proposal.candidate_id.cmp(&b.proposal.candidate_id))
    });

    let mut shown = 0;
    let mut dismissed = 0;
    println!();
    for (found, probed) in ordered {
        let proposal = &found.proposal;
        let decided = states.get(&proposal.candidate_id);
        if proposal.dismissed_reason.is_some() && !show_all {
            dismissed += 1;
            continue;
        }
        shown += 1;
        print_proposal(found, &probed, decided, &now);
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

/// What the probe says about the tree the reviewer is looking at.
///
/// The two arms are not two renderings of one fact — they are different claims,
/// and collapsing them is the defect (#4261). `Fresh` is an answer about the
/// current tree. `Recorded` is an answer about the tree at ingest time, kept
/// because a `manual` or `none` probe has no machine to re-ask and the recorded
/// note is still the best a reviewer has; it renders dimmed and dated so it can
/// never be mistaken for the other one.
enum Probed {
    /// The probe was re-run just now against the workspace.
    Fresh {
        refutation: stella_core::ingest::record::Refutation,
        /// The verdict the proposal file stores, when it disagrees with the
        /// one above — the tree moved under the claim, and that is news.
        superseded: Option<(String, String)>,
    },
    /// No machine could re-ask: the proposal's stored verdict, as stored.
    Recorded(Option<stella_core::ingest::record::Refutation>),
}

impl Probed {
    /// The verdict to sort and colour by — the fresh one whenever there is one.
    fn verdict(&self) -> Option<&str> {
        match self {
            Self::Fresh { refutation, .. } => Some(refutation.verdict.as_str()),
            Self::Recorded(stored) => stored.as_ref().map(|r| r.verdict.as_str()),
        }
    }
}

/// Worst news first: refuted, then unfalsifiable (and unprobed), then supported.
fn rank(verdict: Option<&str>) -> u8 {
    match verdict {
        Some("refuted") => 0,
        Some("unfalsifiable") | None => 1,
        _ => 2,
    }
}

/// Re-run this proposal's probe against the workspace, or explain why nothing
/// could.
///
/// `honored_probe` applies the same gate the sweep does, so a `command_succeeds`
/// or `http_ok` probe on an `imported`/`inferred` record is not run here either
/// — review is a read of the tree, and it does not become the place a document
/// nobody audited gets to execute a command. `evaluate` itself is filesystem
/// only for the kinds that survive that gate, which is what makes re-asking on
/// every review cheap enough to be unconditional.
fn reprobe(root: &Path, found: &FoundProposal, now: &str) -> Probed {
    use stella_core::ingest::record::ProbeKind;

    let stored = found.proposal.refutation.clone();
    let Some(probe) = stella_core::records::honored_probe(&found.proposal.record) else {
        return Probed::Recorded(stored);
    };
    if matches!(probe.kind, ProbeKind::Manual | ProbeKind::None) {
        return Probed::Recorded(stored);
    }
    let refutation = crate::ingest_cmd::probe::evaluate(root, probe, now);
    let superseded = stored
        .filter(|old| old.verdict != refutation.verdict)
        .map(|old| (old.verdict.as_str().to_string(), old.checked_at));
    Probed::Fresh {
        refutation,
        superseded,
    }
}

/// One proposal, with everything that changes the decision.
fn print_proposal(
    found: &FoundProposal,
    probed: &Probed,
    decided: Option<&decision::CandidateState>,
    now: &str,
) {
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

    match probed {
        Probed::Fresh {
            refutation,
            superseded,
        } => {
            println!(
                "    probe: {}  {}",
                verdict_label(refutation.verdict.as_str()),
                refutation.detail.dimmed()
            );
            if let Some((was, at)) = superseded {
                println!(
                    "    {}",
                    format!("the tree has moved: recorded {was} at {at}").yellow()
                );
            }
            if let Some(recommend) = refutation.recommend.as_deref() {
                println!("    {}", format!("recommended: {recommend}").yellow());
            }
        }
        Probed::Recorded(Some(refutation)) => {
            // Dimmed whatever it says, including `supported`. A green cell for
            // a claim nothing re-checked is the exact reading this command was
            // giving a reviewer who had no way to know (#4261).
            println!(
                "    {}",
                format!(
                    "probe: {} (recorded {}, not re-checked — nothing here can re-ask)",
                    refutation.verdict.as_str(),
                    refutation.checked_at
                )
                .dimmed()
            );
            println!("    {}", refutation.detail.dimmed());
            if let Some(recommend) = refutation.recommend.as_deref() {
                println!("    {}", format!("recommended: {recommend}").yellow());
            }
        }
        Probed::Recorded(None) => {}
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
            Decision::Keep => format!("already kept ({})", state.at),
            Decision::Edit => format!("already edited and published ({})", state.at),
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
    // A lineage that is already published forks two ways on content (#2708):
    // the same claim again is a refusal (there is nothing to change, and a
    // published record is somebody's reviewed work), while a *different* claim
    // is a supersession — the new revision replaces the file, carries a
    // `supersedes_record_id` link to the revision it retires, and the old
    // revision survives where every prior revision on this substrate does, in
    // the repository history. Records are never edited; they are succeeded.
    let mut superseded: Option<String> = None;
    if path.exists() {
        match published_record_at(&path) {
            Some(existing) if same_statement(&existing.statement, &record.statement) => {
                return Err(format!(
                    "{} already exists — refusing to overwrite it. A published record is \
                     somebody's reviewed work; this candidate says the same thing, so there is \
                     nothing to supersede. Edit the file directly, or delete it first if you \
                     meant to replace it.",
                    path.display()
                ));
            }
            Some(existing) => match existing.record_id {
                Some(old_id) => {
                    record.supersedes_record_id = Some(old_id.clone());
                    // Re-stamp: the supersession link is canonical content, so
                    // it must be inside the revision's hash, not beside it.
                    record
                        .stamp(&found.defaults)
                        .map_err(|e| format!("cannot canonicalize the record: {e}"))?;
                    superseded = Some(old_id);
                }
                None => {
                    return Err(format!(
                        "{} already exists but its record is unstamped, so a superseding \
                         revision has no identity to cite. Run `stella context validate` to see \
                         the finding, or edit the file directly.",
                        path.display()
                    ));
                }
            },
            None => {
                return Err(format!(
                    "{} already exists — refusing to overwrite it. A published record is \
                     somebody's reviewed work; edit the file directly, or delete it first if \
                     you meant to replace it.",
                    path.display()
                ));
            }
        }
    }

    match &superseded {
        Some(old_id) => {
            replace_record(&path, &found.set_id, &record)?;
            // The accountable supersession event (spec §4, #2728) — file
            // first, ledger second, and a ledger failure is loud: an
            // unrecorded supersession is what the ledger exists to prevent.
            let governance = crate::context_records::read_governance(root);
            crate::context_records::append_promotion(
                root,
                stella_core::records::promotion::PromotionEvent {
                    seq: 0,
                    prev: String::new(),
                    at: now_rfc3339(),
                    lineage_id: record.lineage_id.clone(),
                    from: "active".to_string(),
                    to: "superseded".to_string(),
                    approver: actor(),
                    proposer: None,
                    reason: format!(
                        "{old_id} superseded by {} via `stella context keep`",
                        record.record_id.as_deref().unwrap_or("<unstamped>")
                    ),
                    mode: governance.mode.as_str().to_string(),
                    action: stella_core::records::promotion::LedgerAction::Superseded,
                },
            )?;
        }
        None => write_record(&path, &found.set_id, &record)?,
    }

    let decision_kind = if statement.is_some() {
        Decision::Edit
    } else {
        Decision::Keep
    };
    // Recorded repo-relative when the record is inside the tree: the ledger is read
    // by whoever opens the repository next, and an absolute path names one machine's
    // directory layout. A personal record's path stays absolute — it genuinely is
    // outside this tree.
    let recorded_path = path
        .strip_prefix(root)
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.display().to_string());
    let mut event = DecisionEvent::keep(
        proposal.candidate_id.clone(),
        record.lineage_id.clone(),
        actor(),
        now_rfc3339(),
        recorded_path,
    );
    event.decision = decision_kind;
    event.approved_blocking = enforce;
    if let Some(old_id) = &superseded {
        // The ledger is the audit trail; a supersession that only the file
        // diff records is invisible to anyone replaying decisions.
        event.reason = Some(format!("supersedes {old_id}"));
    }
    append_decision(root, &event)?;

    println!("\n  {}  {}", "published".green(), path.display());
    if let Some(old_id) = &superseded {
        println!(
            "    {}",
            format!(
                "supersedes {old_id} — the prior revision leaves selection now and stays \
                 readable in git history"
            )
            .yellow()
        );
    }
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

/// The record stored in a published file, exactly as the file spells it.
///
/// A raw TOML read, deliberately not [`stella_core::records::load_context_file`]:
/// the loader re-stamps for verification, and a supersession link must cite
/// the `record_id` the file actually carries, not one recomputed today.
/// `None` when the file cannot be read or parsed — the caller then falls back
/// to the plain refusal rather than superseding something it cannot see.
fn published_record_at(path: &Path) -> Option<stella_core::ingest::record::Record> {
    let contents = std::fs::read_to_string(path).ok()?;
    let file: stella_core::ingest::record::ContextFile = toml::from_str(&contents).ok()?;
    file.records.first().cloned()
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
/// Who is deciding. A local username is enough for solo mode; team mode carries a
/// real identity through Git authorship instead.
pub(crate) fn actor() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "local".to_string())
}
