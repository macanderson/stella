//! The SETTINGS tab's tool-switch read models: the tools this session can
//! dispatch, and why any of them is off.
//!
//! Split out of `envelope.rs` (#3493) under #629's 1500-line ratchet. Only
//! the driver can enumerate the assembled tool stack — built-ins the
//! environment satisfied, each connected MCP server's, each custom tool the
//! workspace registered — so these rows arrive built and the panel derives
//! on/off from [`ToolPolicyState::switches`] alone rather than carrying a
//! second answer that can drift.

use std::collections::BTreeMap;

/// Which settings file carries the `"tools"` key that switched a tool off.
///
/// Not the same list as [`AgentScope`](super::AgentScope), and deliberately
/// so: the editor writes to two scopes but has to *report* three, because the
/// third one is the one it cannot write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolScope {
    /// The org-managed settings file. A denial here is a ceiling — no user or
    /// project switch can lift it, which is why such a row renders locked.
    Managed,
    /// `~/.stella/settings.json`.
    User,
    /// `<workspace>/.stella/settings.json`.
    Project,
}

impl ToolScope {
    pub fn label(self) -> &'static str {
        match self {
            ToolScope::Managed => "org-managed",
            ToolScope::User => "user",
            ToolScope::Project => "project",
        }
    }
}

/// Why one tool is off: the `"tools"` key that did it, and where that key
/// lives. Both halves matter — `"off"` alone leaves an operator hunting three
/// settings files for a switch they may not have written themselves, and the
/// key alone does not say which file to edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolDenial {
    /// The exact tool name, its group (`"process"`), or `"*"` — resolved
    /// most-specific-first, the same order the policy itself resolves.
    pub key: String,
    /// The scope whose file carries that key. `None` when the merged posture
    /// and the individual scopes disagree (files changed under the snapshot) —
    /// the row still reports the key rather than inventing a scope.
    pub scope: Option<ToolScope>,
}

/// One tool the session actually has, as the SETTINGS tab's tool editor lists
/// it. Driver-built (only the driver can see the assembled tool stack — MCP
/// servers and a customer's own registered tools exist nowhere else), and
/// deliberately NOT carrying an `enabled` flag: on/off is derived from
/// [`ToolPolicyState::switches`] plus the panel's unsaved edits, so there is
/// exactly one answer to "is this on?" rather than two that can drift.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRow {
    /// The exact dispatch name — and the exact settings key toggling this row
    /// writes.
    pub name: String,
    /// The family this tool belongs to (`"file"`, `"process"`, …, or `"mcp"` /
    /// `"custom"` for tools the catalog has never heard of). The editor's
    /// section headers, and the key a header toggle writes.
    pub group: String,
    /// The org-managed scope denies this tool. The row is read-only: it cannot
    /// be toggled on, and it must not render as though it could.
    pub locked: bool,
    /// Why it is off under the SAVED settings, or `None` when it is on.
    /// Unsaved edits in the panel do not change this — it describes disk.
    pub off: Option<ToolDenial>,
}

/// The tool-switch snapshot the SETTINGS tab's tool editor renders
/// ([`Inbound::ToolPolicy`](super::Inbound::ToolPolicy)).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolPolicyState {
    /// Every tool this session can dispatch, before the policy filters it:
    /// built-ins the environment satisfied, each connected MCP server's, and
    /// each custom tool the workspace registered. Sorted by group then name.
    pub tools: Vec<ToolRow>,
    /// The merged `"tools"` section in force — tool names, group names, and
    /// `"*"` mapped to on/off, with the org-managed ceiling already folded in.
    /// The panel resolves a row's live value against this map (plus its own
    /// unsaved edits) using the policy's own most-specific-first precedence.
    pub switches: BTreeMap<String, bool>,
}
