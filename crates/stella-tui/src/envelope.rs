//! Multi-agent wire types — the envelope that turns the single-session
//! `AgentEvent` stream into a workspace of many agents.
//!
//! The [`run_deck`](crate::deck_shell::run_deck)`(events, submissions)` shell speaks
//! one `AgentEvent` stream for one session. The command deck speaks
//! [`Inbound`] (an agent-id-tagged event) in and [`WorkspaceInput`] out, so N
//! agents share one deck. A single-agent session is just one [`AgentId`].
//!
//! This keeps the L-T1 purity per agent: each agent's derived state is still a
//! pure fold of *its* `AgentEvent`s; the envelope only adds the routing tag the
//! deck needs to keep N folds side by side.

pub(crate) mod broadcast;
mod engine_config;
/// Driver → deck: the [`Inbound`] envelope and the read models only it carries.
mod inbound;
mod inspect;
mod issues;
/// The MCP tab's read models (rows, search results, inspector detail).
mod mcp;
/// The `/models` role table: the open role vocabulary and its render order.
pub mod roles;
mod skills;
/// Why Esc steers rather than cancels — doc-only.
pub mod steering;
mod tool_policy;
/// Deck → driver: the [`WorkspaceInput`] envelope and the types only it carries.
mod workspace_input;

// Every name below keeps the public path it had when this module was one
// file, so no caller learns where anything went.
pub use broadcast::Broadcast;
pub use engine_config::{EngineAgentState, EngineConfigState, EngineRole, RoleWiringRow, SeatRow};
pub use inbound::{
    AgentScope, AgentVersionInfo, Inbound, InstalledAgentEntry, NotificationInfo, SessionInfo,
    SessionPhase, SplashCue,
};
pub use inspect::{InspectMessage, InspectSection, InspectView, JournalEra, RecordedCallInfo};
pub use issues::{EntityField, EntityHit, IssueAction, IssueRow, LinkedWork};
pub use mcp::{
    GRAPH_SERVER, McpLiveIdentity, McpLookupState, McpSearchItem, McpSearchOutcome,
    McpServerDetail, McpServerInfo, McpSignature, McpSourceTier, McpToolRow,
};
pub use roles::{RoleTableEntry, role_table};
pub use skills::{
    LearnedProvenance, LearnedSource, RejectedSkillRow, SkillOp, SkillRow, SkillScope,
    SkillSearchHit, SkillsView,
};
pub use tool_policy::{ToolDenial, ToolPolicyState, ToolRow, ToolScope};
pub use workspace_input::{AgentControl, KeepStrength, Secret, WorkspaceInput};

/// Stable identifier for one agent/run within the workspace. Human-meaningful
/// where possible (`"lead"`, `"sub:auth-refactor"`) — it is shown on screen, so
/// it is never a raw UUID as the primary label (the L-C4 cite-by-label spirit).
pub type AgentId = String;

/// Everything the dashboard needs to introduce an agent before its first event.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentMeta {
    pub id: AgentId,
    /// The project / goal shown in the dashboard row.
    pub title: String,
    /// `"lead"` | `"subagent"` | free-form role label.
    pub role: String,
    /// OS process id for CPU/MEM attribution, once known.
    pub pid: Option<u32>,
    /// The model handling this agent, once routed.
    pub model: Option<String>,
    /// The reasoning effort the agent's calls are pinned to (`low` … `max`),
    /// as the driver resolved it; `None` when nothing pinned one.
    pub effort: Option<String>,
    /// One sentence on what the agent is for — the task it was handed, in
    /// the words it was handed. `None` falls back to `title` on screen.
    pub purpose: Option<String>,
    /// The agent that dispatched this one — its place in the session's agent
    /// tree. `None` for a root (the lead). The deck walks this for Backspace
    /// (back to the dispatcher), the breadcrumb (`lead ▸ sub:2`), and the
    /// SUB-AGENTS overlay's `f` (flag the lane to whoever dispatched it):
    /// all three need the same answer, and a guess (`"lead"`) would be wrong
    /// the day a lane dispatches a lane.
    pub parent: Option<AgentId>,
    /// Wall-clock start (ms since epoch) for elapsed / $-per-hour.
    pub started_ms: u64,
}

impl AgentMeta {
    /// A minimal meta with the free-form defaults filled in.
    pub fn new(id: impl Into<AgentId>, title: impl Into<String>, started_ms: u64) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            role: "agent".to_string(),
            pid: None,
            model: None,
            effort: None,
            purpose: None,
            parent: None,
            started_ms,
        }
    }

    /// Builder: name the agent that dispatched this one.
    pub fn with_parent(mut self, parent: impl Into<AgentId>) -> Self {
        self.parent = Some(parent.into());
        self
    }

    /// Builder: set the pinned reasoning effort.
    pub fn with_effort(mut self, effort: impl Into<String>) -> Self {
        self.effort = Some(effort.into());
        self
    }

    /// Builder: set the one-sentence purpose.
    pub fn with_purpose(mut self, purpose: impl Into<String>) -> Self {
        self.purpose = Some(purpose.into());
        self
    }

    /// Builder: set the role.
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.role = role.into();
        self
    }

    /// Builder: set the OS pid (for resource attribution).
    pub fn with_pid(mut self, pid: u32) -> Self {
        self.pid = Some(pid);
        self
    }
}

/// The lifecycle status of an agent. Most transitions are derivable from the
/// `AgentEvent` stream (a `Stage` means running, `Complete` means done, an
/// `Error` means failed, `AskUser` means waiting) — but `Queued`, `Paused`,
/// and `Killed` are supervisor states that are *not* in the event stream, so
/// they arrive via [`Inbound::Status`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentStatus {
    Queued,
    Running,
    Paused,
    WaitingInput,
    Done,
    Failed,
    Killed,
}

impl AgentStatus {
    /// True while the agent is actively holding resources / dispatchable.
    pub fn is_active(self) -> bool {
        matches!(self, AgentStatus::Running | AgentStatus::WaitingInput)
    }

    /// True once the agent has reached a terminal state.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            AgentStatus::Done | AgentStatus::Failed | AgentStatus::Killed
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            AgentStatus::Queued => "queued",
            AgentStatus::Running => "running",
            AgentStatus::Paused => "paused",
            AgentStatus::WaitingInput => "needs input",
            AgentStatus::Done => "done",
            AgentStatus::Failed => "failed",
            AgentStatus::Killed => "killed",
        }
    }
}

/// One plugin panel the deck is permitted to draw, and everything the deck
/// needs to place it (SPEC 12.2).
///
/// It carries no process and no manifest. A seat is the *result* of a grant,
/// so the deck holds no way to start a plugin and no way to decide that it
/// may: the driver owns both, and the deck owns the rectangle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelSeat {
    /// The manifest name, which the host stamps on the chrome and nothing else
    /// supplies (SPEC 12.3 — a plugin cannot caption its own border).
    pub plugin: String,
    /// Which of the three placements this seat is.
    pub surface: stella_plugin::PanelSurface,
    /// The bare slash name this panel's popup opens under, for a
    /// [`stella_plugin::PanelSurface::Command`] seat whose name the driver
    /// admitted. `None` on every other surface, and on a name that collided
    /// with a built-in — a refused name is a notice, never a quiet seat.
    pub command: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_and_terminal_are_disjoint() {
        for s in [
            AgentStatus::Queued,
            AgentStatus::Running,
            AgentStatus::Paused,
            AgentStatus::WaitingInput,
            AgentStatus::Done,
            AgentStatus::Failed,
            AgentStatus::Killed,
        ] {
            assert!(!(s.is_active() && s.is_terminal()), "{s:?}");
        }
    }

    #[test]
    fn meta_builder_sets_fields() {
        let m = AgentMeta::new("lead", "acme-api", 1000)
            .with_role("lead")
            .with_pid(4242);
        assert_eq!(m.id, "lead");
        assert_eq!(m.role, "lead");
        assert_eq!(m.pid, Some(4242));
    }
}
