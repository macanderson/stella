//! The I/O half of `stella ingest --refresh` (#2708, second slice of #2683).
//!
//! [`stella_core::ingest::refresh`] owns the decision — which published
//! records the re-read source still asserts, which changed, which are gone.
//! This module supplies what the decision needs and carries out its one
//! mutating verdict:
//!
//! - **Loading.** Published records come from `.stella/rules/*.toml`, matched
//!   to the source by `provenance.source_uri`. Repository scope only, and that
//!   is complete for ingest-published records: ingest stamps every proposal
//!   `sharing_scope = repository`, so nothing it produced can publish
//!   anywhere else without a hand edit that also changes its provenance
//!   story.
//! - **Retiring.** A record the source no longer asserts is rewritten in
//!   place as a new revision with `status = "archived"` — re-stamped, so the
//!   file still verifies on load, and durably written, so a crash cannot
//!   leave a half-replaced governance file. Selection already refuses any
//!   non-active record (`records::registry::blocking_reason`), so an archived
//!   revision leaves steering the moment it lands. The prior revision is not
//!   erased anywhere: the rules directory is Git-tracked, and transaction
//!   time on this substrate *is* the repository history. `stella context
//!   validate` reads that archived revision as history rather than as a
//!   finding, which is what lets a completed retirement end green — the two
//!   halves disagreed about it until #3254.
//! - **Changed claims retire nothing here.** Their replacements are proposals
//!   until a reviewer keeps them, and retiring the old record first would
//!   open a steering gap between the two deliberate acts. `stella context
//!   keep` performs the supersession when the reviewer accepts.

use std::path::{Path, PathBuf};

use colored::Colorize;
use stella_core::ingest::ContextFile;
use stella_core::ingest::record::SCHEMA_TAG;
use stella_core::ingest::refresh::{AssertedClaim, PublishedClaim, RefreshPlan, plan};

use crate::context_records::RULES_DIR;

/// One published record file eligible for the diff: where it lives, the
/// parsed file, and the diff-relevant projection of its record.
struct PublishedFile {
    path: PathBuf,
    file: ContextFile,
    claim: PublishedClaim,
}

/// Apply the refresh verdict for one re-extracted source file and narrate it.
///
/// Reports even a no-op run: `--refresh` is an explicit act, and "nothing to
/// retire" is an answer, not an absence of one.
pub(crate) fn apply(root: &Path, rel: &str, asserted: &[AssertedClaim]) -> Result<(), String> {
    let published = published_from_source(root, rel);
    if published.is_empty() {
        println!(
            "    {}",
            "refresh: no published records cite this file yet — nothing to reconcile \
             (`stella context review` publishes proposals)."
                .dimmed()
        );
        return Ok(());
    }

    let claims: Vec<PublishedClaim> = published.iter().map(|p| p.claim.clone()).collect();
    let verdict: RefreshPlan = plan(claims, asserted);

    // Group retirements by the file they live in and rewrite each file ONCE.
    // `write_record` emits one record per file, but a hand-authored file may
    // carry several — and per-record rewrites each started from a snapshot
    // parsed before the loop, so the second retirement in a file silently
    // undid the first on disk while both ledger events said otherwise.
    let mut by_path: std::collections::BTreeMap<&PathBuf, Vec<&PublishedFile>> =
        std::collections::BTreeMap::new();
    for retired in &verdict.retired {
        if let Some(entry) = published.iter().find(|p| p.claim == *retired) {
            by_path.entry(&entry.path).or_default().push(entry);
        }
    }

    let mut failures = Vec::new();
    let mut failed_records = 0usize;
    for (_, entries) in by_path {
        // File first, ledger second: the archived revisions are the
        // retirement; the events are its accountable record (spec §4, #2728).
        // A ledger failure after a successful rewrite is loud, not absorbed —
        // an unrecorded retirement is exactly what the ledger exists to
        // prevent.
        let outcome = retire(&entries).and_then(|()| {
            entries
                .iter()
                .try_for_each(|entry| record_retirement(root, rel, &entry.claim))
        });
        match outcome {
            Ok(()) => {
                for entry in &entries {
                    println!(
                        "    {} {}  {}",
                        "retired".yellow(),
                        entry.claim.lineage_id,
                        "— the source no longer asserts it; revision archived, history in git"
                            .dimmed()
                    );
                }
            }
            Err(err) => {
                failed_records += entries.len();
                failures.push(format!(
                    "{}: {err}",
                    entries
                        .iter()
                        .map(|e| e.claim.lineage_id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
    }

    println!(
        "    {} refresh: {} unchanged (kept), {} changed (proposed — `stella context keep` \
         supersedes the old revision), {} retired",
        "·".dimmed(),
        verdict.unchanged.len(),
        verdict.changed.len(),
        verdict.retired.len() - failed_records,
    );
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "could not retire {} record(s): {}",
            failures.len(),
            failures.join("; ")
        ))
    }
}

/// Every live published record whose provenance cites `rel`, with the file it
/// lives in. Unreadable or unparseable files are skipped — the sweep and the
/// registry already report those; a refresh must not double-report them, and
/// it must never retire what it cannot read.
fn published_from_source(root: &Path, rel: &str) -> Vec<PublishedFile> {
    let dir = root.join(RULES_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    paths.sort();
    for path in paths {
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(file) = toml::from_str::<ContextFile>(&contents) else {
            continue;
        };
        if file.schema != SCHEMA_TAG {
            continue;
        }
        // Published files carry exactly one record (`write_record`); tolerate
        // more, but the diff projects each record independently anyway.
        for record in &file.records {
            let cites_source = record
                .provenance
                .as_ref()
                .and_then(|p| p.source_uri.as_deref())
                == Some(rel);
            let live = matches!(
                record.status,
                None | Some(stella_core::context_record::RecordStatus::Active)
            );
            let Some(record_id) = record.record_id.clone() else {
                // An unstamped record has no identity a supersession or a
                // retirement revision could cite; the validator already
                // reports it, and guessing here could retire the wrong thing.
                continue;
            };
            if cites_source && live {
                out.push(PublishedFile {
                    path: path.clone(),
                    file: file.clone(),
                    claim: PublishedClaim {
                        lineage_id: record.lineage_id.clone(),
                        record_id,
                        statement: record.statement.clone(),
                    },
                });
            }
        }
    }
    out
}

/// Append the accountable retirement event (spec §4): who retired which
/// record, why, and under which governance mode — the same hash-chained
/// ledger that carries enforcement grants, extended with a lifecycle
/// vocabulary in #2728. As a side effect of the latest-event fold, this also
/// revokes any blocking grant the lineage held, which is correct: a retired
/// record must not keep the authority to deny tool calls.
fn record_retirement(root: &Path, rel: &str, claim: &PublishedClaim) -> Result<(), String> {
    let governance = crate::context_records::read_governance(root);
    crate::context_records::append_promotion(
        root,
        stella_core::records::promotion::PromotionEvent {
            seq: 0,
            prev: String::new(),
            at: crate::context_records::now_rfc3339(),
            lineage_id: claim.lineage_id.clone(),
            from: "active".to_string(),
            to: "archived".to_string(),
            approver: crate::context_cmd::actor(),
            proposer: None,
            reason: format!(
                "retired by `stella ingest --refresh {rel}`: the source no longer asserts \
                 {} (was {})",
                claim.lineage_id, claim.record_id
            ),
            mode: governance.mode.as_str().to_string(),
            action: stella_core::records::promotion::LedgerAction::Retired,
        },
    )
    .map(|_| ())
}

/// Rewrite one published file with every named record archived, in one pass.
///
/// All `entries` share the file (the caller grouped by path); mutating one
/// parsed copy and writing once is what makes two retirements in the same
/// file compose — per-record rewrites from per-record snapshots let the
/// second silently undo the first. Each archived revision supersedes the
/// revision it replaces (`supersedes_record_id` carries that record's own
/// retired id), is re-stamped so the file verifies on load, and the write is
/// durable — the same temp+fsync+rename discipline as the promotion ledger,
/// because both are reviewed governance files a crash must not truncate.
fn retire(entries: &[&PublishedFile]) -> Result<(), String> {
    let Some(first) = entries.first() else {
        return Ok(());
    };
    let mut file = first.file.clone();
    let defaults = file.defaults.clone().unwrap_or_default();
    for record in &mut file.records {
        let retired_here = entries
            .iter()
            .any(|e| record.record_id.as_deref() == Some(e.claim.record_id.as_str()));
        if !retired_here {
            continue;
        }
        record.status = Some(stella_core::context_record::RecordStatus::Archived);
        record.supersedes_record_id = record.record_id.clone();
        record
            .stamp(&defaults)
            .map_err(|e| format!("cannot re-stamp the archived revision: {e}"))?;
    }
    let body = toml::to_string_pretty(&file)
        .map_err(|e| format!("cannot serialize the archived revision: {e}"))?;
    stella_store::durable::write_atomic_preserving_mode(
        &first.path,
        body.as_bytes(),
        stella_store::durable::MODE_SHARED,
    )
    .map_err(|e| format!("cannot write {}: {e}", first.path.display()))
}

#[cfg(test)]
mod tests;
