// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Amend: change a published record's **scope**, and re-stamp its identity.
//!
//! # The gap this closes
//!
//! Once a record reaches `.stella/rules/`, its steering scope —
//! `applies_to.paths`, `applies_to.tasks`, `applies_to.keywords`, and
//! `precedence` — had no supported way to change. That is exactly the field
//! most likely to need amending after publication, because a scope defect is
//! invisible until the record meets its neighbours: in this workspace
//! `read-crate-readme-first` was published with `paths = ["crates"]` at the
//! default precedence and suspended eight architecture-invariant records
//! across the whole crate tree. `stella context validate` named the remedies —
//! "an explicit exclusion, a different precedence, or a supersession link" —
//! and none of the three was reachable from the CLI. `stella context edit`
//! takes a *proposal* candidate id and rewrites wording; it does not touch a
//! published file.
//!
//! # Why a hand-edit was not the answer
//!
//! Editing the TOML strands `record_hash`. It is a two-pass stamp over an
//! RFC 8785 JCS preimage ([`Record::stamp`][s]) — hash with identity cleared to
//! derive `record_id`, then hash again with the id present — and reproducing
//! that by hand is not reasonable. Every other `stamp()` call site is a
//! *publish* path, so nothing re-stamped an edited file and every load reported
//! `record_hash mismatch … loaded as a new revision, the stored hash was not
//! honored`.
//!
//! # Enforcement is deliberately not amendable here
//!
//! `enforcement.mode` is what `stella context promote` governs: an approver, a
//! reason, and an immutable hash-chained ledger event, with proposer/approver
//! separation in regulated mode. A field flag on this command would be a second
//! route to the same grant with none of that behind it, so asking for one is
//! refused and told where the grant lives. That is the single-purpose rule
//! (AGENTS.md invariant 9) read the way it is meant: this verb changes *who a
//! record steers*, and changing *what it may do* is another verb's decision.
//!
//! # It amends in place rather than superseding
//!
//! A scope change is the same claim addressed at different work, not a
//! different claim — the statement, the lineage, the evidence and the
//! provenance are all untouched. `record_id` moves, because it is derived from
//! the content, and that is a new *revision* of one lineage, which is what a
//! content-derived id is for. Supersession is for replacing a claim with a
//! different one and stays with the ingest path that mints those links.
//!
//! [s]: stella_core::ingest::record::Record::stamp

use std::path::{Path, PathBuf};

use colored::Colorize;

use stella_core::ingest::record::{AppliesTo, ContextFile, Record};

/// What the caller asked to change. Every field is `None` for "leave it".
///
/// A struct rather than six parameters because the empty case is meaningful:
/// nothing set is a bare re-stamp, which is the whole repair for a file
/// somebody already hand-edited.
#[derive(Debug, Default, Clone)]
pub(crate) struct Amendment {
    /// `--precedence` — which record wins when two cannot both have their way.
    pub precedence: Option<u32>,
    /// `--paths` — replaces `applies_to.paths` wholesale.
    pub paths: Option<Vec<String>>,
    /// `--tasks` — replaces `applies_to.tasks` wholesale.
    pub tasks: Option<Vec<String>>,
    /// `--keywords` — replaces `applies_to.keywords` wholesale.
    pub keywords: Option<Vec<String>>,
}

impl Amendment {
    /// Nothing to change — the bare re-stamp.
    fn is_empty(&self) -> bool {
        self.precedence.is_none()
            && self.paths.is_none()
            && self.tasks.is_none()
            && self.keywords.is_none()
    }
}

/// `stella context amend <rule> [--precedence N] [--paths …] …`.
pub(crate) fn run_amend(root: &Path, needle: &str, amendment: &Amendment) -> Result<(), String> {
    let registry = crate::context_records::load_registry(root);
    let entry = find(&registry, needle)?;
    let path = PathBuf::from(&entry.record.source);
    let lineage = entry.record.record.lineage_id.clone();
    let handle = entry.record.handle.clone();

    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    // The whole file, not the one record: a context file may carry several
    // records and a `[defaults]` header, and rewriting it from a single
    // resolved record would silently drop both.
    let mut file: ContextFile = toml::from_str(&raw)
        .map_err(|e| format!("cannot parse {} as a context file: {e}", path.display()))?;
    let defaults = file.defaults.clone().unwrap_or_default();

    let record = file
        .records
        .iter_mut()
        .find(|record| record.lineage_id == lineage)
        .ok_or_else(|| {
            format!(
                "{} no longer holds ^{handle} ({lineage}) — the registry and the file disagree",
                path.display()
            )
        })?;

    let before = describe(record);
    apply(record, amendment).map_err(|why| format!("^{handle} {why}"))?;
    // Re-stamped over the file's own defaults, so the identity covers exactly
    // the record the loader will resolve. `stamp` clears both identity fields
    // and derives them again, which is why a bare re-stamp repairs a hand-edit
    // without any field having to change.
    record
        .stamp(&defaults)
        .map_err(|e| format!("cannot re-stamp ^{handle}: {e}"))?;
    let after = describe(record);
    let stamped = record.record_id.clone().unwrap_or_default();

    crate::context_records::replace_context_file(&path, &file)?;

    println!();
    println!("  {}  {}", format!("^{handle}").bold(), lineage.dimmed());
    if amendment.is_empty() {
        println!("    re-stamped; the scope is unchanged");
    } else {
        println!("    {}  {before}", "was".dimmed());
        println!("    {}  {after}", "now".dimmed());
    }
    println!(
        "    {}",
        format!("{stamped}  ·  {}", path.display()).dimmed()
    );
    println!();
    println!(
        "  {}",
        "stella context validate   # confirm it steers what you meant, and nothing else".dimmed()
    );
    Ok(())
}

/// Apply the amendment to a record, leaving everything it did not name.
///
/// A named list REPLACES rather than appends. Scope is amended because it is
/// wrong, and the common repair is *narrowing* — `paths = ["crates"]` down to
/// one crate — which an append could not express at all.
///
/// A record with no `[steering]` section is refused rather than given one.
/// `Steering` has no default and cannot have one: `force` decides which channel
/// the record rides — the cached system prefix or the volatile block — and
/// choosing it here would be this verb answering *how hard does it push*, which
/// is not the question it was asked.
fn apply(record: &mut Record, amendment: &Amendment) -> Result<(), String> {
    if amendment.is_empty() {
        return Ok(());
    }
    let steering = record.steering.as_mut().ok_or(
        "declares no [steering] section, so it has no precedence or scope to amend. \
         Publishing it with one is a decision about which channel it rides, not about \
         who it steers",
    )?;
    if let Some(precedence) = amendment.precedence {
        steering.precedence = Some(precedence);
    }
    if amendment.paths.is_some() || amendment.tasks.is_some() || amendment.keywords.is_some() {
        let applies = steering.applies_to.get_or_insert_with(AppliesTo::default);
        if let Some(paths) = &amendment.paths {
            applies.paths = paths.clone();
        }
        if let Some(tasks) = &amendment.tasks {
            applies.tasks = tasks.clone();
        }
        if let Some(keywords) = &amendment.keywords {
            applies.keywords = keywords.clone();
        }
    }
    Ok(())
}

/// One line naming a record's steering scope, for the before/after pair.
fn describe(record: &Record) -> String {
    let Some(steering) = record.steering.as_ref() else {
        return "unscoped · precedence 0".to_string();
    };
    let mut parts = Vec::new();
    if let Some(applies) = steering.applies_to.as_ref().filter(|a| !a.is_empty()) {
        for (label, values) in [
            ("paths", &applies.paths),
            ("tasks", &applies.tasks),
            ("keywords", &applies.keywords),
        ] {
            if !values.is_empty() {
                parts.push(format!("{label} {}", values.join(", ")));
            }
        }
    } else {
        parts.push("unscoped".to_string());
    }
    parts.push(format!("precedence {}", steering.precedence.unwrap_or(0)));
    parts.join(" · ")
}

/// The record this needle names — its `^handle` (caret optional) or its
/// lineage id, matching `stella context explain`'s resolution so the two verbs
/// take the same names.
fn find<'r>(
    registry: &'r stella_core::records::Registry,
    needle: &str,
) -> Result<&'r stella_core::records::Entry, String> {
    let wanted = needle.trim_start_matches('^');
    registry
        .entries
        .iter()
        .find(|entry| entry.record.handle == wanted || entry.record.record.lineage_id == wanted)
        .ok_or_else(|| {
            let known: Vec<String> = registry
                .entries
                .iter()
                .map(|entry| format!("^{}", entry.record.handle))
                .collect();
            if known.is_empty() {
                format!("no record named {needle} — this workspace publishes none")
            } else {
                format!("no record named {needle}. Published: {}", known.join(", "))
            }
        })
}
