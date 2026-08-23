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
//! instead of a generic not-found. Reads are keyed by the absolute path the
//! file resolves to (so `src/./a.rs` and `src/a.rs` count as one file, while
//! two `a.rs` under two different allowed roots do not — #3781). One ledger
//! lives per registry, shared by `read_file`, `edit_file`,
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
///
/// Every method takes `(root, path)` and keys on the absolute path the pair
/// resolves to (see `normalized_key`, which is private so this names it
/// rather than linking it), so `root` must be the directory
/// `path` is relative to — the one `ToolCtx::resolve_for_read` or
/// `resolve_for_write` returned, not the session root, which for a
/// `--allow-dir` path names a different file.
#[derive(Default)]
pub struct ReadLedger {
    states: Mutex<HashMap<String, ReadState>>,
}

/// What one recorded read tells the caller about the history of that path.
///
/// Two counts, not one, because they answer different questions: `reads` is
/// the model-facing "you have looked at this file before" nudge that rides the
/// footer, while `since_change` is the only one a *ceiling* may be measured
/// against: it resets whenever the file's content changes, so a read → edit →
/// read cycle can never accumulate against a ceiling. (`ReadState` holds it and
/// is private, so this names it rather than linking to it.)
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

/// The aggregation key for one file: the **absolute** path `path` resolves
/// to under `root`, not the root-relative remainder.
///
/// The remainder was ambiguous the moment a session had more than one
/// allowed root (#3781). `normalize_workspace_path` strips the canonical
/// root prefix and returns what is left, so with a `--allow-dir` in the
/// scope `<session_root>/a.rs` and `<allow_dir>/a.rs` — two different files
/// — both keyed on `"a.rs"` and shared one entry. The drift oracle (#331)
/// then compared one file's disk bytes against the other's recorded hash,
/// and `write_file`'s no-clobber guard (#3738) could be satisfied by a
/// whole-file read of the other one.
///
/// Resolving to an absolute path removes the ambiguity outright, and it is
/// also what makes the caller's choice of `root` a non-question: any root
/// that actually contains the path yields the same key. Callers still pass
/// the root the path is relative to — `write_file` and `edit_file` key with
/// the `scope_root` `ToolCtx::resolve_for_write` returned rather than the
/// session root, which is the other half of #3781 — because a *wrong* root
/// resolves the relative remainder against the wrong directory and so still
/// names the wrong file.
///
/// Falls back to the raw spelling when the path does not resolve, which it
/// cannot for a path a tool has just read or written.
fn normalized_key(root: &std::path::Path, path: &str) -> String {
    crate::resolve_within_root(root, path)
        .map(|full| full.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// The plural key: several files, or several ranges, in one call.
const FILES_KEY: &str = "files";

/// One file and which slice of it — the unit both spellings of a `read_file`
/// call reduce to.
struct ReadTarget {
    path: String,
    offset: Option<usize>,
    limit: usize,
}

/// Parse one target.
///
/// Shared by the single form and by every element of `files`
/// ([`crate::batch::targets`] is handed this very function), so the two
/// spellings cannot drift into two meanings.
fn read_target(value: &Value) -> Result<ReadTarget, crate::input::InputError> {
    let path = crate::input::required_str(value, "path")?.to_string();
    // A wrong-typed `offset`/`limit` is refused, never silently defaulted —
    // `and_then(as_u64)` read `"limit": "200"` as the default window without a
    // word (#3144). Absent still defaults.
    let offset = crate::input::optional_u64(value, "offset")?.map(|n| n as usize);
    // MAX_LINES is a ceiling, not just the default: the flood protection the
    // module header promises must hold for explicit limits too.
    let limit = crate::input::optional_u64(value, "limit")?
        .map(|n| (n as usize).min(MAX_LINES))
        .unwrap_or(MAX_LINES);
    Ok(ReadTarget {
        path,
        offset,
        limit,
    })
}

#[async_trait]
impl Tool for ReadFile {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "read_file".into(),
            description: "Read a file from the workspace. Returns content with 1-based line numbers. Use offset and limit for ranges. To read several files — or several ranges — read them in ONE call with `files` rather than one call each.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to workspace root" },
                    "offset": { "type": "integer", "description": "1-based start line (optional)" },
                    "limit": { "type": "integer", "description": "Max lines to return (optional; default and ceiling 2000)" },
                    "files": crate::batch::plural_schema(
                        serde_json::json!({
                            "path": { "type": "string", "description": "File path relative to workspace root" },
                            "offset": { "type": "integer", "description": "1-based start line (optional)" },
                            "limit": { "type": "integer", "description": "Max lines to return (optional; default and ceiling 2000)" }
                        }),
                        &["path"],
                        "Several files, or several ranges of one file, read in a single call.",
                    ),
                    "reason": { "type": "string", "description": "Why you are reading this file — recorded in the session's file-touch audit log" }
                },
                "required": []
            }),
            read_only: true,
            speculation_safe: true,
        }
    }

    async fn execute(&self, input: &Value, ctx: &crate::ctx::ToolCtx) -> ToolOutput {
        let targets = match crate::batch::targets(input, FILES_KEY, "path", read_target) {
            Ok(targets) => targets,
            Err(err) => return ToolOutput::from(err),
        };

        if !crate::batch::is_plural(input, FILES_KEY) {
            // Byte-identical to what the single form has always emitted. The
            // footer is what loop comparison strips and what this module's
            // tests assert verbatim, so the batch path must not reshape it.
            return self.read_one(&targets[0], ctx, MAX_RENDER_BYTES).await.0;
        }

        // The payload cap is a BATCH budget, not a per-file one. Ten files at
        // 64 KB each is 640 KB — the exact flood #1842 closed, which a plural
        // key would otherwise reopen. Every section spends from one pot, in
        // the caller's order.
        let mut rendered = String::new();
        let mut spent = 0usize;
        let mut unread: Vec<&str> = Vec::new();
        for target in &targets {
            if spent >= MAX_RENDER_BYTES {
                unread.push(target.path.as_str());
                continue;
            }
            let (out, used) = self.read_one(target, ctx, MAX_RENDER_BYTES - spent).await;
            spent += used;
            if !rendered.is_empty() {
                rendered.push_str("\n\n");
            }
            rendered.push_str(&format!("===== {} =====\n", target.path));
            match out {
                ToolOutput::Ok { content, .. } => rendered.push_str(&content),
                // A failed target does not end the batch. A read leaves
                // nothing half-done, so reporting the miss in place is
                // strictly more useful than discarding the files that did
                // resolve — which is also what the `sed` chains this replaces
                // did, minus having to guess which command failed.
                ToolOutput::Error { message, .. } => {
                    rendered.push_str(&format!("(not read: {message})"));
                }
            }
        }
        if !unread.is_empty() {
            rendered.push_str(&format!(
                "\n\n({} file(s) not read — the {} KB batch payload cap was reached: {}. \
                 Ask for them in a follow-up call.)",
                unread.len(),
                MAX_RENDER_BYTES / 1024,
                unread.join(", ")
            ));
        }
        ToolOutput::ok(rendered)
    }
}

impl ReadFile {
    /// Read one target, spending at most `budget` bytes of rendered payload.
    ///
    /// Returns the section and the bytes it actually spent, so the caller can
    /// hold one budget across a whole batch. Every per-file obligation lives
    /// here rather than in the loop above — the read ledger, the coverage
    /// flag, and the unchanged-read ceiling are all per file and stay that way
    /// whichever spelling asked for them.
    async fn read_one(
        &self,
        target: &ReadTarget,
        ctx: &crate::ctx::ToolCtx,
        budget: usize,
    ) -> (ToolOutput, usize) {
        let out = self.read_section(target, ctx, budget).await;
        // A refusal costs the batch nothing: it is a sentence, and charging
        // the budget for it would let a run of missing paths starve the files
        // that do resolve.
        let used = match &out {
            ToolOutput::Ok { content, .. } => content.len(),
            ToolOutput::Error { .. } => 0,
        };
        (out, used)
    }

    /// One target's section, rendered within `budget` bytes.
    async fn read_section(
        &self,
        target: &ReadTarget,
        ctx: &crate::ctx::ToolCtx,
        budget: usize,
    ) -> ToolOutput {
        let root = ctx.root();
        let path = target.path.as_str();
        let offset = target.offset;
        let limit = target.limit;

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

                // The ceiling targets the paging *sweep* that byte-identical
                // loop detection cannot see — every window it returns differs,
                // so no verdict fires. A whole-file read (offset at line 1, a
                // `limit` that reaches the last line) is the opposite case: it
                // returns the same bytes every time, so `ExactRepeat`/`Stagnant`
                // already catch a spiral of them. Exempting it is what makes the
                // refusal's advertised remedy reachable — "omitting `limit`
                // shows you all of it at once". Without this exemption
                // `record_read` above has already counted the refused read, so
                // a model that follows the advice issues the (N+1)th unchanged
                // read and trips the identical ceiling, and the message steers
                // it toward an action that can never succeed via `read_file`.
                let whole_file_read = start == 0 && end == lines.len();
                if !whole_file_read && tally.since_change > MAX_UNCHANGED_READS {
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
                        .min(budget + 1024)
                        + 64,
                );
                let mut clipped_lines = 0usize;
                let mut shown = 0usize;
                let mut payload_capped = false;
                for (i, line) in lines[start..end].iter().enumerate() {
                    if numbered.len() >= budget {
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
mod tests;
