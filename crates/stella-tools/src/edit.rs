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
//! The ledger is also what the write is held to. The bytes this tool reads at
//! the top of a call are a snapshot, and the file it writes at the end is
//! computed from that snapshot, so anything written in between would be
//! replaced by bytes that never contained it. The crate's `recheck` module
//! re-reads the file through the same descriptor immediately before the write
//! and refuses rather than clobber a change nobody would ever see again.
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
    /// A test's window between the read and the write — see the `recheck`
    /// module's `Seam`. Nothing outside `cfg(test)` can install one.
    #[cfg(test)]
    seam: Option<crate::recheck::Seam>,
}

impl EditFile {
    /// Construct sharing the registry's read-state ledger, so match failures
    /// can be attributed against what the model last saw.
    pub fn with_ledger(ledger: Arc<ReadLedger>) -> Self {
        Self {
            ledger,
            #[cfg(test)]
            seam: None,
        }
    }

    /// Construct with `seam` running in the window between the read and the
    /// write, so a test can put a concurrent writer there.
    #[cfg(test)]
    pub(crate) fn with_seam(ledger: Arc<ReadLedger>, seam: crate::recheck::Seam) -> Self {
        Self {
            ledger,
            seam: Some(seam),
        }
    }

    /// Hand a test its window. Compiles to nothing in a shipped build.
    fn run_seam(&self) {
        #[cfg(test)]
        if let Some(seam) = &self.seam {
            seam();
        }
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
            return ToolOutput::classified_error(
                stella_protocol::ErrorClass::InvalidInput,
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
            Err(refusal) => {
                return ToolOutput::classified_error(
                    stella_protocol::ErrorClass::PermissionDenied,
                    refusal.to_string(),
                );
            }
        };
        let path = path.as_str();

        // One held root descriptor for both halves of the edit: the read below
        // and the write at the end walk the same descriptors rather than
        // resolving `path` twice against a filesystem that can move under
        // them (#938).
        let handle = match crate::rootfd::RootHandle::open(&scope_root) {
            Ok(handle) => std::sync::Arc::new(handle),
            Err(e) => {
                return ToolOutput::classified_error(
                    stella_protocol::ErrorClass::Environment,
                    format!("cannot open workspace root: {e}"),
                );
            }
        };

        let content = match crate::rootfd::read_to_string_async(&handle, path).await {
            Ok(c) => c,
            Err(e) if e.is_escape() => {
                return ToolOutput::classified_error(
                    stella_protocol::ErrorClass::PermissionDenied,
                    format!("path `{path}` escapes workspace root ({e})"),
                );
            }
            Err(e) => {
                return ToolOutput::classified_error(
                    stella_protocol::ErrorClass::Environment,
                    format!("failed to read `{path}`: {e}"),
                );
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
            return match self.ledger.last_seen_sha(&scope_root, path) {
                Some(seen) if seen != current_sha => {
                    // Drift: the file changed after the model last saw it.
                    // Echo the fresh content so the model can re-issue the
                    // edit without a round-trip — and record it as seen, so
                    // a repeat failure against these same bytes is reported
                    // as unchanged (not re-attributed as drift forever).
                    // The echo is capped, so the recorded hash may cover
                    // more than was shown; the unchanged-file message below
                    // still steers a confused model back to read_file.
                    self.ledger.record_known(&scope_root, path, &content);
                    ToolOutput::classified_error(
                        stella_protocol::ErrorClass::NotFound,
                        format!(
                            "old_string not found in `{path}` — the file CHANGED after you last \
                             read it (out-of-band modification); the copy in your context is \
                             stale. Current content follows — re-issue the edit against these \
                             bytes.\n\n--- {path} (current) ---\n{}",
                            drift_echo(&content)
                        ),
                    )
                }
                Some(_) => match indentation_only_match(&content, old_string) {
                    // The needle is right and only its indentation is wrong.
                    // The generic message below is accurate but costs a ranged
                    // re-read to act on; naming the cause and echoing the
                    // file's own bytes for the span removes that round trip.
                    Some(actual) => ToolOutput::classified_error(
                        stella_protocol::ErrorClass::NotFound,
                        format!(
                            "old_string not found in `{path}` — but the same text IS present with \
                         different leading whitespace, so the needle was re-indented. The file \
                         is unchanged since you last saw it. Copy this span byte-exact:\n\n--- \
                         {path} (actual indentation) ---\n{actual}"
                        ),
                    ),
                    None => ToolOutput::classified_error(
                        stella_protocol::ErrorClass::NotFound,
                        format!(
                            "old_string not found in `{path}` — the file is unchanged since you last \
                             saw it, so the copy in your context matches disk; check for exact \
                             whitespace/newline differences"
                        ),
                    ),
                },
                None => ToolOutput::classified_error(
                    stella_protocol::ErrorClass::NotFound,
                    format!(
                        "old_string not found in `{path}` — no read of this file is recorded \
                         this session; read it first and copy old_string byte-exact"
                    ),
                ),
            };
        };
        let count = content.matches(old_string).count();
        if count > 1 && !replace_all {
            return ToolOutput::classified_error(
                stella_protocol::ErrorClass::InvalidInput,
                format!(
                    "old_string appears {count} times in `{path}` — set replace_all=true or provide a more specific string"
                ),
            );
        }

        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };

        // The last look. `new_content` is the whole file computed from
        // the snapshot read at the top of this call, so a write that landed in
        // between is about to be replaced by bytes that never contained it.
        self.run_seam();
        let read_sha = crate::staleness::hex_sha256(content.as_bytes());
        if let Err(drift) = crate::recheck::confirm(&handle, path, &read_sha).await {
            let echo = match drift.fresh() {
                Some(fresh) => {
                    // Recorded as seen for the reason the miss path records it:
                    // the model has now been shown these bytes, so a later
                    // failure against them is reported as unchanged rather than
                    // re-attributed as drift forever.
                    self.ledger.record_known(&scope_root, path, fresh);
                    format!(
                        "\n\nCurrent content follows — re-issue the edit against these \
                         bytes.\n\n--- {path} (current) ---\n{}",
                        drift_echo(fresh)
                    )
                }
                None => String::new(),
            };
            return ToolOutput::classified_error(
                stella_protocol::ErrorClass::RefusedByPolicy,
                format!(
                    "refusing to write `{path}` — {} between the read and the write, so this \
                     edit was computed from bytes disk does not hold and writing it would \
                     destroy that change. Nothing was written.{echo}",
                    drift.because()
                ),
            );
        }

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
                self.ledger.record_known(&scope_root, path, &new_content);
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
            Err(e) => ToolOutput::classified_error(
                stella_protocol::ErrorClass::Environment,
                format!("failed to write `{path}`: {e}"),
            ),
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
                    return ToolOutput::classified_error(
                        stella_protocol::ErrorClass::PermissionDenied,
                        format!(
                            "`{EDITS_KEY}`[{index}] (`{}`): {refusal} — nothing was written",
                            target.path
                        ),
                    );
                }
            };
            if target.old_string.is_empty() {
                return ToolOutput::classified_error(
                    stella_protocol::ErrorClass::InvalidInput,
                    format!(
                        "`{EDITS_KEY}`[{index}] (`{path}`): old_string must not be empty — use \
                     write_file to create or replace a whole file. Nothing was written."
                    ),
                );
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
                            return ToolOutput::classified_error(
                                stella_protocol::ErrorClass::Environment,
                                format!("cannot open workspace root: {e}"),
                            );
                        }
                    };
                    let content = match crate::rootfd::read_to_string_async(&handle, &path).await {
                        Ok(content) => content,
                        Err(e) => {
                            return ToolOutput::classified_error(
                                stella_protocol::ErrorClass::Environment,
                                format!(
                                    "`{EDITS_KEY}`[{index}]: failed to read `{path}`: {e} — nothing \
                                 was written"
                                ),
                            );
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
                return ToolOutput::classified_error(
                    stella_protocol::ErrorClass::NotFound,
                    self.batch_miss(&pending[slot], index, old_string),
                );
            }
            let count = current.matches(old_string).count();
            if count > 1 && !target.replace_all {
                return ToolOutput::classified_error(
                    stella_protocol::ErrorClass::InvalidInput,
                    format!(
                        "`{EDITS_KEY}`[{index}]: old_string appears {count} times in `{}` — set \
                     replace_all=true or provide a more specific string. Nothing was written.",
                        pending[slot].path
                    ),
                );
            }
            pending[slot].content = if target.replace_all {
                current.replace(old_string, new_string)
            } else {
                current.replacen(old_string, new_string, 1)
            };
            pending[slot].edits += 1;
        }

        // Every edit validated against the composed content. Before anything
        // reaches the disk, confirm every file still holds the bytes this batch
        // read — all of them first, because a batch that would clobber
        // a concurrent write to its third file must not have written its first.
        self.run_seam();
        let mut handles = Vec::with_capacity(pending.len());
        for file in &pending {
            let handle = match crate::rootfd::RootHandle::open(&file.scope_root) {
                Ok(handle) => Arc::new(handle),
                Err(e) => {
                    return ToolOutput::classified_error(
                        stella_protocol::ErrorClass::Environment,
                        format!("cannot open workspace root: {e}"),
                    );
                }
            };
            let read_sha = crate::staleness::hex_sha256(file.original.as_bytes());
            if let Err(drift) = crate::recheck::confirm(&handle, &file.path, &read_sha).await {
                return ToolOutput::classified_error(
                    stella_protocol::ErrorClass::RefusedByPolicy,
                    format!(
                        "refusing to write `{}` — {} between the read and the write, so this \
                         batch was composed from bytes disk does not hold. Nothing was \
                         written — re-read it and re-issue the batch.",
                        file.path,
                        drift.because()
                    ),
                );
            }
            handles.push(handle);
        }

        let mut report = Vec::with_capacity(pending.len());
        let mut changes = Vec::with_capacity(pending.len());
        for (file, handle) in pending.iter().zip(handles) {
            if let Err(e) = crate::durable_write::write_file_durably_at(
                handle,
                file.path.clone(),
                file.content.as_bytes().to_vec(),
                false,
            )
            .await
            {
                return ToolOutput::classified_error(
                    stella_protocol::ErrorClass::Environment,
                    format!("failed to write `{}`: {e}", file.path),
                );
            }
            // The model knows the bytes it just produced — record them so its
            // own edit is never later misattributed as drift.
            self.ledger
                .record_known(&file.scope_root, &file.path, &file.content);
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
    fn batch_miss(&self, file: &Pending, index: usize, old_string: &str) -> String {
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
        match self.ledger.last_seen_sha(&file.scope_root, path) {
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
mod tests;
