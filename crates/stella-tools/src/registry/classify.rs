//! Pre-execution file-op classification for [`super::ToolRegistry`] — the
//! `[C|R|U|D]` reading of a tool call's *input* that feeds the file ledger
//! and the pre-write gate. A child module of `registry` so the ledger types
//! stay reachable, split out to keep the registry under the size gate.

use super::*;

impl ToolRegistry {
    /// [`Self::classify_file_op`] generalized to tools that touch several
    /// files in one call: `apply_edits` yields one Update per distinct file
    /// in its batch (a dry run yields none — nothing is written), everything
    /// else defers to the single-path classification.
    pub(super) fn classify_file_ops(&self, tool: &str, input: &Value) -> Vec<PendingTouch> {
        if tool != "apply_edits" {
            return self.classify_file_op(tool, input).into_iter().collect();
        }
        if input
            .get("dry_run")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return Vec::new();
        }
        let Some(edits) = input.get("edits").and_then(|v| v.as_array()) else {
            return Vec::new();
        };
        let mut seen = std::collections::HashSet::new();
        let mut ops = Vec::new();
        for edit in edits {
            let Some(raw) = edit.get("path").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(path) = normalize_workspace_path(&self.root, raw) else {
                continue;
            };
            if seen.insert(path.clone()) {
                ops.push(PendingTouch {
                    path,
                    op: FileOp::Update,
                });
            }
        }
        ops
    }

    /// `[C|R|U|D]`-classify a single-path call: reads → R, writes → C (new) or
    /// U (existing), edits → U, deletes → D. The path is normalized to its
    /// workspace-relative POSIX form here, so equivalent spellings
    /// (`src/./a.rs`, `src/../src/a.rs`) aggregate into one ledger record;
    /// escaping paths classify as `None` (the tool rejects them anyway).
    /// `bash` is opaque — file ops done through the shell aren't
    /// attributable, which is why the CRUD tools exist and the prompt steers
    /// agents toward them.
    pub(super) fn classify_file_op(&self, tool: &str, input: &Value) -> Option<PendingTouch> {
        let raw = input.get("path").and_then(|v| v.as_str())?;
        let path = normalize_workspace_path(&self.root, raw)?;
        let op = match tool {
            "read_file" => FileOp::Read,
            "edit_file" => FileOp::Update,
            "delete_file" => FileOp::Delete,
            // `web_download` lands a file exactly like `write_file`, so it
            // takes the same ledger classification and hook gating.
            "write_file" | "web_download" => {
                if crate::rootfd::exists_confined(&self.root, &path) {
                    FileOp::Update
                } else {
                    FileOp::Create
                }
            }
            _ => return None,
        };
        Some(PendingTouch { path, op })
    }
}
