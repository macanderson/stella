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
//!   time on this substrate *is* the repository history.
//! - **Changed claims retire nothing here.** Their replacements are proposals
//!   until a reviewer keeps them, and retiring the old record first would
//!   open a steering gap between the two deliberate acts. `stella context
//!   keep` performs the supersession when the reviewer accepts.

use std::path::{Path, PathBuf};

use colored::Colorize;
use stella_core::ingest::record::SCHEMA_TAG;
use stella_core::ingest::refresh::{AssertedClaim, PublishedClaim, RefreshPlan, plan};
use stella_core::ingest::ContextFile;

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

    let mut failures = Vec::new();
    for retired in &verdict.retired {
        let Some(entry) = published.iter().find(|p| p.claim == *retired) else {
            continue;
        };
        match retire(entry) {
            Ok(()) => println!(
                "    {} {}  {}",
                "retired".yellow(),
                retired.lineage_id,
                "— the source no longer asserts it; revision archived, history in git".dimmed()
            ),
            Err(err) => failures.push(format!("{}: {err}", retired.lineage_id)),
        }
    }

    println!(
        "    {} refresh: {} unchanged (kept), {} changed (proposed — `stella context keep` \
         supersedes the old revision), {} retired",
        "·".dimmed(),
        verdict.unchanged.len(),
        verdict.changed.len(),
        verdict.retired.len() - failures.len(),
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

/// Rewrite one published file as an archived revision of its record.
///
/// The archived revision supersedes the live one (`supersedes_record_id`
/// carries the retired id), is re-stamped so the file verifies on load, and
/// is written durably — the same temp+fsync+rename discipline as the
/// promotion ledger, because both are reviewed governance files a crash must
/// not truncate.
fn retire(entry: &PublishedFile) -> Result<(), String> {
    let mut file = entry.file.clone();
    let defaults = file.defaults.clone().unwrap_or_default();
    for record in &mut file.records {
        if record.record_id.as_deref() != Some(entry.claim.record_id.as_str()) {
            continue;
        }
        record.status = Some(stella_core::context_record::RecordStatus::Archived);
        record.supersedes_record_id = Some(entry.claim.record_id.clone());
        record
            .stamp(&defaults)
            .map_err(|e| format!("cannot re-stamp the archived revision: {e}"))?;
    }
    let body = toml::to_string_pretty(&file)
        .map_err(|e| format!("cannot serialize the archived revision: {e}"))?;
    stella_store::durable::write_atomic_preserving_mode(
        &entry.path,
        body.as_bytes(),
        stella_store::durable::MODE_SHARED,
    )
    .map_err(|e| format!("cannot write {}: {e}", entry.path.display()))
}

#[cfg(test)]
mod tests;
