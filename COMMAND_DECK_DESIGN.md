# Stella Command Deck — TUI design contract

Transforms `stella-tui` from a single-session event-log REPL into a multi-tab,
multi-agent **operations deck**, preserving the existing pure-core / thin-shell
discipline (L-T1: render exclusively from events; ephemeral view state never in
the model).

This file is the **frozen contract** leaf-view builders code against. Types
here are authoritative — do not change signatures without updating this doc.

## Module layout (`stella-tui/src/`)

Existing (kept, reused by the Session tab — do not break):
`lib, model, render, shell, ui, composer, scroll, input`.

New:
- `envelope.rs` — the multi-agent wire types: `AgentId`, `AgentMeta`,
  `Inbound`, `AgentStatus`, and the outbound `WorkspaceInput`, `AgentControl`.
- `deck.rs` — `WorkspaceModel` (holds N per-agent `SessionModel`s + shared
  read-models), `AgentEntry`, `DeckTab`, and `apply_inbound()`.
- `theme.rs` — color/style tokens. One source of truth for look (Stella amber
  `#FFAC26` accent, semantic status colors, glyphs).
- `resource.rs` — `ResourceMonitor` (sysinfo): global CPU%, per-pid CPU%/MEM.
  `ResourceSample { cpu_pct, mem_bytes }`.
- `fx.rs` — tachyonfx effect helpers (content fade-in, tab transition, sweep).
- `splash.rs` — the animated branded splash (tui-big-text + tachyonfx), skippable.
- `deck_render.rs` — top-level frame: comfy-tabs bar + active view + status bar
  + splash overlay. The tab dispatcher.
- `deck_shell.rs` — `run_deck(...)`: the new async loop (events + keys + a
  ~16ms animation/resource tick).
- `views/{session,agents,traces,graph,files}.rs` — one module per tab.

## Frozen core types

```rust
// envelope.rs
pub type AgentId = String; // stable per agent/run, e.g. "lead", "sub:auth"

#[derive(Clone, Debug, PartialEq)]
pub struct AgentMeta {
    pub id: AgentId,
    pub title: String,        // project/goal shown in the dashboard
    pub role: String,         // "lead" | "subagent" | ...
    pub pid: Option<u32>,     // OS pid for CPU/MEM attribution
    pub model: Option<String>,
    pub started_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentStatus { Queued, Running, Paused, WaitingInput, Done, Failed, Killed }

/// One item on the workspace inbound channel — the multi-agent envelope.
#[derive(Clone, Debug)]
pub enum Inbound {
    Register(AgentMeta),                         // a new agent row appears
    Event { agent: AgentId, event: AgentEvent }, // an AgentEvent for one agent
    Status { agent: AgentId, status: AgentStatus }, // supervisor lifecycle state
}

/// Outbound: what the deck sends back to the caller/engine.
#[derive(Clone, Debug, PartialEq)]
pub enum WorkspaceInput {
    ToAgent { agent: AgentId, input: UserInput }, // route a UserInput to an agent
    Enqueue { text: String },                     // non-blocking new prompt
    Control { agent: AgentId, control: AgentControl },
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentControl { Pause, Resume, Stop, Restart }
```

```rust
// deck.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeckTab { Session, Agents, Traces, Graph, Files }
impl DeckTab { pub const ALL: [DeckTab; 5] = [ /* … */ ]; pub fn title(self) -> &'static str; }

pub struct AgentEntry {
    pub meta: AgentMeta,
    pub model: SessionModel,      // the existing pure per-agent fold
    pub status: AgentStatus,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost_usd: f64,
    pub last_activity_ms: u64,
    pub res: ResourceSample,      // stamped by ResourceMonitor (out-of-band)
    pub activity: ActivitySpark,  // ring buffer of recent intensity for a sparkline
}

pub struct WorkspaceModel {
    pub agents: Vec<AgentEntry>,  // insertion-ordered; find by meta.id
    pub focused: usize,           // index into agents
    pub ledger: FileLedger,       // cross-agent file CRUD + line +/-
    pub routes: RouteLog,         // prompt→model decisions
    pub queue: PromptQueue,       // pending prompts (non-blocking submit)
    pub trace: TraceLog,          // unified cross-agent event ring buffer
    pub now_ms: u64,              // last tick time, for elapsed/$per-hr
}

impl WorkspaceModel {
    pub fn apply_inbound(&mut self, inbound: &Inbound);   // the sole event fold
    pub fn focused_agent(&self) -> Option<&AgentEntry>;
}
```

## Read-models (derived; each has a builder subagent)

- `FileLedger` (`views/files.rs` owns the type): per (agent, path) a
  `FileRecord { path, agent, kind: FileChangeKind, added: u32, removed: u32,
  changes: u32 }`. `added`/`removed` are parsed from the `FileChange.diff`
  unified-diff string (count `+`/`-` lines, ignore `+++/---` headers and `@@`).
  Aggregates: total files, total +/-.
- `RouteLog`: append a `RouteEntry { prompt_snippet, model, cost_usd, ts }` on
  each `StepUsage`/`Complete`; surfaces which model handled what.
- `PromptQueue`: `Vec<QueuedPrompt { text, ts, dispatched: bool }>`. Submitting
  a prompt ALWAYS enqueues and returns — never blocks on a busy agent.
- `TraceLog`: ring buffer of `TraceRow { ts, agent, kind, summary }` across all
  agents, filterable by agent.
- `ActivitySpark`: fixed-size ring (e.g. 32) of `u8` intensity, one bar per
  recent tick; rendered as a sparkline in the dashboard.

## Tabs & keybindings

| Tab | # | Contents |
|---|---|---|
| Session | 1 | the existing REPL for the focused agent (reuse `render`) |
| Agents  | 2 | claudectl-style table: agent · status · context% · cost · $/hr · elapsed · **CPU%** · MEM · in/out · activity sparkline |
| Traces  | 3 | unified cross-agent event timeline, filter by agent |
| Graph   | 4 | code-graph inspector (node/edge canvas from `CodeGraph::neighbors`) |
| Files   | 5 | file ledger: every file touched, CRUD op + lines +/−, totals |

- Tab switch: `Tab`/`Shift-Tab` cycle; comfy-tabs also handles mouse click/scroll.
- Global: `Ctrl-C` quit · composer is always focusable and **enqueues without blocking**.
- Agents tab: `↑/↓` select · `p` pause · `s` stop · `r` restart · `Enter` focus that agent's Session.
- Graph tab: `↑/↓/←/→` move cursor · `Enter` expand · `/` search symbol.
- Bottom status bar (always visible): routed model · **global CPU% gauge** · total spend + $/hr · active agents · queue depth.

## The purity boundary (L-T1, honored explicitly)

- **Event-pure (replayable):** every per-agent `SessionModel`, plus
  `FileLedger`, `RouteLog`, `PromptQueue`, `TraceLog` — all a deterministic
  fold of the `Inbound` stream.
- **Out-of-band read-models (labeled, NOT from events):** `ResourceSample`
  (sampled from the OS via sysinfo on the tick) and the code-graph snapshot
  (queried from `stella-graph`). These are the only two exceptions and they are
  named as such — no other view may read state that isn't in the event fold.

## Backend seams (what's live vs. seam-fed today)

- Live now: per-agent event fold, file +/- from diffs, routing from
  `StepUsage`/`Complete`, graph via `CodeGraph::neighbors`, CPU/MEM via sysinfo.
- Seam-fed (no backend supervisor yet — build UI against the seam, drive with
  the scenario feed): multi-agent `Register`/`Status` envelopes and
  `AgentControl` (Pause/Stop/Restart). `Stop` maps to `UserInput::Cancel`
  today; deep per-agent pause/kill needs a new `stella-fleet` abort API (noted
  as the follow-up integration).

## Build/test rules for every subagent (verbatim)

- Work in `/Users/macanderson/Workspaces/stella` on branch
  `feat/stella-command-deck-tui`. Commit on that branch with explicit pathspecs.
- **NEVER run all tests — hard rule.** Run ONLY `cargo test -p stella-tui`
  (scoped). Never a whole-workspace build/test. CI is the full gate.
- New pure logic (folds, diff parsing, ledger math) needs co-located unit tests
  that assert on the model/buffer, not ANSI. No `unwrap` in render paths that a
  malformed event could reach.
