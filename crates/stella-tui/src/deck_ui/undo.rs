// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `u` on a highlighted delete event — resolving the highlight to the paths
//! that one `delete_file` call removed (SPEC 6.3's `· git-backed · u undo`
//! affordance, SPEC 11's `u`). The key itself is routed in
//! `handle_session_key`; this module answers the one question that routing
//! needs: *is the highlight on a delete, and what did it delete?*

use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;
use crate::model::TranscriptEntry;

/// The delete event under the transcript highlight, as the paths that one
/// `delete_file` call removed — `None` for any other selection, which leaves
/// `u` to the composer.
///
/// Both rows of the visual block answer: the call head (which carries the
/// `· git-backed · u undo` label) and its paired result, resolved back to the
/// head by `call_id` — a reader's ↑ from the bottom lands on the result
/// first, and the affordance must not depend on knowing which of the two is
/// highlighted. A batch delete carries its targets in the call's raw argument
/// object rather than the head's `path`.
pub(super) fn selected_delete_paths(model: &WorkspaceModel, ui: &DeckUi) -> Option<Vec<String>> {
    let idx = ui.session_selected?;
    let transcript = &model.agents.get(ui.focused)?.model.transcript;
    let (name, path, raw) =
        match transcript.get(idx)? {
            TranscriptEntry::ToolStart {
                name, path, raw, ..
            } => (name, path, raw),
            TranscriptEntry::ToolResult { call_id, .. } => transcript
                .iter()
                .take(idx)
                .rev()
                .find_map(|entry| match entry {
                    TranscriptEntry::ToolStart {
                        call_id: start_id,
                        name,
                        path,
                        raw,
                        ..
                    } if start_id == call_id => Some((name, path, raw)),
                    _ => None,
                })?,
            _ => return None,
        };
    if name != "delete_file" {
        return None;
    }
    if let Some(path) = path {
        return Some(vec![path.clone()]);
    }
    let parsed: serde_json::Value = serde_json::from_str(raw).ok()?;
    let paths: Vec<String> = parsed
        .get("files")?
        .as_array()?
        .iter()
        .filter_map(|f| Some(f.get("path")?.as_str()?.to_string()))
        .collect();
    (!paths.is_empty()).then_some(paths)
}
