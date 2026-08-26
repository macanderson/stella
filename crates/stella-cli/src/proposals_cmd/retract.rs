// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Taking back a published rule — the reverse of `stella proposals keep`
//! (#4866).
//!
//! Every other evolution surface has a named rollback artifact: memory has
//! `stella memory reaffirm` / `restore`, tools have `stella tools --disable`,
//! workflow has `stella tune rollback`. A published rule had none. It is also
//! the one self-modification that leaves the ledger and lands as a *file*, and
//! files are what the ledger was designed not to need — so reversal was `rm
//! .stella/rules/<id>.toml` or a git revert, neither of which leaves any record
//! that Stella ever believed the thing.
//!
//! # Retraction appends; it never deletes
//!
//! Two writes, in this order:
//!
//! 1. the record's own file is rewritten with `status = "retracted"` and
//!    `supersedes_record_id` pointing at the revision it replaces, re-stamped
//!    so the file still verifies on load;
//! 2. a `LedgerAction::Retired` event is appended to the hash-chained
//!    `.stella/rules/promotions.jsonl`, carrying the status transition, the
//!    approver and the reason.
//!
//! The file stays on disk, retired rather than gone, which is what keeps "what
//! did Stella believe, and when did it stop" readable. `stella context
//! validate` already treats a non-active record this way — it reports it under
//! *Retired* and does not fail the run (#3254) — and the ledger's own
//! latest-event fold revokes any blocking grant the lineage held, which is the
//! right side effect for a rule that must no longer steer.
//!
//! # What makes it take effect
//!
//! `stella_core::records::registry`'s loader refuses to select a record whose
//! status is anything but active, so the retracted rule stops reaching the
//! cached prefix `assemble_system_prompt` renders on the next load. No new
//! filter is needed on `load_workspace_rules`: the standing is carried on the
//! record itself, which is the same place `stella ingest --refresh` writes it.
//!
//! The order matters the way `decide_in`'s does, and is the mirror image. A
//! *decision* records its event first because the event is the authority and
//! the file is a projection of it. A *retraction* rewrites the file first,
//! because the file is what steers the next turn: a ledger entry with a live
//! rule still on disk would be a retraction with no effect, while a rewritten
//! file with no ledger entry is a rule that stopped steering and can be
//! re-appended for.

use std::path::{Path, PathBuf};

use colored::Colorize;
use stella_core::context_record::RecordStatus;
use stella_core::ingest::record::{ContextFile, Record};

/// One published record, with the file it shares and where that file lives.
struct Published {
    path: PathBuf,
    file: ContextFile,
    /// Index into `file.records` — the record this retraction is about.
    index: usize,
}

impl Published {
    fn record(&self) -> &Record {
        &self.file.records[self.index]
    }
}

/// Retract a published rule: suppress it, and record who did so and why.
///
/// `id` is the candidate id `stella proposals list` shows, or the record's full
/// `ctx.<set>.<id>` lineage when the short form is ambiguous — the same two
/// spellings [`super::resolve_proposal`] accepts, for the same reason.
pub(super) fn retract_rule(workspace_root: &Path, id: &str, reason: &str) -> Result<(), String> {
    if reason.trim().is_empty() {
        return Err("a retraction must carry a reason — a governance decision \
                    with none is not auditable"
            .to_string());
    }
    let mut published = resolve_published(workspace_root, id)?;
    let record = published.record();
    if !matches!(record.status, None | Some(RecordStatus::Active)) {
        return Err(format!(
            "`{}` is already {} — nothing to retract",
            record.lineage_id,
            record
                .status
                .map(RecordStatus::as_str)
                .unwrap_or("of unset status")
        ));
    }
    let lineage_id = record.lineage_id.clone();

    retract_in_place(&mut published)?;
    record_retraction(workspace_root, &lineage_id, reason)?;

    println!("  {} retracted {}", "✓".green(), lineage_id.bold());
    println!(
        "    {} {} keeps the record, marked retracted — the loader stops \
         selecting it on the next load",
        "·".dimmed(),
        published.path.display()
    );
    Ok(())
}

/// Find the published record `id` names, reading `.stella/rules/` rather than
/// deriving the path from today's set id.
///
/// Derivation would be shorter and wrong: `derive_set_id` reads the workspace
/// directory name, so a renamed or cloned-into-a-different-directory workspace
/// would compute a lineage no file on disk carries and report the rule as
/// missing. The file is the authority for what was published, so it is what is
/// searched.
fn resolve_published(workspace_root: &Path, id: &str) -> Result<Published, String> {
    let dir = crate::memory::rules_mining::workspace_rules_dir(workspace_root);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Err(format!(
            "no published rules to retract — {} does not exist",
            dir.display()
        ));
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|x| x.to_str()) == Some("toml"))
        .filter(|path| {
            !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| crate::rules::RESERVED_RULE_FILENAMES.contains(&name))
        })
        .collect();
    paths.sort();

    let mut found: Vec<Published> = Vec::new();
    for path in paths {
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(file) = toml::from_str::<ContextFile>(&body) else {
            continue;
        };
        for index in 0..file.records.len() {
            let lineage = &file.records[index].lineage_id;
            let names_it = lineage == id
                || lineage
                    .rsplit('.')
                    .next()
                    .is_some_and(|candidate| candidate == id);
            if names_it {
                found.push(Published {
                    path: path.clone(),
                    file: file.clone(),
                    index,
                });
            }
        }
    }

    match found.len() {
        0 => Err(format!(
            "no published rule named `{id}` under {} — `stella context list` \
             shows what is published",
            dir.display()
        )),
        1 => Ok(found.remove(0)),
        // The same ambiguity `resolve_proposal` refuses, one hop later: two
        // sets can publish the same candidate id under different lineages, and
        // retracting the wrong one is the failure hardest to notice.
        _ => Err(format!(
            "`{id}` names {} published records. Re-run with one of these \
             lineage ids instead:\n{}",
            found.len(),
            found
                .iter()
                .map(|p| format!("      {}", p.record().lineage_id))
                .collect::<Vec<_>>()
                .join("\n")
        )),
    }
}

/// Rewrite the file with this record retracted, keeping every sibling record in
/// it intact.
///
/// The whole `ContextFile` is written back rather than rebuilt from the one
/// record: a published file can hold several, plus its `set_id` and
/// `[defaults]` header, and rebuilding from one resolved record silently drops
/// the rest — the hazard `replace_context_file` exists for. Durable for the
/// reason the ledger's own write is: this is Git-tracked governance and a crash
/// mid-write must not leave a truncated record where a reviewed one stood.
fn retract_in_place(published: &mut Published) -> Result<(), String> {
    let defaults = published.file.defaults.clone().unwrap_or_default();
    let record = &mut published.file.records[published.index];
    record.status = Some(RecordStatus::Retracted);
    record.supersedes_record_id = record.record_id.clone();
    record
        .stamp(&defaults)
        .map_err(|e| format!("cannot re-stamp the retracted revision: {e}"))?;
    crate::context_records::replace_context_file(&published.path, &published.file)
}

/// Append the accountable retraction event to the hash-chained ledger.
///
/// `from`/`to` carry the **status** transition rather than an enforcement
/// level, which is what `LedgerAction::Retired` means on this ledger (#2728) —
/// the same shape `stella ingest --refresh` writes when a source stops
/// asserting a claim, and the same fold that drops any blocking grant the
/// lineage held.
fn record_retraction(workspace_root: &Path, lineage_id: &str, reason: &str) -> Result<(), String> {
    let governance = crate::context_records::read_governance(workspace_root);
    crate::context_records::append_promotion(
        workspace_root,
        stella_core::records::promotion::PromotionEvent {
            seq: 0,
            prev: String::new(),
            at: crate::context_records::now_rfc3339(),
            lineage_id: lineage_id.to_string(),
            from: RecordStatus::Active.as_str().to_string(),
            to: RecordStatus::Retracted.as_str().to_string(),
            approver: crate::context_cmd::actor(),
            proposer: None,
            reason: format!("retracted by `stella proposals retract`: {}", reason.trim()),
            mode: governance.mode.as_str().to_string(),
            action: stella_core::records::promotion::LedgerAction::Retired,
        },
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests;
