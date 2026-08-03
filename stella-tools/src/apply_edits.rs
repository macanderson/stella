//! `apply_edits` — transactional multi-file edits with a validate-first
//! preflight (#333). A batch of `old_string → new_string` edits — across
//! multiple files — either applies **in full** or not at all:
//!
//! 1. **Validate**: every edit is resolved against an in-memory simulation
//!    of the current files (edits to the same file compose in order). If ANY
//!    edit fails, a structured per-edit report comes back and **nothing** is
//!    written. Match failures reuse the read→edit drift attribution (#331):
//!    a stale `old_string` on a file that changed out-of-band is reported as
//!    drift, not a generic miss.
//! 2. **Apply**: only when every edit validated, each touched file is
//!    written once with its final content. If a write fails midway (disk
//!    full, permissions), every touched file — the one that failed included —
//!    is **rolled back** to its original bytes so the tree is never left
//!    half-applied; a file that cannot be restored is named loudly in the
//!    error instead of being reported clean. The rollback restores only what
//!    the batch still owns: a file whose bytes no longer match what this batch
//!    wrote belongs to whoever wrote it last, and is reported rather than
//!    reverted (see `roll_back_prior_writes`).
//!
//! One transactional call also fits the engine's concurrency model better
//! than N serial `edit_file` barriers: mutating tools are never
//! parallelized, so a five-file rename is five sequential steps today and
//! one step through this tool.
//!
//! Storage-definition files ride the registry's schema gate like any other
//! write (#442): the gate simulates the batch's composed edits per touched
//! schema file — via `simulate_batch`, the same in-order composition the
//! validate phase performs — and judges each result before anything lands.
//! The transactional path is not a way around the storage map.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::read::ReadLedger;
use crate::registry::Tool;

/// Hard ceiling on edits per call — enough for a wide rename, small enough
/// that a runaway batch can't hold the whole tree in memory.
const MAX_EDITS: usize = 64;

#[derive(Default)]
pub struct ApplyEdits {
    ledger: Arc<ReadLedger>,
}

impl ApplyEdits {
    /// Construct sharing the registry's read-state ledger, so validate-phase
    /// match failures are drift-attributed against what the model last saw.
    pub fn with_ledger(ledger: Arc<ReadLedger>) -> Self {
        Self { ledger }
    }
}

struct ParsedEdit {
    path: String,
    old_string: String,
    new_string: String,
    replace_all: bool,
}

/// One edit's validate-phase outcome, rendered into the per-edit report.
enum EditVerdict {
    /// Resolves cleanly; `occurrences` will be replaced.
    Ok {
        occurrences: usize,
    },
    Failed {
        reason: String,
    },
}

fn parse_edits(input: &Value) -> Result<Vec<ParsedEdit>, String> {
    let edits = input
        .get("edits")
        .and_then(|v| v.as_array())
        .ok_or("missing required field `edits` (array of {path, old_string, new_string})")?;
    if edits.is_empty() {
        return Err("`edits` must not be empty".into());
    }
    if edits.len() > MAX_EDITS {
        return Err(format!(
            "`edits` has {} entries — the ceiling is {MAX_EDITS} per call; split the batch",
            edits.len()
        ));
    }
    edits
        .iter()
        .enumerate()
        .map(|(i, edit)| {
            let field = |key: &str| -> Result<String, String> {
                edit.get(key)
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .ok_or(format!("edit {i}: missing required field `{key}`"))
            };
            let parsed = ParsedEdit {
                path: field("path")?,
                old_string: field("old_string")?,
                new_string: field("new_string")?,
                replace_all: edit
                    .get("replace_all")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            };
            if parsed.old_string.is_empty() {
                // Same destructive-empty-match hazard `edit_file` refuses.
                return Err(format!(
                    "edit {i}: old_string must not be empty — use write_file to create or \
                     replace a whole file"
                ));
            }
            Ok(parsed)
        })
        .collect()
}

/// One touched file: (workspace-relative path, original bytes, final bytes).
///
/// The path is the one the batch first named the file by, and is what every
/// later read, write and rollback for that file uses — so the whole batch
/// resolves each file exactly one way.
type TouchedFile = (String, String, String);

/// Restore every file this batch already wrote back to its pre-batch bytes,
/// returning the lines the caller appends to its error.
///
/// A file is rolled back only while the batch still owns it. Between our write
/// and the failure that triggered this, something else — a formatter, a
/// watcher, the user's editor — may have rewritten the file; restoring the
/// pre-batch bytes over that silently reverts THEIR change under the banner of
/// a clean abort. Ownership is decided by comparing the bytes on disk to the
/// bytes we wrote, which are already in memory, so an exact comparison costs no
/// more than a hash and cannot collide. A file we cannot read is not evidence
/// of foreign ownership, so it still rolls back.
///
/// An empty return means every already-written file is back to its original
/// content.
async fn roll_back_prior_writes(
    handle: &std::sync::Arc<crate::rootfd::RootHandle>,
    files: &HashMap<String, TouchedFile>,
    written: &[String],
) -> String {
    let mut note = String::new();
    for key in written {
        let (path, original, ours) = &files[key];
        if matches!(crate::rootfd::read_async(handle, path).await, Ok(now) if now != ours.as_bytes())
        {
            note.push_str(&format!(
                "\n  NOT ROLLED BACK: `{path}` was changed by something else after this batch \
                 wrote it — its current content was left alone; reconcile it by hand"
            ));
            continue;
        }
        // The rollback especially must not truncate: a failed rollback with a
        // truncating write turns a partial batch into a destroyed file.
        if crate::durable_write::write_file_durably_at(
            std::sync::Arc::clone(handle),
            path.clone(),
            original.as_bytes().to_vec(),
            false,
        )
        .await
        .is_err()
        {
            note.push_str(&format!(
                "\n  ROLLBACK FAILED for `{path}` — restore it manually from the content you \
                 last read"
            ));
        }
    }
    note
}

/// The batch's composed post-edit content for one `path` — the SAME
/// in-order composition (and the same zero-match / ambiguous-match refusals)
/// the validate phase applies, shared with the registry's storage gate
/// (#442) so the gate judges exactly the bytes apply would write. `None`
/// when the input doesn't parse, no edit targets `path`, or any edit to it
/// fails to resolve — the tool itself then reports the failure, unwritten,
/// and an ungated failing batch writes nothing.
pub(crate) fn simulate_batch(input: &Value, path: &str, current: &str) -> Option<String> {
    let edits = parse_edits(input).ok()?;
    let mut content = current.to_string();
    let mut touched = false;
    for edit in edits.iter().filter(|e| e.path == path) {
        touched = true;
        // Same CRLF reconciliation the validate phase applies, so the gate
        // keeps judging exactly the bytes apply would write.
        let promoted = crate::edit::crlf_promoted(&content, &edit.old_string, &edit.new_string);
        let (old, new) = match &promoted {
            Some((old, new)) => (old.as_str(), new.as_str()),
            None => (edit.old_string.as_str(), edit.new_string.as_str()),
        };
        let count = content.matches(old).count();
        if count == 0 || (count > 1 && !edit.replace_all) {
            return None;
        }
        content = if edit.replace_all {
            content.replace(old, new)
        } else {
            content.replacen(old, new, 1)
        };
    }
    touched.then_some(content)
}

#[async_trait]
impl Tool for ApplyEdits {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "apply_edits".into(),
            description: "Apply multiple exact-substring edits — across multiple files — in one transactional call. Every edit is validated first; if any fails, NOTHING is written and a per-edit report explains why. Edits to the same file compose in order. Set dry_run to validate without writing.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "edits": {
                        "type": "array",
                        "description": "Edits applied in order; each targets one file",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": { "type": "string", "description": "File path relative to workspace root" },
                                "old_string": { "type": "string", "description": "Exact text to find" },
                                "new_string": { "type": "string", "description": "Replacement text" },
                                "replace_all": { "type": "boolean", "description": "Replace all occurrences (default false)" }
                            },
                            "required": ["path", "old_string", "new_string"]
                        }
                    },
                    "dry_run": { "type": "boolean", "description": "Validate every edit and report, but write nothing (default false)" },
                    "reason": { "type": "string", "description": "Why you are editing these files — recorded in the session's file-touch audit log" }
                },
                "required": ["edits"]
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, root: &std::path::Path) -> ToolOutput {
        let edits = match parse_edits(input) {
            Ok(edits) => edits,
            Err(message) => return ToolOutput::Error { message },
        };
        let dry_run = input
            .get("dry_run")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // One held root descriptor for the whole batch: every read below and
        // every write and rollback in phase 2 walks descriptors from it rather
        // than re-resolving a path string per operation (#938).
        let handle = match crate::rootfd::RootHandle::open(root) {
            Ok(handle) => std::sync::Arc::new(handle),
            Err(e) => {
                return ToolOutput::Error {
                    message: format!("cannot open workspace root: {e}"),
                };
            }
        };

        // ---- Phase 1: validate everything against an in-memory simulation.
        // `files` maps normalized-path key → (workspace-relative path,
        // original bytes, simulated current bytes). The path is the one the
        // batch first named the file by, and is what every later read, write
        // and rollback for that file uses — so the whole batch resolves each
        // file exactly one way. Edits to one file compose: edit 3 sees edit
        // 1's result, exactly as the apply will.
        let mut files: HashMap<String, TouchedFile> = HashMap::new();
        let mut order: Vec<String> = Vec::new(); // first-touch order, for stable output
        let mut verdicts: Vec<EditVerdict> = Vec::new();
        let mut failures = 0usize;

        for edit in &edits {
            let verdict = 'verdict: {
                let key = crate::file_touch::normalize_workspace_path(root, &edit.path)
                    .unwrap_or_else(|| edit.path.clone());
                if !files.contains_key(&key) {
                    match crate::rootfd::read_to_string_async(&handle, &edit.path).await {
                        Ok(content) => {
                            order.push(key.clone());
                            files
                                .insert(key.clone(), (edit.path.clone(), content.clone(), content));
                        }
                        Err(e) if e.is_escape() => {
                            break 'verdict EditVerdict::Failed {
                                reason: format!(
                                    "path `{}` escapes workspace root ({e})",
                                    edit.path
                                ),
                            };
                        }
                        Err(e) => {
                            break 'verdict EditVerdict::Failed {
                                reason: format!("failed to read `{}`: {e}", edit.path),
                            };
                        }
                    }
                }
                let (_, original, simulated) = files.get_mut(&key).expect("inserted above");
                // A needle copied out of `read_file`'s render carries LF
                // newlines even for a CRLF file — see
                // [`crate::edit::crlf_promoted`].
                let promoted =
                    crate::edit::crlf_promoted(simulated, &edit.old_string, &edit.new_string);
                let (old_string, new_string) = match &promoted {
                    Some((old, new)) => (old.as_str(), new.as_str()),
                    None => (edit.old_string.as_str(), edit.new_string.as_str()),
                };
                let count = simulated.matches(old_string).count();
                if count == 0 {
                    // A prior edit in this batch consuming the match is a
                    // composition mistake, not drift — say which it is.
                    let attribution = if original.contains(&edit.old_string) {
                        " — a PRIOR edit in this batch already changed that region; edits to \
                         one file compose in order, so target the intermediate content"
                    } else {
                        // Reuse the #331 drift attribution against the
                        // original disk bytes the batch started from.
                        let original_sha = crate::staleness::hex_sha256(original.as_bytes());
                        match self.ledger.last_seen_sha(root, &edit.path) {
                            Some(seen) if seen != original_sha => {
                                " — the file CHANGED after you last read it (out-of-band \
                                 modification); re-read it before retrying"
                            }
                            Some(_) => {
                                " — the file is unchanged since you last saw it; check for \
                                 exact whitespace/newline differences"
                            }
                            None => {
                                " — no read of this file is recorded this session; read it first"
                            }
                        }
                    };
                    break 'verdict EditVerdict::Failed {
                        reason: format!("old_string not found in `{}`{attribution}", edit.path),
                    };
                }
                if count > 1 && !edit.replace_all {
                    break 'verdict EditVerdict::Failed {
                        reason: format!(
                            "old_string appears {count} times in `{}` — set replace_all=true or \
                             provide a more specific string",
                            edit.path
                        ),
                    };
                }
                *simulated = if edit.replace_all {
                    simulated.replace(old_string, new_string)
                } else {
                    simulated.replacen(old_string, new_string, 1)
                };
                EditVerdict::Ok { occurrences: count }
            };
            if matches!(verdict, EditVerdict::Failed { .. }) {
                failures += 1;
            }
            verdicts.push(verdict);
        }

        let report = |verdicts: &[EditVerdict]| -> String {
            verdicts
                .iter()
                .enumerate()
                .map(|(i, v)| match v {
                    EditVerdict::Ok { occurrences } => format!(
                        "  edit {i} ({}): ok — {occurrences} occurrence(s)",
                        edits[i].path
                    ),
                    EditVerdict::Failed { reason } => format!("  edit {i}: FAILED — {reason}"),
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        if failures > 0 {
            return ToolOutput::Error {
                message: format!(
                    "{failures} of {} edits failed validation — NOTHING was written \
                     (all-or-nothing):\n{}",
                    edits.len(),
                    report(&verdicts)
                ),
            };
        }

        if dry_run {
            return ToolOutput::Ok {
                content: format!(
                    "dry run: all {} edits validate cleanly across {} file(s) — nothing \
                     written:\n{}",
                    edits.len(),
                    files.len(),
                    report(&verdicts)
                ),
            };
        }

        // ---- Phase 2: apply — one write per touched file, in first-touch
        // order. On a mid-batch write failure, roll back every file already
        // written to its original bytes so the tree never stays half-applied.
        let mut written: Vec<String> = Vec::new();
        for key in &order {
            let (path, _, simulated) = &files[key];
            if let Err(e) = crate::durable_write::write_file_durably_at(
                std::sync::Arc::clone(&handle),
                path.clone(),
                simulated.as_bytes().to_vec(),
                false,
            )
            .await
            {
                let mut rollback_note = String::new();
                // The failing file first: `durable_write` rewrites in place,
                // so a failure inside `write_all` (disk full) can leave new
                // bytes spliced over the old tail — and this is exactly the
                // file the old rollback skipped while the error claimed the
                // tree was intact. Only rewritten when the bytes actually
                // moved, so a pre-write failure (open denied) stays silent
                // rather than raising a false "restore manually" alarm.
                let (fail_path, fail_original, _) = &files[key];
                let needs_restore = match crate::rootfd::read_async(&handle, fail_path).await {
                    Ok(now) => now != fail_original.as_bytes(),
                    Err(_) => true,
                };
                if needs_restore
                    && crate::durable_write::write_file_durably_at(
                        std::sync::Arc::clone(&handle),
                        fail_path.clone(),
                        fail_original.as_bytes().to_vec(),
                        false,
                    )
                    .await
                    .is_err()
                {
                    rollback_note.push_str(&format!(
                        "\n  ROLLBACK FAILED for `{fail_path}` — restore it manually from \
                         the content you last read"
                    ));
                }
                rollback_note.push_str(&roll_back_prior_writes(&handle, &files, &written).await);
                if rollback_note.is_empty() {
                    rollback_note = format!(
                        "\n  every touched file (including `{path}`) holds its original \
                         content; {} already-written file(s) were rolled back",
                        written.len()
                    );
                }
                return ToolOutput::Error {
                    message: format!(
                        "failed to write `{path}`: {e} — batch aborted{rollback_note}"
                    ),
                };
            }
            written.push(key.clone());
        }
        // The model knows the bytes it just produced (#331) — record every
        // final content so these writes are never misattributed as drift.
        for key in &order {
            let (path, _, simulated) = &files[key];
            self.ledger.record_known(root, path, simulated);
        }

        ToolOutput::Ok {
            content: format!(
                "applied {} edits across {} file(s):\n{}",
                edits.len(),
                files.len(),
                report(&verdicts)
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read::ReadFile;

    fn edit(path: &str, old: &str, new: &str) -> Value {
        serde_json::json!({ "path": path, "old_string": old, "new_string": new })
    }

    #[tokio::test]
    async fn applies_a_multi_file_batch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() { one }\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn b() { one }\n").unwrap();

        let result = ApplyEdits::default()
            .execute(
                &serde_json::json!({ "edits": [
                    edit("a.rs", "one", "two"),
                    edit("b.rs", "one", "two"),
                ]}),
                dir.path(),
            )
            .await;
        match result {
            ToolOutput::Ok { content } => {
                assert!(
                    content.contains("applied 2 edits across 2 file(s)"),
                    "{content}"
                );
            }
            ToolOutput::Error { message } => panic!("expected ok, got: {message}"),
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "fn a() { two }\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.rs")).unwrap(),
            "fn b() { two }\n"
        );
    }

    /// The #333 witness: a two-edit batch where the second edit's
    /// `old_string` is absent must leave BOTH files unchanged and report
    /// which edit failed — failing before transactional apply existed.
    #[tokio::test]
    async fn failed_edit_leaves_every_file_unchanged_and_names_the_failure() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() { one }\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn b() { one }\n").unwrap();

        let result = ApplyEdits::default()
            .execute(
                &serde_json::json!({ "edits": [
                    edit("a.rs", "one", "two"),
                    edit("b.rs", "MISSING", "two"),
                ]}),
                dir.path(),
            )
            .await;
        match result {
            ToolOutput::Error { message } => {
                assert!(message.contains("NOTHING was written"), "{message}");
                assert!(message.contains("edit 0 (a.rs): ok"), "{message}");
                assert!(message.contains("edit 1: FAILED"), "{message}");
            }
            ToolOutput::Ok { content } => panic!("expected error, got: {content}"),
        }
        // Both files untouched — including the one whose edit validated.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "fn a() { one }\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.rs")).unwrap(),
            "fn b() { one }\n"
        );
    }

    #[tokio::test]
    async fn edits_to_the_same_file_compose_in_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "alpha\n").unwrap();

        let result = ApplyEdits::default()
            .execute(
                &serde_json::json!({ "edits": [
                    edit("a.rs", "alpha", "beta"),
                    // Only matches AFTER the first edit applied.
                    edit("a.rs", "beta", "gamma"),
                ]}),
                dir.path(),
            )
            .await;
        assert!(!result.is_error(), "{result:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "gamma\n"
        );
    }

    #[tokio::test]
    async fn dry_run_validates_but_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "one\n").unwrap();

        let result = ApplyEdits::default()
            .execute(
                &serde_json::json!({ "edits": [edit("a.rs", "one", "two")], "dry_run": true }),
                dir.path(),
            )
            .await;
        match result {
            ToolOutput::Ok { content } => {
                assert!(content.contains("dry run"), "{content}");
                assert!(content.contains("nothing"), "{content}");
            }
            ToolOutput::Error { message } => panic!("expected ok, got: {message}"),
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "one\n"
        );
    }

    #[tokio::test]
    async fn validate_failure_is_drift_attributed_via_the_shared_ledger() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "original\n").unwrap();
        let ledger = Arc::new(ReadLedger::default());
        let read = ReadFile::with_ledger(ledger.clone());
        let apply = ApplyEdits::with_ledger(ledger.clone());

        let seen = read
            .execute(&serde_json::json!({"path": "a.rs"}), dir.path())
            .await;
        assert!(!seen.is_error());
        // Out-of-band change between the read and the batch.
        std::fs::write(dir.path().join("a.rs"), "rewritten\n").unwrap();

        let result = apply
            .execute(
                &serde_json::json!({ "edits": [edit("a.rs", "original", "x")] }),
                dir.path(),
            )
            .await;
        match result {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("CHANGED after you last read it"),
                    "drift must be attributed in the per-edit report: {message}"
                );
            }
            ToolOutput::Ok { content } => panic!("expected error, got: {content}"),
        }
    }

    #[test]
    fn simulate_batch_composes_edits_exactly_like_validate() {
        let input = serde_json::json!({ "edits": [
            edit("a.sql", "one", "two"),
            edit("a.sql", "two three", "four"),
            edit("b.sql", "x", "y"),
        ]});
        // Edits to one file compose in order — the second targets the
        // intermediate content the first produced.
        assert_eq!(
            simulate_batch(&input, "a.sql", "one three").as_deref(),
            Some("four")
        );
        assert_eq!(simulate_batch(&input, "b.sql", "x").as_deref(), Some("y"));
        // A path the batch never touches simulates to nothing.
        assert_eq!(simulate_batch(&input, "c.sql", "zzz"), None);
        // The validate phase's refusals mirror exactly: zero matches and
        // ambiguous multi-matches both fail the simulation.
        assert_eq!(simulate_batch(&input, "a.sql", "nope"), None);
        assert_eq!(simulate_batch(&input, "b.sql", "x x"), None);
    }

    #[tokio::test]
    async fn plain_code_files_are_not_mistaken_for_storage() {
        // .ts/.py/.js are storage *candidates* (marker-gated adapters): a
        // plain code file extracts empty and must pass through the batch.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.ts"), "export const one = 1;\n").unwrap();

        let result = ApplyEdits::default()
            .execute(
                &serde_json::json!({ "edits": [edit("app.ts", "one = 1", "one = 2")] }),
                dir.path(),
            )
            .await;
        assert!(!result.is_error(), "{result:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("app.ts")).unwrap(),
            "export const one = 2;\n"
        );
    }

    #[tokio::test]
    async fn prior_batch_edit_consuming_a_match_is_named_as_composition() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "alpha\n").unwrap();

        let result = ApplyEdits::default()
            .execute(
                &serde_json::json!({ "edits": [
                    edit("a.rs", "alpha", "beta"),
                    edit("a.rs", "alpha", "gamma"),
                ]}),
                dir.path(),
            )
            .await;
        match result {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("PRIOR edit in this batch"),
                    "composition mistakes must not be blamed on drift: {message}"
                );
            }
            ToolOutput::Ok { content } => panic!("expected error, got: {content}"),
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "alpha\n"
        );
    }

    #[tokio::test]
    async fn ambiguous_match_without_replace_all_fails_the_batch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "x x\n").unwrap();

        let result = ApplyEdits::default()
            .execute(
                &serde_json::json!({ "edits": [edit("a.rs", "x", "y")] }),
                dir.path(),
            )
            .await;
        match result {
            ToolOutput::Error { message } => {
                assert!(message.contains("appears 2 times"), "{message}");
            }
            ToolOutput::Ok { content } => panic!("expected error, got: {content}"),
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "x x\n"
        );
    }

    #[tokio::test]
    async fn empty_and_oversized_batches_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let empty = ApplyEdits::default()
            .execute(&serde_json::json!({ "edits": [] }), dir.path())
            .await;
        assert!(empty.is_error());

        let oversized: Vec<Value> = (0..65).map(|i| edit("a.rs", "x", &i.to_string())).collect();
        let result = ApplyEdits::default()
            .execute(&serde_json::json!({ "edits": oversized }), dir.path())
            .await;
        match result {
            ToolOutput::Error { message } => assert!(message.contains("ceiling"), "{message}"),
            ToolOutput::Ok { content } => panic!("expected error, got: {content}"),
        }
    }

    /// The batch path carries the same CRLF reconciliation `edit_file` does —
    /// see [`crate::edit::crlf_promoted`]. Without it a five-file rename in a
    /// Windows-line-ending repo failed validation on every multi-line edit and
    /// wrote nothing.
    #[tokio::test]
    async fn a_multi_line_batch_edit_of_a_crlf_file_lands_and_keeps_crlf() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("win.rs"), "fn a() {\r\n    one();\r\n}\r\n").unwrap();

        let result = ApplyEdits::default()
            .execute(
                &serde_json::json!({ "edits": [
                    edit("win.rs", "fn a() {\n    one();", "fn a() {\n    two();"),
                ]}),
                dir.path(),
            )
            .await;
        assert!(!result.is_error(), "{result:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("win.rs")).unwrap(),
            "fn a() {\r\n    two();\r\n}\r\n"
        );
    }

    #[test]
    fn simulate_batch_reconciles_crlf_like_the_validate_phase() {
        let input = serde_json::json!({ "edits": [
            edit("a.sql", "one\ntwo", "one\nTWO"),
        ]});
        assert_eq!(
            simulate_batch(&input, "a.sql", "one\r\ntwo\r\n").as_deref(),
            Some("one\r\nTWO\r\n")
        );
    }

    #[tokio::test]
    async fn escaping_path_fails_validation() {
        let dir = tempfile::tempdir().unwrap();
        let result = ApplyEdits::default()
            .execute(
                &serde_json::json!({ "edits": [edit("../../etc/passwd", "root", "x")] }),
                dir.path(),
            )
            .await;
        assert!(result.is_error());
    }

    /// A mid-batch write failure rolls back the already-written files and
    /// reports the tree's true state — the failing file's own content is
    /// verified (and restored when the bytes moved) rather than skipped.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_mid_batch_write_failure_rolls_back_the_written_files() {
        use std::os::unix::fs::PermissionsExt;
        // Root bypasses DAC: a 0444 file is still writable, so the induced
        // failure never happens and the batch (correctly) succeeds. Skip
        // rather than assert the wrong thing in a root container.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() { one }\n").unwrap();
        let locked = dir.path().join("b.rs");
        std::fs::write(&locked, "fn b() { one }\n").unwrap();
        // Make the SECOND file unwritable so file one lands first and the
        // batch then aborts.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o444)).unwrap();

        let result = ApplyEdits::default()
            .execute(
                &serde_json::json!({ "edits": [
                    edit("a.rs", "one", "two"),
                    edit("b.rs", "one", "two"),
                ]}),
                dir.path(),
            )
            .await;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();

        let message = match result {
            ToolOutput::Error { message } => message,
            ToolOutput::Ok { content } => panic!("expected the batch to abort, got: {content}"),
        };
        assert!(message.contains("batch aborted"), "{message}");
        assert!(message.contains("rolled back"), "{message}");
        // The written file is back to its original bytes, and the failed
        // file was never corrupted.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "fn a() { one }\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.rs")).unwrap(),
            "fn b() { one }\n"
        );
        // Nothing was corrupted, so no manual-restore alarm may fire.
        assert!(
            !message.contains("ROLLBACK FAILED"),
            "an intact failing file must not raise a manual-restore alarm: {message}"
        );
    }

    /// One entry of the rollback's view of a touched file: the bytes it found
    /// before the batch, and the bytes the batch wrote.
    fn touched(key: &str, original: &str, ours: &str) -> (String, TouchedFile) {
        (
            key.to_string(),
            (key.to_string(), original.to_string(), ours.to_string()),
        )
    }

    /// The root descriptor the rollback resolves through — the same handle the
    /// apply phase holds, so the tests exercise the production path.
    fn rooted(dir: &tempfile::TempDir) -> std::sync::Arc<crate::rootfd::RootHandle> {
        std::sync::Arc::new(crate::rootfd::RootHandle::open(dir.path()).unwrap())
    }

    /// The audit witness. The rollback restored the bytes captured during
    /// validation unconditionally, so a write that landed from ANOTHER process
    /// between our write and the abort was silently reverted — a third party's
    /// change destroyed, and the error still read as a clean all-or-nothing
    /// abort. A file whose bytes are no longer ours is left alone and named.
    #[tokio::test]
    async fn a_foreign_write_after_ours_is_reported_not_reverted() {
        let dir = tempfile::tempdir().unwrap();
        let full = dir.path().join("a.rs");
        std::fs::write(&full, "someone else wrote this\n").unwrap();

        let files: HashMap<String, TouchedFile> = HashMap::from([touched(
            "a.rs",
            "before the batch\n",
            "the batch wrote this\n",
        )]);
        let note = roll_back_prior_writes(&rooted(&dir), &files, &["a.rs".to_string()]).await;

        assert!(note.contains("NOT ROLLED BACK"), "{note}");
        assert!(note.contains("a.rs"), "{note}");
        assert_eq!(
            std::fs::read_to_string(&full).unwrap(),
            "someone else wrote this\n",
            "the rollback must not revert a write this batch does not own"
        );
    }

    /// The other side of the same rule: bytes still matching what the batch
    /// wrote are the batch's to undo, and the note stays empty.
    #[tokio::test]
    async fn the_rollback_restores_a_file_the_batch_still_owns() {
        let dir = tempfile::tempdir().unwrap();
        let full = dir.path().join("a.rs");
        std::fs::write(&full, "the batch wrote this\n").unwrap();

        let files: HashMap<String, TouchedFile> = HashMap::from([touched(
            "a.rs",
            "before the batch\n",
            "the batch wrote this\n",
        )]);
        let note = roll_back_prior_writes(&rooted(&dir), &files, &["a.rs".to_string()]).await;

        assert!(note.is_empty(), "{note}");
        assert_eq!(
            std::fs::read_to_string(&full).unwrap(),
            "before the batch\n"
        );
    }

    /// A file we cannot read tells us nothing about who owns it, so the
    /// all-or-nothing contract wins and it is restored.
    #[tokio::test]
    async fn an_unreadable_file_is_still_rolled_back() {
        let dir = tempfile::tempdir().unwrap();
        let full = dir.path().join("vanished.rs");

        let files: HashMap<String, TouchedFile> = HashMap::from([touched(
            "vanished.rs",
            "before the batch\n",
            "the batch wrote this\n",
        )]);
        let note =
            roll_back_prior_writes(&rooted(&dir), &files, &["vanished.rs".to_string()]).await;

        assert!(note.is_empty(), "{note}");
        assert_eq!(
            std::fs::read_to_string(&full).unwrap(),
            "before the batch\n"
        );
    }
}
