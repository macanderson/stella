//! `read_file` — read a file with optional line range, a line cap, and a
//! byte cap. Mirrors the TS `read_file` tool: 1-based line numbers,
//! `offset`/`limit` params, and a cap on the number of lines *returned*
//! (`MAX_LINES`) so a huge file can't flood the context.
//!
//! Lines alone are not a bound on context, though: a minified bundle, a
//! single-line JSON fixture, or a generated SQL dump is one or two "lines"
//! and sails under the line cap. Two byte caps close that: each emitted line
//! is clipped at `MAX_LINE_BYTES` and the whole rendered payload at
//! `MAX_RENDER_BYTES`, both with a loud `[… N bytes elided …]` marker and
//! both named in the trailing footer so the model knows to narrow its range
//! rather than assume it saw the file. A file within `MAX_FILE_BYTES` is
//! still read into memory in full before anything is capped; anything larger
//! is refused from metadata before the load, so a pathologically large file
//! costs a `stat`, not its own size in RAM.
//!
//! The tool also keeps the session's read-state ledger ([`ReadLedger`]): every
//! successful read records a per-file tally (reported in the tool output) and
//! the sha256 of the bytes that were current at read time. The hash is the
//! read→edit drift oracle's baseline (#331): `edit_file` compares it against
//! current disk bytes to attribute a failed match to an out-of-band change
//! instead of a generic not-found. Reads are keyed by the file's normalized
//! workspace-relative path (so `src/./a.rs` and `src/a.rs` count as one
//! file). One ledger lives per registry, shared by `read_file`, `read_symbol`
//! (which reads through this same tool), `edit_file`, and `write_file` — so
//! the ledger tracks the content the model last *saw*, whichever surface
//! showed (or produced) it. The audit-grade equivalent (one `R` event per
//! read) lands in the registry's file-touch ledger.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::registry::Tool;

/// Crate-visible so `read_symbol` (which reads through this tool) can name
/// the cap honestly when a symbol's span exceeds it.
pub(crate) const MAX_LINES: usize = 2000;

/// Per-line width cap. Real source lines are far shorter; the cases this
/// catches are machine-generated (minified JS, base64 blobs, one-line JSON),
/// where the tail of the line carries no more information than its head.
/// Looser than `grep`'s column cap on purpose: `read_file` is the tool you
/// call when you want the file's actual contents.
const MAX_LINE_BYTES: usize = 1_000;

/// Ceiling on the whole rendered payload. 400 KB is ~114k estimated tokens —
/// already more than any single tool result should spend, and the point at
/// which the honest answer is "narrow the range", not "here is everything".
const MAX_RENDER_BYTES: usize = 400 * 1024;

/// Ceiling on the file this tool will load at all.
///
/// Every cap above bounds what reaches the MODEL; none of them bounds what
/// reaches Stella's heap, because the render only happens after the whole
/// file has been read, UTF-8-validated and sha256'd. `offset`/`limit` do not
/// help — a one-line read of a 4 GB database dump paid for all 4 GB. Above
/// this ceiling the honest answer is a named refusal that points at the tools
/// which stream, not a multi-gigabyte allocation followed by a 400 KB answer.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// What one confined open of the target found. The classification travels
/// back from the blocking worker alongside the bytes so the refusals above
/// are decided from the metadata of the descriptor that was actually read,
/// not from a second `stat` of the same name.
enum Loaded {
    Bytes(Vec<u8>),
    Directory,
    TooLarge { bytes: u64 },
}

/// Per-file record of what the model last saw: how many times the file was
/// read, and the sha256 of the file's full content the last time the model
/// saw it (via a read, or via its own successful edit/write).
#[derive(Debug, Clone, Default)]
struct ReadState {
    reads: u64,
    sha256: String,
}

/// Session-scoped ledger of the last file content the model has *seen*.
///
/// Updated by successful reads (`read_file` and `read_symbol`) and by the
/// model's own successful mutations (`edit_file`, `write_file` — the model
/// knows the content it just produced). A mismatch between an entry's hash
/// and current disk bytes therefore means the file changed out-of-band since
/// the model last looked — the read→edit drift signal (#331).
#[derive(Default)]
pub struct ReadLedger {
    states: Mutex<HashMap<String, ReadState>>,
}

impl ReadLedger {
    /// Record one successful read of `path` whose full content was `content`,
    /// returning the new per-file read count.
    pub fn record_read(&self, root: &std::path::Path, path: &str, content: &str) -> u64 {
        let mut states = self.states.lock().unwrap_or_else(|p| p.into_inner());
        let state = states.entry(normalized_key(root, path)).or_default();
        state.reads += 1;
        state.sha256 = crate::staleness::hex_sha256(content.as_bytes());
        state.reads
    }

    /// Record content the model produced or was shown for `path` without
    /// counting a read — a successful `edit_file`/`write_file`, or the fresh
    /// content echoed inside a drift-attributed error.
    pub fn record_known(&self, root: &std::path::Path, path: &str, content: &str) {
        let mut states = self.states.lock().unwrap_or_else(|p| p.into_inner());
        let state = states.entry(normalized_key(root, path)).or_default();
        state.sha256 = crate::staleness::hex_sha256(content.as_bytes());
    }

    /// How many times `path` (under any workspace-relative spelling) has been
    /// successfully read this session.
    pub fn read_count(&self, root: &std::path::Path, path: &str) -> u64 {
        self.states
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&normalized_key(root, path))
            .map(|s| s.reads)
            .unwrap_or(0)
    }

    /// sha256 of the content the model last saw for `path` — `None` when the
    /// file was never read (or written) through the ledger this session.
    pub fn last_seen_sha(&self, root: &std::path::Path, path: &str) -> Option<String> {
        self.states
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&normalized_key(root, path))
            .map(|s| s.sha256.clone())
            .filter(|sha| !sha.is_empty())
    }
}

#[derive(Default)]
pub struct ReadFile {
    ledger: Arc<ReadLedger>,
}

impl ReadFile {
    /// Construct sharing the registry's read-state ledger, so edits and
    /// writes see the hashes this tool records.
    pub fn with_ledger(ledger: Arc<ReadLedger>) -> Self {
        Self { ledger }
    }

    /// How many times `path` (under any workspace-relative spelling) has been
    /// successfully read this session.
    pub fn read_count(&self, root: &std::path::Path, path: &str) -> u64 {
        self.ledger.read_count(root, path)
    }
}

/// The aggregation key for a read: the same normalized workspace-relative
/// path the file-touch ledger uses, falling back to the raw spelling when
/// normalization fails (it can't for a path that resolved and read OK).
fn normalized_key(root: &std::path::Path, path: &str) -> String {
    crate::file_touch::normalize_workspace_path(root, path).unwrap_or_else(|| path.to_string())
}

#[async_trait]
impl Tool for ReadFile {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "read_file".into(),
            description: "Read a file from the workspace. Returns content with 1-based line numbers. Use offset and limit for ranges.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to workspace root" },
                    "offset": { "type": "integer", "description": "1-based start line (optional)" },
                    "limit": { "type": "integer", "description": "Max lines to return (optional; default and ceiling 2000)" },
                    "reason": { "type": "string", "description": "Why you are reading this file — recorded in the session's file-touch audit log" }
                },
                "required": ["path"]
            }),
            read_only: true,
            speculation_safe: true,
        }
    }

    async fn execute(&self, input: &Value, root: &std::path::Path) -> ToolOutput {
        let path = match crate::input::required_str(input, "path") {
            Ok(v) => v,
            Err(message) => return ToolOutput::Error { message },
        };

        let offset = input
            .get("offset")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);
        // MAX_LINES is a ceiling, not just the default: the flood protection
        // the module header promises must hold for explicit limits too.
        let limit = input
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).min(MAX_LINES))
            .unwrap_or(MAX_LINES);

        let handle = match crate::rootfd::RootHandle::open(root) {
            Ok(handle) => std::sync::Arc::new(handle),
            Err(e) => {
                return ToolOutput::Error {
                    message: format!("cannot open workspace root: {e}"),
                };
            }
        };

        // Open once, then classify and load from the descriptor we are holding.
        // A `metadata` by path followed by a `read` by path is two resolutions
        // of the same name, and the second one is what a swapped intermediate
        // redirects — an out-of-root file landing in the transcript (#938).
        let loaded = tokio::task::spawn_blocking({
            let (handle, path) = (std::sync::Arc::clone(&handle), path.to_string());
            move || -> Result<Loaded, crate::rootfd::RootError> {
                use std::io::Read as _;
                let mut file = handle.open_read(&path)?;
                let meta = file.metadata()?;
                // Classify BEFORE loading: a directory and an oversized blob
                // both used to come back as a raw `io::Error` string ("Is a
                // directory (os error 21)"), which tells the model what the
                // syscall thought but not what to do instead — and the blob
                // was already resident by then.
                if meta.is_dir() {
                    return Ok(Loaded::Directory);
                }
                if meta.len() > MAX_FILE_BYTES {
                    return Ok(Loaded::TooLarge { bytes: meta.len() });
                }
                let mut bytes = Vec::with_capacity(meta.len() as usize);
                file.read_to_end(&mut bytes)?;
                Ok(Loaded::Bytes(bytes))
            }
        })
        .await;

        let bytes = match loaded {
            Ok(Ok(Loaded::Bytes(bytes))) => bytes,
            Ok(Ok(Loaded::Directory)) => {
                return ToolOutput::Error {
                    message: format!(
                        "`{path}` is a directory, not a file — list it with \
                         glob({{\"pattern\": \"*\", \"path\": \"{path}\"}})"
                    ),
                };
            }
            Ok(Ok(Loaded::TooLarge { bytes })) => {
                return ToolOutput::Error {
                    message: format!(
                        "`{path}` is {} MB, past read_file's {} MB ceiling — the whole file is \
                         loaded to render any range, so offset/limit would not help. Search it \
                         with grep, or page it with bash (`sed -n '1,200p' {path}`).",
                        bytes / (1024 * 1024),
                        MAX_FILE_BYTES / (1024 * 1024)
                    ),
                };
            }
            Ok(Err(e)) if e.is_escape() => {
                return ToolOutput::Error {
                    message: format!("path `{path}` escapes workspace root ({e})"),
                };
            }
            Ok(Err(e)) => {
                return ToolOutput::Error {
                    message: format!("failed to read `{path}`: {e}"),
                };
            }
            Err(e) => {
                return ToolOutput::Error {
                    message: format!("failed to read `{path}`: {e}"),
                };
            }
        };

        // Bytes then decode, rather than `read_to_string`: the same single
        // UTF-8 validation, but the failure can name the file as binary
        // instead of surfacing "stream did not contain valid UTF-8", which
        // reads to a model as a transient IO problem worth retrying.
        let decoded = String::from_utf8(bytes).map_err(|e| {
            let at = e.utf8_error().valid_up_to();
            let len = e.as_bytes().len();
            format!(
                "`{path}` is not UTF-8 text — it is binary ({len} bytes; first invalid byte \
                 at offset {at}). read_file returns text only; inspect it with bash \
                 (`file {path}`, `xxd -l 256 {path}`)."
            )
        });

        match decoded {
            Ok(content) => {
                // Count every successful read — including a past-end offset,
                // which still read the file — so the tally matches the
                // ledger's one-R-event-per-successful-read rule. The hash is
                // of the FULL content (even for a ranged read): drift asks
                // "has the file changed since the model looked", and the
                // whole file was current at that moment.
                let reads = self.ledger.record_read(root, path, &content);
                let lines: Vec<&str> = content.lines().collect();
                let start = offset.unwrap_or(1).saturating_sub(1);
                let end = start.saturating_add(limit).min(lines.len());

                if start >= lines.len() {
                    // Report the offset the CALLER passed (1-based, as the
                    // schema documents), not the 0-based index derived from
                    // it — "offset 4 is past end" for a call that said 5 sent
                    // the model hunting for an off-by-one that wasn't there.
                    return ToolOutput::Ok {
                        content: format!(
                            "(file has {} lines, offset {} is past end)",
                            lines.len(),
                            offset.unwrap_or(1)
                        ),
                    };
                }

                // `write!` into one pre-sized buffer rather than a `format!`
                // allocation per line: this is the crate's hottest render
                // path (up to MAX_LINES lines on every single read). The
                // reservation is itself capped — a 5 MB single line must not
                // make the *buffer* the flood the caps exist to prevent.
                use std::fmt::Write as _;
                let mut numbered = String::with_capacity(
                    lines[start..end]
                        .iter()
                        .map(|l| l.len().min(MAX_LINE_BYTES) + 32)
                        .sum::<usize>()
                        .min(MAX_RENDER_BYTES + 1024)
                        + 64,
                );
                let mut clipped_lines = 0usize;
                let mut shown = 0usize;
                let mut payload_capped = false;
                for (i, line) in lines[start..end].iter().enumerate() {
                    if numbered.len() >= MAX_RENDER_BYTES {
                        payload_capped = true;
                        break;
                    }
                    let line_num = start + i + 1;
                    if line.len() <= MAX_LINE_BYTES {
                        let _ = writeln!(numbered, "{line_num:>6}\t{line}");
                    } else {
                        // Char-boundary-safe: `String::truncate`/byte slicing
                        // would panic mid-UTF-8 on a long non-ASCII line.
                        let head = crate::exec::truncate_preview(line, MAX_LINE_BYTES);
                        let elided = line.len() - head.len();
                        let _ =
                            writeln!(numbered, "{line_num:>6}\t{head}[… {elided} bytes elided …]");
                        clipped_lines += 1;
                    }
                    shown += 1;
                }
                let total = lines.len();
                let _ = write!(
                    numbered,
                    "\n({shown}/{total} lines shown · read {reads}× this session"
                );
                if clipped_lines > 0 {
                    let _ = write!(
                        numbered,
                        " · {clipped_lines} line(s) clipped at the {MAX_LINE_BYTES}-byte \
                         per-line cap"
                    );
                }
                if payload_capped {
                    let _ = write!(
                        numbered,
                        " · stopped at the {} KB payload cap — re-read with offset/limit to \
                         continue",
                        MAX_RENDER_BYTES / 1024
                    );
                }
                numbered.push(')');
                ToolOutput::Ok { content: numbered }
            }
            Err(message) => ToolOutput::Error { message },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_file_with_line_numbers() {
        let dir = std::env::temp_dir();
        let path = format!("stella_test_read_{}.txt", std::process::id());
        let full = dir.join(&path);
        tokio::fs::write(&full, "line one\nline two\nline three\n")
            .await
            .unwrap();

        let result = ReadFile::default()
            .execute(&serde_json::json!({"path": path}), &dir)
            .await;
        match result {
            ToolOutput::Ok { content } => {
                assert!(content.contains("1\tline one"));
                assert!(content.contains("2\tline two"));
                assert!(content.contains("3/3 lines shown"));
            }
            ToolOutput::Error { message } => panic!("expected ok, got: {message}"),
        }
        let _ = tokio::fs::remove_file(&full).await;
    }

    #[tokio::test]
    async fn respects_offset_and_limit() {
        let dir = std::env::temp_dir();
        let path = format!("stella_test_range_{}.txt", std::process::id());
        let full = dir.join(&path);
        tokio::fs::write(&full, "a\nb\nc\nd\ne\n").await.unwrap();

        let result = ReadFile::default()
            .execute(
                &serde_json::json!({"path": path, "offset": 2, "limit": 2}),
                &dir,
            )
            .await;
        match result {
            ToolOutput::Ok { content } => {
                assert!(content.contains("2\tb"));
                assert!(content.contains("3\tc"));
                assert!(!content.contains("4\td"));
                assert!(content.contains("2/5 lines shown"));
            }
            ToolOutput::Error { message } => panic!("expected ok, got: {message}"),
        }
        let _ = tokio::fs::remove_file(&full).await;
    }

    #[tokio::test]
    async fn counts_reads_per_file_and_reports_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "one\ntwo\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "one\n").unwrap();
        let tool = ReadFile::default();

        // Two reads of the same file under different spellings aggregate.
        for spelling in ["src/a.rs", "src/./a.rs"] {
            let out = tool
                .execute(&serde_json::json!({"path": spelling}), dir.path())
                .await;
            assert!(!out.is_error(), "{out:?}");
        }
        let third = tool
            .execute(&serde_json::json!({"path": "src/a.rs"}), dir.path())
            .await;
        match third {
            ToolOutput::Ok { content } => {
                assert!(
                    content.contains("read 3× this session"),
                    "third read reports its count: {content}"
                );
            }
            ToolOutput::Error { message } => panic!("expected ok, got: {message}"),
        }
        assert_eq!(tool.read_count(dir.path(), "src/a.rs"), 3);
        assert_eq!(tool.read_count(dir.path(), "src/./a.rs"), 3);

        // Other files and failed reads don't inflate the tally.
        let other = tool
            .execute(&serde_json::json!({"path": "b.rs"}), dir.path())
            .await;
        assert!(!other.is_error());
        assert_eq!(tool.read_count(dir.path(), "b.rs"), 1);
        let missing = tool
            .execute(&serde_json::json!({"path": "ghost.rs"}), dir.path())
            .await;
        assert!(missing.is_error());
        assert_eq!(tool.read_count(dir.path(), "ghost.rs"), 0);
    }

    #[tokio::test]
    async fn read_records_last_seen_hash_even_for_ranged_reads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "one\ntwo\nthree\n").unwrap();
        let ledger = Arc::new(ReadLedger::default());
        let tool = ReadFile::with_ledger(ledger.clone());

        assert_eq!(ledger.last_seen_sha(dir.path(), "a.rs"), None);
        let out = tool
            .execute(
                &serde_json::json!({"path": "a.rs", "offset": 2, "limit": 1}),
                dir.path(),
            )
            .await;
        assert!(!out.is_error(), "{out:?}");
        // The hash covers the FULL file content current at read time, not
        // just the displayed range — drift asks about the file, not the view.
        assert_eq!(
            ledger.last_seen_sha(dir.path(), "a.rs"),
            Some(crate::staleness::hex_sha256(b"one\ntwo\nthree\n")),
        );

        // An out-of-band change is visible as a hash mismatch…
        std::fs::write(dir.path().join("a.rs"), "rewritten\n").unwrap();
        assert_ne!(
            ledger.last_seen_sha(dir.path(), "a.rs"),
            Some(crate::staleness::hex_sha256(b"rewritten\n")),
        );
        // …until the next read refreshes the baseline.
        let again = tool
            .execute(&serde_json::json!({"path": "a.rs"}), dir.path())
            .await;
        assert!(!again.is_error());
        assert_eq!(
            ledger.last_seen_sha(dir.path(), "a.rs"),
            Some(crate::staleness::hex_sha256(b"rewritten\n")),
        );
    }

    /// The line cap alone never saw this file: 5 MB on ONE line is a single
    /// line, so it sailed under `MAX_LINES` and landed in the transcript
    /// whole (~1.4M estimated tokens — enough to hard-fail the next provider
    /// call). The width cap must clip it, loudly, and say so.
    #[tokio::test]
    async fn a_single_pathologically_long_line_is_clipped_and_named() {
        let dir = tempfile::tempdir().unwrap();
        let huge = "x".repeat(5 * 1024 * 1024);
        std::fs::write(dir.path().join("bundle.min.js"), &huge).unwrap();

        let out = ReadFile::default()
            .execute(&serde_json::json!({"path": "bundle.min.js"}), dir.path())
            .await;
        let ToolOutput::Ok { content } = out else {
            panic!("expected ok, got: {out:?}");
        };
        assert!(
            content.len() < 64 * 1024,
            "a 5 MB one-liner must not reach the model whole (got {} bytes)",
            content.len()
        );
        assert!(
            content.contains("bytes elided"),
            "elision is loud: {content}"
        );
        assert!(
            content.contains("clipped at the 1000-byte per-line cap"),
            "the footer names the cap: {content}"
        );
        assert!(content.contains("1/1 lines shown"), "{content}");
    }

    /// Many long lines blow the payload ceiling even though each one is
    /// individually clipped — the render stops and the footer says so, with
    /// an honest shown/total count.
    #[tokio::test]
    async fn the_total_payload_cap_stops_the_render_and_reports_it() {
        let dir = tempfile::tempdir().unwrap();
        let line = "y".repeat(4096);
        let body: String = std::iter::repeat_n(line.as_str(), 800)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.path().join("dump.sql"), &body).unwrap();

        let out = ReadFile::default()
            .execute(&serde_json::json!({"path": "dump.sql"}), dir.path())
            .await;
        let ToolOutput::Ok { content } = out else {
            panic!("expected ok, got: {out:?}");
        };
        assert!(
            content.len() < MAX_RENDER_BYTES + 4096,
            "payload stays under the ceiling (got {} bytes)",
            content.len()
        );
        assert!(
            content.contains("stopped at the 400 KB payload cap"),
            "the footer names the cap: {content}"
        );
        assert!(
            !content.contains("800/800 lines shown"),
            "the shown count must be the lines actually emitted: {content}"
        );
        assert!(content.contains("/800 lines shown"), "{content}");
    }

    /// The caps must be invisible for ordinary source: no marker, no footer
    /// noise, byte-identical numbering.
    #[tokio::test]
    async fn ordinary_files_are_unaffected_by_the_byte_caps() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn main() {}\nlet x = 1;\n").unwrap();
        let out = ReadFile::default()
            .execute(&serde_json::json!({"path": "a.rs"}), dir.path())
            .await;
        let ToolOutput::Ok { content } = out else {
            panic!("expected ok, got: {out:?}");
        };
        assert!(!content.contains("elided"), "{content}");
        assert!(!content.contains("cap"), "{content}");
        assert!(
            content.ends_with("(2/2 lines shown · read 1× this session)"),
            "{content}"
        );
    }

    /// A binary file used to come back as "stream did not contain valid
    /// UTF-8", which reads to a model as a transient IO fault worth retrying.
    /// It must be named as binary and point somewhere useful.
    #[tokio::test]
    async fn a_binary_file_is_named_as_binary_not_as_an_io_fault() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blob.bin"), [0x00u8, 0xff, 0xfe, 0x41]).unwrap();
        let out = ReadFile::default()
            .execute(&serde_json::json!({"path": "blob.bin"}), dir.path())
            .await;
        match out {
            ToolOutput::Error { message } => {
                assert!(message.contains("binary"), "{message}");
                assert!(message.contains("not UTF-8"), "{message}");
            }
            ToolOutput::Ok { content } => panic!("expected error, got: {content}"),
        }
    }

    /// A directory used to surface the raw `Is a directory (os error 21)`,
    /// which says what the syscall thought and not what to do instead.
    #[tokio::test]
    async fn a_directory_is_refused_with_the_tool_that_does_answer() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        let out = ReadFile::default()
            .execute(&serde_json::json!({"path": "src"}), dir.path())
            .await;
        match out {
            ToolOutput::Error { message } => {
                assert!(message.contains("is a directory"), "{message}");
                assert!(message.contains("glob("), "names the tool: {message}");
            }
            ToolOutput::Ok { content } => panic!("expected error, got: {content}"),
        }
    }

    /// The heap half of the caps: `offset`/`limit` bound what the MODEL sees,
    /// never what Stella loads, so a one-line read of a multi-gigabyte dump
    /// paid for the whole file. Above the ceiling the refusal is named and
    /// points at the tools that stream.
    #[tokio::test]
    async fn an_oversized_file_is_refused_before_it_is_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dump.sql");
        let file = std::fs::File::create(&path).unwrap();
        // Sparse: the ceiling is decided from metadata, so no bytes are spent.
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        drop(file);

        let out = ReadFile::default()
            .execute(
                &serde_json::json!({"path": "dump.sql", "offset": 1, "limit": 1}),
                dir.path(),
            )
            .await;
        match out {
            ToolOutput::Error { message } => {
                assert!(message.contains("ceiling"), "{message}");
                assert!(message.contains("grep"), "points somewhere: {message}");
            }
            ToolOutput::Ok { content } => {
                panic!("an oversized file must be refused, got: {content}")
            }
        }
    }

    #[tokio::test]
    async fn missing_file_returns_error() {
        let dir = std::env::temp_dir();
        let result = ReadFile::default()
            .execute(
                &serde_json::json!({"path": "nonexistent_xyz_123.txt"}),
                &dir,
            )
            .await;
        assert!(result.is_error());
    }

    #[tokio::test]
    async fn path_escape_returns_error() {
        let dir = std::env::temp_dir();
        let result = ReadFile::default()
            .execute(&serde_json::json!({"path": "../../etc/passwd"}), &dir)
            .await;
        assert!(result.is_error());
    }

    #[tokio::test]
    async fn missing_path_field_returns_error() {
        let dir = std::env::temp_dir();
        let result = ReadFile::default()
            .execute(&serde_json::json!({}), &dir)
            .await;
        assert!(result.is_error());
    }
}
