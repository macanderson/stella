// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The two ways a file touch reaches this registry's ledger: the one the
//! registry **performed** ([`ToolRegistry::record_touch`], the choke point
//! every successful tool call funnels through) and the one it was **told
//! about** ([`ToolRegistry::attribute_external_change`], #2907). A child
//! module of `registry` so the ledger internals stay reachable, split out
//! because `registry.rs` is a god file closed to growth.
//!
//! Keeping the pair together is the point: they are the complete set of
//! writers to one ledger, and the second is defined almost entirely by which
//! of the first's side effects it deliberately does *not* have — a comparison
//! a reader can only make with both in view.
//!
//! # The hole the second one fills
//!
//! An isolated pipeline candidate executes against its *own* [`ToolRegistry`]
//! rooted at a shadow worktree, so every edit it makes lands on that
//! registry's ledger and is discarded with the workspace. The **session**
//! registry — whose ledger is the one persisted as `files_touched`, and which
//! `execution_reflection.wrote_files` is derived from — sees only the reads
//! that happened outside the candidate. So the durable "what did this session
//! change" record read empty for sessions that changed files, while the event
//! stream (which adoption re-emits onto) read correctly. One reality, two
//! records, disagreeing.
//!
//! # Why the measurement comes from git, and why nothing decides on it
//!
//! Adoption is the moment a candidate's edits become the user's tree's, and it
//! already measures exactly what moved with `git diff --numstat` against the
//! delivered baseline. Git is the authority on what changed. This module is
//! careful to stay what #2882 established `file_change` must be —
//! observability, never evidence: it *records* an answer git computed, it does
//! not compute one and nothing branches on what lands here.

use std::path::Path;

use super::*;

impl ToolRegistry {
    /// Record one SUCCESSFUL file touch: derive the event's line delta per
    /// the counting rules (R: zero; C: full new line count; U: line diff of
    /// pre vs post content; D: full pre-deletion line count), attach the
    /// model-supplied `reason` (or a per-op default), and append it to the
    /// ledger. The append and its aggregate updates happen under one lock,
    /// so concurrent tool completions never lose or interleave an event.
    ///
    /// With a bus attached, the touch also emits its `file.*` fact event
    /// (path + reason + line deltas, never contents) and a
    /// `files_touched.updated` carrying the changed record plus the ledger
    /// `revision` assigned under the lock — concurrent touches may deliver
    /// out of order, but consumers keep the highest revision and stay
    /// consistent.
    pub(super) fn record_touch(
        &self,
        pending: PendingTouch,
        pre_content: Option<String>,
        tool: &str,
        input: &Value,
        bus: Option<&HookBus>,
    ) {
        // Refresh this session's belief about the file before anything else in
        // here can fail or return early: the ledger is telemetry, but this is
        // the guarantee, and a touch whose digest went unrecorded would leave
        // the NEXT write to this path unguarded.
        //
        // A read earns the WHOLE-file grade only if it showed the model every
        // line: `read_file` defaults to a 2000-line window and `read_symbol`
        // reads a span, so the partial case is the common one. A mutation
        // always earns it — the agent authored what it just wrote, so there is
        // no unseen remainder to be wrong about. What a partial read may then
        // do to an existing belief is `remember_observed`'s rule.
        let coverage = if !matches!(pending.op, FileOp::Read)
            || self.read_ledger.saw_whole_file(&self.root, &pending.path)
        {
            Coverage::Whole
        } else {
            Coverage::Partial
        };
        self.remember_observed(&pending.path, coverage);
        // Stella's own state directory is not workspace content: probe-observed
        // churn under `.stella/` (the code-graph DB and its WAL/SHM sidecars)
        // drowned real task edits 60:1 on a single bench trial (#1537). The
        // staleness refresh above still runs — a write there stays guarded —
        // but nothing downstream records, journals, or announces it.
        if crate::file_touch::is_stella_state_path(&pending.path) {
            return;
        }
        let reason = input
            .get("reason")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| crate::file_touch::default_reason(pending.op).to_string());
        // The durable record, written from the same choke point as the ledger
        // and the staleness map, so all three describe one reality. Before the
        // diff work below, which is presentation: if anything here is going to
        // be expensive, the commit that makes the work recoverable goes first.
        self.journal_mutation(&pending, &reason);
        // Counts and rendered diff come out of the SAME pre/post pair, so the
        // ledger, the telemetry payload and the TUI describe one reality.
        let pre = pre_content.as_deref().unwrap_or("");
        // `post` rides alongside the counts so the authored-change ledger can
        // keep BOTH images of this write. It costs nothing extra to produce —
        // every arm below already computes it to render its own diff — and it
        // is the difference between a verifier that can read the agent's change
        // and one that is handed a filename (see [`crate::authored_diff`]).
        let (lines_added, lines_removed, diff, post) = match pending.op {
            FileOp::Read => (0, 0, None, None),
            FileOp::Create => {
                // `write_file` carries the new content in its input. A file
                // created by an opaque call (a shell redirect, a compiler
                // output) carries it only on disk, so fall back to reading
                // it — otherwise every shell-created file would be recorded
                // as zero lines long, understating exactly the work this
                // attribution path exists to see.
                let from_input = input.get("content").and_then(|v| v.as_str());
                let from_disk = from_input.is_none().then(|| {
                    crate::rootfd::read_confined_lossy(&self.root, &pending.path)
                        .unwrap_or_default()
                });
                let content =
                    from_input.unwrap_or_else(|| from_disk.as_deref().unwrap_or_default());
                (
                    count_lines(content),
                    0,
                    crate::file_touch::changed_region_diff("", content),
                    Some(content.to_string()),
                )
            }
            FileOp::Update => {
                // write_file carries the post content in its input; edit_file
                // landed it on disk (mutating tools are never parallelized —
                // see the read_only partition test — so this read-back sees
                // exactly what the edit wrote).
                let post = match tool {
                    "write_file" => input
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    _ => crate::rootfd::read_confined_lossy(&self.root, &pending.path)
                        .unwrap_or_default(),
                };
                let (added, removed) = line_diff(pre, &post);
                let diff = crate::file_touch::changed_region_diff(pre, &post);
                (added, removed, diff, Some(post))
            }
            FileOp::Delete => (
                0,
                count_lines(pre),
                crate::file_touch::changed_region_diff(pre, ""),
                Some(String::new()),
            ),
        };
        // Reads yield `None` above and the ledger refuses them again on its own
        // account — two guards, because this caller has already been the place
        // a read leaked into a change count once.
        if let Some(post) = &post {
            self.authored
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .record(
                    &pending.path,
                    pending.op,
                    crate::authored_diff::Provenance::for_tool(tool),
                    pre,
                    post,
                );
        }
        let path = pending.path;
        let (revision, record, total_files) = {
            let mut touched = self.touched.lock().unwrap_or_else(|p| p.into_inner());
            touched.record(
                path.clone(),
                FileTouchEvent {
                    event: pending.op,
                    reason: reason.clone(),
                    lines_added,
                    lines_removed,
                },
            );
            (
                touched.revision(),
                touched.get(&path).cloned(),
                touched.len(),
            )
        };
        if let Some(bus) = bus {
            let fact = match pending.op {
                FileOp::Read => hook_names::FILE_READ,
                FileOp::Create => hook_names::FILE_CREATED,
                FileOp::Update => hook_names::FILE_UPDATED,
                FileOp::Delete => hook_names::FILE_DELETED,
            };
            bus.emit_named(
                fact,
                serde_json::json!({
                    "path": path,
                    "reason": reason,
                    "lines_added": lines_added,
                    "lines_removed": lines_removed,
                }),
            );
            bus.emit_named(
                hook_names::FILES_TOUCHED_UPDATED,
                serde_json::json!({
                    "revision": revision,
                    "path": path,
                    "record": record,
                    "total_files": total_files,
                }),
            );
        }
        // The one place a `FileChange` is born. A closed channel (the turn
        // ended) is not an error worth surfacing: the ledger above is the
        // durable record, and this is its live projection.
        let kind = match pending.op {
            FileOp::Read => stella_protocol::FileChangeKind::Read,
            FileOp::Create => stella_protocol::FileChangeKind::Created,
            FileOp::Update => stella_protocol::FileChangeKind::Modified,
            FileOp::Delete => stella_protocol::FileChangeKind::Deleted,
        };
        // Counted off the event's OWN `kind`, and unconditionally — the tally
        // must not depend on whether a channel happened to be attached, or a
        // candidate registry (deliberately unattached) would report having
        // touched nothing. Reads are excluded here exactly as every consumer
        // excludes them, so the two agree by construction rather than by two
        // copies of the same `match`.
        if kind.is_mutation() {
            self.mutations
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        // A candidate registry (`attach_read_events`) announces its reads and
        // withholds its mutations: the edits are not the user's tree's until
        // adoption re-emits them. The counter above is deliberately OUTSIDE
        // this gate — `mutations_recorded` must stay true no matter which
        // events were announced, or the verification ladder goes blind exactly
        // where #973 found it blind.
        let announce = !kind.is_mutation()
            || self
                .announce_mutations
                .load(std::sync::atomic::Ordering::Relaxed);
        if announce && let Some(events) = self.events() {
            let _ = events.send(stella_protocol::AgentEvent::FileChange {
                path,
                kind,
                added: lines_added as u32,
                removed: lines_removed as u32,
                diff,
            });
        }
    }

    /// Record a mutation this registry did **not** perform, measured by
    /// something else, against `absolute_path` in the real tree.
    ///
    /// The one caller is candidate adoption — see the module doc for why the
    /// session ledger is blind without it, and why the counts are git's.
    ///
    /// # What this deliberately does not do
    ///
    /// - **No journal entry.** The candidate's own registry already committed
    ///   one at the moment of the write; a second would double the durable
    ///   mutation record for one edit.
    /// - **No staleness refresh.** The no-clobber map is this session's
    ///   *belief about bytes it has seen*, and this session has not seen
    ///   these. Seeding it from a measurement would tell the next write that a
    ///   file it has never read is safe to clobber — trading a telemetry gap
    ///   for a correctness hole, which is the wrong direction to trade in.
    /// - **No event.** The caller emits the `FileChange` for adopted work; a
    ///   second one from here would double every row in the Files tab.
    ///
    /// So this is strictly additive to the ledger and the mutation count, and
    /// the return says whether it landed: `false` for a path outside this
    /// registry's root (a candidate that changed a file the session workspace
    /// does not contain) or under `.stella/` (state, never workspace content —
    /// the same exclusion `record_touch` applies to real touches). Neither is
    /// an error: an unattributable path is a fact about the path, not a
    /// failure of the caller, and there is nothing a caller could do
    /// differently on being told.
    ///
    /// `lines_added`/`lines_removed` are recorded verbatim rather than
    /// recounted, for the reason every other counting site in this crate
    /// states: one measurement, one source.
    #[must_use]
    pub fn attribute_external_change(
        &self,
        absolute_path: &Path,
        op: FileOp,
        reason: &str,
        lines_added: u64,
        lines_removed: u64,
    ) -> bool {
        let raw = absolute_path.to_string_lossy();
        let Some(path) = crate::file_touch::normalize_workspace_path(&self.root, &raw) else {
            return false;
        };
        if crate::file_touch::is_stella_state_path(&path) {
            return false;
        }
        self.touched
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record(
                path,
                FileTouchEvent {
                    event: op,
                    reason: reason.to_string(),
                    lines_added,
                    lines_removed,
                },
            );
        // Gated on the op exactly as `record_touch` gates on its event's own
        // kind, so the two agree by construction: a read is not a mutation
        // here either, even though no caller has one to attribute today.
        if !matches!(op, FileOp::Read) {
            self.mutations
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry(root: &Path) -> ToolRegistry {
        ToolRegistry::with_issue_backend(root.to_path_buf(), None)
    }

    /// The happy path, at the unit: an attributed change lands in the ledger
    /// with the op and the counts the caller measured, and moves the mutation
    /// counter the verification ladder reads.
    #[test]
    fn an_attributed_change_lands_in_the_ledger_with_its_measured_counts() {
        let root = tempfile::tempdir().unwrap();
        let reg = registry(root.path());

        assert!(reg.attribute_external_change(
            &root.path().join("src/lib.rs"),
            FileOp::Update,
            "adopted",
            7,
            2,
        ));

        assert_eq!(
            reg.files_touched(),
            vec![("src/lib.rs".to_string(), "U".to_string())]
        );
        assert_eq!(reg.mutations_recorded(), 1);
        let record = reg
            .file_touch_telemetry()
            .files_touched
            .into_iter()
            .next()
            .expect("one record");
        assert_eq!((record.lines_added, record.lines_removed), (7, 2));
        assert_eq!(record.events[0].reason, "adopted");
    }

    /// A path outside this registry's root is refused rather than recorded
    /// under a name that escapes the workspace. The direction matters: a
    /// ledger that can name `../../etc/passwd` is worse than one missing a
    /// row, because every consumer of `files_touched` treats its paths as
    /// workspace-relative.
    #[test]
    fn a_path_outside_the_root_is_refused_not_recorded() {
        let root = tempfile::tempdir().unwrap();
        let reg = registry(root.path());

        assert!(!reg.attribute_external_change(
            Path::new("/etc/passwd"),
            FileOp::Update,
            "adopted",
            1,
            0,
        ));
        assert!(reg.files_touched().is_empty());
        assert_eq!(
            reg.mutations_recorded(),
            0,
            "a refused attribution must not move the counter either"
        );
    }

    /// Stella's own state directory is not workspace content — the same
    /// exclusion `record_touch` applies (#1537), applied here too so the two
    /// writers to this ledger cannot disagree about what belongs in it.
    #[test]
    fn stella_state_is_excluded_here_exactly_as_it_is_for_a_real_touch() {
        let root = tempfile::tempdir().unwrap();
        let reg = registry(root.path());

        assert!(!reg.attribute_external_change(
            &root.path().join(".stella/private/codegraph.db"),
            FileOp::Update,
            "adopted",
            900,
            0,
        ));
        assert!(reg.files_touched().is_empty());
        assert_eq!(reg.mutations_recorded(), 0);
    }

    /// Two attributions of the same path aggregate into one record with both
    /// events and summed deltas — the ledger's existing contract, which this
    /// entry point must not sidestep by taking a different route into it.
    #[test]
    fn repeated_attributions_of_one_path_aggregate_like_any_other_touch() {
        let root = tempfile::tempdir().unwrap();
        let reg = registry(root.path());

        assert!(reg.attribute_external_change(
            &root.path().join("a.rs"),
            FileOp::Create,
            "adopted",
            3,
            0
        ));
        assert!(reg.attribute_external_change(
            &root.path().join("a.rs"),
            FileOp::Update,
            "adopted",
            4,
            1
        ));

        assert_eq!(
            reg.files_touched(),
            vec![("a.rs".to_string(), "CU".to_string())]
        );
        assert_eq!(reg.mutations_recorded(), 2);
        let record = reg
            .file_touch_telemetry()
            .files_touched
            .into_iter()
            .next()
            .expect("one record");
        assert_eq!((record.lines_added, record.lines_removed), (7, 1));
    }
}
