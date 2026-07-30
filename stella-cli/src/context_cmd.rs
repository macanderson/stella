//! `stella context` — the review, publication, and explanation surface for
//! context records.
//!
//! `stella ingest` writes proposals to `.stella/proposals/*.toml`. Before this
//! command existed, nothing consumed them: a proposal nobody can act on is a dead
//! end, and the extraction work was invisible. This is the other half.
//!
//! ```text
//! stella context review              # what was proposed, and what the probe says
//! stella context keep <id>           # publish it as a record the engine loads
//! stella context edit <id>           # publish your wording instead
//! stella context ignore <id>         # decline it, with a cooldown
//! stella context list                # what currently steers this workspace
//! stella context validate            # re-probe every claim, on demand
//! stella context explain <handle>    # why did that rule apply?
//! stella context propose <handle>    # turn a record into a reviewable change
//! ```
//!
//! # Offline by construction
//!
//! Every subcommand reads and writes local files only. No API key, no network —
//! `review` shows what the ingest run already decided, `validate` re-runs
//! declarative filesystem probes, and `keep` writes a TOML file and appends a line
//! to a ledger. The local-first solo path is a requirement, not a convenience
//! (`docs/context-pr.md` §18: no centralized service for the solo path).
//!
//! # Two proposal substrates, deliberately separate
//!
//! This reviews **file-based** proposals extracted from documents.
//! `stella proposals` reviews **behavioral** proposals mined from what the agent
//! did, out of `context.db`. They are not merged, and the reason is authority: an
//! explicit instruction in a tracked file is eligible immediately, while a mined
//! pattern must clear a distinct-task threshold first (§7). One review surface over
//! both would apply one of those two gates to content it does not fit.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use colored::Colorize;

use stella_core::ingest::record::{ContextFile, Proposal};

mod explain;
mod propose;
mod review;
mod validate;

/// `stella context <cmd>`.
#[derive(Debug, Subcommand)]
pub enum ContextCmd {
    /// What `stella ingest` proposed, with the evidence and the probe verdict for
    /// each claim. Nothing steers until you keep it.
    Review {
        /// Show dismissed proposals too — the compound claims and quarantined
        /// executable content extraction refused.
        #[arg(long)]
        all: bool,
    },

    /// Publish a proposal as a record the engine loads. Writes
    /// `.stella/rules/<lineage>.toml` (or `~/.stella/rules/` for a personal
    /// record) and never overwrites an existing file.
    Keep {
        /// The proposal's candidate id, or a unique prefix of it.
        candidate: String,
        /// Also approve this record to block matching tool calls. Separate from
        /// keeping on purpose: keeping a record and authorizing it to deny a tool
        /// call are different grants (§11).
        #[arg(long)]
        enforce: bool,
    },

    /// Publish your wording instead of the extractor's. The claim keeps its
    /// lineage and evidence; the statement becomes yours.
    Edit {
        /// The proposal's candidate id, or a unique prefix of it.
        candidate: String,
        /// The statement to publish.
        #[arg(long)]
        statement: String,
        /// Also approve this record to block matching tool calls.
        #[arg(long)]
        enforce: bool,
    },

    /// Decline a proposal. Records negative evidence and a re-proposal cooldown,
    /// so the next ingest of the same document does not ask again.
    Ignore {
        /// The proposal's candidate id, or a unique prefix of it.
        candidate: String,
        /// Why. A decline with no reason is evidence nobody can act on later.
        #[arg(long)]
        reason: Option<String>,
        /// How long to decline for, as an ISO-8601 duration (default P90D).
        #[arg(long)]
        cooldown: Option<String>,
    },

    /// What currently steers this workspace: every loaded record, its handle, its
    /// force, and whether it actually blocks anything.
    List {
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },

    /// Re-run every claim's truth probe now and report the result, plus every
    /// validation finding and equal-precedence conflict. Exits non-zero when
    /// something must not steer.
    Validate {
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },

    /// Why did that rule apply? Provenance, evidence, enforcement, and efficacy
    /// for one record.
    Explain {
        /// The record's `^handle` (the caret is optional) or its lineage id.
        rule: String,
    },

    /// Turn a record into a reviewable change: a branch, a single-concern diff,
    /// and a PR body — or, in solo mode, the local commit that publishes it.
    Propose {
        /// The record's `^handle` or lineage id.
        rule: String,
        /// Create the branch and commit locally instead of only printing the plan.
        #[arg(long)]
        commit: bool,
    },
}

/// Entry point for `stella context`.
pub fn run_context(cmd: &ContextCmd) -> Result<(), String> {
    let root = std::env::current_dir().map_err(|e| format!("cannot determine workspace: {e}"))?;
    match cmd {
        ContextCmd::Review { all } => review::run_review(&root, *all),
        ContextCmd::Keep { candidate, enforce } => {
            review::run_keep(&root, candidate, None, *enforce)
        }
        ContextCmd::Edit {
            candidate,
            statement,
            enforce,
        } => review::run_keep(&root, candidate, Some(statement), *enforce),
        ContextCmd::Ignore {
            candidate,
            reason,
            cooldown,
        } => review::run_ignore(&root, candidate, reason.clone(), cooldown.as_deref()),
        ContextCmd::List { json } => validate::run_list(&root, *json),
        ContextCmd::Validate { json } => validate::run_validate(&root, *json),
        ContextCmd::Explain { rule } => explain::run_explain(&root, rule),
        ContextCmd::Propose { rule, commit } => propose::run_propose(&root, rule, *commit),
    }
}

/// One proposal, with the file it came from — everything a review action needs.
#[derive(Debug)]
pub(crate) struct FoundProposal {
    /// The proposal.
    pub proposal: Proposal,
    /// The file it was read from.
    pub path: PathBuf,
    /// The record-set slug from that file's header.
    pub set_id: String,
    /// The file's defaults, which the proposal's record inherits on publication.
    pub defaults: stella_core::ingest::record::Defaults,
}

/// Every proposal in `.stella/proposals/*.toml`, file-name order.
///
/// A file that does not parse is reported to stderr and skipped: one malformed
/// proposal file must not hide the readable ones, and staying silent about it would
/// make a failed ingest look like an ingest that found nothing.
pub(crate) fn read_proposals(root: &Path) -> Vec<FoundProposal> {
    let dir = root.join(crate::context_records::PROPOSALS_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|x| x.to_str()) == Some("toml"))
        .collect();
    paths.sort();

    let mut out = Vec::new();
    for path in paths {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        match toml::from_str::<ContextFile>(&raw) {
            Ok(file) => {
                let defaults = file.defaults.clone().unwrap_or_default();
                for proposal in file.proposals {
                    out.push(FoundProposal {
                        proposal,
                        path: path.clone(),
                        set_id: file.set_id.clone(),
                        defaults: defaults.clone(),
                    });
                }
            }
            Err(err) => eprintln!(
                "  {}  {}",
                path.display().to_string().red(),
                err.to_string().dimmed()
            ),
        }
    }
    out
}

/// Resolve a candidate id or unique prefix to exactly one proposal.
///
/// An ambiguous prefix is an error naming every match rather than a guess: these
/// commands publish policy, and publishing the wrong claim because two ids shared
/// six characters is not a mistake a person would find quickly.
pub(crate) fn resolve_candidate<'a>(
    proposals: &'a [FoundProposal],
    needle: &str,
) -> Result<&'a FoundProposal, String> {
    let exact: Vec<&FoundProposal> = proposals
        .iter()
        .filter(|found| found.proposal.candidate_id == needle)
        .collect();
    if let [found] = exact.as_slice() {
        return Ok(found);
    }
    let matches: Vec<&FoundProposal> = proposals
        .iter()
        .filter(|found| found.proposal.candidate_id.starts_with(needle))
        .collect();
    match matches.as_slice() {
        [found] => Ok(found),
        [] => Err(format!(
            "no proposal matches \"{needle}\" — run `stella context review` to see what is pending"
        )),
        many => Err(format!(
            "\"{needle}\" matches {} proposals:\n{}",
            many.len(),
            many.iter()
                .map(|found| format!("  {}", found.proposal.candidate_id))
                .collect::<Vec<_>>()
                .join("\n")
        )),
    }
}

/// The three-valued verdict, colored the way it reads: a refutation is the one a
/// person must look at.
pub(crate) fn verdict_label(verdict: &str) -> colored::ColoredString {
    match verdict {
        "supported" => "supported".green(),
        "refuted" => "refuted".red(),
        _ => "unfalsifiable".yellow(),
    }
}

#[cfg(test)]
mod tests;
