//! Per-hunk approval for mutating tool calls (#1265).
//!
//! Approval in stella was either **per-plan** (scope review) or
//! **per-tool-call** (the hook bus). Neither can express "apply that change but
//! not this one", so a call carrying one wanted and one unwanted edit was
//! all-or-nothing: take both, or reject both and re-prompt. The reviewer could
//! see the granularity in the rendered diff and could not act on it.
//!
//! # Why this is a decorator that rewrites the input
//!
//! [`HunkGate`] wraps a [`ToolExecutor`], works out what the call would write,
//! puts the hunks in front of a human, and — if any were declined — hands the
//! **same tool** a rewritten input describing only the accepted content. It
//! never writes anything itself.
//!
//! That shape is what makes the issue's four guarantees fall out rather than
//! being re-implemented:
//!
//! - **The transaction is unchanged.** A partial `apply_edits` is still one
//!   `apply_edits` call, so the validate-then-apply preflight, the per-edit
//!   report and the mid-batch rollback are the ones already proven in
//!   [`crate::apply_edits`]. A bespoke "write the accepted bytes" path would
//!   have had to re-earn all of it.
//! - **`FileChange` describes what landed, not what was proposed.**
//!   `ToolRegistry::record_touch` stays the single emitter and reads the file
//!   back from disk after the write, so it reports the accepted content with no
//!   knowledge that a gate exists.
//! - **A rewritten call is a compare-and-swap.** The synthesized `old_string`
//!   is the file's *entire* pre-image, so if anything changed the file between
//!   the review and the apply, `apply_edits` fails it with the existing
//!   read→edit drift attribution instead of clobbering the newer bytes.
//!   `write_file` has nowhere to put that precondition — it writes `content`
//!   unconditionally — so the one gated tool that cannot compare-and-swap for
//!   itself is given the property by `HunkGate::stale`, immediately before
//!   the dispatch it guards. This has to hold for every gated tool or it is not
//!   a guarantee, only a property that two of the three happen to have.
//! - **The model is told.** Declined hunks are named in the tool result, so the
//!   model knows why the file does not look the way it wrote it — the silent
//!   partial-apply would manufacture exactly the drift the oracle exists to
//!   attribute.
//!
//! # Fail-closed, but never self-answering
//!
//! A reviewer that cannot be reached is a refusal, not an approval: the call
//! fails and nothing is written. The other half of that posture is that a run
//! with no reviewer must not *install* this gate at all — an approval surface
//! that auto-approves is worse than no approval surface, because it reports
//! consent nobody gave.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use serde_json::Value;
use stella_core::ToolExecutor;
use stella_protocol::{HunkProposal, ProposedHunk, ToolOutput, ToolSchema};

use crate::hunk::{self, Hunk};

/// The mutating tools whose writes are reviewable. Every one of them resolves
/// to a "this file goes from these bytes to those bytes" pair, which is all the
/// decomposition needs; tools that mutate opaquely (`bash`) cannot be gated
/// this way and are not pretended to be.
const GATED_TOOLS: [&str; 3] = ["apply_edits", "edit_file", "write_file"];

/// How the gate reaches the human. The host supplies this — the deck over its
/// channels, a test with a script.
#[async_trait]
pub trait HunkReviewIo: Send + Sync {
    /// Present `proposal` and return the indices of the hunks to **apply**.
    ///
    /// Indices are into `proposal.hunks`; out-of-range and duplicate entries
    /// are ignored by the caller, and an empty result means "apply nothing",
    /// which is a legitimate answer rather than an error. `Err` means the
    /// reviewer could not be reached at all, and the caller fails closed.
    async fn review(&self, proposal: &HunkProposal) -> Result<Vec<usize>, String>;
}

/// Monotonic source of review ids. Distinct from the model's tool-call ids: a
/// review is the host's object, raised by the host, cleared by the host.
static NEXT_REVIEW_ID: AtomicU64 = AtomicU64::new(1);

/// One file's proposed change, decomposed.
struct FileChange {
    /// The path as the call named it — what the rewritten input must use, so
    /// the whole call resolves each file exactly one way.
    path: String,
    before: String,
    after: String,
    hunks: Vec<Hunk>,
}

/// A [`ToolExecutor`] that gates mutating calls on per-hunk approval.
pub struct HunkGate<'a> {
    inner: &'a dyn ToolExecutor,
    io: Option<Arc<dyn HunkReviewIo>>,
    root: PathBuf,
}

impl<'a> HunkGate<'a> {
    /// Wrap `inner`, reviewing through `io`. `root` is the workspace root the
    /// inner executor resolves paths against — the gate reads the current
    /// bytes through the same confinement.
    ///
    /// `io: None` makes the gate **inert**: every call passes straight through,
    /// no proposal is built and no event is emitted, so the stack behaves
    /// exactly as it did before this layer existed. That is the shape a run
    /// with no reviewer must take — the alternative, a gate present and
    /// answering itself, would report consent nobody gave.
    pub fn new(
        inner: &'a dyn ToolExecutor,
        io: Option<Arc<dyn HunkReviewIo>>,
        root: PathBuf,
    ) -> Self {
        Self { inner, io, root }
    }

    /// What this call would write, per file — or `None` when the gate does not
    /// apply and the call must pass through untouched.
    ///
    /// `None` covers every case where reviewing would be wrong or impossible:
    /// a tool that is not gated, a `dry_run` that writes nothing, a file that
    /// cannot be read, and — importantly — a call that would **fail
    /// validation anyway**. Letting that last one through unreviewed is what
    /// keeps the tools' own error messages exactly what they were: the gate
    /// never gets between the model and "your `old_string` was not found".
    ///
    /// Passing an unreadable file through *unreviewed* is safe only because
    /// `apply_edits` validates the whole batch before it applies any of it, so
    /// the call that skipped the gate is a call that writes nothing. That
    /// coupling is load-bearing rather than incidental: were validation ever to
    /// become per-edit, one unreadable path in a batch would turn this early
    /// return into a way to write the other paths past the reviewer.
    fn proposed_changes(&self, name: &str, input: &Value) -> Option<Vec<FileChange>> {
        self.io.as_ref()?;
        if !GATED_TOOLS.contains(&name) {
            return None;
        }
        if input
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return None;
        }

        let mut changes = Vec::new();
        if name == "write_file" {
            let path = input.get("path").and_then(Value::as_str)?;
            let after = input.get("content").and_then(Value::as_str)?;
            // An absent file is a create: no pre-image, so the decomposition
            // yields exactly one hunk and the reviewer's only real choices are
            // the whole file or none of it.
            let before = crate::rootfd::read_confined_lossy(&self.root, path).unwrap_or_default();
            changes.push(FileChange {
                path: path.to_string(),
                before,
                after: after.to_string(),
                hunks: Vec::new(),
            });
        } else {
            // `edit_file` is one edit, so it simulates through the batch
            // simulator too — which makes `apply_edits`' own composition and
            // its zero-match / ambiguous-match refusals the single authority on
            // what the call would produce. Re-deriving that here is how a gate
            // ends up reviewing bytes the tool would never have written.
            let batch = match name {
                "edit_file" => serde_json::json!({ "edits": [input.clone()] }),
                _ => input.clone(),
            };
            for path in batch_paths(&batch, &self.root) {
                let before = crate::rootfd::read_confined_lossy(&self.root, &path)?;
                let after = crate::apply_edits::simulate_batch(&batch, &path, &before)?;
                changes.push(FileChange {
                    path,
                    before,
                    after,
                    hunks: Vec::new(),
                });
            }
        }

        for change in &mut changes {
            change.hunks = hunk::split(&change.before, &change.after);
        }
        changes.retain(|c| !c.hunks.is_empty());
        (!changes.is_empty()).then_some(changes)
    }

    /// The input that applies only `accepted`, or `None` when the accepted set
    /// leaves every file exactly as it already is.
    ///
    /// Written as the same tool the model called, so the ledger, the hook bus
    /// and the storage gate all keep seeing the call that was actually made.
    /// The synthesized `old_string` is the whole pre-image — see the module
    /// note on why that is the point rather than a shortcut.
    fn rewrite(
        &self,
        name: &str,
        input: &Value,
        changes: &[FileChange],
        mask: &[Vec<bool>],
    ) -> Option<Value> {
        let mut edits = Vec::new();
        for (change, accepted) in changes.iter().zip(mask) {
            let old = hunk::split_lines(&change.before);
            let new = hunk::split_lines(&change.after);
            let composed = hunk::compose(&old, &new, &change.hunks, accepted);
            if composed == change.before {
                continue; // every hunk for this file was declined
            }
            let mut rewritten = input.clone();
            match name {
                // The rewrite is the accepted content in the same field, so a
                // partial `write_file` stays one whole-file write rather than
                // becoming a batch. Nothing rides along to say which bytes it
                // expected to find — [`Self::stale`] supplies the precondition
                // this field cannot express.
                "write_file" => {
                    rewritten["content"] = Value::String(composed);
                    return Some(rewritten);
                }
                // One file, so `edit_file` stays `edit_file`. Swapping it for a
                // batch would be a lie in three places at once: the tool policy
                // that may name the two differently, the file-touch ledger's
                // attribution, and the hook bus's `tool.call.*` payloads.
                // `replace_all` goes with it — a whole-file `old_string` occurs
                // exactly once by construction.
                "edit_file" => {
                    rewritten["old_string"] = Value::String(change.before.clone());
                    rewritten["new_string"] = Value::String(composed);
                    if let Some(map) = rewritten.as_object_mut() {
                        map.remove("replace_all");
                    }
                    return Some(rewritten);
                }
                _ => edits.push(serde_json::json!({
                    "path": change.path,
                    "old_string": change.before,
                    "new_string": composed,
                })),
            }
        }
        if edits.is_empty() {
            return None;
        }
        // `apply_edits` keeps its batch shape — one synthesized edit per file
        // that kept anything — so the surviving set still applies as one
        // transaction rather than N independent writes.
        let mut rewritten = serde_json::json!({ "edits": edits });
        for carried in ["reason", "storage_intent"] {
            if let Some(value) = input.get(carried) {
                rewritten[carried] = value.clone();
            }
        }
        Some(rewritten)
    }

    /// The first file whose bytes on disk are no longer the pre-image
    /// [`Self::rewrite`] composed against, if any.
    ///
    /// Only `write_file` needs asking. `apply_edits` and `edit_file` carry the
    /// pre-image in `old_string` and refuse a moved file themselves; a
    /// `write_file` has no such field, so for it the window between the read
    /// that built the proposal and the write that answers it is exactly as long
    /// as the human took to decide — which is the one window on this path long
    /// enough for somebody to save the file in their editor.
    ///
    /// The comparison is on **raw bytes**, not on the decoded string, because
    /// that catches a second way the composition can be wrong for the same
    /// price: `before` came from a *lossy* read, so on a file that is not valid
    /// UTF-8 the unchanged regions of `composed` hold U+FFFD where the file
    /// holds bytes, and writing it would re-encode the parts nobody reviewed.
    /// Both failures have the same remedy — do not write — so they share a
    /// check. An absent file reads as empty, which is what a create's pre-image
    /// already is, so creating a file is not mistaken for drifting.
    fn stale(&self, name: &str, changes: &[FileChange]) -> Option<String> {
        if name != "write_file" {
            return None;
        }
        changes
            .iter()
            .find(|change| {
                crate::rootfd::read_confined_bytes(&self.root, &change.path).unwrap_or_default()
                    != change.before.as_bytes()
            })
            .map(|change| change.path.clone())
    }
}

/// Paths an `apply_edits` batch touches, in first-touch order, as the batch
/// named them. Deduplicated on the *normalized* path — the same key
/// `apply_edits`' validate phase groups by, and with the same fallback to the
/// raw string when a path will not resolve — so two spellings of one file are
/// simulated once, while the name used for reads and writes stays the one the
/// call supplied.
fn batch_paths(batch: &Value, root: &std::path::Path) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for edit in batch
        .get("edits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(path) = edit.get("path").and_then(Value::as_str) else {
            continue;
        };
        let key = crate::file_touch::normalize_workspace_path(root, path)
            .unwrap_or_else(|| path.to_string());
        if seen.insert(key) {
            out.push(path.to_string());
        }
    }
    out
}

/// One line naming a hunk, for the report the model reads. The `@@` header is
/// the part that positions it; the counts come from the decomposition, never
/// from re-counting the capped diff text.
fn label(hunk: &ProposedHunk) -> String {
    let header = hunk.diff.lines().next().unwrap_or("@@").trim();
    format!(
        "  - {} {header} (+{} −{})",
        hunk.path, hunk.lines_added, hunk.lines_removed
    )
}

#[async_trait]
impl ToolExecutor for HunkGate<'_> {
    fn schemas(&self) -> Vec<ToolSchema> {
        self.inner.schemas()
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        let Some(changes) = self.proposed_changes(name, input) else {
            return self.inner.execute(name, input).await;
        };

        let mut hunks = Vec::new();
        for change in &changes {
            let old = hunk::split_lines(&change.before);
            let new = hunk::split_lines(&change.after);
            for h in &change.hunks {
                hunks.push(ProposedHunk {
                    path: change.path.clone(),
                    diff: hunk::render(&old, &new, h),
                    lines_added: h.new_len as u32,
                    lines_removed: h.old_len as u32,
                });
            }
        }
        let total = hunks.len();
        let proposal = HunkProposal {
            id: format!(
                "hunk-review-{}",
                NEXT_REVIEW_ID.fetch_add(1, Ordering::Relaxed)
            ),
            tool: name.to_string(),
            hunks,
        };

        let io = self
            .io
            .as_ref()
            .expect("proposed_changes returns None without an io");
        let accepted = match io.review(&proposal).await {
            Ok(accepted) => accepted,
            // Fail closed. An unreachable reviewer is the one case where
            // guessing costs the user their files.
            Err(e) => {
                return ToolOutput::Error {
                    message: format!(
                        "`{name}` was not applied: the hunk reviewer could not be reached ({e}) \
                         — NOTHING was written"
                    ),
                };
            }
        };

        // Per-file masks in the same flat order the proposal presented.
        let mut flat = vec![false; total];
        for i in accepted {
            if let Some(slot) = flat.get_mut(i) {
                *slot = true;
            }
        }
        let mut mask = Vec::with_capacity(changes.len());
        let mut cursor = 0usize;
        for change in &changes {
            mask.push(flat[cursor..cursor + change.hunks.len()].to_vec());
            cursor += change.hunks.len();
        }

        let declined: Vec<&ProposedHunk> = proposal
            .hunks
            .iter()
            .zip(&flat)
            .filter(|(_, keep)| !**keep)
            .map(|(h, _)| h)
            .collect();
        if declined.is_empty() {
            // Nothing was refused, so the call runs exactly as written —
            // untouched input, unchanged report, no trace of the gate.
            return self.inner.execute(name, input).await;
        }

        let report = format!(
            "\n\n[hunk review] the reviewer declined {} of {total} proposed hunk(s), so the \
             file(s) do NOT look the way you wrote them. Declined:\n{}\nRe-read anything you \
             intend to change again before proposing further edits.",
            declined.len(),
            declined
                .iter()
                .map(|h| label(h))
                .collect::<Vec<_>>()
                .join("\n")
        );

        let Some(rewritten) = self.rewrite(name, input, &changes, &mask) else {
            return ToolOutput::Error {
                message: format!(
                    "`{name}` was declined in full by the reviewer — NOTHING was written{report}"
                ),
            };
        };
        // Fail closed on drift, for the same reason an unreachable reviewer
        // fails closed: the accepted hunks describe bytes that are no longer
        // there, and applying them anyway would overwrite a write nobody
        // reviewed with content nobody proposed.
        if let Some(path) = self.stale(name, &changes) {
            return ToolOutput::Error {
                message: format!(
                    "`{name}` was not applied: `{path}` no longer holds the bytes that were \
                     reviewed, so the accepted hunk(s) no longer describe it — NOTHING was \
                     written. Re-read it before proposing again.{report}"
                ),
            };
        }
        match self.inner.execute(name, &rewritten).await {
            ToolOutput::Ok { content } => ToolOutput::Ok {
                content: format!("{content}{report}"),
            },
            // The inner failure already says nothing was written; the report
            // still rides along so the model does not retry the declined hunks
            // believing the reviewer never saw them.
            ToolOutput::Error { message } => ToolOutput::Error {
                message: format!("{message}{report}"),
            },
        }
    }

    /// Forwarded — a decorator that swallows this drops real sub-agent spend
    /// out of the budget accounting with no compiler to catch it.
    fn drain_sub_agent_spend_usd(&self) -> f64 {
        self.inner.drain_sub_agent_spend_usd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ToolRegistry;

    /// An [`HunkReviewIo`] answering from a script, recording what it was
    /// shown so tests can assert on the card as well as the outcome.
    struct ScriptedIo {
        accept: Vec<usize>,
        fail: bool,
        seen: std::sync::Mutex<Option<HunkProposal>>,
    }

    impl ScriptedIo {
        fn accepting(accept: Vec<usize>) -> Arc<Self> {
            Arc::new(Self {
                accept,
                fail: false,
                seen: std::sync::Mutex::new(None),
            })
        }

        fn unreachable() -> Arc<Self> {
            Arc::new(Self {
                accept: Vec::new(),
                fail: true,
                seen: std::sync::Mutex::new(None),
            })
        }

        fn proposal(&self) -> Option<HunkProposal> {
            self.seen.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl HunkReviewIo for ScriptedIo {
        async fn review(&self, proposal: &HunkProposal) -> Result<Vec<usize>, String> {
            *self.seen.lock().unwrap() = Some(proposal.clone());
            if self.fail {
                return Err("the deck closed".into());
            }
            Ok(self.accept.clone())
        }
    }

    /// Rewrites the file from inside `review`, i.e. in the window after the
    /// gate has read the pre-image and before the rewritten call applies —
    /// the human-length window a real reviewer holds open.
    struct DriftingIo(PathBuf);

    #[async_trait]
    impl HunkReviewIo for DriftingIo {
        async fn review(&self, _proposal: &HunkProposal) -> Result<Vec<usize>, String> {
            std::fs::write(&self.0, "somebody else got here first\n").unwrap();
            Ok(vec![0])
        }
    }

    /// A workspace holding `thirty numbered lines`, plus the real registry —
    /// the gate is only worth testing against the transaction it rides.
    fn fixture() -> (tempfile::TempDir, ToolRegistry, String) {
        let dir = tempfile::tempdir().unwrap();
        let content: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.path().join("a.rs"), &content).unwrap();
        let reg = ToolRegistry::with_issue_backend(dir.path().to_path_buf(), None);
        (dir, reg, content)
    }

    fn edit(path: &str, old: &str, new: &str) -> Value {
        serde_json::json!({ "path": path, "old_string": old, "new_string": new })
    }

    /// The issue's acceptance criterion, end to end and through the real
    /// `apply_edits` transaction: two hunks proposed, one declined, exactly one
    /// applied.
    #[tokio::test]
    async fn a_two_hunk_batch_with_one_declined_applies_exactly_one_hunk() {
        let (dir, reg, _) = fixture();
        let io = ScriptedIo::accepting(vec![0]);
        let gate = HunkGate::new(
            &reg,
            Some(io.clone() as Arc<dyn HunkReviewIo>),
            dir.path().to_path_buf(),
        );

        let out = gate
            .execute(
                "apply_edits",
                &serde_json::json!({ "edits": [
                    edit("a.rs", "line 2\n", "TWO\n"),
                    edit("a.rs", "line 25\n", "TWENTY-FIVE\n"),
                ]}),
            )
            .await;
        assert!(!out.is_error(), "{out:?}");

        let landed = std::fs::read_to_string(dir.path().join("a.rs")).unwrap();
        assert!(landed.contains("TWO\n"), "the accepted hunk must apply");
        assert!(
            landed.contains("line 25\n") && !landed.contains("TWENTY-FIVE"),
            "the declined hunk must NOT apply: {landed}"
        );
        // The reviewer was shown two separately-answerable hunks.
        assert_eq!(io.proposal().unwrap().hunks.len(), 2);
    }

    /// The model has to learn what was refused, or it will read the file back,
    /// find its own edit missing, and attribute it to drift.
    #[tokio::test]
    async fn the_declined_hunk_is_named_in_the_tool_result() {
        let (dir, reg, _) = fixture();
        let gate = HunkGate::new(
            &reg,
            Some(ScriptedIo::accepting(vec![0])),
            dir.path().to_path_buf(),
        );

        let out = gate
            .execute(
                "apply_edits",
                &serde_json::json!({ "edits": [
                    edit("a.rs", "line 2\n", "TWO\n"),
                    edit("a.rs", "line 25\n", "TWENTY-FIVE\n"),
                ]}),
            )
            .await;
        let ToolOutput::Ok { content } = out else {
            panic!("expected the accepted hunk to apply: {out:?}");
        };
        assert!(content.contains("declined 1 of 2"), "{content}");
        assert!(content.contains("a.rs"), "{content}");
        assert!(
            content.contains("@@"),
            "the hunk must be positioned: {content}"
        );
    }

    /// Declining everything is a refusal, and a refusal must not read as a
    /// successful write.
    #[tokio::test]
    async fn declining_every_hunk_writes_nothing_and_errors() {
        let (dir, reg, original) = fixture();
        let gate = HunkGate::new(
            &reg,
            Some(ScriptedIo::accepting(vec![])),
            dir.path().to_path_buf(),
        );

        let out = gate
            .execute(
                "apply_edits",
                &serde_json::json!({ "edits": [
                    edit("a.rs", "line 2\n", "TWO\n"),
                    edit("a.rs", "line 25\n", "X\n"),
                ]}),
            )
            .await;
        let ToolOutput::Error { message } = out else {
            panic!("a full refusal must be an error: {out:?}");
        };
        assert!(message.contains("declined in full"), "{message}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            original
        );
    }

    /// Accepting everything must be indistinguishable from an ungated call —
    /// same input, same report, no gate residue.
    #[tokio::test]
    async fn accepting_every_hunk_passes_the_original_input_through() {
        let (dir, reg, _) = fixture();
        let gate = HunkGate::new(
            &reg,
            Some(ScriptedIo::accepting(vec![0, 1])),
            dir.path().to_path_buf(),
        );

        let out = gate
            .execute(
                "apply_edits",
                &serde_json::json!({ "edits": [
                    edit("a.rs", "line 2\n", "TWO\n"),
                    edit("a.rs", "line 25\n", "TWENTY-FIVE\n"),
                ]}),
            )
            .await;
        let ToolOutput::Ok { content } = out else {
            panic!("{out:?}");
        };
        // The tool's own per-edit report, not a rewritten one-edit batch.
        assert!(
            content.contains("applied 2 edits across 1 file(s)"),
            "{content}"
        );
        assert!(!content.contains("hunk review"), "{content}");
        let landed = std::fs::read_to_string(dir.path().join("a.rs")).unwrap();
        assert!(landed.contains("TWO\n") && landed.contains("TWENTY-FIVE\n"));
    }

    /// A reviewer the host cannot reach is a refusal, never an approval.
    #[tokio::test]
    async fn an_unreachable_reviewer_fails_closed() {
        let (dir, reg, original) = fixture();
        let gate = HunkGate::new(
            &reg,
            Some(ScriptedIo::unreachable()),
            dir.path().to_path_buf(),
        );

        let out = gate
            .execute(
                "apply_edits",
                &serde_json::json!({ "edits": [edit("a.rs", "line 2\n", "TWO\n")] }),
            )
            .await;
        assert!(out.is_error(), "{out:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            original
        );
    }

    /// Two files, one wholly declined: the other still applies, and the
    /// declined file is untouched.
    #[tokio::test]
    async fn a_multi_file_batch_can_decline_one_file_entirely() {
        let (dir, reg, original) = fixture();
        std::fs::write(dir.path().join("b.rs"), "keep me\n").unwrap();
        let gate = HunkGate::new(
            &reg,
            Some(ScriptedIo::accepting(vec![1])),
            dir.path().to_path_buf(),
        );

        let out = gate
            .execute(
                "apply_edits",
                &serde_json::json!({ "edits": [
                    edit("a.rs", "line 2\n", "TWO\n"),
                    edit("b.rs", "keep me\n", "CHANGED\n"),
                ]}),
            )
            .await;
        assert!(!out.is_error(), "{out:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            original,
            "the wholly-declined file must be untouched"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.rs")).unwrap(),
            "CHANGED\n"
        );
    }

    /// A `dry_run` writes nothing, so there is nothing to consent to — asking
    /// would train reviewers to wave through prompts that never mattered.
    #[tokio::test]
    async fn a_dry_run_is_never_gated() {
        let (dir, reg, _) = fixture();
        let io = ScriptedIo::accepting(vec![]);
        let gate = HunkGate::new(
            &reg,
            Some(io.clone() as Arc<dyn HunkReviewIo>),
            dir.path().to_path_buf(),
        );

        let out = gate
            .execute(
                "apply_edits",
                &serde_json::json!({
                    "edits": [edit("a.rs", "line 2\n", "TWO\n")],
                    "dry_run": true
                }),
            )
            .await;
        assert!(!out.is_error(), "{out:?}");
        assert!(io.proposal().is_none(), "a dry run must not raise a card");
    }

    /// A call that would fail validation must reach the model as the tool's own
    /// error, not as a review of bytes that were never going to be written.
    #[tokio::test]
    async fn a_call_that_would_fail_validation_is_not_gated() {
        let (dir, reg, _) = fixture();
        let io = ScriptedIo::accepting(vec![]);
        let gate = HunkGate::new(
            &reg,
            Some(io.clone() as Arc<dyn HunkReviewIo>),
            dir.path().to_path_buf(),
        );

        let out = gate
            .execute(
                "apply_edits",
                &serde_json::json!({ "edits": [edit("a.rs", "NOT PRESENT", "x")] }),
            )
            .await;
        let ToolOutput::Error { message } = out else {
            panic!("{out:?}");
        };
        assert!(message.contains("old_string not found"), "{message}");
        assert!(
            io.proposal().is_none(),
            "no card for a call that cannot run"
        );
    }

    /// A run with no reviewer must be indistinguishable from one before this
    /// layer existed: no card, no rewrite, the tool's own report verbatim. This
    /// is the other half of "fail closed" — closed applies to a reviewer that
    /// was supposed to be there, not to a run that never had one.
    #[tokio::test]
    async fn a_gate_with_no_reviewer_is_inert() {
        let (dir, reg, _) = fixture();
        let gate = HunkGate::new(&reg, None, dir.path().to_path_buf());

        let out = gate
            .execute(
                "apply_edits",
                &serde_json::json!({ "edits": [
                    edit("a.rs", "line 2\n", "TWO\n"),
                    edit("a.rs", "line 25\n", "TWENTY-FIVE\n"),
                ]}),
            )
            .await;
        let ToolOutput::Ok { content } = out else {
            panic!("{out:?}");
        };
        assert!(
            content.contains("applied 2 edits across 1 file(s)"),
            "{content}"
        );
        assert!(!content.contains("hunk review"), "{content}");
        let landed = std::fs::read_to_string(dir.path().join("a.rs")).unwrap();
        assert!(landed.contains("TWO\n") && landed.contains("TWENTY-FIVE\n"));
    }

    /// Two spellings of one path are one file. Without normalizing against the
    /// real workspace root the dedup key falls back to the raw string, and the
    /// batch would be simulated — and reviewed — as two independent files.
    #[tokio::test]
    async fn two_spellings_of_one_path_are_reviewed_as_one_file() {
        let (dir, reg, _) = fixture();
        let io = ScriptedIo::accepting(vec![0, 1]);
        let gate = HunkGate::new(
            &reg,
            Some(io.clone() as Arc<dyn HunkReviewIo>),
            dir.path().to_path_buf(),
        );

        let out = gate
            .execute(
                "apply_edits",
                &serde_json::json!({ "edits": [
                    edit("a.rs", "line 2\n", "TWO\n"),
                    edit("./a.rs", "line 25\n", "TWENTY-FIVE\n"),
                ]}),
            )
            .await;
        assert!(!out.is_error(), "{out:?}");
        let proposal = io.proposal().expect("a card was raised");
        let mut paths: Vec<&str> = proposal.hunks.iter().map(|h| h.path.as_str()).collect();
        paths.dedup();
        assert_eq!(paths.len(), 1, "one file, however it is spelled: {paths:?}");
    }

    /// Read-only and opaque tools pass straight through.
    #[tokio::test]
    async fn ungated_tools_are_untouched() {
        let (dir, reg, _) = fixture();
        let io = ScriptedIo::accepting(vec![]);
        let gate = HunkGate::new(
            &reg,
            Some(io.clone() as Arc<dyn HunkReviewIo>),
            dir.path().to_path_buf(),
        );

        let out = gate
            .execute("read_file", &serde_json::json!({ "path": "a.rs" }))
            .await;
        assert!(!out.is_error(), "{out:?}");
        assert!(io.proposal().is_none());
    }

    /// `edit_file` gets the same granularity: one call replacing a span that
    /// decomposes into two hunks is two separate decisions, and the partial
    /// apply rides `apply_edits`' transaction.
    #[tokio::test]
    async fn a_single_edit_file_call_is_reviewable_hunk_by_hunk() {
        let (dir, reg, _) = fixture();
        let gate = HunkGate::new(
            &reg,
            Some(ScriptedIo::accepting(vec![0])),
            dir.path().to_path_buf(),
        );

        // One edit spanning lines 2..25 — far enough apart that the two real
        // changes inside it decompose into two hunks.
        let old: String = (2..=25).map(|i| format!("line {i}\n")).collect();
        let new = old.replace("line 2\n", "TWO\n").replace("line 25\n", "X\n");
        let out = gate.execute("edit_file", &edit("a.rs", &old, &new)).await;
        assert!(!out.is_error(), "{out:?}");

        let landed = std::fs::read_to_string(dir.path().join("a.rs")).unwrap();
        assert!(landed.contains("TWO\n"), "{landed}");
        assert!(
            landed.contains("line 25\n") && !landed.contains("\nX\n"),
            "{landed}"
        );
    }

    /// A `write_file` over an existing file is reviewable the same way, and its
    /// partial apply stays a `write_file` (one whole-file write, no batch).
    #[tokio::test]
    async fn a_write_file_over_an_existing_file_is_reviewable() {
        let (dir, reg, original) = fixture();
        let gate = HunkGate::new(
            &reg,
            Some(ScriptedIo::accepting(vec![0])),
            dir.path().to_path_buf(),
        );

        let replacement = original
            .replace("line 2\n", "TWO\n")
            .replace("line 25\n", "TWENTY-FIVE\n");
        let out = gate
            .execute(
                "write_file",
                &serde_json::json!({ "path": "a.rs", "content": replacement }),
            )
            .await;
        assert!(!out.is_error(), "{out:?}");

        let landed = std::fs::read_to_string(dir.path().join("a.rs")).unwrap();
        assert!(landed.contains("TWO\n"), "{landed}");
        assert!(!landed.contains("TWENTY-FIVE"), "{landed}");
    }

    /// The acceptance criterion the single-emitter invariant depends on: the
    /// `FileChange` announced for a partial apply describes the bytes that
    /// LANDED, not the bytes that were proposed.
    #[tokio::test]
    async fn the_file_change_event_reflects_the_applied_content() {
        let (dir, reg, _) = fixture();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        reg.attach_events(stella_core::EventSender::new(tx));
        let gate = HunkGate::new(
            &reg,
            Some(ScriptedIo::accepting(vec![0])),
            dir.path().to_path_buf(),
        );

        let out = gate
            .execute(
                "apply_edits",
                &serde_json::json!({ "edits": [
                    edit("a.rs", "line 2\n", "TWO\n"),
                    edit("a.rs", "line 25\n", "TWENTY-FIVE\n"),
                ]}),
            )
            .await;
        assert!(!out.is_error(), "{out:?}");

        let mut changes = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let stella_protocol::AgentEvent::FileChange {
                path,
                added,
                removed,
                diff,
                ..
            } = event
            {
                changes.push((path, added, removed, diff.unwrap_or_default()));
            }
        }
        assert_eq!(changes.len(), 1, "{changes:?}");
        let (path, added, removed, diff) = &changes[0];
        assert_eq!(path, "a.rs");
        // One line in, one line out — the ACCEPTED hunk alone.
        assert_eq!((*added, *removed), (1, 1), "{changes:?}");
        assert!(diff.contains("TWO"), "{diff}");
        assert!(
            !diff.contains("TWENTY-FIVE"),
            "the declined hunk must not appear in the announced change: {diff}"
        );
    }

    /// The issue's third acceptance criterion, verbatim: a **partial** apply
    /// that fails mid-way rolls back completely. The rewritten batch is still
    /// one `apply_edits` call, so this is that tool's existing guarantee — and
    /// this test is what proves the gate did not weaken it by routing around
    /// the transaction.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_partial_apply_that_fails_mid_way_rolls_back_completely() {
        use std::os::unix::fs::PermissionsExt;
        // Root bypasses DAC, so the induced write failure never happens —
        // skip rather than assert the wrong thing in a root container.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let (dir, reg, original) = fixture();
        let second: String = (1..=30).map(|i| format!("row {i}\n")).collect();
        let locked = dir.path().join("b.rs");
        std::fs::write(&locked, &second).unwrap();

        // Four hunks across two files; one declined in each, so BOTH files get
        // a synthesized edit and the batch is a genuine two-file partial.
        let gate = HunkGate::new(
            &reg,
            Some(ScriptedIo::accepting(vec![0, 2])),
            dir.path().to_path_buf(),
        );
        // The SECOND file is unwritable, so a.rs lands first and the batch then
        // aborts with one write already on disk.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o444)).unwrap();
        let out = gate
            .execute(
                "apply_edits",
                &serde_json::json!({ "edits": [
                    edit("a.rs", "line 2\n", "TWO\n"),
                    edit("a.rs", "line 25\n", "TWENTY-FIVE\n"),
                    edit("b.rs", "row 2\n", "ROW-TWO\n"),
                    edit("b.rs", "row 25\n", "ROW-TWENTY-FIVE\n"),
                ]}),
            )
            .await;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();

        let ToolOutput::Error { message } = out else {
            panic!("expected the batch to abort: {out:?}");
        };
        assert!(message.contains("batch aborted"), "{message}");
        // The accepted-and-written file is back to its pre-batch bytes: a
        // partial apply is still all-or-nothing across the surviving set.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            original,
            "the already-written file must roll back"
        );
        assert_eq!(std::fs::read_to_string(&locked).unwrap(), second);
        // …and the reviewer's refusal still reaches the model, so it does not
        // retry the declined hunks believing nobody saw them.
        assert!(message.contains("declined 2 of 4"), "{message}");
    }

    /// The compare-and-swap property. If the file moves between the review and
    /// the apply, the rewritten batch must fail with the existing drift
    /// attribution rather than overwrite the newer bytes.
    #[tokio::test]
    async fn a_file_that_drifts_after_review_fails_instead_of_clobbering() {
        let (dir, reg, _) = fixture();
        let path = dir.path().join("a.rs");
        let gate = HunkGate::new(
            &reg,
            Some(Arc::new(DriftingIo(path.clone()))),
            dir.path().to_path_buf(),
        );
        let out = gate
            .execute(
                "apply_edits",
                &serde_json::json!({ "edits": [
                    edit("a.rs", "line 2\n", "TWO\n"),
                    edit("a.rs", "line 25\n", "X\n"),
                ]}),
            )
            .await;
        assert!(out.is_error(), "{out:?}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "somebody else got here first\n",
            "the foreign write must survive"
        );
    }

    /// The same property on the one gated tool that cannot express a
    /// precondition of its own. `write_file` writes `content` unconditionally,
    /// so without the gate's own check a partial apply would compose against a
    /// pre-image that is no longer there and overwrite whatever arrived while
    /// the reviewer was deciding.
    #[tokio::test]
    async fn a_write_file_that_drifts_after_review_fails_instead_of_clobbering() {
        let (dir, reg, original) = fixture();
        let path = dir.path().join("a.rs");
        let gate = HunkGate::new(
            &reg,
            Some(Arc::new(DriftingIo(path.clone()))),
            dir.path().to_path_buf(),
        );

        // Two hunks with one accepted, so this is the *partial* path — a full
        // accept passes the original input through and never composes at all.
        let replacement = original
            .replace("line 2\n", "TWO\n")
            .replace("line 25\n", "TWENTY-FIVE\n");
        let out = gate
            .execute(
                "write_file",
                &serde_json::json!({ "path": "a.rs", "content": replacement }),
            )
            .await;
        assert!(out.is_error(), "{out:?}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "somebody else got here first\n",
            "the foreign write must survive"
        );
    }

    /// The pre-image is read *lossily*, so on a file that is not valid UTF-8
    /// the unchanged regions of the composed content are U+FFFD where the file
    /// has bytes. Writing that would re-encode the parts nobody reviewed, so
    /// the same check refuses it.
    #[tokio::test]
    async fn a_partial_write_file_will_not_re_encode_a_non_utf8_file() {
        let dir = tempfile::tempdir().unwrap();
        let reg = ToolRegistry::with_issue_backend(dir.path().to_path_buf(), None);
        let path = dir.path().join("a.bin");
        // Valid lines around one invalid byte, so the decomposition still finds
        // two hunks to offer and the invalid line is in neither of them.
        let mut raw: Vec<u8> = Vec::new();
        for i in 1..=30u32 {
            if i == 15 {
                raw.extend_from_slice(b"bad \xff byte\n");
            } else {
                raw.extend_from_slice(format!("line {i}\n").as_bytes());
            }
        }
        std::fs::write(&path, &raw).unwrap();

        let gate = HunkGate::new(
            &reg,
            Some(ScriptedIo::accepting(vec![0])),
            dir.path().to_path_buf(),
        );
        let replacement = String::from_utf8_lossy(&raw)
            .replace("line 2\n", "TWO\n")
            .replace("line 25\n", "TWENTY-FIVE\n");
        let out = gate
            .execute(
                "write_file",
                &serde_json::json!({ "path": "a.bin", "content": replacement }),
            )
            .await;
        assert!(out.is_error(), "{out:?}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            raw,
            "the byte the reviewer never saw must survive"
        );
    }
}
