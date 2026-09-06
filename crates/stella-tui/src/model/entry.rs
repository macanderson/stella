// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The semantic records the fold produces: [`TranscriptEntry`] and the payload
//! types that ride on it.
//!
//! Split out of `model.rs` so that file stays under the ungrandfathered
//! 1500-line ceiling rather than taking a baseline entry, which the baseline
//! refuses anyway (#4217, #3441). Pure relocation: no type, field, or doc
//! sentence changed in the move, and every name is re-exported from
//! [`mod@super`], so `crate::model::TranscriptEntry` still resolves and no call
//! site moved with it.
//!
//! The seam is declarations-versus-logic, which is why it is this cut and not
//! a slice of the fold: nothing here has a body. `super::SessionModel::apply`
//! constructs these, [`mod@crate::render`] turns them into styled rows, and
//! neither half needs to read the other to be understood. Styling is
//! deliberately absent — an entry carries content only (L-T1's replay
//! determinism extends through the renderer precisely because these are inert).

use stella_protocol::{
    BudgetMode, CiStatus, GateBoard, MediaJobState, MediaKind, MemoryClass, PrStatus,
    RevisionProposal, StageName, SubAgentStatus, Withholder,
};

use super::recall::{RecallBudget, RecalledFrameRow};
use super::turn::TurnOpening;

/// A parked wait that has not woken yet — the live half of the ⏳ chip.
///
/// Carries the park's own parameters rather than a reference into the
/// transcript, because the row and the chip answer different questions: the
/// transcript row is the log of a thing that happened, this is current state,
/// and the settled row must keep reading as history once the wake lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPark {
    /// What the wait is for, as the tool described it.
    pub description: String,
    /// Seconds between engine-side probes of the watched state.
    pub poll_interval_secs: u64,
    /// Seconds the park may last before it wakes with a timeout — the
    /// denominator of the countdown.
    pub deadline_secs: u64,
}

/// A pending `ask_user` question. The renderer contract is binding: present
/// the structured `options` **and** always exactly one additional free-text
/// affordance so the user can answer in their own words on every question
/// (`stella_protocol::AgentEvent::AskUser`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskUserPrompt {
    /// Correlates the answer back to the question (the `ask_user` tool call's
    /// `call_id`).
    pub id: String,
    pub question: String,
    pub options: Vec<String>,
}

/// How a sub-agent's child turn ended, for the finish row of
/// [`TranscriptEntry::SubAgent`].
///
/// `absorbed_messages` is the number worth reading: it is the transcript
/// growth the parent did NOT take on, and therefore does not re-send on
/// every later step. Cost sits beside it so the trade is legible.
#[derive(Debug, Clone, PartialEq)]
pub struct SubAgentSummary {
    pub status: SubAgentStatus,
    pub cost_usd: f64,
    pub steps: usize,
    pub absorbed_messages: usize,
    /// Why it did not complete; `None` when it did.
    pub reason: Option<String>,
}

/// One semantic entry in the transcript. Rendering (colour, borders, glyphs)
/// is applied by [`mod@crate::render`]; this type carries only content.
#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptEntry {
    /// Stands in for entries dropped by the retention cap
    /// (`MAX_TRANSCRIPT_ENTRIES`) — always the first entry when present.
    /// `count` is cumulative across eviction passes (a pass that drains an
    /// earlier marker absorbs its count), so the retained window stays a pure
    /// function of the event sequence and the monotonically growing count
    /// doubles as a cache-invalidation generation for consumers indexing the
    /// transcript (front-eviction shifts every retained index).
    Evicted { count: usize },
    /// A user-submitted prompt. Not an `AgentEvent` — the deck driver pushes
    /// this when `PromptStarted` arrives so the user's message is visible
    /// inline in the transcript, matching the Crush-style conversational layout.
    User(String),
    /// A stage boundary marker (`triage`, `plan`, `execute`, …) — or a stage a
    /// plugin contributed, under its own word. Open vocabulary, so the deck
    /// renders a stage it has never heard of instead of dropping it.
    Stage {
        name: StageName,
        /// Set on the one boundary that **opens** a turn — SPEC 6.1's labelled
        /// rule. `None` on every later boundary of the same turn, which stays
        /// the plain section rule it has always been.
        opens: Option<TurnOpening>,
    },
    /// Accumulated assistant natural-language output.
    Text(String),
    /// Accumulated model reasoning (rendered dimmed).
    Reasoning(String),
    /// A tool invocation began.
    ToolStart {
        call_id: String,
        name: String,
        input: String,
        /// The call's compact-JSON arguments, capped — the expanded (ctrl+o)
        /// view pretty-prints these; `input` stays the humanized one-liner.
        raw: String,
        /// The workspace-relative path the call targets, parsed from its
        /// input's `path` field (every file tool uses that key). Retained so a
        /// mutating tool's `ToolResult` can be correlated back to the file it
        /// touched without re-parsing the elided input summary.
        path: Option<String>,
        /// Which sub-agent made this call — `AgentEvent::ToolStart`'s field of
        /// the same name, carried straight through. `None` is the lead's own
        /// call, which is the ordinary case (#4699).
        sub_agent_id: Option<String>,
        /// The board task this call was tagged with, rendered `→ task 3` on
        /// the head (SPEC 6.2).
        ///
        /// `ToolStart` is a declared task-tag carrier
        /// (`stella_protocol::event::task_tag`), and the tag was being stamped
        /// and then dropped here: nothing in this crate read
        /// `AgentEvent::task_id`, so the head SPEC 6.2 renders the tag on had
        /// nothing to render (#5030).
        ///
        /// A `u32` rather than the protocol's `TaskId`: the board's ids are
        /// per-session ordinals spelled as strings (`"1"`, `"2"`), and the tag
        /// is the ordinal. An id that does not parse as one carries no tag
        /// rather than a tag the reader cannot act on.
        task: Option<u32>,
    },
    /// A tool invocation finished — `ok` is `false` for a typed tool error.
    ToolResult {
        call_id: String,
        /// Resolved from the matching `ToolStart` at fold time so call and
        /// result rows read as one aligned pair.
        name: String,
        /// The path the call targeted, resolved from the same `ToolStart` as
        /// `name` and by the same lookup.
        ///
        /// Carried so the renderer can tell what *language* a result body is
        /// written in: a `read_file` output is the named file's source, and
        /// the extension is the only evidence of that on the entry. Without it
        /// the deck could colour a body only when the body itself announced
        /// its format — which is why a file read went uncoloured for every
        /// language, JSON included (#4019).
        path: Option<String>,
        ok: bool,
        summary: String,
        /// The output, capped at `OUTPUT_BUDGET` chars — the collapsed row
        /// shows one line; ctrl+o reveals this.
        full: String,
        duration_ms: u64,
        /// True when the result was produced by speculative execution
        /// (the call ran while the model was still streaming); the renderer
        /// marks these so overlap is visible, since `duration_ms` alone
        /// would read as ordinary post-stream latency.
        speculated: bool,
        /// For a *successful* call that moved the work tree, one reference per
        /// change it made — the whole call's measurement, not a sample of it.
        ///
        /// Gated on the change having folded under this very call, not on the
        /// tool's name (`super::inline_diff`) — so a measured `bash`, MCP or
        /// custom-tool call renders what it did, exactly as `edit_file` does
        /// (#4213). **Empty** for reads, for failed calls, and for a call whose
        /// own measurement came back empty, which is what keeps the inline diff
        /// to mutations that actually happened. The diff itself is never stored
        /// here (L-T5: one event-borne diff path).
        ///
        /// A collection rather than an `Option` because one call can mutate
        /// several paths — an `apply_edits` batch, a `bash` codemod — and the
        /// per-call producer emits one `FileChange` for each (#4214). The
        /// first entry is the change the row shows inline; the rest are
        /// counted by the row and revealed with ctrl+o. The ordering is the
        /// claim window's (`super::inline_diff::ClaimWindow::claim`) and the
        /// renderer does not re-derive it.
        diff: Vec<InlineDiffRef>,
        /// The line coverage the tool reported for itself in its structured
        /// `data` payload — `read_file`'s producer (#4297). Its own carrier,
        /// deliberately not a widening of the mutation path above: a read
        /// stamps no [`InlineDiffRef`] because it changes nothing, and the
        /// number here is the producer's own measurement off the wire, never
        /// a recount of the rendered body (#2290's defect) — that body is
        /// capped at `OUTPUT_BUDGET` besides. `None` for every tool that
        /// reports no coverage, which is every result written before the
        /// payload existed.
        read_size: Option<ReadSize>,
        /// What the workspace code graph said about this call's own path,
        /// off the same structured `data` half of the wire (#5034).
        ///
        /// `None` whenever nothing measured it — a tool that publishes no
        /// graph fact, a workspace with no index, a result written before the
        /// payload existed. The row renders nothing in that case rather than
        /// a plausible zero: a deletion claiming a clean graph check it never
        /// ran is worse than a deletion that says nothing.
        graph: Option<GraphFact>,
        /// Resolved the same way as `name` and `path`: from the matching
        /// `ToolStart` when one is on the transcript, else this result's own
        /// `AgentEvent::ToolResult.sub_agent_id` — the fallback a consumer
        /// that never saw the start still needs (#4699).
        sub_agent_id: Option<String>,
    },
    /// One settled model call — SPEC 6.3's `◐ model <activity> · tok/s`.
    ///
    /// Folded from [`stella_protocol::AgentEvent::StepUsage`], which the driver
    /// already emits once per call with the call's own output tokens and its
    /// own wall clock (`turn::model_call_row`). The rate is a division
    /// over two numbers `stella-core` measured, so nothing new rides the wire
    /// to carry it.
    Model {
        /// What the call was for, as the wire spells the role — `worker`,
        /// `summarization`, `reflection`.
        ///
        /// `None` is [`stella_protocol::ModelCallRole::Unknown`]: a stream
        /// recorded before call-role attribution existed. The head then names
        /// no activity at all rather than printing `unknown`, on the rule
        /// `turn::supplies_the_turns_model` already applies to that
        /// role — a call this build cannot identify states no fact about
        /// itself.
        activity: Option<String>,
        /// Output tokens per second over the call's own wall clock, or `None`
        /// when the division has no inputs to divide (see
        /// `turn::model_call_row` for the three cases).
        tokens_per_sec: Option<u32>,
        /// The call's own wall clock, rendered `⚡Nms` like every other head.
        duration_ms: u64,
        /// Which delegate made the call, `None` for the lead's own (#4699).
        sub_agent_id: Option<String>,
    },
    /// A model call was retried (surfaced only once the step commits — the
    /// engine defers these, L-E10).
    Retry { attempt: u32, reason: String },
    /// The turn parked on an engine-side wait (#1857) — kept typed rather
    /// than folded into `Text` so the deck can render a park distinctly from
    /// model narration, which was the whole point of the wire pair.
    Parked {
        description: String,
        poll_interval_secs: u64,
        deadline_secs: u64,
    },
    /// The parked turn woke; `reason` is the closed `WakeReason` token.
    Woken { reason: String, polls_used: u64 },
    /// A compaction pass ran.
    Compaction {
        before_tokens: u64,
        after_tokens: u64,
        evicted: usize,
        deduped: usize,
    },
    /// A spend tick (also folded into [`super::Hud`]); kept on the transcript as a
    /// dim one-liner so the money trail is visible in scrollback.
    BudgetTick {
        spent_usd: f64,
        limit_usd: Option<f64>,
        mode: BudgetMode,
    },
    /// A provider circuit breaker opened and the router fell back — never
    /// silent (L-M7).
    ProviderFallback {
        from: String,
        to: String,
        reason: String,
    },
    /// The trust gate held back this checkout's steering (#2302, #3616,
    /// #4463): counts of what was not loaded, and which authority withheld
    /// it — because the remedy differs and only one of the two is something
    /// the user can act on.
    ///
    /// Counts only. The withheld text is repository-controlled and the wire
    /// event carries none of it, so there is no filename or body here to
    /// render even by accident.
    ///
    /// A session-level fact rather than a step of a turn, kept on the
    /// transcript anyway because that is the only surface the deck has: the
    /// stderr line the plain door prints is swallowed under the alternate
    /// screen, so without a row the deck told the user nothing at all.
    SteeringWithheld {
        withheld_by: Withholder,
        memories: usize,
        records: usize,
        skills: usize,
        commands: usize,
        agents: usize,
    },
    /// One workspace skill entered this turn's context (SPEC 6.3).
    ///
    /// The complement of [`Self::SteeringWithheld`]'s `skills` count, which
    /// says how many were held back and never which ones: a user could see
    /// that steering was withheld and still not learn that anything fired.
    Skill {
        /// The skill's slug — the head's subject.
        name: String,
        /// The skill's own one-line description, rendered `injected <summary>`.
        summary: String,
        /// What the injected block cost.
        tokens: u32,
        /// Which channel put it in the prompt — the head's `auto|/cmd`.
        trigger: stella_protocol::SkillTrigger,
    },
    /// Context recall completed; frames are cited by human label, never raw
    /// id (L-C4).
    ///
    /// The frames are kept **whole** rather than projected to a label list.
    /// A recall is five-or-so records with four attributes each — a small
    /// table — and this entry used to hold `{ frames: usize, tokens: u32,
    /// labels: Vec<String> }`, which is a table with three of its four
    /// columns deleted. Every surface downstream then had no choice but to
    /// comma-join the labels into a paragraph that wrapped mid-word at the
    /// pane edge: a symbol frame and an episodic memory rendered identically,
    /// the per-frame cost was unrepresentable, and `ctrl+o` had nothing left
    /// to reveal. Keeping the row means the renderer decides what to elide,
    /// which is the only layer that knows the width.
    ContextRecall {
        frames: Vec<RecalledFrameRow>,
        /// Sum of the frames' token costs — the turn's context bill.
        tokens: u32,
        /// Wall-clock milliseconds recall took. `0` means *not measured*, not
        /// "instant" (the wire contract, `AgentEvent::ContextRecall`), so the
        /// renderer omits it at `0` rather than printing a false `0ms`.
        latency_ms: u32,
        /// Whether the ANN index fired; `None` when the recall path did not
        /// report it. Tri-state on the wire for a reason — see the protocol
        /// event — and carried tri-state here so the deck can say "nobody
        /// said" instead of "the index never fires".
        used_ann_index: Option<bool>,
        /// `(provider, frames)` — the recall's provider mix, already folded
        /// on the wire.
        providers: Vec<(String, u32)>,
        /// The CGP budget report, when a host produced one.
        budget: Option<RecallBudget>,
    },
    /// Context write-back completed.
    ContextWrite {
        provider: String,
        upserts: u32,
        superseded: u32,
    },
    /// One memory landed — SPEC 6.3's `memory` (log) event.
    ///
    /// Every field is settled, none optional: the event is emitted after the
    /// memory has landed, so there is no in-flight state for it the way there
    /// is for a tool call between its dispatch and its result.
    MemoryLog {
        /// The `nod_…` handle `x reject` names and `stella memory forget`
        /// takes.
        memory_id: String,
        text: String,
        class: MemoryClass,
        /// `0..=100`, rendered `conf 0.62`.
        confidence: u8,
        /// The store's own word — `reflection`, `note`, `insight`. Open, so it
        /// is carried as text rather than mapped onto an enum this crate would
        /// then have to keep in step with the store's.
        kind: String,
        decays: bool,
        /// `0..=100`, the confidence at which it reaches the next rung.
        promotes_at: u8,
    },
    /// A memory moved up the ladder — SPEC 6.3's `memory` (promote) event.
    MemoryPromote {
        /// The lesson's lineage, shared by its proposal and its published
        /// record — see the wire event for why this is not a memory's own id.
        lineage_id: String,
        from: MemoryClass,
        to: MemoryClass,
        confidence: u8,
        /// The governance record the promotion is auditable through.
        audit_event_id: String,
    },
    /// A media job changed state. The wire enum is retained (not a label)
    /// so the renderer can distinguish failure — labeling is wording, and
    /// wording lives in [`crate::textline`].
    MediaProgress {
        artifact_id: String,
        kind: MediaKind,
        state: MediaJobState,
    },
    /// A media artifact landed on disk.
    MediaComplete {
        label: String,
        path: String,
        kind: MediaKind,
    },
    /// A verification verdict — `deterministic` distinguishes the flip-oracle
    /// ladder from a model verifier (L-E11).
    Verdict {
        passed: bool,
        summary: String,
        deterministic: bool,
    },
    /// A verify turn's gate board (`AgentEvent::GateBoard`) — SPEC 8.1.
    ///
    /// The board rides whole rather than as one entry per gate, because
    /// `^N jump` and the selection both work on entries: split into rows, a
    /// jump would land on a gate with its own header scrolled off, and `l` on a
    /// failure block would have no board to fold back into. The protocol type
    /// is carried unchanged — every field on it is already content with no
    /// styling, which is what an entry is (module docs).
    GateBoard { board: GateBoard },
    /// The plan revision a failing gate put up, and has not had answered —
    /// SPEC 8.1 item 3.
    ///
    /// A transcript row rather than an overlay, which is what lends it the
    /// bare `a` / `e` / `x` keys: `deck_ui::row_keys` admits a letter only
    /// when the composer is blank and the reader has highlighted the row,
    /// where `views::approval`'s card arrives unbidden over a composer
    /// somebody may be typing into and refuses bare letters for that reason.
    ///
    /// The proposal is carried whole. Every field on it is content the
    /// producer settled — `stella_store::plan_graph::RevisionGate` authors it
    /// from a board the host evaluated — and nothing here re-derives any of it.
    RevisionProposal { proposal: RevisionProposal },
    /// A goal-check verdict from the verifier loop (`AgentEvent::GoalVerdict`) —
    /// the symmetric scrollback row to [`Self::Verdict`]. `met` is the
    /// pass/fail; `round` is the verifier iteration it settled on.
    GoalVerdict {
        met: bool,
        round: usize,
        reasoning: String,
    },
    /// A bounded child turn started or finished (`AgentEvent::SubAgent`).
    /// The child's own narration never reaches this stream by design, so
    /// this pair of rows is the entire visible record that one ran — which
    /// is why the finish row carries what it cost AND what it saved.
    SubAgent {
        agent_id: String,
        /// `None` on the start row; `Some` once the child has finished.
        /// Keeping one variant for both keeps the bracket obviously paired
        /// in scrollback rather than two rows a reader has to associate.
        finished: Option<SubAgentSummary>,
        /// The start row's task preview; empty on the finish row.
        instruction_preview: String,
        write_access: bool,
    },
    /// A scope-review gate was presented (the actionable card is driven off
    /// [`super::SessionModel::pending_scope_review`]; this line is the scrollback
    /// record of it).
    ScopeReview {
        summary: String,
        steps: usize,
        estimated_files: u32,
    },
    /// An `ask_user` question was presented (the actionable card is driven off
    /// [`super::SessionModel::pending_ask_user`]; this line is the scrollback record).
    AskUser { question: String, options: usize },
    /// A per-hunk approval gate was presented (the actionable card is driven
    /// off [`super::SessionModel::pending_hunk_review`]; this line is the scrollback
    /// record). `files` is the distinct file count, which is the part a reader
    /// scanning back needs — "3 hunks" alone does not say whether one file or
    /// three were about to change.
    HunkReview {
        tool: String,
        hunks: usize,
        files: usize,
    },
    /// A commit landed.
    Commit { sha: String, message: String },
    /// A pull request opened or changed status.
    Pr {
        url: String,
        status: PrStatus,
        number: Option<u64>,
        ci: Option<CiStatus>,
    },
    /// A one-line digest of a task-board change ("tasks 2/5 · doing X").
    /// The full checklist renders from `SessionModel::tasks`, not from
    /// scrollback.
    TaskUpdate {
        done: usize,
        total: usize,
        active: Option<String>,
    },
    /// An error event.
    Error { message: String, retryable: bool },
    /// The turn completed.
    ///
    /// `turn` is this turn's ordinal in the session, 1-based, so the closing
    /// rule can name it (SPEC 6.1). Carried on the entry rather than counted at
    /// render time: the renderer sees one entry at a time and front-eviction
    /// shifts every index, so a position-derived number would renumber the
    /// whole scrollback the first time the retention cap bit.
    Complete {
        model: String,
        cost_usd: f64,
        turn: u32,
        /// What the turn's own counters settled to — SPEC 6.1's receipt.
        /// Rides the entry because `render::entry` holds no session state.
        receipt: super::TurnReceipt,
    },
}

/// A mutating tool result's handle on the diff it may render inline: the
/// path into [`super::SessionModel::files`] plus the `changes` seq of the mutation
/// *this call produced*. The renderer resolves it against
/// [`super::FileState::recent_diffs`], so a row shows the change its own call made
/// and never a neighbour's. Only the *reference* lives here; the diff bytes
/// stay on the single event-borne path (L-T5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineDiffRef {
    /// The key into [`super::SessionModel::files`].
    pub path: String,
    /// The [`super::FileState::changes`] value this call's own mutation is
    /// recorded at.
    ///
    /// Which value that is depends on **which producer measured the call**,
    /// and the fold decides it per call rather than assuming either (#4205).
    /// The registry measures a solo mutating call the moment it returns, so
    /// that change folds *before* this result and the seq is the counter as it
    /// stands; the turn boundary sweeps whatever no single call can own
    /// (`delegate`, a concurrent group, an external edit) and folds *after*
    /// every result, so there the seq is one past. See the `ToolResult` arm in
    /// [`super::SessionModel::apply`], and `tests::producer_seq` for both
    /// directions.
    ///
    /// It resolves for as long as that mutation is remembered, and dangles
    /// (rendering nothing) when nothing measured a net change to the path.
    pub seq: u32,
}

/// The line coverage a `read_file` result reported for itself (#4297).
///
/// Both numbers, because a truncated read must be statable as truncated:
/// rendering `total` alone would silently overstate what entered context, and
/// `shown` alone would silently understate the file. The pair comes off the
/// wire as the producer's structured `data` (`lines_shown` / `lines_total` in
/// `stella_tools::read`), never parsed back out of the rendered footer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadSize {
    /// Lines actually handed to the model — what a truncated read put in
    /// context.
    pub shown: u32,
    /// Lines in the file at read time.
    pub total: u32,
}

/// What the workspace code graph said about a mutated path (#5034).
///
/// The producer publishes one fact per path it touched
/// (`stella_tools::graph_fact`), so a batch carries several and the row
/// rendering one path picks its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphFact {
    /// Files whose imports resolved to the path, counted while it still
    /// existed. Absent rather than zero when the index had never seen it.
    InboundRefs(u32),
    /// The write that created the path registered it as a module node.
    RegisteredModule,
}

impl GraphFact {
    /// The fact a tool result reported for its own `path`, or `None` when it
    /// reported none.
    ///
    /// Resolved against the path rather than taken wholesale, because the
    /// payload is keyed by path so one batch call can carry a fact per file —
    /// which also means a result whose path the fold could not recover has no
    /// fact to render, and states none.
    #[must_use]
    pub fn for_result(output: &stella_protocol::ToolOutput, path: Option<&str>) -> Option<Self> {
        let stella_protocol::ToolOutput::Ok {
            data: Some(data), ..
        } = output
        else {
            return None;
        };
        Self::from_data(data, path?)
    }

    /// The fact this result reported for `path`, out of its structured
    /// `data`, or `None` when it reported none for that path.
    ///
    /// Lenient like the producer's own reader: a payload in a shape this
    /// version does not parse is a call with nothing to report, never an
    /// error at the seam that renders it.
    #[must_use]
    pub fn from_data(data: &serde_json::Value, path: &str) -> Option<Self> {
        data.get("graph_facts")?
            .as_array()?
            .iter()
            .find(|fact| fact.get("path").and_then(serde_json::Value::as_str) == Some(path))
            .and_then(
                |fact| match fact.get("fact").and_then(serde_json::Value::as_str)? {
                    "inbound_refs" => u32::try_from(fact.get("inbound")?.as_u64()?)
                        .ok()
                        .map(GraphFact::InboundRefs),
                    "registered" => Some(GraphFact::RegisteredModule),
                    _ => None,
                },
            )
    }
}

impl ReadSize {
    /// Both counts out of a tool result's structured `data`, or `None` when
    /// this result reported none for `path` — a coverage the fold cannot
    /// state exactly is not stated at all, never saturated into a plausible
    /// lie.
    ///
    /// The single form's payload is a flat `{lines_shown, lines_total}`
    /// pair with no path of its own. `path` decides nothing there. A
    /// batched `read_file` call's payload is a `files` array instead, one
    /// entry per target, each with its own `path`. `path` picks out the
    /// row's own lead entry — the same rule [`GraphFact::from_data`]
    /// already uses for a batch's graph facts.
    #[must_use]
    pub fn from_data(data: &serde_json::Value, path: Option<&str>) -> Option<Self> {
        fn counts(value: &serde_json::Value) -> Option<ReadSize> {
            let count = |key: &str| u32::try_from(value.get(key)?.as_u64()?).ok();
            Some(ReadSize {
                shown: count("lines_shown")?,
                total: count("lines_total")?,
            })
        }
        if let Some(size) = counts(data) {
            return Some(size);
        }
        let path = path?;
        data.get("files")?.as_array()?.iter().find_map(|entry| {
            (entry.get("path").and_then(serde_json::Value::as_str) == Some(path))
                .then(|| counts(entry))
                .flatten()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ReadSize;

    /// A batched `read_file`'s `files` array is resolved by path, not just
    /// by whether it exists. On `main` this function took no `path`
    /// argument. It read only the single form's flat `{lines_shown,
    /// lines_total}` pair. So a batch's `files` payload produced `None` for
    /// every row, no matter which file the row named.
    #[test]
    fn a_batch_payload_is_resolved_by_its_lead_path() {
        let data = serde_json::json!({"files": [
            {"path": "a.rs", "lines_shown": 1, "lines_total": 1},
            {"path": "b.rs", "lines_shown": 1, "lines_total": 2}
        ]});
        assert_eq!(
            ReadSize::from_data(&data, Some("b.rs")),
            Some(ReadSize { shown: 1, total: 2 })
        );
        assert_eq!(
            ReadSize::from_data(&data, Some("nope.rs")),
            None,
            "a path absent from the batch must not borrow another file's counts"
        );
        assert_eq!(
            ReadSize::from_data(&data, None),
            None,
            "a batch payload with no lead path to resolve against states nothing"
        );
    }

    /// The single form is unaffected by the new parameter: its flat pair is
    /// read the same way regardless of what `path` is passed.
    #[test]
    fn the_single_forms_flat_pair_ignores_path() {
        let data = serde_json::json!({"lines_shown": 3, "lines_total": 5});
        let want = Some(ReadSize { shown: 3, total: 5 });
        assert_eq!(ReadSize::from_data(&data, Some("whatever.rs")), want);
        assert_eq!(ReadSize::from_data(&data, None), want);
    }
}
