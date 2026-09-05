// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a `!!` or `!!!` mark saves: one context record, written the way the
//! record plane writes every other one.
//!
//! [`inferred_rule_record`] is the near twin of this file. It saves what a
//! miner *guessed*. So it is soft: `origin = inferred`, `force = should`, and
//! no name on it. A bang mark is the other case. A person typed the words. So
//! the record says `origin = user`, its `truth` is a `decree`, and the name of
//! the person is on it. Those are the two things the sweep gate asks a record
//! for before it trusts one ([`honored_probe`]).
//!
//! The rest is the path `stella context keep` walks now. Same folder. Same
//! refusal to clobber. Same new draft when the words change. Same two ledgers.
//! A save from the composer must not be a back door.
//!
//! [`inferred_rule_record`]: super::inferred_rule_record
//! [`honored_probe`]: stella_records::records::sweep::honored_probe

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use stella_records::ingest::record as rec;
use stella_records::ingest::refresh::same_statement;
use stella_records::records::DecisionEvent;
use stella_records::records::promotion::{LedgerAction, PromotionEvent};
use stella_tui::KeepStrength;

use crate::context_cmd::review::{actor, published_record_at};
use crate::context_records::{
    append_decision, append_promotion, now_rfc3339, publication_path, read_governance,
    replace_record, separation_checked_proposer, write_record,
};

/// The `precedence` a `!!!` record is saved at.
///
/// A record carries no number for how hard it binds. `precedence` is the one
/// number it has. It settles two things: which record wins a clash, and which
/// one keeps its place when the budget runs short. A rule worth stopping a
/// turn for should lose neither. So it is saved at the top.
///
/// `enforcement` is left off at both strengths. A `hard` mode with no guard to
/// check claims a block that can never fire. A typed sentence brings no guard.
const RULE_PRECEDENCE: u32 = 100;

/// How many words of the claim go into the file name. Enough to find it by eye
/// in `.stella/rules/`.
const SLUG_WORDS: usize = 6;

/// What one save did. The caller reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Saved {
    /// The file that was written.
    pub path: PathBuf,
    /// The `lineage_id` the claim lives under.
    pub lineage_id: String,
    /// The `record_id` this draft retired. Set when the claim was already
    /// saved, in other words or at another strength.
    pub superseded: Option<String>,
    /// True when the file already said this, at this strength, and nothing was
    /// written. Saying a thing twice is not an error.
    pub unchanged: bool,
}

/// Save `statement` as a context record, at the strength the mark asked for.
///
/// The `lineage_id` comes from the words. So one sentence always lands in one
/// lineage. Say it twice and nothing happens. Change the words, or raise `!!`
/// to `!!!`, and the new draft retires the old one. It never writes a second
/// record that argues with the first.
pub(crate) fn publish(root: &Path, keep: KeepStrength, statement: &str) -> Result<Saved, String> {
    let statement = statement.trim();
    if statement.is_empty() {
        return Err("there was nothing after the mark to keep".to_string());
    }
    if stella_records::records::validate::statement_reads_as_pasted(statement) {
        return Err(format!(
            "a context record's statement is one sentence, and this is {} characters over {} \
             lines — say it shorter and the mark will keep it",
            statement.chars().count(),
            statement.lines().count()
        ));
    }
    let set_id = crate::ingest_cmd::derive_set_id(root);
    let mut record = record(&set_id, keep, statement, &actor())?;
    let path = publication_path(root, rec::SharingScope::Repository, &record.lineage_id)
        .ok_or_else(|| "cannot work out where to publish this record".to_string())?;

    let existing = path.exists().then(|| published_record_at(&path)).flatten();
    let superseded = match existing {
        Some(existing)
            if same_statement(&existing.statement, &record.statement)
                && force_of(&existing) == Some(force(keep)) =>
        {
            return Ok(Saved {
                path,
                lineage_id: record.lineage_id,
                superseded: None,
                unchanged: true,
            });
        }
        Some(existing) => Some(existing.record_id.ok_or_else(|| {
            format!(
                "{} already holds this claim but carries no record id, so a new revision has \
                 nothing to cite — run `stella context validate` to see the finding",
                path.display()
            )
        })?),
        None if path.exists() => {
            return Err(format!(
                "{} exists and does not parse as a context record — repair or delete it before \
                 the mark can publish here",
                path.display()
            ));
        }
        None => None,
    };

    match &superseded {
        Some(old_id) => {
            // The link to the old draft is content. So it goes inside the new
            // draft's hash, not beside it.
            record.supersedes_record_id = Some(old_id.clone());
            record
                .stamp(&rec::Defaults::default())
                .map_err(|e| format!("cannot canonicalize the record: {e}"))?;
            replace_record(&path, &set_id, &record)?;
            // File first, ledger second. A ledger error is loud: a swap nobody
            // logged is the thing the ledger is for. The separation gate runs
            // first, as `stella context keep` runs it. A swap clears any block
            // grant the lineage holds. Under separation, an author may not
            // disarm their own record alone.
            let governance = read_governance(root)?;
            let approver = actor();
            let proposer = separation_checked_proposer(
                root,
                &record.lineage_id,
                &approver,
                "superseding it from the composer",
            )?;
            append_promotion(
                root,
                PromotionEvent {
                    seq: 0,
                    prev: String::new(),
                    at: now_rfc3339(),
                    lineage_id: record.lineage_id.clone(),
                    from: "active".to_string(),
                    to: "superseded".to_string(),
                    approver,
                    proposer,
                    reason: format!(
                        "{old_id} superseded by {} — kept from the composer",
                        record.record_id.as_deref().unwrap_or("<unstamped>")
                    ),
                    mode: governance.mode.as_str().to_string(),
                    action: LedgerAction::Superseded,
                },
            )?;
        }
        None => write_record(&path, &set_id, &record)?,
    }

    // The same event `stella context keep` writes. One log then says where
    // every record came from. The path is short when the file is in the tree.
    // The next person to open the repo reads this, and a full path names one
    // machine.
    let recorded_path = path
        .strip_prefix(root)
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.display().to_string());
    let mut event = DecisionEvent::keep(
        candidate_id(&record.lineage_id),
        record.lineage_id.clone(),
        actor(),
        now_rfc3339(),
        recorded_path,
    );
    if let Some(old_id) = &superseded {
        event.reason = Some(format!("supersedes {old_id}"));
    }
    append_decision(root, &event)?;

    Ok(Saved {
        path,
        lineage_id: record.lineage_id,
        superseded,
        unchanged: false,
    })
}

/// The `force` a strength is saved at. `!!` steers, and a later order can
/// override it. `!!!` binds. Both ride the cached prefix. That is what makes
/// either one reach every later turn with nothing to select it.
fn force(keep: KeepStrength) -> rec::Force {
    match keep {
        KeepStrength::Guidance => rec::Force::Should,
        KeepStrength::Rule => rec::Force::Must,
    }
}

/// The `force` a saved file names, if it names one.
fn force_of(record: &rec::Record) -> Option<rec::Force> {
    record.steering.as_ref().map(|steering| steering.force)
}

/// Build and stamp the record a mark saves.
///
/// `verified_by` is the person the `decree` rests on. The sweep gate reads it
/// with the `origin`. A decree nobody signed is not one.
fn record(
    set_id: &str,
    keep: KeepStrength,
    statement: &str,
    by: &str,
) -> Result<rec::Record, String> {
    // A folder whose name starts with a dot would leave an empty part in the
    // lineage (`ctx..foo.bar`). Trim it rather than write a bad one.
    let set_id = set_id.trim_matches('.');
    let set_id = if set_id.is_empty() {
        "workspace"
    } else {
        set_id
    };
    let mut record = rec::Record {
        lineage_id: format!("ctx.{set_id}.{}", lineage_suffix(statement)),
        record_id: None,
        record_hash: None,
        kind: rec::RecordKind::Rule,
        statement: statement.to_string(),
        tags: vec!["decreed".to_string()],
        origin: Some(stella_records::context_record::Origin::User),
        sharing_scope: Some(rec::SharingScope::Repository),
        status: Some(stella_records::context_record::RecordStatus::Active),
        supersedes_record_id: None,
        provenance: Some(rec::Provenance {
            source_kind: Some("decree".to_string()),
            source_uri: Some("composer".to_string()),
            // A person typed the words and stands behind them. That is
            // `HumanReview`, not a measurement. It is the top grade a claim
            // reaches with nothing in the world to back it up.
            evidence_grade: Some(stella_protocol::provenance::ProvenanceGrade::HumanReview),
            ..Default::default()
        }),
        steering: Some(rec::Steering {
            force: force(keep),
            precedence: match keep {
                KeepStrength::Guidance => None,
                KeepStrength::Rule => Some(RULE_PRECEDENCE),
            },
            tier: None,
            applies_to: None,
        }),
        enforcement: None,
        truth: Some(rec::Truth {
            basis: rec::TruthBasis::Decree,
            confidence: None,
            verified_by: Some(by.to_string()),
            verified_at: Some(now_rfc3339()),
            valid_from: None,
            ttl: None,
            on_expiry: None,
            review_every: None,
            probe: None,
        }),
        links: Vec::new(),
    };
    record
        .stamp(&rec::Defaults::default())
        .map_err(|e| format!("cannot stamp the record: {e}"))?;
    Ok(record)
}

/// The last part of the `lineage_id`: a few plain words off the claim, then a
/// hash of the whole of it.
///
/// The words are for the person who opens `.stella/rules/`. The hash is what
/// holds the lineage still, so one sentence always maps to one file however
/// long it runs. It comes from the words alone, never the strength. That is
/// what lets `!!!` raise a claim `!!` saved, in place of a rival record.
fn lineage_suffix(statement: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(statement.as_bytes());
    let hash = format!("{:x}", digest.finalize());
    let words: Vec<String> = statement
        .split_whitespace()
        .take(SLUG_WORDS)
        .map(|word| {
            word.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .map(|c| c.to_ascii_lowercase())
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .collect();
    let stem = words.join("-");
    if stem.is_empty() {
        format!("decreed-{}", &hash[..8])
    } else {
        format!("decreed-{stem}-{}", &hash[..8])
    }
}

/// The ledger's `<slug>-<hash8>` id for this claim. A record kept from the
/// composer has no proposal behind it. So the last part of the `lineage_id` is
/// the id. One claim decided twice lands on one id, which is what the ledger's
/// fold wants.
fn candidate_id(lineage_id: &str) -> String {
    lineage_id
        .rsplit('.')
        .next()
        .unwrap_or(lineage_id)
        .to_string()
}

#[cfg(test)]
mod tests;
