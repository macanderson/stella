// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One `call_id -> name` lookup, shared by every surface that needs it
//! (`#2450`).
//!
//! `AgentEvent::ToolResult` names only the `call_id` it answers. The tool's
//! own name is on the matching `ToolStart`, sent earlier. Any surface that
//! labels a result row must save the pairing itself. Each surface kept its
//! own copy: the interactive terminal scans its saved log for the match,
//! and the plain-text renderer kept a private map at its own call site.
//!
//! That is the copy `#2421` closes: two copies of one fact, free to drift
//! apart. A third caller — a future format, or a remote host — would need a
//! third copy. This type is the one to share instead.
//!
//! It holds two fields: a tool's name and its sub-agent id. That is all a
//! result card needs. The interactive terminal's own lookup finds more (a
//! file path, a plan-gate refusal) and keeps doing that. This type does not
//! replace it.

use std::collections::HashMap;

/// What one `ToolStart` recorded, kept until its `ToolResult` arrives.
struct Started {
    name: String,
    sub_agent_id: Option<String>,
}

/// The `call_id -> (name, sub_agent_id)` table. Insert on `ToolStart`, read
/// on `ToolResult`.
#[derive(Default)]
pub struct ToolCallIndex {
    started: HashMap<String, Started>,
}

/// The name a `ToolResult` gets when this table has no matching `ToolStart`
/// — a resumed session with a gap in its log, say. A caller can always
/// print this; it never has to unwrap an absent value.
pub const UNKNOWN_TOOL_NAME: &str = "tool";

impl ToolCallIndex {
    /// A table with nothing recorded yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one call's name (and its sub-agent, if it has one). A second
    /// `ToolStart` for the same `call_id` replaces the first, since the wire
    /// never sends two.
    pub fn observe_start(&mut self, call_id: &str, name: &str, sub_agent_id: Option<&str>) {
        self.started.insert(
            call_id.to_string(),
            Started {
                name: name.to_string(),
                sub_agent_id: sub_agent_id.map(str::to_string),
            },
        );
    }

    /// The name and sub-agent a `ToolResult` for `call_id` belongs to.
    ///
    /// A plain lookup, not a `take`: reading the same `call_id` twice gives
    /// the same answer both times, so a re-render of an old result still
    /// labels it correctly.
    #[must_use]
    pub fn resolve(&self, call_id: &str) -> (&str, Option<&str>) {
        self.started
            .get(call_id)
            .map(|s| (s.name.as_str(), s.sub_agent_id.as_deref()))
            .unwrap_or((UNKNOWN_TOOL_NAME, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The witness (`#2450`).** Without this type, this test cannot even
    /// compile: nothing offers one lookup shared across surfaces. Once it
    /// compiles, this is the contract every caller gets.
    #[test]
    fn a_result_resolves_the_name_and_sub_agent_its_start_recorded() {
        let mut index = ToolCallIndex::new();
        index.observe_start("call-1", "edit_file", Some("worker-a"));
        index.observe_start("call-2", "read_file", None);

        assert_eq!(index.resolve("call-1"), ("edit_file", Some("worker-a")));
        assert_eq!(index.resolve("call-2"), ("read_file", None));
    }

    /// A result with no matching start gets a safe placeholder name, not a
    /// crash.
    #[test]
    fn an_unseen_call_id_resolves_to_the_unknown_placeholder() {
        let index = ToolCallIndex::new();
        assert_eq!(index.resolve("never-started"), (UNKNOWN_TOOL_NAME, None));
    }

    /// A second start for one call id replaces the first. There is only one
    /// slot, so the newest start is the one a later result answers.
    #[test]
    fn a_second_start_for_one_call_id_replaces_the_first() {
        let mut index = ToolCallIndex::new();
        index.observe_start("call-1", "read_file", None);
        index.observe_start("call-1", "edit_file", Some("worker-a"));
        assert_eq!(index.resolve("call-1"), ("edit_file", Some("worker-a")));
    }
}
