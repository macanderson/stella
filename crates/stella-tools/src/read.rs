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
//! That footer has a second reader: the loop detector strips it before
//! comparing two reads, because the per-session tally it carries differs on
//! every read and would otherwise make a reread spiral structurally
//! undetectable. Its shape is therefore not this module's to spell — the
//! fragments come from [`stella_core::driver::loop_evidence`], which is where
//! the stripping lives, and
//! `the_footer_a_read_writes_is_the_one_loop_comparison_strips` below pins the
//! two ends together. (Named, not linked: it is `#[cfg(test)]`, so rustdoc
//! cannot resolve it.)
//!
//! The tool also keeps the session's read-state ledger ([`ReadLedger`]): every
//! successful read records a per-file tally (reported in the tool output) and
//! the sha256 of the bytes that were current at read time. The hash is the
//! read→edit drift oracle's baseline (#331): `edit_file` compares it against
//! current disk bytes to attribute a failed match to an out-of-band change
//! instead of a generic not-found. Reads are keyed by the file's normalized
//! workspace-relative path (so `src/./a.rs` and `src/a.rs` count as one
//! file). One ledger lives per registry, shared by `read_file`, `edit_file`,
//! and `write_file` — so the ledger tracks the content the model last *saw*,
//! whichever surface showed (or produced) it. The audit-grade equivalent (one
//! `R` event per read) lands in the registry's file-touch ledger.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;
use stella_core::driver::loop_evidence::{
    READ_FOOTER_CLAUSE_SEP, READ_FOOTER_CLOSE, READ_FOOTER_OPEN, READ_FOOTER_TALLY_END,
    READ_FOOTER_TALLY_MID,
};
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::registry::Tool;

/// How many times one path may be read while its content does not change
/// before `read_file` refuses (#4034).
///
/// # Why a ceiling exists at all
///
/// All four loop verdicts in `stella_core::loop_detect` are defined on
/// byte-identical *output*: "identical input with identical output means the
/// model gained no new information". A model paging linearly through one file
/// defeats every one of them, because each read returns a genuinely different
/// window — and yet the turn makes no progress. One observed turn spent
/// **$7.83 and 18.8M input tokens** on 164 forty-line reads of a single
/// 3,943-line file, ran off the end, wrapped back to offset 1 and started
/// over. No verdict fired; `max_steps` was the only remaining backstop and had
/// not been reached when the user killed it by hand.
///
/// # Why this number, and why it cannot fire on real work
///
/// `limit` defaults to — and is capped at — [`MAX_LINES`], so **two** reads
/// cover any file under 4,000 lines and one covers almost every file anyone
/// writes. Reaching a 25th read of bytes that have not moved is not paging; it
/// is the sweep above. The two escape hatches that make this safe are both in
/// the refusal text and both inside this same tool: raise `limit` (or drop it,
/// which is the same thing), or use `search`.
///
/// Content change resets the tally ([`ReadState::reads_since_change`]), so the
/// read → edit → read cycle that is most of an agent's working life never
/// approaches it however long the session runs.
const MAX_UNCHANGED_READS: u64 = 24;

/// The refusal [`MAX_UNCHANGED_READS`] produces.
///
/// **Byte-identical across calls by construction** — it names the path and the
/// file's size, never the running tally. That is load-bearing rather than
/// tidy: this refusal is a `ToolOutput::error`, which loop comparison does not
/// strip a footer from, so a tally inside it would make every refusal a
/// different string and leave a model that ignores the ceiling exactly as
/// undetectable as the sweep that earned it. Constant, it is caught by the
/// existing rungs — `ExactRepeat` after three identical retries, `Stagnant`
/// after six with the offsets still moving — which is how the ceiling and the
/// detector cover each other rather than duplicating one guess.
fn unchanged_read_ceiling(path: &str, total_lines: usize) -> String {
    format!(
        "`{path}` has already been read {MAX_UNCHANGED_READS} times without its content \
         changing, and re-reading unchanged bytes cannot tell you anything new. The file \
         is {total_lines} lines and `limit` defaults to {MAX_LINES}, so omitting `limit` \
         shows you all of it at once — paging it in small windows is what exhausted this \
         budget. If you are looking for something specific, use `search` instead; if you \
         need a different view of the file, use `bash`."
    )
}

/// The default window and the ceiling on an explicit `limit`. Private: it was
/// `pub(crate)` for `read_symbol`, which read through this tool and named the
/// cap in its own output, and that tool is retired
/// ([`crate::catalog::RETIRED_TOOL_NAMES`]).
const MAX_LINES: usize = 2000;

/// Per-line width cap. Real source lines are far shorter; the cases this
/// catches are machine-generated (minified JS, base64 blobs, one-line JSON),
/// where the tail of the line carries no more information than its head.
/// Looser than `grep`'s column cap on purpose: `read_file` is the tool you
/// call when you want the file's actual contents.
const MAX_LINE_BYTES: usize = 1_000;

/// Ceiling on the whole rendered payload.
///
/// This was 400 KB — about 114k estimated tokens, or **76% of the whole 150k
/// compaction budget in one tool result**. That is worse than it sounds,
/// because a single large result does not trigger compaction to reclaim
/// itself: with the rest of the transcript small, `compact_measured` returns
/// early (under budget) and the retention horizon
/// (`tool_result_horizon_steps: Some(8)`) then keeps the result verbatim for
/// the next eight tool-bearing steps. One read of a lockfile, a schema dump or
/// a bundled JS file cost roughly 900k input tokens (#1842).
///
/// 64 KB is ~18k tokens, about 12% of that budget — a bound a turn can carry
/// eight times over.
///
/// **The trade-off, stated rather than buried:** a full 2000-line read of
/// ordinary source (~45 bytes a line, ~90 KB) now stops around line 1400 and
/// the model pages once. That cost is deliberate and it is what the fix is —
/// `read_file` already supports `offset`/`limit`, and the footer now names the
/// exact line to resume from, so paging is one more call rather than a guess.
/// Moving this number is one edit if a maintainer wants a different point on
/// that curve.
const MAX_RENDER_BYTES: usize = 64 * 1024;

/// Ceiling on the file this tool will load at all.
///
/// Every cap above bounds what reaches the MODEL; none of them bounds what
/// reaches Stella's heap, because the render only happens after the whole
/// file has been read, UTF-8-validated and sha256'd. `offset`/`limit` do not
/// help — a one-line read of a 4 GB database dump paid for all 4 GB. Above
/// this ceiling the honest answer is a named refusal that points at the tools
/// which stream, not a multi-gigabyte allocation followed by a 64 KB answer.
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
    /// Reads of this path since its content last changed — the tally
    /// [`MAX_UNCHANGED_READS`] is measured against.
    ///
    /// Separate from `reads` because `reads` can only ever grow, and a model
    /// that reads a file, edits it, and reads it again is doing the most
    /// ordinary thing there is. Resetting on every content change is what lets
    /// a ceiling exist at all without ever standing in the way of that loop:
    /// what it bounds is re-reading bytes that have not moved.
    reads_since_change: u64,
    sha256: String,
    /// Whether the most recent read actually put the WHOLE file in front of
    /// the model — every line, uncapped and unclipped.
    ///
    /// Distinct from `sha256`, which is deliberately the hash of the full file
    /// even for a ranged read: that answers "did this file change since the
    /// model looked", and the whole file was current at that moment. This
    /// answers the different question "did the model SEE all of it", which is
    /// the only one the no-clobber guard may act on. `false` by default, so a
    /// path that arrived through `record_known` alone never claims coverage it
    /// did not earn.
    whole_file: bool,
}

/// Session-scoped ledger of the last file content the model has *seen*.
///
/// Updated by successful reads (`read_file`) and by the model's own
/// successful mutations (`edit_file`, `write_file` — the model knows the
/// content it just produced). A mismatch between an entry's hash and current
/// disk bytes therefore means the file changed out-of-band since the model
/// last looked — the read→edit drift signal (#331).
#[derive(Default)]
pub struct ReadLedger {
    states: Mutex<HashMap<String, ReadState>>,
}

/// What one recorded read tells the caller about the history of that path.
///
/// Two counts, not one, because they answer different questions: `reads` is
/// the model-facing "you have looked at this file before" nudge that rides the
/// footer, while `since_change` is the only one a *ceiling* may be measured
/// against — see [`ReadState::reads_since_change`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadTally {
    /// Reads of this path this session, under any spelling.
    pub reads: u64,
    /// Reads of this path since its content last changed, this one included.
    pub since_change: u64,
}

impl ReadLedger {
    /// Record one successful read of `path` whose full content was `content`,
    /// returning the path's read counts including this one.
    pub fn record_read(&self, root: &std::path::Path, path: &str, content: &str) -> ReadTally {
        let mut states = self.states.lock().unwrap_or_else(|p| p.into_inner());
        let state = states.entry(normalized_key(root, path)).or_default();
        state.reads += 1;
        let sha256 = crate::staleness::hex_sha256(content.as_bytes());
        if state.sha256 != sha256 {
            state.reads_since_change = 0;
        }
        state.reads_since_change += 1;
        state.sha256 = sha256;
        // Coverage is not known until the payload has been rendered, so it is
        // cleared here and set by `record_coverage` below. Clearing rather than
        // leaving the previous value standing means every path that returns
        // before rendering — a past-end offset, an early error — falls back to
        // "the model did not see all of it", which is the safe direction.
        state.whole_file = false;
        ReadTally {
            reads: state.reads,
            since_change: state.reads_since_change,
        }
    }

    /// Record whether the read that just rendered showed the model the whole
    /// file. Paired with [`Self::record_read`], which clears the flag first.
    pub fn record_coverage(&self, root: &std::path::Path, path: &str, whole_file: bool) {
        let mut states = self.states.lock().unwrap_or_else(|p| p.into_inner());
        states
            .entry(normalized_key(root, path))
            .or_default()
            .whole_file = whole_file;
    }

    /// Whether the model has been shown every line of `path` by its most
    /// recent read. `false` for a file never read, a ranged read, a read that
    /// hit the payload cap, and a read whose lines were clipped.
    ///
    /// [`crate::write::WriteFile`]'s no-clobber guard keys on this: a belief
    /// is only as good as what the agent actually saw, and recording one for
    /// bytes it never looked at is worse than recording none — it converts a
    /// caught clobber into a silent one.
    pub fn saw_whole_file(&self, root: &std::path::Path, path: &str) -> bool {
        self.states
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&normalized_key(root, path))
            .is_some_and(|s| s.whole_file)
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
    crate::normalize_workspace_path(root, path).unwrap_or_else(|| path.to_string())
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

    async fn execute(&self, input: &Value, ctx: &crate::ctx::ToolCtx) -> ToolOutput {
        let root = ctx.root();
        let path = match crate::input::required_str(input, "path") {
            Ok(v) => v,
            Err(err) => {
                return ToolOutput::from(err);
            }
        };

        // A wrong-typed `offset`/`limit` is refused, never silently
        // defaulted — `and_then(as_u64)` read `"limit": "200"` as the
        // default window without a word (#3144). Absent still defaults.
        let offset = match crate::input::optional_u64(input, "offset") {
            Ok(offset) => offset.map(|n| n as usize),
            Err(err) => {
                return ToolOutput::from(err);
            }
        };
        // MAX_LINES is a ceiling, not just the default: the flood protection
        // the module header promises must hold for explicit limits too.
        let limit = match crate::input::optional_u64(input, "limit") {
            Ok(limit) => limit
                .map(|n| (n as usize).min(MAX_LINES))
                .unwrap_or(MAX_LINES),
            Err(err) => {
                return ToolOutput::from(err);
            }
        };

        // The one read the scope refuses: another session's git worktree. Not
        // a security boundary — it is a correctness one. See
        // `stella_core::workspace_scope` on why a parallel checkout of the
        // same repository is the read an agent must not silently get.
        if let Some(refusal) = ctx.refuse_read(path) {
            return ToolOutput::error(refusal);
        }

        // Read from whichever allowed root holds this path, not from the
        // session root alone.
        //
        // Without this, a directory granted by `--allow-dir` was **writable
        // but not readable**: `write_file` opened the scope root that
        // `resolve_for_write` chose, while this tool opened `ctx.root()` and
        // `rootfd` then refused the absolute path as an escape. An agent
        // could create a file and be told the file it had just written did
        // not exist — the worst shape a boundary bug can take, because
        // nothing about the message points at the boundary.
        let (root, path) = match ctx.resolve_for_read(path) {
            Some(resolved) => resolved,
            // Outside every root: fall back to the session root and let
            // `rootfd` answer. Reads are not scope-confined (see
            // `stella_core::workspace_scope`), so this preserves the previous
            // behaviour exactly rather than inventing a new refusal.
            None => (root.to_path_buf(), path.to_string()),
        };
        let path = path.as_str();

        let handle = match crate::rootfd::RootHandle::open(&root) {
            Ok(handle) => std::sync::Arc::new(handle),
            Err(e) => {
                return ToolOutput::error(format!("cannot open workspace root: {e}"));
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
                return ToolOutput::error(format!(
                    "`{path}` is a directory, not a file — list it with \
                         glob({{\"pattern\": \"*\", \"path\": \"{path}\"}})"
                ));
            }
            Ok(Ok(Loaded::TooLarge { bytes })) => {
                return ToolOutput::error(format!(
                    "`{path}` is {} MB, past read_file's {} MB ceiling — the whole file is \
                         loaded to render any range, so offset/limit would not help. Search it \
                         with grep, or page it with bash (`sed -n '1,200p' {path}`).",
                    bytes / (1024 * 1024),
                    MAX_FILE_BYTES / (1024 * 1024)
                ));
            }
            Ok(Err(e)) if e.is_escape() => {
                return ToolOutput::error(format!("path `{path}` escapes workspace root ({e})"));
            }
            Ok(Err(e)) => {
                return ToolOutput::error(format!("failed to read `{path}`: {e}"));
            }
            Err(e) => {
                return ToolOutput::error(format!("failed to read `{path}`: {e}"));
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
                let tally = self.ledger.record_read(&root, path, &content);
                let lines: Vec<&str> = content.lines().collect();
                let start = offset.unwrap_or(1).saturating_sub(1);
                let end = start.saturating_add(limit).min(lines.len());

                if tally.since_change > MAX_UNCHANGED_READS {
                    return ToolOutput::error(unchanged_read_ceiling(path, lines.len()));
                }

                if start >= lines.len() {
                    // The line count is the constant half and stays in the
                    // payload; the offset the caller passed is the volatile
                    // half and rides the footer, which loop comparison strips.
                    // Embedding it in the payload gave a sweep running off the
                    // end a DIFFERENT string on every call, so no two past-end
                    // reads ever compared equal and the stagnation rung could
                    // not fire on them (#4034).
                    //
                    // It is still reported 1-based, as the schema documents,
                    // and never the 0-based index derived from it — "offset 4
                    // is past end" for a call that said 5 sent the model
                    // hunting for an off-by-one that wasn't there.
                    return ToolOutput::ok(format!(
                        "(file has {total} lines; the requested offset is past the end)\
                         {READ_FOOTER_OPEN}0/{total}{READ_FOOTER_TALLY_MID}{reads}\
                         {READ_FOOTER_TALLY_END}{READ_FOOTER_CLAUSE_SEP}requested offset \
                         {offset} is past the end{READ_FOOTER_CLOSE}",
                        total = lines.len(),
                        reads = tally.reads,
                        offset = offset.unwrap_or(1),
                    ));
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
                let reads = tally.reads;
                let _ = write!(
                    numbered,
                    "{READ_FOOTER_OPEN}{shown}/{total}{READ_FOOTER_TALLY_MID}{reads}\
                     {READ_FOOTER_TALLY_END}"
                );
                if clipped_lines > 0 {
                    let _ = write!(
                        numbered,
                        "{READ_FOOTER_CLAUSE_SEP}{clipped_lines} line(s) clipped at the \
                         {MAX_LINE_BYTES}-byte per-line cap"
                    );
                }
                if payload_capped {
                    // Name the line to resume from, not just the existence of
                    // more. "re-read with offset/limit to continue" left the
                    // model to work out WHERE from — and the answer is not the
                    // line count it can see, because `start` may be non-zero
                    // and clipped lines still count as shown. A paging note
                    // that costs a guess is a note that costs another read.
                    let _ = write!(
                        numbered,
                        "{READ_FOOTER_CLAUSE_SEP}stopped at the {} KB payload cap — \
                         continue with offset={}",
                        MAX_RENDER_BYTES / 1024,
                        start + shown + 1
                    );
                }
                numbered.push_str(READ_FOOTER_CLOSE);
                // Every line, and every line whole. `shown == total` alone is
                // not enough: a full-range read can still hand back clipped
                // lines or stop at the payload cap, and in both cases there are
                // bytes on disk the model was never shown.
                self.ledger.record_coverage(
                    &root,
                    path,
                    shown == total && clipped_lines == 0 && !payload_capped,
                );
                ToolOutput::ok(numbered)
            }
            Err(message) => ToolOutput::error(message),
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

    /// #3145: an input the tool could not read is classified
    /// [`stella_protocol::ErrorClass::InvalidInput`] — the model's mistake,
    /// excluded from the tool's own error rate — with the message bytes
    /// unchanged from the pre-class wording.
    #[tokio::test]
    async fn missing_path_is_classified_invalid_input() {
        use stella_protocol::ErrorClass;
        let result = ReadFile::default()
            .execute(&serde_json::json!({}), &cx(std::env::temp_dir()))
            .await;
        let ToolOutput::Error { message, class } = result else {
            panic!("expected an error for a missing required field");
        };
        assert_eq!(class, Some(ErrorClass::InvalidInput));
        assert_eq!(message, "missing required field `path`");
    }

    #[tokio::test]
    async fn reads_file_with_line_numbers() {
        let dir = std::env::temp_dir();
        let path = format!("stella_test_read_{}.txt", std::process::id());
        let full = dir.join(&path);
        tokio::fs::write(&full, "line one\nline two\nline three\n")
            .await
            .unwrap();

        let result = ReadFile::default()
            .execute(&serde_json::json!({"path": path}), &cx(&dir))
            .await;
        match result {
            ToolOutput::Ok { content, .. } => {
                assert!(content.contains("1\tline one"));
                assert!(content.contains("2\tline two"));
                assert!(content.contains("3/3 lines shown"));
            }
            ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
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
                &cx(&dir),
            )
            .await;
        match result {
            ToolOutput::Ok { content, .. } => {
                assert!(content.contains("2\tb"));
                assert!(content.contains("3\tc"));
                assert!(!content.contains("4\td"));
                assert!(content.contains("2/5 lines shown"));
            }
            ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
        }
        let _ = tokio::fs::remove_file(&full).await;
    }

    /// The #3144 witness: a wrong-typed `offset`/`limit` is refused, never
    /// silently defaulted. On main, `{"limit": "200"}` vanished into the
    /// default window — no refusal, no note, wrong-sized read.
    #[tokio::test]
    async fn a_mistyped_offset_or_limit_is_refused_not_defaulted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "a\nb\nc\n").unwrap();
        let tool = ReadFile::default();

        let out = tool
            .execute(
                &serde_json::json!({"path": "f.txt", "limit": "200"}),
                &cx(dir.path()),
            )
            .await;
        let ToolOutput::Error { message, .. } = out else {
            panic!("a mistyped limit must be an error, got: {out:?}");
        };
        assert_eq!(
            message,
            "field `limit` must be a non-negative integer, got string"
        );

        let out = tool
            .execute(
                &serde_json::json!({"path": "f.txt", "offset": true}),
                &cx(dir.path()),
            )
            .await;
        let ToolOutput::Error { message, .. } = out else {
            panic!("a mistyped offset must be an error, got: {out:?}");
        };
        assert_eq!(
            message,
            "field `offset` must be a non-negative integer, got boolean"
        );

        // Absent still defaults — the fix refuses wrong types, not absence.
        let out = tool
            .execute(&serde_json::json!({"path": "f.txt"}), &cx(dir.path()))
            .await;
        assert!(!out.is_error(), "{out:?}");
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
                .execute(&serde_json::json!({"path": spelling}), &cx(dir.path()))
                .await;
            assert!(!out.is_error(), "{out:?}");
        }
        let third = tool
            .execute(&serde_json::json!({"path": "src/a.rs"}), &cx(dir.path()))
            .await;
        match third {
            ToolOutput::Ok { content, .. } => {
                assert!(
                    content.contains("read 3× this session"),
                    "third read reports its count: {content}"
                );
            }
            ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
        }
        assert_eq!(tool.read_count(dir.path(), "src/a.rs"), 3);
        assert_eq!(tool.read_count(dir.path(), "src/./a.rs"), 3);

        // Other files and failed reads don't inflate the tally.
        let other = tool
            .execute(&serde_json::json!({"path": "b.rs"}), &cx(dir.path()))
            .await;
        assert!(!other.is_error());
        assert_eq!(tool.read_count(dir.path(), "b.rs"), 1);
        let missing = tool
            .execute(&serde_json::json!({"path": "ghost.rs"}), &cx(dir.path()))
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
                &cx(dir.path()),
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
            .execute(&serde_json::json!({"path": "a.rs"}), &cx(dir.path()))
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
            .execute(
                &serde_json::json!({"path": "bundle.min.js"}),
                &cx(dir.path()),
            )
            .await;
        let ToolOutput::Ok { content, .. } = out else {
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
            .execute(&serde_json::json!({"path": "dump.sql"}), &cx(dir.path()))
            .await;
        let ToolOutput::Ok { content, .. } = out else {
            panic!("expected ok, got: {out:?}");
        };
        assert!(
            content.len() < MAX_RENDER_BYTES + 4096,
            "payload stays under the ceiling (got {} bytes)",
            content.len()
        );
        // Derived from the constant, not written out: this assertion said
        // "400 KB" and would have gone on passing for a cap that moved, which
        // is the shape a size guard can least afford.
        assert!(
            content.contains(&format!(
                "stopped at the {} KB payload cap",
                MAX_RENDER_BYTES / 1024
            )),
            "the footer names the cap: {content}"
        );
        assert!(
            !content.contains("800/800 lines shown"),
            "the shown count must be the lines actually emitted: {content}"
        );
        assert!(content.contains("/800 lines shown"), "{content}");

        // The paging half (#1842). A cap that says "there is more" without
        // saying WHERE costs the model a guess, and the answer is not the
        // shown count — `start` may be non-zero and clipped lines still count
        // as shown. Asserted by re-reading at the named offset and requiring
        // the continuation to begin exactly one line after the last shown.
        let resume: usize = content
            .split("continue with offset=")
            .nth(1)
            .and_then(|tail| {
                let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
                digits.parse().ok()
            })
            .unwrap_or_else(|| panic!("the footer must name the line to resume from: {content}"));
        assert!(resume > 1, "a resume offset of {resume} names no progress");

        // The offset has to be usable, not merely present: re-reading at it
        // must begin exactly where this render stopped. An off-by-one here
        // silently skips a line or repeats one, and the model cannot tell.
        assert!(
            !content.contains(&format!("\n{resume:>6}\t")),
            "line {resume} must NOT already be in this render — it is where the \
             next one starts"
        );
        let next = ReadFile::default()
            .execute(
                &serde_json::json!({"path": "dump.sql", "offset": resume}),
                &cx(dir.path()),
            )
            .await;
        let ToolOutput::Ok { content: next, .. } = next else {
            panic!("expected ok, got: {next:?}");
        };
        assert!(
            next.starts_with(&format!("{resume:>6}\t")),
            "reading at the named offset must continue at line {resume}: {next}"
        );
    }

    /// The caps must be invisible for ordinary source: no marker, no footer
    /// noise, byte-identical numbering.
    #[tokio::test]
    async fn ordinary_files_are_unaffected_by_the_byte_caps() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn main() {}\nlet x = 1;\n").unwrap();
        let out = ReadFile::default()
            .execute(&serde_json::json!({"path": "a.rs"}), &cx(dir.path()))
            .await;
        let ToolOutput::Ok { content, .. } = out else {
            panic!("expected ok, got: {out:?}");
        };
        assert!(!content.contains("elided"), "{content}");
        assert!(!content.contains("cap"), "{content}");
        assert!(
            content.ends_with("(2/2 lines shown · read 1× this session)"),
            "{content}"
        );
    }

    /// **The #4034 witness.** A monotonic paging sweep of one file is stopped
    /// before it can spend a turn's whole budget.
    ///
    /// The observed turn made 164 forty-line reads of one 3,943-line file for
    /// $7.83, ran off the end, wrapped to offset 1 and started over. Every
    /// loop verdict stayed silent because each read returned a genuinely
    /// different window — all four are defined on byte-identical *output*, and
    /// this sweep never repeats one. On `main` this test's forty reads all
    /// succeed and the turn is left to grind to `max_steps`.
    #[tokio::test]
    async fn a_monotonic_paging_sweep_is_refused_before_it_burns_a_turn() {
        let dir = tempfile::tempdir().unwrap();
        let body = (1..=4000)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.path().join("deck_ui.rs"), &body).unwrap();
        let tool = ReadFile::default();
        let ctx = cx(dir.path());

        let mut refused_at = None;
        let mut refusals = Vec::new();
        for step in 0..40u64 {
            let out = tool
                .execute(
                    &serde_json::json!({
                        "path": "deck_ui.rs",
                        "offset": step * 40 + 1,
                        "limit": 40,
                    }),
                    &ctx,
                )
                .await;
            if let ToolOutput::Error { message, .. } = out {
                refused_at.get_or_insert(step);
                refusals.push(message);
            }
        }
        let refused_at = refused_at.expect("the sweep must be refused, not run to the cap");
        assert!(
            refused_at < 40,
            "the sweep has to be stopped before step 40, got {refused_at}"
        );
        assert_eq!(
            refused_at, MAX_UNCHANGED_READS,
            "the 25th read of unchanged bytes is the first refused"
        );
        // Constant by construction — a tally inside it would make every
        // refusal a different string and leave a model that ignores the
        // ceiling as undetectable as the sweep that earned it.
        assert!(
            refusals.windows(2).all(|w| w[0] == w[1]),
            "every refusal must be byte-identical: {refusals:?}"
        );
        // The remedy has to be reachable from inside this same tool, or the
        // ceiling is a wall rather than a redirection.
        assert!(refusals[0].contains("omitting `limit`"), "{}", refusals[0]);
        assert!(refusals[0].contains("`search`"), "{}", refusals[0]);
    }

    /// The ceiling counts reads of bytes that have not moved, so the
    /// read → edit → read cycle that is most of an agent's working life never
    /// approaches it. Without the reset this would refuse the 25th pass of an
    /// ordinary edit loop.
    #[tokio::test]
    async fn editing_the_file_resets_the_read_ceiling() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        let tool = ReadFile::default();
        let ctx = cx(dir.path());
        for pass in 0..40 {
            std::fs::write(&path, format!("fn main() {{}} // pass {pass}\n")).unwrap();
            let out = tool
                .execute(&serde_json::json!({"path": "a.rs"}), &ctx)
                .await;
            assert!(
                matches!(out, ToolOutput::Ok { .. }),
                "pass {pass} of a read→edit→read cycle must never be refused: {out:?}"
            );
        }
    }

    /// A sweep that runs off the end of the file must stagnate like anything
    /// else. The past-end reply embedded the caller's own offset in its
    /// payload, so every call produced a different string, nothing ever
    /// compared equal, and the stagnation rung could not fire on it (#4034).
    /// The offset now rides the footer, which loop comparison strips.
    #[tokio::test]
    async fn past_end_reads_compare_equal_once_the_footer_is_stripped() {
        use stella_core::driver::loop_evidence::comparable_output;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "one\ntwo\n").unwrap();
        let tool = ReadFile::default();
        let ctx = cx(dir.path());
        let mut compared = Vec::new();
        for offset in [10, 50, 900] {
            let out = tool
                .execute(&serde_json::json!({"path": "a.rs", "offset": offset}), &ctx)
                .await;
            assert!(matches!(out, ToolOutput::Ok { .. }), "{out:?}");
            let ToolOutput::Ok { content, .. } = &*comparable_output(&out) else {
                panic!("expected ok, got: {out:?}");
            };
            compared.push(content.clone());
        }
        assert!(
            compared.windows(2).all(|w| w[0] == w[1]),
            "three past-end reads at different offsets must normalize to one \
             string, or stagnation can never fire on a sweep past EOF: {compared:?}"
        );
        assert!(
            !compared[0].contains("900"),
            "the caller's offset must not survive normalization: {}",
            compared[0]
        );
        assert!(
            compared[0].contains("past the end"),
            "the reply still has to say what happened: {}",
            compared[0]
        );
    }

    /// The footer's producer is here and its consumer is in `stella-core`, so
    /// neither crate's own tests can catch the two drifting apart. This is the
    /// seam that can: real tool output, run through the real normalization the
    /// loop detector compares. Reword the footer on either side without the
    /// other and this fails.
    ///
    /// Every shape has to normalize away, not just the bare tally. A read that
    /// clipped a long line or stopped at the payload cap appends further
    /// clauses after it, and a match that ended at the tally would leave the
    /// per-session count in the compared bytes for exactly the large-file
    /// rereads a stuck agent produces most.
    #[tokio::test]
    async fn the_footer_a_read_writes_is_the_one_loop_comparison_strips() {
        use stella_core::driver::loop_evidence::comparable_output;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plain.rs"), "fn main() {}\nlet x = 1;\n").unwrap();
        std::fs::write(
            dir.path().join("wide.min.js"),
            "x".repeat(4 * MAX_LINE_BYTES),
        )
        .unwrap();
        let dump = std::iter::repeat_n("y".repeat(4096), 800)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.path().join("dump.sql"), &dump).unwrap();

        // One tool, so the ledger's tally really does move between the two
        // reads of a file — which is the whole reason the footer is volatile.
        let tool = ReadFile::default();
        for name in ["plain.rs", "wide.min.js", "dump.sql"] {
            let input = serde_json::json!({ "path": name });
            let first = tool.execute(&input, &cx(dir.path())).await;
            let second = tool.execute(&input, &cx(dir.path())).await;
            let (
                ToolOutput::Ok { content: raw, .. },
                ToolOutput::Ok {
                    content: raw_again, ..
                },
            ) = (&first, &second)
            else {
                panic!("expected ok for {name}, got: {first:?} / {second:?}");
            };
            assert!(
                raw.ends_with(READ_FOOTER_CLOSE) && raw.contains(READ_FOOTER_TALLY_END),
                "{name} emitted no recognizable footer: {raw}"
            );
            assert_ne!(
                raw, raw_again,
                "{name}: the tally must move, or this proves nothing"
            );

            let normalized = comparable_output(&first);
            assert_eq!(
                normalized,
                comparable_output(&second),
                "{name}: two identical reads must compare equal once the footer is off"
            );
            let ToolOutput::Ok { content, .. } = normalized.as_ref() else {
                unreachable!("normalizing an Ok output cannot change its variant");
            };
            assert!(
                !content.contains(READ_FOOTER_TALLY_END),
                "{name} kept part of the footer: {content}"
            );
        }
    }

    /// The engine's working-set restoration (#2685) replays this tool by the
    /// name and path parameter `stella_core::restore` spells, and refuses the
    /// replay unless the schema declares `read_only` — the same one-definition
    /// tie as the footer test above. A drift on either side is not cosmetic:
    /// it is restoration silently ceasing to restore files.
    ///
    /// This replaces the narrowed `the_read_tool_keeps_the_schema_shape_a_replay_requires`
    /// that stood in while the replay was disarmed by the tool purge (#3244):
    /// the constants exist again, so the pin goes back to asserting against
    /// them rather than against literals (#3470).
    #[test]
    fn the_read_tool_is_the_one_the_engines_restoration_replays() {
        let schema = ReadFile::default().schema();
        assert_eq!(schema.name, stella_core::restore::READ_TOOL);
        assert!(
            schema.read_only,
            "restoration (and the parked-wait probe) replay only schema-declared \
             read-only tools; dropping the claim silently disables both"
        );
        assert!(
            schema.input_schema["properties"]
                .get(stella_core::restore::READ_PATH_PARAM)
                .is_some(),
            "the path parameter must keep the spelling the engine replays"
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
            .execute(&serde_json::json!({"path": "blob.bin"}), &cx(dir.path()))
            .await;
        match out {
            ToolOutput::Error { message, .. } => {
                assert!(message.contains("binary"), "{message}");
                assert!(message.contains("not UTF-8"), "{message}");
            }
            ToolOutput::Ok { content, .. } => panic!("expected error, got: {content}"),
        }
    }

    /// A directory used to surface the raw `Is a directory (os error 21)`,
    /// which says what the syscall thought and not what to do instead.
    #[tokio::test]
    async fn a_directory_is_refused_with_the_tool_that_does_answer() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        let out = ReadFile::default()
            .execute(&serde_json::json!({"path": "src"}), &cx(dir.path()))
            .await;
        match out {
            ToolOutput::Error { message, .. } => {
                assert!(message.contains("is a directory"), "{message}");
                assert!(message.contains("glob("), "names the tool: {message}");
            }
            ToolOutput::Ok { content, .. } => panic!("expected error, got: {content}"),
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
                &cx(dir.path()),
            )
            .await;
        match out {
            ToolOutput::Error { message, .. } => {
                assert!(message.contains("ceiling"), "{message}");
                assert!(message.contains("grep"), "points somewhere: {message}");
            }
            ToolOutput::Ok { content, .. } => {
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
                &cx(&dir),
            )
            .await;
        assert!(result.is_error());
    }

    /// **Reads are not confined to the workspace**, and this test now says so.
    ///
    /// It used to assert that `../../etc/passwd` was refused. That was the
    /// behaviour when `read_file` opened only the session root, and it is the
    /// behaviour that was deliberately changed: an agent fixing a build needs
    /// system headers, the toolchain and a dependency's source, and a read
    /// cannot damage the user's tree (`stella_core::workspace_scope`).
    ///
    /// Worth noting how it was passing on macOS while the change was already
    /// in: `std::env::temp_dir()` there is `/var/folders/…/T/`, so
    /// `../../etc/passwd` resolves to a path that does not exist, and the read
    /// failed for the wrong reason. On Linux CI the same expression resolves
    /// to the real `/etc/passwd` and the read succeeded — which is how the
    /// stale assertion surfaced at all. A test that passes on one platform by
    /// accident of path arithmetic is worse than no test, so this one now
    /// pins the rule directly, on a file it creates itself.
    #[tokio::test]
    async fn a_read_outside_the_workspace_is_allowed() {
        let workspace = tempfile::tempdir().expect("workspace");
        let elsewhere = tempfile::tempdir().expect("elsewhere");
        let outside = elsewhere.path().join("readable.txt");
        std::fs::write(&outside, "readable\n").expect("write");

        let result = ReadFile::default()
            .execute(
                &serde_json::json!({ "path": outside.to_string_lossy() }),
                &cx(workspace.path()),
            )
            .await;
        let ToolOutput::Ok { content, .. } = result else {
            panic!("a read outside the workspace must succeed: {result:?}");
        };
        assert!(content.contains("readable"), "{content}");
    }

    /// The one read that IS refused: another session's worktree — a second
    /// checkout of the same repository at another revision, so reading it
    /// answers about the wrong copy of the file being edited.
    #[tokio::test]
    async fn a_read_into_a_sibling_worktree_is_refused() {
        let workspace = tempfile::tempdir().expect("workspace");
        let worktree = workspace.path().join(".stella/worktrees/sibling");
        std::fs::create_dir_all(&worktree).expect("mkdir");
        std::fs::write(worktree.join("other.rs"), "pub fn other() {}\n").expect("write");

        let result = ReadFile::default()
            .execute(
                &serde_json::json!({ "path": ".stella/worktrees/sibling/other.rs" }),
                &cx(workspace.path()),
            )
            .await;
        assert!(result.is_error(), "{result:?}");
    }

    #[tokio::test]
    async fn missing_path_field_returns_error() {
        let dir = std::env::temp_dir();
        let result = ReadFile::default()
            .execute(&serde_json::json!({}), &cx(&dir))
            .await;
        assert!(result.is_error());
    }
}
