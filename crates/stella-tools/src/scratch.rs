//! Session scratch state: `save_state`, `get_state`, `list_state`,
//! `delete_state`.
//!
//! The one place a turn may park intermediate state — a parse result, a
//! fetched artifact, a computed digest — so later steps reference it by key
//! instead of re-deriving it. Backed by a `tempfile::TempDir` owned by the
//! tool set: created at registration, deleted on drop. Nothing here
//! survives the session, and that is the contract, not a limitation —
//! anything the user should keep belongs in the workspace.
//!
//! (The directory is exported to `bash`'s child as `STELLA_SCRATCH`
//! ([`crate::subprocess_env::inject_scratch_env`]), so a command the model
//! runs can put a working file somewhere that is neither the workspace —
//! where it would land in the turn's diff — nor `/tmp`, where it would
//! outlive the session. A host that runs subprocesses of its own passes the
//! path itself; `get_state` pages whatever lands in the directory, however
//! it got there.)
//!
//! Contracts:
//! - Keys are filenames: `[A-Za-z0-9._-]`, 1–128 chars. Anything else is a
//!   named error, which is what makes traversal impossible rather than
//!   filtered.
//! - `save_state` refuses content over `MAX_SAVE_BYTES` with a named error.
//! - `save_state`'s success output embeds the saved content's `sha256/8`, so
//!   distinct saves render distinct outputs. The engine's stagnation
//!   detector (`stella-core`'s `loop_detect`) kills consecutive same-tool
//!   calls whose outputs are byte-identical, and a constant-shape
//!   `saved {key} (N bytes)` string made a legitimate checkpoint loop —
//!   different same-length content under one key — indistinguishable from a
//!   stuck one (#3297). The suffix is content-derived only: a timing or any
//!   randomness would blind the detector to genuinely stuck loops (#2706).
//! - `get_state` middle-truncates nothing: it pages (`offset`/`limit`
//!   in bytes, clamped to a `PAGE_CAP` slice) and names the remainder, so
//!   a partial read is always visibly partial.
//! - All errors are typed and named: bad key, unknown key, oversized save.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::registry::Tool;

/// Cap on model-authored `save_state` content. Files a host process writes
/// into the same directory are deliberately uncapped.
const MAX_SAVE_BYTES: usize = 1024 * 1024;

/// Byte cap on one `get_state` page — large enough for any reasonable
/// scratch note, small enough that one page never swamps a model context.
const PAGE_CAP: usize = 30 * 1024;

/// The session scratch directory. Owned by the tool set; the `TempDir`
/// deletes itself when this drops, which is the entire cleanup story.
pub struct ScratchDir {
    dir: tempfile::TempDir,
}

impl ScratchDir {
    /// Create the session scratch directory in the OS temp root.
    pub fn new() -> std::io::Result<Self> {
        Ok(Self {
            dir: tempfile::TempDir::with_prefix("stella-scratch-")?,
        })
    }

    /// The directory path — reported by `get_environment`, and how a host
    /// aims its own subprocesses at scratch.
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Resolve a validated key to its file path. `Err` is the named
    /// validation failure, phrased for the model.
    fn resolve(&self, key: &str) -> Result<PathBuf, String> {
        let ok_len = (1..=128).contains(&key.len());
        let ok_chars = key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if !ok_len || !ok_chars {
            return Err(format!(
                "invalid key {key:?}: keys are 1-128 chars of [A-Za-z0-9._-] \
                 (a filename, never a path)"
            ));
        }
        Ok(self.dir.path().join(key))
    }
}

fn str_field<'a>(input: &'a Value, name: &str) -> Result<&'a str, String> {
    input
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing required string field {name:?}"))
}

/// `save_state` — write one scratch entry.
pub struct SaveState(pub Arc<ScratchDir>);

#[async_trait]
impl Tool for SaveState {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "save_state".into(),
            description: "Save intermediate session state under a key so later steps can \
                reference it instead of re-deriving it (parse results, extracted lists, \
                computed digests). Scratch is session-private and deleted when the session \
                ends; anything the user should keep belongs in the workspace instead. \
                Content is capped at 1 MiB per save."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "key": {"type": "string", "description": "Entry name: 1-128 chars of [A-Za-z0-9._-]."},
                    "content": {"type": "string", "description": "The state to save (UTF-8, max 1 MiB)."}
                },
                "required": ["key", "content"]
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, _ctx: &crate::ctx::ToolCtx) -> ToolOutput {
        let (key, content) = match (str_field(input, "key"), str_field(input, "content")) {
            (Ok(k), Ok(c)) => (k, c),
            (Err(m), _) | (_, Err(m)) => return ToolOutput::error(m),
        };
        if content.len() > MAX_SAVE_BYTES {
            return ToolOutput::error(format!(
                "content is {} bytes; save_state caps at {MAX_SAVE_BYTES}. Save the \
                     artifact in smaller keyed pieces, or keep it in the workspace and \
                     reference it by path.",
                content.len()
            ));
        }
        let path = match self.0.resolve(key) {
            Ok(p) => p,
            Err(message) => {
                return ToolOutput::error(message);
            }
        };
        match std::fs::write(&path, content) {
            Ok(()) => {
                // The digest is the output's content-derived identity: without
                // it, different same-length saves under one key render
                // byte-identical outputs and the stagnation detector kills the
                // loop as stuck (#3297). Deterministic by contract — never a
                // timing, never randomness (#2706). `sha256/8` = the first 8
                // hex chars of the sha256; `digest` always returns 64 ASCII
                // hex chars, so the slice cannot panic.
                let identity = &crate::foundry_gate::digest(content.as_bytes())[..8];
                ToolOutput::Ok {
                    content: format!("saved {key} ({} bytes, sha256/8 {identity})", content.len()),
                    data: None,
                }
            }
            Err(e) => ToolOutput::error(format!("failed to save {key}: {e}")),
        }
    }
}

/// `get_state` — read one scratch entry, paged.
pub struct GetState(pub Arc<ScratchDir>);

#[async_trait]
impl Tool for GetState {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "get_state".into(),
            description: "Read a saved scratch entry by key. Pages by byte offset/limit; a \
                partial read names the remaining bytes. Prefer this over re-deriving the \
                state that produced it."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "key": {"type": "string"},
                    "offset": {"type": "integer", "description": "Byte offset to start from (default 0)."},
                    "limit": {"type": "integer", "description": "Max bytes to return (default and cap 30720)."}
                },
                "required": ["key"]
            }),
            read_only: true,
            speculation_safe: true,
        }
    }

    async fn execute(&self, input: &Value, _ctx: &crate::ctx::ToolCtx) -> ToolOutput {
        let key = match str_field(input, "key") {
            Ok(k) => k,
            Err(message) => {
                return ToolOutput::error(message);
            }
        };
        let path = match self.0.resolve(key) {
            Ok(p) => p,
            Err(message) => {
                return ToolOutput::error(message);
            }
        };
        let offset = input.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .map_or(PAGE_CAP, |l| (l as usize).min(PAGE_CAP));

        let mut file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(_) => {
                return ToolOutput::error(format!(
                    "no state saved under {key:?} — list_state shows what exists"
                ));
            }
        };
        let total = file.metadata().map(|m| m.len()).unwrap_or(0);
        if let Err(e) = file.seek(SeekFrom::Start(offset)) {
            return ToolOutput::error(format!("seek failed on {key}: {e}"));
        }
        let mut buf = vec![0u8; limit];
        let mut read = 0usize;
        loop {
            match file.read(&mut buf[read..]) {
                Ok(0) => break,
                Ok(n) => read += n,
                Err(e) => {
                    return ToolOutput::error(format!("read failed on {key}: {e}"));
                }
            }
            if read == buf.len() {
                break;
            }
        }
        buf.truncate(read);
        let body = String::from_utf8_lossy(&buf);
        let end = offset + read as u64;
        let header = if end < total {
            format!(
                "[{key}: bytes {offset}..{end} of {total}; {} remain]\n",
                total - end
            )
        } else {
            format!("[{key}: bytes {offset}..{end} of {total}; end]\n")
        };
        ToolOutput::Ok {
            content: format!("{header}{body}"),
            data: None,
        }
    }
}

/// `list_state` — enumerate scratch entries.
pub struct ListState(pub Arc<ScratchDir>);

#[async_trait]
impl Tool for ListState {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "list_state".into(),
            description: "List saved scratch entries (key and size in bytes). Check here \
                before re-deriving anything expensive."
                .into(),
            input_schema: json!({"type": "object", "properties": {}}),
            read_only: true,
            speculation_safe: true,
        }
    }

    async fn execute(&self, _input: &Value, _ctx: &crate::ctx::ToolCtx) -> ToolOutput {
        let mut rows = Vec::new();
        let entries = match std::fs::read_dir(self.0.path()) {
            Ok(e) => e,
            Err(e) => {
                return ToolOutput::error(format!("scratch dir unreadable: {e}"));
            }
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
            rows.push((name, len));
        }
        rows.sort();
        // The structured half the contract's `output_schema` checks (#3285):
        // the same facts as the prose, as values. `list_state` is the first
        // declarer because the scratch plane is the reference tool shape.
        let data = json!({
            "entries": rows
                .iter()
                .map(|(key, bytes)| json!({ "key": key, "bytes": bytes }))
                .collect::<Vec<_>>()
        });
        if rows.is_empty() {
            return ToolOutput::ok_with_data("no scratch state saved this session", data);
        }
        let content = rows
            .iter()
            .map(|(key, bytes)| format!("{key}  {bytes} bytes"))
            .collect::<Vec<_>>()
            .join("\n");
        ToolOutput::ok_with_data(content, data)
    }
}

/// `delete_state` — invalidate one scratch entry.
pub struct DeleteState(pub Arc<ScratchDir>);

#[async_trait]
impl Tool for DeleteState {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "delete_state".into(),
            description: "Delete one scratch entry by key — use when saved state is stale or \
                superseded so later steps cannot read an invalidated value. The whole scratch \
                directory is deleted automatically at session end; delete_state is for \
                invalidation, not cleanup."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {"key": {"type": "string"}},
                "required": ["key"]
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, _ctx: &crate::ctx::ToolCtx) -> ToolOutput {
        let key = match str_field(input, "key") {
            Ok(k) => k,
            Err(message) => {
                return ToolOutput::error(message);
            }
        };
        let path = match self.0.resolve(key) {
            Ok(p) => p,
            Err(message) => {
                return ToolOutput::error(message);
            }
        };
        match std::fs::remove_file(&path) {
            Ok(()) => ToolOutput::Ok {
                content: format!("deleted {key}"),
                data: None,
            },
            Err(_) => ToolOutput::error(format!("no state saved under {key:?} — nothing deleted")),
        }
    }
}
