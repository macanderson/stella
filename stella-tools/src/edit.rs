//! `edit_file` — replace an exact substring in a file. Surgical edits, not
//! full rewrites. Supports `replace_all` for multi-occurrence.
//!
//! The tool shares the session's read-state ledger (#331): when `old_string`
//! fails to match, it compares current disk bytes against the hash of what
//! the model last saw (recorded by `read_file`/`read_symbol` and by the
//! model's own edits/writes) and *attributes* the failure — a drifted file
//! gets a drift-named error carrying the fresh content so the model can
//! re-issue the edit against current bytes, instead of a generic not-found
//! that sends it back into a read→edit-fail thrash. Because the drift echo
//! embeds the changed content, a legitimate recovery never produces
//! byte-identical outputs, so the loop detector (which requires identical
//! outputs to flag a loop) keeps treating it as progress.

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

#[async_trait]
impl Tool for EditFile {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "edit_file".into(),
            description: "Replace an exact substring in a file. By default the old_string must appear exactly once; set replace_all to replace every occurrence.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to workspace root" },
                    "old_string": { "type": "string", "description": "Exact text to find" },
                    "new_string": { "type": "string", "description": "Replacement text" },
                    "replace_all": { "type": "boolean", "description": "Replace all occurrences (default false)" },
                    "reason": { "type": "string", "description": "Why you are editing this file — recorded in the session's file-touch audit log" },
                    "storage_intent": { "type": "string", "description": "Only when creating a database table/column that the storage gate flagged as similar to an existing one: one sentence of purpose plus why the existing objects don't fit. Recorded in stella.storage.toml." }
                },
                "required": ["path", "old_string", "new_string"]
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, root: &std::path::Path) -> ToolOutput {
        let path = match input.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => {
                return ToolOutput::Error {
                    message: "missing required field `path`".into(),
                };
            }
        };
        let old_string = match input.get("old_string").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => {
                return ToolOutput::Error {
                    message: "missing required field `old_string`".into(),
                };
            }
        };
        // An empty `old_string` is destructive: `"".matches("")` reports
        // char_count+1 hits, so the tool would tell the model to set
        // replace_all=true and then `replace("", new)` interleaves `new` at
        // every char boundary — shredding the file (and allocating O(len^2)).
        // On an empty file it would silently overwrite. Refuse it outright.
        if old_string.is_empty() {
            return ToolOutput::Error {
                message: "old_string must not be empty — use write_file to create or replace a \
                          whole file"
                    .into(),
            };
        }
        let new_string = match input.get("new_string").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => {
                return ToolOutput::Error {
                    message: "missing required field `new_string`".into(),
                };
            }
        };
        let replace_all = input
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // One held root descriptor for both halves of the edit: the read below
        // and the write at the end walk the same descriptors rather than
        // resolving `path` twice against a filesystem that can move under
        // them (#938).
        let handle = match crate::rootfd::RootHandle::open(root) {
            Ok(handle) => std::sync::Arc::new(handle),
            Err(e) => {
                return ToolOutput::Error {
                    message: format!("cannot open workspace root: {e}"),
                };
            }
        };

        let content = match crate::rootfd::read_to_string_async(&handle, path).await {
            Ok(c) => c,
            Err(e) if e.is_escape() => {
                return ToolOutput::Error {
                    message: format!("path `{path}` escapes workspace root ({e})"),
                };
            }
            Err(e) => {
                return ToolOutput::Error {
                    message: format!("failed to read `{path}`: {e}"),
                };
            }
        };

        // A needle copied out of `read_file`'s render carries LF newlines even
        // when the file on disk is CRLF — see [`crlf_promoted`].
        let promoted = crlf_promoted(&content, old_string, new_string);
        let (old_string, new_string) = match &promoted {
            Some((old, new)) => (old.as_str(), new.as_str()),
            None => (old_string, new_string),
        };

        let count = content.matches(old_string).count();
        if count == 0 {
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
                    ToolOutput::Error {
                        message: format!(
                            "old_string not found in `{path}` — the file CHANGED after you last \
                             read it (out-of-band modification); the copy in your context is \
                             stale. Current content follows — re-issue the edit against these \
                             bytes.\n\n--- {path} (current) ---\n{}",
                            drift_echo(&content)
                        ),
                    }
                }
                Some(_) => ToolOutput::Error {
                    message: format!(
                        "old_string not found in `{path}` — the file is unchanged since you last \
                         saw it, so the copy in your context matches disk; check for exact \
                         whitespace/newline differences"
                    ),
                },
                None => ToolOutput::Error {
                    message: format!(
                        "old_string not found in `{path}` — no read of this file is recorded \
                         this session; read it first and copy old_string byte-exact"
                    ),
                },
            };
        }
        if count > 1 && !replace_all {
            return ToolOutput::Error {
                message: format!(
                    "old_string appears {count} times in `{path}` — set replace_all=true or provide a more specific string"
                ),
            };
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
                ToolOutput::Ok {
                    content: format!("replaced {replaced} occurrence(s) in {path}"),
                }
            }
            Err(e) => ToolOutput::Error {
                message: format!("failed to write `{path}`: {e}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read::ReadFile;

    #[tokio::test]
    async fn replaces_unique_substring() {
        let dir = std::env::temp_dir();
        let path = format!("stella_edit_{}.rs", std::process::id());
        let full = dir.join(&path);
        tokio::fs::write(&full, "fn main() { old }").await.unwrap();

        let result = EditFile::default()
            .execute(
                &serde_json::json!({"path": path, "old_string": "old", "new_string": "new"}),
                &dir,
            )
            .await;
        match result {
            ToolOutput::Ok { content } => assert!(content.contains("replaced 1")),
            ToolOutput::Error { message } => panic!("expected ok, got: {message}"),
        }
        let after = tokio::fs::read_to_string(&full).await.unwrap();
        assert_eq!(after, "fn main() { new }");
        let _ = tokio::fs::remove_file(&full).await;
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
                &dir,
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
                &dir,
            )
            .await;
        match result {
            ToolOutput::Ok { content } => assert!(content.contains("replaced 3")),
            ToolOutput::Error { message } => panic!("expected ok, got: {message}"),
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
                &dir,
            )
            .await;
        match result {
            ToolOutput::Error { message } => {
                assert!(message.contains("old_string not found"), "got: {message}");
                assert!(
                    message.contains("no read of this file is recorded"),
                    "an unread file must be attributed as such: {message}"
                );
            }
            ToolOutput::Ok { content } => panic!("expected error, got: {content}"),
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
            .execute(&serde_json::json!({"path": "a.rs"}), dir.path())
            .await;
        assert!(!seen.is_error(), "{seen:?}");

        // Out-of-band change (another process, the user, a subagent).
        std::fs::write(dir.path().join("a.rs"), "rewritten elsewhere\n").unwrap();

        let result = edit
            .execute(
                &serde_json::json!({"path": "a.rs", "old_string": "original", "new_string": "x"}),
                dir.path(),
            )
            .await;
        match result {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("CHANGED after you last read it"),
                    "drift must be attributed: {message}"
                );
                assert!(
                    message.contains("rewritten elsewhere"),
                    "fresh content must be echoed: {message}"
                );
            }
            ToolOutput::Ok { content } => panic!("expected drift error, got: {content}"),
        }

        // The echo counts as seen: a repeat failure against the SAME bytes is
        // reported as unchanged, not re-attributed as drift forever.
        let repeat = edit
            .execute(
                &serde_json::json!({"path": "a.rs", "old_string": "original", "new_string": "x"}),
                dir.path(),
            )
            .await;
        match repeat {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("unchanged since you last saw it"),
                    "got: {message}"
                );
            }
            ToolOutput::Ok { content } => panic!("expected error, got: {content}"),
        }

        // And the recovery works: an edit against current bytes succeeds.
        let recovered = edit
            .execute(
                &serde_json::json!({"path": "a.rs", "old_string": "rewritten", "new_string": "fixed"}),
                dir.path(),
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
            .execute(&serde_json::json!({"path": "a.rs"}), dir.path())
            .await;
        assert!(!seen.is_error());

        let result = edit
            .execute(
                &serde_json::json!({"path": "a.rs", "old_string": "helo world", "new_string": "x"}),
                dir.path(),
            )
            .await;
        match result {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("unchanged since you last saw it"),
                    "an unchanged file must not be blamed on drift: {message}"
                );
                assert!(
                    message.contains("whitespace/newline"),
                    "the classic hint stays: {message}"
                );
            }
            ToolOutput::Ok { content } => panic!("expected error, got: {content}"),
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
            .execute(&serde_json::json!({"path": "a.rs"}), dir.path())
            .await;
        assert!(!seen.is_error());

        // The model's own edit changes the file relative to the read…
        let first = edit
            .execute(
                &serde_json::json!({"path": "a.rs", "old_string": "two", "new_string": "2"}),
                dir.path(),
            )
            .await;
        assert!(!first.is_error(), "{first:?}");

        // …but a subsequent bad old_string is the model's mistake, not drift.
        let second = edit
            .execute(
                &serde_json::json!({"path": "a.rs", "old_string": "bogus", "new_string": "x"}),
                dir.path(),
            )
            .await;
        match second {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("unchanged since you last saw it"),
                    "own edits must update the seen hash: {message}"
                );
            }
            ToolOutput::Ok { content } => panic!("expected error, got: {content}"),
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
            .execute(&serde_json::json!({"path": "win.rs"}), dir.path())
            .await;
        let ToolOutput::Ok { content } = seen else {
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
                dir.path(),
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
            .execute(&serde_json::json!({"path": "big.txt"}), dir.path())
            .await;
        assert!(!seen.is_error());

        let big: String = (1..=1000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.path().join("big.txt"), &big).unwrap();

        let result = edit
            .execute(
                &serde_json::json!({"path": "big.txt", "old_string": "seed", "new_string": "x"}),
                dir.path(),
            )
            .await;
        match result {
            ToolOutput::Error { message } => {
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
            ToolOutput::Ok { content } => panic!("expected drift error, got: {content}"),
        }
    }
}
