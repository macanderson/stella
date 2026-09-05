// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Mid-session freshness for the context-record registry.
//!
//! The registry is loaded once per session and rendered per turn; before this
//! module a record edited, promoted, or retired while a session ran was
//! invisible until restart. The freshness check is lazy rather than watched:
//! it runs at the two moments the answer can matter — the turn-opening recall
//! and each re-query boundary — by digesting the rule files' bytes and
//! reloading only when they moved. No file watcher, no background task, no
//! platform event quirks; the unchanged path costs one read of a handful of
//! small policy files.
//!
//! What a swap can and cannot do is stated: the volatile channel picks the
//! new registry up at the next boundary, and the swap forces one re-query by
//! bumping the generation the fingerprint folds. The cached system prefix is
//! byte-stable for the session (AGENTS.md's byte-stable-prompts rule), so a change to a
//! pinned (`must`/`should`) record rides the next recall block as guidance,
//! led by a line saying it binds fully — prompt and tool guards — next
//! session. A file that stops parsing mid-edit keeps the last good registry
//! and says so, because losing loaded steering to a half-saved buffer is the
//! quiet failure this module exists to end.

use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::atomic::Ordering;

use stella_records::ingest::record::Tier;
use stella_records::records::Registry;

impl super::SessionMemory {
    /// The registry swap counter the re-query fingerprint folds.
    pub(crate) fn records_generation(&self) -> u64 {
        self.records_generation.load(Ordering::Relaxed)
    }

    /// Reload the registry when the rule files' bytes have moved since the
    /// last look; a no-op (one digest pass) when they have not.
    pub(crate) fn refresh_records_if_changed(&self) {
        let digest = rules_digest(&self.workspace_root);
        {
            let mut last = self.records_fingerprint.lock().expect("records digest");
            if *last == digest {
                return;
            }
            // Recorded before the reload, so a load that changes nothing is
            // not re-attempted at every boundary; a further edit moves the
            // digest again and gets a fresh look.
            *last = digest;
        }
        let fresh = crate::context_records::load_registry(&self.workspace_root);
        let mut lock = self.record_registry.write().expect("records lock");
        let old = lock.take();
        if let Some(broken) = newly_broken(old.as_ref(), &fresh) {
            *lock = old;
            drop(lock);
            self.push_records_note(format!(
                "## Workspace rules\n\nA rules file changed and does not parse \
                 ({broken}); the rules loaded at session start stay in effect \
                 until it parses again."
            ));
            return;
        }
        let pinned_note = pinned_changes(old.as_ref(), &fresh);
        *lock = (!fresh.entries.is_empty()).then_some(fresh);
        drop(lock);
        self.records_generation.fetch_add(1, Ordering::Relaxed);
        if let Some(note) = pinned_note {
            self.push_records_note(note);
        }
    }

    /// Park a one-shot section for the next recall block. A newer note
    /// replaces an unread older one: both describe the same file set, and the
    /// later look is the truer one.
    fn push_records_note(&self, note: String) {
        *self.records_note.lock().expect("records note") = Some(note);
    }

    /// Take the parked note, if any — consumed by the recall block builders.
    pub(super) fn take_records_note(&self) -> Option<String> {
        self.records_note.lock().expect("records note").take()
    }
}

/// Digest of every rule file's path and bytes across both trust tiers — the
/// same file set [`crate::context_records::load_registry`] parses, so the
/// digest cannot say "unchanged" about a file the loader would read.
pub(super) fn rules_digest(root: &Path) -> u64 {
    let files = crate::context_records::rule_files(root, true, true);
    let mut hasher = std::hash::DefaultHasher::new();
    for file in files.all() {
        (file.path.as_str(), file.contents.as_str()).hash(&mut hasher);
    }
    hasher.finish()
}

/// A source that contributed records before, contributes none now, and
/// carries a diagnostic — the signature of a file that stopped parsing.
/// `None` when the fresh load lost nothing it can be blamed for: a deleted
/// file has no diagnostic (retirement, honored by the swap), and a diagnostic
/// the old load already carried is not new damage.
fn newly_broken(old: Option<&Registry>, fresh: &Registry) -> Option<String> {
    let old = old?;
    let sources = |registry: &Registry| -> BTreeSet<String> {
        registry
            .entries
            .iter()
            .map(|entry| entry.record.source.clone())
            .collect()
    };
    let had = sources(old);
    let has = sources(fresh);
    fresh
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.source.clone())
        .find(|source| had.contains(source) && !has.contains(source))
}

/// The recall-block section for pinned (`must`/`should`) records the swap
/// could not bind into the byte-stable prefix: each changed or new pinned
/// record, stating what applies now and what waits for the next session.
fn pinned_changes(old: Option<&Registry>, fresh: &Registry) -> Option<String> {
    let unchanged = |entry: &stella_records::records::Entry| {
        old.is_some_and(|old| {
            old.entries.iter().any(|prior| {
                prior.record.record.lineage_id == entry.record.record.lineage_id
                    && prior.record.record.statement == entry.record.record.statement
                    && prior.record.record.record_hash == entry.record.record.record_hash
            })
        })
    };
    let listed: Vec<String> = fresh
        .entries
        .iter()
        .filter(|entry| entry.record.record.tier() == Tier::Pinned && !unchanged(entry))
        .map(|entry| {
            format!(
                "- ^{}: {}",
                entry.record.handle, entry.record.record.statement
            )
        })
        .collect();
    if listed.is_empty() {
        return None;
    }
    Some(format!(
        "## Workspace rules — changed while this session runs\n\n\
         These binding (must/should) records changed after this session's \
         prompt was built. Apply them now as if they were in the system \
         prompt; they bind into it — and arm any tool guards they declare — \
         when the next session starts.\n\n{}",
        listed.join("\n")
    ))
}
