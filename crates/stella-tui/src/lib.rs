// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella-tui` — the ratatui event-log REPL.
//!
//! This crate renders **exclusively** from [`stella_protocol::AgentEvent`]s
//! (L-T1). It never touches the engine directly: `AgentEvent`s flow in over a
//! channel, [`UserInput`]s flow back out. The design is two layers:
//!
//! - **A pure core.** [`SessionModel`] folds the append-only event log into
//!   derived state — transcript lines, the files-touched map, HUD numbers, the
//!   pending scope-review — via its single mutator [`SessionModel::apply`]. No
//!   panel owns state that isn't reconstructible by replaying the log from seq
//!   1 (so replay is a supported debug mode, and the panic boundary is sound).
//!   [`render_deck`] draws that model into a `ratatui` frame as a
//!   deterministic function of `(model, ui)`. Ephemeral interaction state
//!   (scroll, composer, focus) lives in [`DeckUi`], never in the model.
//!
//! - **A thin shell.** [`run_deck`] wires the pure core to a real terminal:
//!   raw mode + alternate screen (always restored on drop), the crossterm
//!   event loop, and the two channels. It carries no decision logic —
//!   key→action is [`handle_deck_key`], event→state is [`ingest_inbound`],
//!   both unit-tested.
//!
//! The Command Deck is the only surface. A second, single-session shell
//! (`shell::run` + `ui` + a top-level `render` composer) lived here until
//! #936: it was a divergent second implementation of composer layout, gate
//! handling, and slash wiring that no product path reached, and its tests
//! made the suite overstate what it protected. It was deleted; its leaf
//! panels — the ones the deck actually draws with — stayed, and now live in
//! [`mod@render`] with a single caller.
//!
//! That is also why **accessibility is a mode on the deck, not a surface
//! beside it** ([`mod@accessible`], #1258): a separate accessible surface is a
//! permanent second-class tier, since every deck feature shipped afterwards
//! becomes a new gap it never closes. [`DeckOptions::accessible`] runs the same
//! `run_deck` on the user's own screen, with settled transcript entries moving
//! into the terminal's scrollback, panels in one column, and the grid views as
//! labelled text.
//!
//! The binding TUI requirements are honored
//! structurally: event-derived rendering (L-T1), mouse-off-by-default for
//! native copy (L-T2, [`DeckOptions::mouse_capture`]), paste chips (L-T3,
//! [`Composer::paste`]), line-exact scroll (L-T4, [`ScrollState`]), diffs on
//! the single event path (L-T5, [`model::FileState`]), buffer-not-ANSI tests
//! (L-T6), the panel panic boundary (L-T7, `panel_guard` — covering the
//! deck's bands), and the debug channel (L-T8, [`DebugLog`]).

pub mod accessible;
pub mod ansi;
pub mod attach;
pub mod clipboard;
pub mod composer;
pub mod debug_log;
pub mod input;
pub mod model;
pub(crate) mod panel_guard;
pub mod render;
pub mod scroll;
pub(crate) mod term;
pub mod textline;

// ── Command Deck: the multi-tab, multi-agent operations workspace ───────────
// The tabbed deck (Session · Agents ·
// Traces · Graph · Files · Skills · MCP · Issues · Settings) while preserving
// the pure-core / thin-shell design.
pub mod cache_panel;
pub mod deck;
pub mod deck_render;
pub mod deck_shell;
pub mod deck_ui;
pub mod diff;
pub mod envelope;
pub mod fleet_dashboard;
pub mod graph;
pub mod markdown;
pub mod notice;
pub mod palette;
pub mod plan;
pub mod progress;
pub mod proof;
pub mod resource;
pub mod scenario;
pub mod splash;
pub mod statline;
pub mod syntax;
pub mod theme;
pub mod transcript_nav;
pub mod views;

pub use accessible::{FlushBlock, NOTICE_MARKER, Scrollback};
pub use ansi::strip_ansi;
pub use attach::probe_path_attachment;
pub use clipboard::{ClipboardPaste, default_attachments_dir};
pub use composer::{
    Composer, ComposerEntry, DEFAULT_PASTE_LINE_THRESHOLD, SlashCommand, SlashKind, SlashMenu,
    Submission,
};
pub use debug_log::DebugLog;
pub use input::{ScopeDecision, UserInput};
pub use model::{FileState, Hud, SessionModel, TranscriptEntry};
pub use scroll::ScrollState;
pub use textline::{EventLine, Tone, event_line};

// Command Deck public surface.
pub use deck::{
    AgentEntry, DeckTab, FileLedger, FileRecord, PrInfo, ResourceSample, RouteLog, TraceKind,
    TraceLog, TraceRow, WorkspaceModel,
};
pub use deck_render::render_deck;
pub use deck_shell::{DeckOptions, run_deck};
pub use deck_ui::{
    DeckAction, DeckUi, IssueField, IssuesMode, IssuesPanel, ScopeAction, SkillPrompt, SkillsFocus,
    SkillsPanel, TypeAhead, handle_deck_key, ingest_inbound,
};
pub use envelope::{
    AgentControl, AgentId, AgentMeta, AgentScope, AgentStatus, AgentVersionInfo, EngineAgentState,
    EngineConfigState, EngineRole, EntityField, EntityHit, Inbound, InspectMessage, InspectView,
    InstalledAgentEntry, IssueAction, IssueRow, McpLiveIdentity, McpLookupState, McpSearchItem,
    McpSearchOutcome, McpServerDetail, McpServerInfo, McpToolRow, NotificationInfo,
    RecordedCallInfo, Secret, SessionInfo, SessionPhase, SkillOp, SkillRow, SkillScope,
    SkillSearchHit, SkillsView, SplashCue, ToolDenial, ToolPolicyState, ToolRow, ToolScope,
    WorkspaceInput,
};
pub use fleet_dashboard::{
    FleetControl, FleetDashResult, FleetMsg, FleetStatus, TaskSummary, run as run_fleet_dashboard,
};
pub use graph::{GraphEdge, GraphNode, GraphSnapshot};
pub use resource::ResourceMonitor;
pub use splash::SplashState;
pub use views::settings::SettingsPane;
