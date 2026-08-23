//! `edit_file` — replace an exact substring in a file. Surgical edits, not
//! full rewrites. Supports `replace_all` for multi-occurrence.
//!
//! The tool shares the session's read-state ledger (#331): when `old_string`
//! fails to match, it compares current disk bytes against the hash of what
//! the model last saw (recorded by `read_file` and by the model's own
//! edits/writes) and *attributes* the failure — a drifted file gets a
//! drift-named error carrying the fresh content so the model can re-issue the
//! edit against current bytes, instead of a generic not-found that sends it
//! back into a read→edit-fail thrash. Because the drift echo
//! embeds the changed content, a legitimate recovery never produces
//! byte-identical outputs, so the loop detector (which requires identical
//! outputs to flag a loop) keeps treating it as progress.
//!
//! The success path holds itself to the same contract (#3176): every success
//! string carries the match's byte offset and a short digest of the resulting
//! file, so N distinct edits to one file produce N distinct outputs. A
//! constant `replaced 1 occurrence(s) in {path}` once made seven different,
//! correct edits look byte-identical to that detector, which killed the run
//! as stagnant mid-solve. Both stamps are deterministic — identity, never a
//! timing.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::read::ReadLedger;
use crate::registry::Tool;

/// Ceiling on the fresh-content echo inside a drift-attributed error, so a
/// huge drifted file doesn't flood the context through an error message.
const DRIFT_ECHO_MAX_LINES: usize = 400;

/// Per-line width cap on the echo, mirroring `read_file`'s. A line cap alone
/// is not a bound on context: a drifted minified bundle, a one-line JSON
/// fixture or a generated SQL dump is ONE line of megabytes, sails under
/// [`DRIFT_ECHO_MAX_LINES`], and would land in the transcript whole — through
/// an *error message*, which no caller thinks to budget for.
const DRIFT_ECHO_MAX_LINE_BYTES: usize = 1_000;

/// Ceiling on the whole echo. Many long-but-individually-clipped lines still
/// add up, so the render stops here and says so.
const DRIFT_ECHO_MAX_BYTES: usize = 60_000;

/// Reconcile an LF-newline needle against CRLF file bytes.
///
/// `read_file` renders a file through `str::lines()`, which STRIPS the `\r`
/// of every `\r\n`. A model that copies two or more lines out of that render
/// into `old_string` therefore hands back a needle whose newlines are bare
/// `\n` — bytes that occur nowhere in a CRLF file. Every multi-line edit of a
/// CRLF file was consequently impossible, and the tool blamed the model
/// ("check for exact whitespace/newline differences") for a round trip it had
/// broken itself.
///
/// When the literal needle misses and the file is CRLF, retry with the
/// needle's `\n` promoted to `\r\n`. The replacement is promoted with it, so
/// the edit keeps the file's own convention instead of splicing LF islands
/// into a CRLF file — which is what a naive "normalize everything to LF" fix
/// would do, and it would show up as a whole-file diff in the user's next
/// `git status`.
///
/// `None` whenever the literal needle already matches, the file is not CRLF,
/// the needle is single-line, the needle already carries `\r`, or the
/// promoted needle still does not occur — so every previously-working call is
/// byte-identical.
pub(crate) fn crlf_promoted(content: &str, old: &str, new: &str) -> Option<(String, String)> {
    if !old.contains('\n') || old.contains('\r') || !content.contains("\r\n") {
        return None;
    }
    if content.contains(old) {
        return None;
    }
    let promoted_old = old.replace('\n', "\r\n");
    if !content.contains(&promoted_old) {
        return None;
    }
    let promoted_new = if new.contains('\r') {
        new.to_string()
    } else {
        new.replace('\n', "\r\n")
    };
    Some((promoted_old, promoted_new))
}

/// Lines of a needle that missed, but whose only difference from the file is
/// leading whitespace — returned as the file's own bytes for that span.
///
/// The most common shape of an "unchanged file, still no match" miss, and the
/// one the generic message cannot resolve without a ranged re-read: a needle
/// copied out of a nested context and re-indented by a few spaces, or copied
/// from a `read_file` render whose line prefix was trimmed off unevenly. The
/// literal comparison is right to fail — an edit must be byte-exact — but the
/// tool knows *why* it failed and can say so.
///
/// Deliberately narrow, so this can never claim a match the real edit would
/// not have made:
///
/// - Every line must be equal after stripping leading whitespace **only**.
///   Trailing whitespace still counts, because it is a real difference the
///   model must reproduce and one that a "check whitespace" message covers.
/// - The first matching window wins and a second one yields `None`. An
///   ambiguous span would send the model to re-issue against the wrong copy,
///   which is worse than the generic message.
/// - Bounded by [`INDENT_HINT_MAX_LINES`]: this is a hint inside an error, not
///   a file echo, and a huge needle is not the confusion this diagnoses.
fn indentation_only_match(content: &str, needle: &str) -> Option<String> {
    let needle_lines: Vec<&str> = needle.lines().collect();
    if needle_lines.is_empty() || needle_lines.len() > INDENT_HINT_MAX_LINES {
        return None;
    }
    // A single line with no leading whitespace of its own cannot be an
    // indentation miss: there is nothing to have got wrong.
    let trimmed: Vec<&str> = needle_lines
        .iter()
        .map(|l| l.trim_start_matches([' ', '\t']))
        .collect();
    if trimmed.iter().all(|l| l.is_empty()) {
        return None;
    }

    let content_lines: Vec<&str> = content.lines().collect();
    let mut found: Option<String> = None;
    for window in content_lines.windows(needle_lines.len()) {
        let matches = window
            .iter()
            .zip(&trimmed)
            .all(|(actual, want)| actual.trim_start_matches([' ', '\t']) == *want);
        if !matches {
            continue;
        }
        if found.is_some() {
            // Ambiguous — say nothing rather than point at the wrong span.
            return None;
        }
        found = Some(window.join("\n"));
    }
    found
}

/// Ceiling on the needle this hint will diagnose. A long needle that misses is
/// unlikely to be a pure indentation slip, and the hint has to stay small
/// enough to belong inside an error message.
const INDENT_HINT_MAX_LINES: usize = 40;

#[derive(Default)]
pub struct EditFile {
    ledger: Arc<ReadLedger>,
}

impl EditFile {
    /// Construct sharing the registry's read-state ledger, so match failures
    /// can be attributed against what the model last saw.
    pub fn with_ledger(ledger: Arc<ReadLedger>) -> Self {
        Self { ledger }
    }
}

/// Render the fresh content echoed inside a drift-attributed error:
/// line-numbered like `read_file` output (so the model can re-anchor edits),
/// bounded on all three axes `read_file` bounds — lines
/// ([`DRIFT_ECHO_MAX_LINES`]), per-line width
/// ([`DRIFT_ECHO_MAX_LINE_BYTES`]) and total payload
/// ([`DRIFT_ECHO_MAX_BYTES`]) — each elision loud, so a capped echo can never
/// be mistaken for the whole file.
fn drift_echo(content: &str) -> String {
    use std::fmt::Write as _;

    let lines: Vec<&str> = content.lines().collect();
    let mut numbered = String::new();
    let mut shown = 0usize;
    let mut stopped_at_byte_cap = false;
    for (i, line) in lines.iter().take(DRIFT_ECHO_MAX_LINES).enumerate() {
        if numbered.len() >= DRIFT_ECHO_MAX_BYTES {
            stopped_at_byte_cap = true;
            break;
        }
        if line.len() <= DRIFT_ECHO_MAX_LINE_BYTES {
            let _ = writeln!(numbered, "{:>6}\t{line}", i + 1);
        } else {
            // Char-boundary-safe: byte slicing would panic mid-UTF-8 on a
            // long non-ASCII line, and the drifted content is not ours.
            let head = crate::exec::truncate_preview(line, DRIFT_ECHO_MAX_LINE_BYTES);
            let elided = line.len() - head.len();
            let _ = writeln!(numbered, "{:>6}\t{head}[… {elided} bytes elided …]", i + 1);
        }
        shown += 1;
    }
    if shown < lines.len() {
        let why = if stopped_at_byte_cap {
            format!(
                " — stopped at the {} KB echo cap",
                DRIFT_ECHO_MAX_BYTES / 1024
            )
        } else {
            String::new()
        };
        let _ = writeln!(
            numbered,
            "(first {shown} of {} lines{why} — use read_file for the rest)",
            lines.len()
        );
    }
    numbered
}

/// The plural key: several edits, applied as one all-or-nothing change.
const EDITS_KEY: &str = "edits";

/// One replacement — the unit both spellings of an `edit_file` call reduce to.
struct EditTarget {
    path: String,
    old_string: String,
    new_string: String,
    replace_all: bool,
}

/// Parse one edit. Shared by the single form and by every element of `edits`.
fn edit_target(value: &Value) -> Result<EditTarget, crate::input::InputError> {
    Ok(EditTarget {
        path: crate::input::required_str(value, "path")?.to_string(),
        old_string: crate::input::required_str(value, "old_string")?.to_string(),
        new_string: crate::input::required_str(value, "new_string")?.to_string(),
        replace_all: crate::input::optional_bool(value, "replace_all")?.unwrap_or(false),
    })
}

/// One file's in-flight content while a batch is being composed.
struct Pending {
    scope_root: std::path::PathBuf,
    path: String,
    /// The bytes on disk when the batch first loaded this file — the old side
    /// of the change the batch reports once it lands.
    original: String,
    content: String,
    edits: usize,
}

#[async_trait]
impl Tool for EditFile {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "edit_file".into(),
            description: "Replace an exact substring in a file. By default the old_string must appear exactly once; set replace_all to replace every occurrence. To make several edits — in one file or across files — send them in ONE call with `edits`: the whole batch applies or none of it does, and later edits see the earlier ones.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to workspace root" },
                    "old_string": { "type": "string", "description": "Exact text to find" },
                    "new_string": { "type": "string", "description": "Replacement text" },
                    "replace_all": { "type": "boolean", "description": "Replace all occurrences (default false)" },
                    "edits": crate::batch::plural_schema(
                        serde_json::json!({
                            "path": { "type": "string", "description": "File path relative to workspace root" },
                            "old_string": { "type": "string", "description": "Exact text to find" },
                            "new_string": { "type": "string", "description": "Replacement text" },
                            "replace_all": { "type": "boolean", "description": "Replace all occurrences (default false)" }
                        }),
                        &["path", "old_string", "new_string"],
                        "Several edits applied as ONE all-or-nothing change, in order — \
                         two edits to the same file compose, and if any edit fails nothing \
                         is written.",
                    ),
                    "reason": { "type": "string", "description": "Why you are editing this file — recorded in the session's file-touch audit log" },
                    "storage_intent": { "type": "string", "description": "Only when creating a database table/column that the storage gate flagged as similar to an existing one: one sentence of purpose plus why the existing objects don't fit. Recorded in stella.storage.toml." }
                },
                "required": []
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, ctx: &crate::ctx::ToolCtx) -> ToolOutput {
        if crate::batch::is_plural(input, EDITS_KEY) {
            return self.edit_batch(input, ctx).await;
        }
        // The single form runs the original path over the original `input`,
        // untouched: its success string is an identity stamp the stagnation
        // detector keys on (#3176) and its drift attribution is asserted
        // verbatim, so the batch work must not reshape either.
        self.edit_one(input, ctx).await
    }
}

impl EditFile {
    async fn edit_one(&self, input: &Value, ctx: &crate::ctx::ToolCtx) -> ToolOutput {
        let root = ctx.root();
        let path = match crate::input::required_str(input, "path") {
            Ok(v) => v,
            Err(err) => {
                return ToolOutput::from(err);
            }
        };
        let old_string = match crate::input::required_str(input, "old_string") {
            Ok(v) => v,
            Err(err) => {
                return ToolOutput::from(err);
            }
        };
        // An empty `old_string` is destructive: `"".matches("")` reports
        // char_count+1 hits, so the tool would tell the model to set
        // replace_all=true and then `replace("", new)` interleaves `new` at
        // every char boundary — shredding the file (and allocating O(len^2)).
        // On an empty file it would silently overwrite. Refuse it outright.
        if old_string.is_empty() {
            return ToolOutput::error(
                "old_string must not be empty — use write_file to create or replace a \
                          whole file",
            );
        }
        let new_string = match crate::input::required_str(input, "new_string") {
            Ok(v) => v,
            Err(err) => {
                return ToolOutput::from(err);
            }
        };
        let replace_all = input
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Which directory this edit is allowed to land in, decided before
        // anything is opened.
        let (scope_root, path) = match ctx.resolve_for_write(path) {
            Ok(resolved) => resolved,
            Err(refusal) => return ToolOutput::error(refusal.to_string()),
        };
        let path = path.as_str();

        // One held root descriptor for both halves of the edit: the read below
        // and the write at the end walk the same descriptors rather than
        // resolving `path` twice against a filesystem that can move under
        // them (#938).
        let handle = match crate::rootfd::RootHandle::open(&scope_root) {
            Ok(handle) => std::sync::Arc::new(handle),
            Err(e) => {
                return ToolOutput::error(format!("cannot open workspace root: {e}"));
            }
        };

        let content = match crate::rootfd::read_to_string_async(&handle, path).await {
            Ok(c) => c,
            Err(e) if e.is_escape() => {
                return ToolOutput::error(format!("path `{path}` escapes workspace root ({e})"));
            }
            Err(e) => {
                return ToolOutput::error(format!("failed to read `{path}`: {e}"));
            }
        };

        // A needle copied out of `read_file`'s render carries LF newlines even
        // when the file on disk is CRLF — see [`crlf_promoted`].
        let promoted = crlf_promoted(&content, old_string, new_string);
        let (old_string, new_string) = match &promoted {
            Some((old, new)) => (old.as_str(), new.as_str()),
            None => (old_string, new_string),
        };

        // The first match's byte offset is half of the success output's
        // identity stamp (#3176); `find` returning `None` is exactly the
        // zero-match case the attribution below explains.
        let Some(offset) = content.find(old_string) else {
            // Attribute the miss (#331): compare current bytes against what
            // the model last saw. Three distinguishable causes, three
            // different recoveries — a generic not-found forces the model to
            // guess which one it is.
            let current_sha = crate::staleness::hex_sha256(content.as_bytes());
            return match self.ledger.last_seen_sha(root, path) {
                Some(seen) if seen != current_sha => {
                    // Drift: the file changed after the model last saw it.
                    // Echo the fresh content so the model can re-issue the
                    // edit without a round-trip — and record it as seen, so
                    // a repeat failure against these same bytes is reported
                    // as unchanged (not re-attributed as drift forever).
                    // The echo is capped, so the recorded hash may cover
                    // more than was shown; the unchanged-file message below
                    // still steers a confused model back to read_file.
                    self.ledger.record_known(root, path, &content);
                    ToolOutput::error(format!(
                        "old_string not found in `{path}` — the file CHANGED after you last \
                             read it (out-of-band modification); the copy in your context is \
                             stale. Current content follows — re-issue the edit against these \
                             bytes.\n\n--- {path} (current) ---\n{}",
                        drift_echo(&content)
                    ))
                }
                Some(_) => match indentation_only_match(&content, old_string) {
                    // The needle is right and only its indentation is wrong.
                    // The generic message below is accurate but costs a ranged
                    // re-read to act on; naming the cause and echoing the
                    // file's own bytes for the span removes that round trip.
                    Some(actual) => ToolOutput::error(format!(
                        "old_string not found in `{path}` — but the same text IS present with \
                         different leading whitespace, so the needle was re-indented. The file \
                         is unchanged since you last saw it. Copy this span byte-exact:\n\n--- \
                         {path} (actual indentation) ---\n{actual}"
                    )),
                    None => ToolOutput::error(format!(
                        "old_string not found in `{path}` — the file is unchanged since you last \
                             saw it, so the copy in your context matches disk; check for exact \
                             whitespace/newline differences"
                    )),
                },
                None => ToolOutput::error(format!(
                    "old_string not found in `{path}` — no read of this file is recorded \
                         this session; read it first and copy old_string byte-exact"
                )),
            };
        };
        let count = content.matches(old_string).count();
        if count > 1 && !replace_all {
            return ToolOutput::error(format!(
                "old_string appears {count} times in `{path}` — set replace_all=true or provide a more specific string"
            ));
        }

        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };

        match crate::durable_write::write_file_durably_at(
            handle,
            path.to_string(),
            new_content.as_bytes().to_vec(),
            false,
        )
        .await
        {
            Ok(()) => {
                // The model knows the bytes it just produced — record them so
                // its own edit is never later misattributed as drift.
                self.ledger.record_known(root, path, &new_content);
                let replaced = if replace_all { count } else { 1 };
                // The offset and digest are the edit's identity (#3176): the
                // stagnation detector keys on byte-identical tool output, so
                // a constant success string made N distinct edits to one file
                // indistinguishable from a stuck loop. Both stamps are
                // deterministic — never a timestamp, which broke the detector
                // in the opposite direction once.
                let digest = crate::staleness::sha256_8(new_content.as_bytes());
                let change = crate::own_change::own_change(
                    &crate::own_change::workspace_path(root, &scope_root, path),
                    Some(&content),
                    &new_content,
                );
                crate::own_change::attach(
                    ToolOutput::ok(format!(
                        "replaced {replaced} occurrence(s) in {path} at byte {offset} \
                         (file sha256/8 {digest})"
                    )),
                    &[change],
                )
            }
            Err(e) => ToolOutput::error(format!("failed to write `{path}`: {e}")),
        }
    }

    /// Apply several edits as one change: compose every replacement in memory,
    /// and touch the disk only once all of them have landed.
    ///
    /// **All-or-nothing is the whole point.** A `sed -i` chain applies edit 1,
    /// fails edit 2, and leaves a tree that neither the model nor the turn's
    /// diff can describe — the model must now work out which half happened
    /// before it can retry. Here a miss writes nothing, so a failed batch costs
    /// a retry instead of a repair. That is a guarantee the shell cannot offer
    /// at any length, which is what makes this the better tool rather than
    /// merely the sanctioned one.
    ///
    /// Edits compose **in order**, so a second edit to a file sees the first.
    /// That is what lets one call rename a symbol and then edit the line that
    /// now mentions it.
    async fn edit_batch(&self, input: &Value, ctx: &crate::ctx::ToolCtx) -> ToolOutput {
        let root = ctx.root();
        let targets = match crate::batch::targets(input, EDITS_KEY, "path", edit_target) {
            Ok(targets) => targets,
            Err(err) => return ToolOutput::from(err),
        };

        let mut pending: Vec<Pending> = Vec::new();
        for (index, target) in targets.iter().enumerate() {
            // Scope is consulted per target. There is no batch-level path for
            // a gate to miss: the plural key changes the arity of this loop
            // and nothing else.
            let (scope_root, path) = match ctx.resolve_for_write(&target.path) {
                Ok(resolved) => resolved,
                Err(refusal) => {
                    return ToolOutput::error(format!(
                        "`{EDITS_KEY}`[{index}] (`{}`): {refusal} — nothing was written",
                        target.path
                    ));
                }
            };
            if target.old_string.is_empty() {
                return ToolOutput::error(format!(
                    "`{EDITS_KEY}`[{index}] (`{path}`): old_string must not be empty — use \
                     write_file to create or replace a whole file. Nothing was written."
                ));
            }

            // One load per file. Every later edit to it composes on the
            // in-memory copy rather than re-reading a file this batch has not
            // written yet.
            let slot = match pending
                .iter()
                .position(|p| p.scope_root == scope_root && p.path == path)
            {
                Some(slot) => slot,
                None => {
                    let handle = match crate::rootfd::RootHandle::open(&scope_root) {
                        Ok(handle) => Arc::new(handle),
                        Err(e) => {
                            return ToolOutput::error(format!("cannot open workspace root: {e}"));
                        }
                    };
                    let content = match crate::rootfd::read_to_string_async(&handle, &path).await {
                        Ok(content) => content,
                        Err(e) => {
                            return ToolOutput::error(format!(
                                "`{EDITS_KEY}`[{index}]: failed to read `{path}`: {e} — nothing \
                                 was written"
                            ));
                        }
                    };
                    pending.push(Pending {
                        scope_root,
                        path,
                        original: content.clone(),
                        content,
                        edits: 0,
                    });
                    pending.len() - 1
                }
            };

            // A needle copied out of `read_file`'s render carries LF newlines
            // even when the file on disk is CRLF — see [`crlf_promoted`].
            let promoted = crlf_promoted(
                &pending[slot].content,
                &target.old_string,
                &target.new_string,
            );
            let (old_string, new_string) = match &promoted {
                Some((old, new)) => (old.as_str(), new.as_str()),
                None => (target.old_string.as_str(), target.new_string.as_str()),
            };

            let current = &pending[slot].content;
            if !current.contains(old_string) {
                return ToolOutput::error(self.batch_miss(root, &pending[slot], index, old_string));
            }
            let count = current.matches(old_string).count();
            if count > 1 && !target.replace_all {
                return ToolOutput::error(format!(
                    "`{EDITS_KEY}`[{index}]: old_string appears {count} times in `{}` — set \
                     replace_all=true or provide a more specific string. Nothing was written.",
                    pending[slot].path
                ));
            }
            pending[slot].content = if target.replace_all {
                current.replace(old_string, new_string)
            } else {
                current.replacen(old_string, new_string, 1)
            };
            pending[slot].edits += 1;
        }

        // Every edit validated against the composed content. Only now does
        // anything reach the disk.
        let mut report = Vec::with_capacity(pending.len());
        let mut changes = Vec::with_capacity(pending.len());
        for file in &pending {
            let handle = match crate::rootfd::RootHandle::open(&file.scope_root) {
                Ok(handle) => Arc::new(handle),
                Err(e) => return ToolOutput::error(format!("cannot open workspace root: {e}")),
            };
            if let Err(e) = crate::durable_write::write_file_durably_at(
                handle,
                file.path.clone(),
                file.content.as_bytes().to_vec(),
                false,
            )
            .await
            {
                return ToolOutput::error(format!("failed to write `{}`: {e}", file.path));
            }
            // The model knows the bytes it just produced — record them so its
            // own edit is never later misattributed as drift.
            self.ledger.record_known(root, &file.path, &file.content);
            report.push(format!(
                "{} — {} edit(s), file sha256/8 {}",
                file.path,
                file.edits,
                crate::staleness::sha256_8(file.content.as_bytes())
            ));
            changes.push(crate::own_change::own_change(
                &crate::own_change::workspace_path(root, &file.scope_root, &file.path),
                Some(&file.original),
                &file.content,
            ));
        }
        // The per-file digests are the batch's identity stamp, for the same
        // reason the single form carries one (#3176): N distinct batches must
        // not produce byte-identical output, or the stagnation detector reads
        // correct work as a stuck loop.
        crate::own_change::attach(
            ToolOutput::ok(format!(
                "applied {} edit(s) across {} file(s), all or nothing:\n{}",
                targets.len(),
                pending.len(),
                report.join("\n")
            )),
            &changes,
        )
    }

    /// Attribute a miss inside a batch, in the vocabulary the single form uses.
    ///
    /// The one thing this must not do is cry drift at its own work: once this
    /// batch has edited a file, the composed content no longer matches what the
    /// ledger last saw, and reporting that as an out-of-band modification would
    /// send the model hunting for a second writer that is itself.
    fn batch_miss(
        &self,
        root: &std::path::Path,
        file: &Pending,
        index: usize,
        old_string: &str,
    ) -> String {
        let path = &file.path;
        let head = format!("`{EDITS_KEY}`[{index}]: old_string not found in `{path}`");
        if let Some(actual) = indentation_only_match(&file.content, old_string) {
            return format!(
                "{head} — but the same text IS present with different leading whitespace, so \
                 the needle was re-indented. Nothing was written. Copy this span \
                 byte-exact:\n\n--- {path} (actual indentation) ---\n{actual}"
            );
        }
        let composed = if file.edits > 0 {
            format!(
                " (an earlier edit in this same batch already changed `{path}`, so match \
                 against the text as that edit left it)"
            )
        } else {
            String::new()
        };
        let current_sha = crate::staleness::hex_sha256(file.content.as_bytes());
        match self.ledger.last_seen_sha(root, path) {
            // Only meaningful before this batch touched the file.
            Some(seen) if seen != current_sha && file.edits == 0 => format!(
                "{head} — the file CHANGED after you last read it (out-of-band modification); \
                 the copy in your context is stale. Nothing was written — re-read it and \
                 re-issue the batch."
            ),
            Some(_) => format!(
                "{head} — the file is otherwise unchanged since you last saw it{composed}; \
                 check for exact whitespace/newline differences. Nothing was written."
            ),
            None => format!(
                "{head} — no read of this file is recorded this session; read it first and \
                 copy old_string byte-exact. Nothing was written."
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare execution context rooted at `root` — every file-tool test
    /// drives the tool through one, since `Tool::execute` takes the
    /// context rather than the bare root path it used to (#3284).
    fn cx(root: impl AsRef<std::path::Path>) -> crate::ctx::ToolCtx {
        crate::ctx::ToolCtx::bare(root.as_ref().to_path_buf())
    }
    use crate::read::ReadFile;

    /// The observed failure this diagnoses: a needle that is byte-correct
    /// except for a missing list indent. The generic "check for exact
    /// whitespace" message is true but costs a ranged re-read to act on.
    #[test]
    fn an_indentation_only_miss_hands_back_the_files_own_bytes() {
        let content = "prose\n   - one\n   - two\nmore\n";
        let hit = indentation_only_match(content, "- one\n- two").expect("the span");
        assert_eq!(
            hit, "   - one\n   - two",
            "the file's indentation, verbatim"
        );
    }

    /// The hint must never claim a match the real edit would not have made.
    #[test]
    fn the_hint_declines_when_it_would_be_a_guess() {
        // Genuinely absent text is not an indentation problem.
        assert_eq!(indentation_only_match("a\nb\n", "- nope"), None);
        // Two candidate spans: pointing at either one could be wrong.
        assert_eq!(indentation_only_match("  x\nsep\n    x\n", "x"), None);
        // Trailing whitespace is a real difference the model must reproduce,
        // and the generic message already covers it.
        assert_eq!(indentation_only_match("  keep  \n", "keep"), None);
        // Nothing to have mis-indented.
        assert_eq!(indentation_only_match("\n\n", "   "), None);
    }

    #[tokio::test]
    async fn replaces_unique_substring() {
        let dir = std::env::temp_dir();
        let path = format!("stella_edit_{}.rs", std::process::id());
        let full = dir.join(&path);
        tokio::fs::write(&full, "fn main() { old }").await.unwrap();

        let result = EditFile::default()
            .execute(
                &serde_json::json!({"path": path, "old_string": "old", "new_string": "new"}),
                &cx(&dir),
            )
            .await;
        match result {
            ToolOutput::Ok { content, .. } => assert!(content.contains("replaced 1")),
            ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
        }
        let after = tokio::fs::read_to_string(&full).await.unwrap();
        assert_eq!(after, "fn main() { new }");
        let _ = tokio::fs::remove_file(&full).await;
    }

    /// The #3176 witness: the stagnation detector keys on byte-identical
    /// tool output, and the old constant success string made every edit to
    /// one file render the same bytes — seven distinct, correct edits were
    /// killed as a stuck loop mid-solve. Two DIFFERENT edits back-to-back
    /// must produce two different success outputs.
    #[tokio::test]
    async fn distinct_edits_produce_distinct_success_outputs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("input.tex"), "very big and very large\n").unwrap();
        let edit = EditFile::default();

        let first = edit
            .execute(
                &serde_json::json!({"path": "input.tex", "old_string": "big", "new_string": "huge"}),
                &cx(dir.path()),
            )
            .await;
        let ToolOutput::Ok { content: first, .. } = first else {
            panic!("expected ok, got: {first:?}");
        };
        let second = edit
            .execute(
                &serde_json::json!({"path": "input.tex", "old_string": "large", "new_string": "vast"}),
                &cx(dir.path()),
            )
            .await;
        let ToolOutput::Ok {
            content: second, ..
        } = second
        else {
            panic!("expected ok, got: {second:?}");
        };

        assert_ne!(
            first, second,
            "two different edits must not render byte-identical output — \
             the stagnation detector keys on repeated identical tool output"
        );
        // The identity is stamped, not incidental: offset and digest are
        // both present, so the guarantee survives edits that happen to
        // share one of the two.
        for output in [&first, &second] {
            assert!(output.contains("at byte "), "offset missing: {output}");
            assert!(
                output.contains("file sha256/8 "),
                "digest missing: {output}"
            );
        }
    }

    #[tokio::test]
    async fn errors_on_multiple_without_replace_all() {
        let dir = std::env::temp_dir();
        let path = format!("stella_edit_multi_{}.rs", std::process::id());
        let full = dir.join(&path);
        tokio::fs::write(&full, "a a a").await.unwrap();

        let result = EditFile::default()
            .execute(
                &serde_json::json!({"path": path, "old_string": "a", "new_string": "b"}),
                &cx(&dir),
            )
            .await;
        assert!(result.is_error());
        let _ = tokio::fs::remove_file(&full).await;
    }

    #[tokio::test]
    async fn replace_all_works() {
        let dir = std::env::temp_dir();
        let path = format!("stella_edit_all_{}.rs", std::process::id());
        let full = dir.join(&path);
        tokio::fs::write(&full, "a a a").await.unwrap();

        let result = EditFile::default()
            .execute(
                &serde_json::json!({"path": path, "old_string": "a", "new_string": "b", "replace_all": true}),
                &cx(&dir),
            )
            .await;
        match result {
            ToolOutput::Ok { content, .. } => assert!(content.contains("replaced 3")),
            ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
        }
        let after = tokio::fs::read_to_string(&full).await.unwrap();
        assert_eq!(after, "b b b");
        let _ = tokio::fs::remove_file(&full).await;
    }

    #[tokio::test]
    async fn not_found_without_a_read_names_the_missing_read() {
        let dir = std::env::temp_dir();
        let path = format!("stella_edit_nf_{}.rs", std::process::id());
        let full = dir.join(&path);
        tokio::fs::write(&full, "hello world").await.unwrap();

        let result = EditFile::default()
            .execute(
                &serde_json::json!({"path": path, "old_string": "xyz", "new_string": "abc"}),
                &cx(&dir),
            )
            .await;
        match result {
            ToolOutput::Error { message, .. } => {
                assert!(message.contains("old_string not found"), "got: {message}");
                assert!(
                    message.contains("no read of this file is recorded"),
                    "an unread file must be attributed as such: {message}"
                );
            }
            ToolOutput::Ok { content, .. } => panic!("expected error, got: {content}"),
        }
        let _ = tokio::fs::remove_file(&full).await;
    }

    /// The #331 witness: read a file, mutate it out-of-band, then edit with
    /// an `old_string` that no longer matches — the error must name the
    /// concurrent change (not the generic not-found) and carry the fresh
    /// content so the model can re-issue the edit without a round-trip.
    #[tokio::test]
    async fn drift_is_attributed_and_fresh_content_echoed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "original contents\n").unwrap();
        let ledger = Arc::new(ReadLedger::default());
        let read = ReadFile::with_ledger(ledger.clone());
        let edit = EditFile::with_ledger(ledger.clone());

        let seen = read
            .execute(&serde_json::json!({"path": "a.rs"}), &cx(dir.path()))
            .await;
        assert!(!seen.is_error(), "{seen:?}");

        // Out-of-band change (another process, the user, a subagent).
        std::fs::write(dir.path().join("a.rs"), "rewritten elsewhere\n").unwrap();

        let result = edit
            .execute(
                &serde_json::json!({"path": "a.rs", "old_string": "original", "new_string": "x"}),
                &cx(dir.path()),
            )
            .await;
        match result {
            ToolOutput::Error { message, .. } => {
                assert!(
                    message.contains("CHANGED after you last read it"),
                    "drift must be attributed: {message}"
                );
                assert!(
                    message.contains("rewritten elsewhere"),
                    "fresh content must be echoed: {message}"
                );
            }
            ToolOutput::Ok { content, .. } => panic!("expected drift error, got: {content}"),
        }

        // The echo counts as seen: a repeat failure against the SAME bytes is
        // reported as unchanged, not re-attributed as drift forever.
        let repeat = edit
            .execute(
                &serde_json::json!({"path": "a.rs", "old_string": "original", "new_string": "x"}),
                &cx(dir.path()),
            )
            .await;
        match repeat {
            ToolOutput::Error { message, .. } => {
                assert!(
                    message.contains("unchanged since you last saw it"),
                    "got: {message}"
                );
            }
            ToolOutput::Ok { content, .. } => panic!("expected error, got: {content}"),
        }

        // And the recovery works: an edit against current bytes succeeds.
        let recovered = edit
            .execute(
                &serde_json::json!({"path": "a.rs", "old_string": "rewritten", "new_string": "fixed"}),
                &cx(dir.path()),
            )
            .await;
        assert!(!recovered.is_error(), "{recovered:?}");
    }

    #[tokio::test]
    async fn unchanged_file_failure_is_not_drift_attributed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "hello world\n").unwrap();
        let ledger = Arc::new(ReadLedger::default());
        let read = ReadFile::with_ledger(ledger.clone());
        let edit = EditFile::with_ledger(ledger.clone());

        let seen = read
            .execute(&serde_json::json!({"path": "a.rs"}), &cx(dir.path()))
            .await;
        assert!(!seen.is_error());

        let result = edit
            .execute(
                &serde_json::json!({"path": "a.rs", "old_string": "helo world", "new_string": "x"}),
                &cx(dir.path()),
            )
            .await;
        match result {
            ToolOutput::Error { message, .. } => {
                assert!(
                    message.contains("unchanged since you last saw it"),
                    "an unchanged file must not be blamed on drift: {message}"
                );
                assert!(
                    message.contains("whitespace/newline"),
                    "the classic hint stays: {message}"
                );
            }
            ToolOutput::Ok { content, .. } => panic!("expected error, got: {content}"),
        }
    }

    #[tokio::test]
    async fn own_successful_edit_is_not_later_misattributed_as_drift() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "one two three\n").unwrap();
        let ledger = Arc::new(ReadLedger::default());
        let read = ReadFile::with_ledger(ledger.clone());
        let edit = EditFile::with_ledger(ledger.clone());

        let seen = read
            .execute(&serde_json::json!({"path": "a.rs"}), &cx(dir.path()))
            .await;
        assert!(!seen.is_error());

        // The model's own edit changes the file relative to the read…
        let first = edit
            .execute(
                &serde_json::json!({"path": "a.rs", "old_string": "two", "new_string": "2"}),
                &cx(dir.path()),
            )
            .await;
        assert!(!first.is_error(), "{first:?}");

        // …but a subsequent bad old_string is the model's mistake, not drift.
        let second = edit
            .execute(
                &serde_json::json!({"path": "a.rs", "old_string": "bogus", "new_string": "x"}),
                &cx(dir.path()),
            )
            .await;
        match second {
            ToolOutput::Error { message, .. } => {
                assert!(
                    message.contains("unchanged since you last saw it"),
                    "own edits must update the seen hash: {message}"
                );
            }
            ToolOutput::Ok { content, .. } => panic!("expected error, got: {content}"),
        }
    }

    /// The CRLF round trip. `read_file` renders a Windows-line-ending file
    /// through `str::lines()`, which strips the `\r`; a multi-line
    /// `old_string` copied out of that render matched nothing on disk, so
    /// EVERY multi-line edit of a CRLF file was impossible — and the tool
    /// blamed the model's whitespace for it. The edit must land, and the file
    /// must still be CRLF afterwards (an LF island would show up as a
    /// whole-file diff in the user's next `git status`).
    #[tokio::test]
    async fn a_multi_line_edit_of_a_crlf_file_lands_and_keeps_crlf() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("win.rs"), "fn a() {\r\n    old();\r\n}\r\n").unwrap();
        let ledger = Arc::new(ReadLedger::default());
        let read = ReadFile::with_ledger(ledger.clone());
        let edit = EditFile::with_ledger(ledger.clone());

        let seen = read
            .execute(&serde_json::json!({"path": "win.rs"}), &cx(dir.path()))
            .await;
        let ToolOutput::Ok { content, .. } = seen else {
            panic!("expected ok, got: {seen:?}");
        };
        assert!(
            !content.contains('\r'),
            "the render the model copies from has no CR: {content:?}"
        );

        // Exactly what a model copies back out of that render.
        let out = edit
            .execute(
                &serde_json::json!({
                    "path": "win.rs",
                    "old_string": "fn a() {\n    old();",
                    "new_string": "fn a() {\n    new();",
                }),
                &cx(dir.path()),
            )
            .await;
        assert!(!out.is_error(), "{out:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("win.rs")).unwrap(),
            "fn a() {\r\n    new();\r\n}\r\n",
            "the file keeps its CRLF convention"
        );
    }

    #[test]
    fn crlf_promotion_fires_only_where_it_is_needed() {
        // An LF file needs nothing.
        assert_eq!(crlf_promoted("a\nb\n", "a\nb", "x"), None);
        // A single-line needle matches inside a CRLF line already.
        assert_eq!(crlf_promoted("a\r\nb\r\n", "b", "x"), None);
        // A needle that already carries CR is the model's own bytes — leave it.
        assert_eq!(crlf_promoted("a\r\nb\r\n", "a\r\nb", "x"), None);
        // A needle that is simply absent stays absent (a real not-found).
        assert_eq!(crlf_promoted("a\r\nb\r\n", "zz\nqq", "x"), None);
        // The one case that fires — and the replacement is promoted with it.
        assert_eq!(
            crlf_promoted("a\r\nb\r\n", "a\nb", "p\nq"),
            Some(("a\r\nb".to_string(), "p\r\nq".to_string()))
        );
    }

    /// The line cap alone never saw this file: 4 MB on ONE line is one line,
    /// so it sailed under `DRIFT_ECHO_MAX_LINES` and the whole bundle went to
    /// the model inside an *error message* — the one payload nobody budgets
    /// for. The width cap must clip it, loudly.
    #[test]
    fn drift_echo_clips_a_pathologically_long_line() {
        let echo = drift_echo(&"x".repeat(4 * 1024 * 1024));
        assert!(
            echo.len() < 8 * 1024,
            "a 4 MB one-liner must not be echoed whole (got {} bytes)",
            echo.len()
        );
        assert!(echo.contains("bytes elided"), "elision is loud: {echo}");
    }

    /// Many individually-clipped long lines still add up — the render stops
    /// at the total cap and says which cap it hit.
    #[test]
    fn drift_echo_stops_at_the_total_byte_cap() {
        let body: String = std::iter::repeat_n("y".repeat(4096), 300)
            .collect::<Vec<_>>()
            .join("\n");
        let echo = drift_echo(&body);
        assert!(
            echo.len() < DRIFT_ECHO_MAX_BYTES + 4096,
            "echo stays under the ceiling (got {} bytes)",
            echo.len()
        );
        assert!(
            echo.contains("echo cap"),
            "the footer names the cap: {echo}"
        );
    }

    #[tokio::test]
    async fn drift_echo_is_capped_for_huge_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("big.txt"), "seed\n").unwrap();
        let ledger = Arc::new(ReadLedger::default());
        let read = ReadFile::with_ledger(ledger.clone());
        let edit = EditFile::with_ledger(ledger.clone());

        let seen = read
            .execute(&serde_json::json!({"path": "big.txt"}), &cx(dir.path()))
            .await;
        assert!(!seen.is_error());

        let big: String = (1..=1000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.path().join("big.txt"), &big).unwrap();

        let result = edit
            .execute(
                &serde_json::json!({"path": "big.txt", "old_string": "seed", "new_string": "x"}),
                &cx(dir.path()),
            )
            .await;
        match result {
            ToolOutput::Error { message, .. } => {
                assert!(message.contains("CHANGED after you last read it"));
                assert!(message.contains("line 400"), "echo shows the cap window");
                assert!(
                    !message.contains("line 401"),
                    "echo must stop at the cap: {}",
                    &message[message.len().saturating_sub(200)..]
                );
                assert!(
                    message.contains("first 400 of 1000 lines"),
                    "truncation must be named: {message}"
                );
            }
            ToolOutput::Ok { content, .. } => panic!("expected drift error, got: {content}"),
        }
    }

    // ── batching (#4151) ──────────────────────────────────────────────────

    /// One call, several edits, across more than one file.
    #[tokio::test]
    async fn one_call_applies_several_edits_across_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "let a = 1;\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "let b = 2;\n").unwrap();

        let out = EditFile::default()
            .execute(
                &serde_json::json!({"edits": [
                    {"path": "a.rs", "old_string": "1", "new_string": "10"},
                    {"path": "b.rs", "old_string": "2", "new_string": "20"}
                ]}),
                &cx(dir.path()),
            )
            .await;
        assert!(!out.is_error(), "{out:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "let a = 10;\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.rs")).unwrap(),
            "let b = 20;\n"
        );
    }

    /// **The guarantee `sed -i` cannot offer at any length.**
    ///
    /// A shell chain applies edit 1, fails edit 2, and leaves a tree neither
    /// the model nor the turn's diff can describe — the model has to work out
    /// which half happened before it can retry. Here a miss anywhere writes
    /// nothing, so a failed batch costs a retry rather than a repair.
    #[tokio::test]
    async fn a_batch_that_fails_midway_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "let a = 1;\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "let b = 2;\n").unwrap();

        let out = EditFile::default()
            .execute(
                &serde_json::json!({"edits": [
                    {"path": "a.rs", "old_string": "1", "new_string": "10"},
                    {"path": "b.rs", "old_string": "NOT PRESENT", "new_string": "x"}
                ]}),
                &cx(dir.path()),
            )
            .await;
        let ToolOutput::Error { message, .. } = out else {
            panic!("a batch with an unmatchable edit must fail: {out:?}");
        };
        assert!(message.contains("[1]"), "names the failing edit: {message}");
        assert!(message.contains("Nothing was written"), "{message}");
        // The first edit validated cleanly and must STILL not be on disk.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "let a = 1;\n",
            "the edit that would have succeeded must not have landed"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.rs")).unwrap(),
            "let b = 2;\n"
        );
    }

    /// Two edits to one file compose in order, so the second sees the first.
    /// That is what lets a single call rename a symbol and then edit the line
    /// that now mentions it.
    #[tokio::test]
    async fn two_edits_to_one_file_compose_in_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn old_name() {}\n").unwrap();

        let out = EditFile::default()
            .execute(
                &serde_json::json!({"edits": [
                    {"path": "a.rs", "old_string": "old_name", "new_string": "new_name"},
                    {"path": "a.rs", "old_string": "fn new_name() {}", "new_string": "pub fn new_name() {}"}
                ]}),
                &cx(dir.path()),
            )
            .await;
        assert!(
            !out.is_error(),
            "the second edit must see the first: {out:?}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "pub fn new_name() {}\n"
        );
    }

    /// A batch must not cry drift at its own work: once an earlier edit has
    /// changed the file, the composed content no longer matches the ledger, and
    /// calling that an out-of-band modification sends the model hunting for a
    /// second writer that is itself.
    #[tokio::test]
    async fn a_batch_does_not_report_its_own_earlier_edit_as_drift() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "let a = 1;\n").unwrap();
        let ledger = Arc::new(ReadLedger::default());
        ledger.record_known(dir.path(), "a.rs", "let a = 1;\n");

        let out = EditFile::with_ledger(ledger)
            .execute(
                &serde_json::json!({"edits": [
                    {"path": "a.rs", "old_string": "1", "new_string": "10"},
                    {"path": "a.rs", "old_string": "NOT PRESENT", "new_string": "x"}
                ]}),
                &cx(dir.path()),
            )
            .await;
        let ToolOutput::Error { message, .. } = out else {
            panic!("expected the second edit to miss: {out:?}");
        };
        assert!(
            !message.contains("out-of-band"),
            "the batch's own edit must not be reported as drift: {message}"
        );
        assert!(
            message.contains("earlier edit in this same batch"),
            "the real cause has to be named: {message}"
        );
    }
}
