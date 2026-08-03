//! The pure render model: a deterministic fold of the `AgentEvent` log into
//! the derived state every panel draws from ( L-T1).
//!
//! [`SessionModel`] owns **only** state that is reconstructible by replaying
//! the event log from seq 1 — transcript lines, the files-touched map, HUD
//! numbers, and the pending scope-review. It has exactly one mutator,
//! [`SessionModel::apply`]; there is no other way to change it. Ephemeral
//! interaction state (scroll offset, composer buffer, panel focus) that is
//! *not* derived from events lives in [`crate::deck_ui::DeckUi`], never here —
//! that boundary is what makes replay-from-seq-1 a supported debug mode and
//! what makes the panel panic boundary sound (render is a pure function over
//! `&SessionModel`, so a panicking panel can be caught and discarded without
//! leaving torn state — L-T7).
//!
//! Styling is deliberately *not* stored here: entries are semantic records,
//! and [`mod@crate::render`] converts them to styled `ratatui` lines as a pure
//! function of the model. Determinism therefore extends all the way to the
//! backing cell buffer (the replay-determinism test in [`mod@crate::render`]).

use crate::ansi::strip_ansi;
use stella_protocol::{
    AgentEvent, BudgetMode, CiStatus, FileChangeKind, MediaJobState, MediaKind, PrStatus,
    ScopeProposal, StageKind, SubAgentPhase, SubAgentStatus, TaskItem, TaskStatus, ToolOutput,
};

use std::collections::VecDeque;

mod error_rows;
pub mod file_state;
#[cfg(test)]
pub use file_state::DIFF_HISTORY;
pub use file_state::{FileState, MAX_TRACKED_FILES, RememberedDiff};

/// How many characters of a tool input / output summary we retain on a
/// transcript line before eliding — the full payload is never needed on the
/// one-line card (the diff panel and detail views carry the rest).
const SUMMARY_BUDGET: usize = 200;

/// Retention cap on transcript entries. The per-entry char budgets
/// ([`INPUT_BUDGET`], [`OUTPUT_BUDGET`]) bound one entry to ~20 KiB, but
/// without an entry-count cap a long-running session grows without bound;
/// 4 000 entries bounds the worst case to low tens of MiB while staying far
/// deeper than any scrollback a user actually walks. Below the cap the fold
/// is unchanged.
pub(crate) const MAX_TRANSCRIPT_ENTRIES: usize = 4_000;

/// Entries dropped per eviction pass — 10% of the cap, so the O(chunk) drain
/// and the deck fold-cache rebuild amortize over hundreds of events instead
/// of firing on every push once the cap is reached.
pub(crate) const TRANSCRIPT_EVICTION_CHUNK: usize = MAX_TRANSCRIPT_ENTRIES / 10;

// A pass must drop more than the one marker it inserts (or the transcript
// never shrinks) and must never drain the live tail.
const _: () =
    assert!(TRANSCRIPT_EVICTION_CHUNK >= 2 && TRANSCRIPT_EVICTION_CHUNK < MAX_TRANSCRIPT_ENTRIES);

/// The whole derived state of a session, folded from its `AgentEvent` log.
///
/// Every field is a pure function of the sequence of events applied so far;
/// two `SessionModel`s that have seen the same event vector are identical
/// (the L-T1 replay-determinism guarantee, exercised by tests here and in
/// [`mod@crate::render`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionModel {
    /// The scrollback transcript, oldest first. Streaming `Text`/`Reasoning`
    /// deltas are accumulated into the trailing entry rather than producing
    /// one line per token.
    pub transcript: Vec<TranscriptEntry>,
    /// Files the agent touched, in first-touched order, each retaining the
    /// latest diff that rode its `FileChange` event (L-T5 — there is no
    /// second data path for diffs). Capped at [`MAX_TRACKED_FILES`]; the
    /// least-recently-touched path is evicted to admit a new one.
    pub files: Vec<FileState>,
    /// How many paths [`MAX_TRACKED_FILES`] eviction has dropped — surfaced
    /// in the files panel title so a capped ledger never reads as complete.
    pub files_evicted: u32,
    /// Monotonic touch counter stamping [`FileState::touched_seq`].
    file_touch_seq: u64,
    /// Live HUD numbers: spend/limit/mode, current stage, model.
    pub hud: Hud,
    /// What this turn has established about its own work — the proof rail
    /// ([`crate::proof`]). Per-turn: cleared when the next turn starts, not
    /// when this one ends, so a finished proof stays readable.
    pub proof: crate::proof::ProofState,
    /// A scope-review gate awaiting the user's decision (L-E5). Set by a
    /// `ScopeReview` event and cleared by the engine's follow-on event
    /// (a non-scope-review `Stage`, `Complete`, or `Error`) — so the pending
    /// state is itself purely event-derived and reconstructs on replay.
    pub pending_scope_review: Option<ScopeProposal>,
    /// An `ask_user` question awaiting the user's answer. Set by an `AskUser`
    /// event; cleared purely by events — the answer returns as the tool call's
    /// ordinary `ToolResult` (matched by `id`), so a `ToolResult` with the
    /// question's `call_id` clears it (also cleared on `Complete`/`Error`).
    pub pending_ask_user: Option<AskUserPrompt>,
    /// The latest task-board snapshot (the `task_*` tools). Each
    /// `TaskUpdate` event replaces the whole board — snapshot semantics keep
    /// the fold pure and make a dead session's board reconstruct on replay.
    pub tasks: Vec<TaskItem>,
    /// The in-progress answer preview, accumulated from `TextDelta` events
    /// while a model call streams and rendered as a live trailing entry.
    /// Best-effort by protocol contract: REPLACED (never merged) when the
    /// step's authoritative `Text` event lands — which also folds retries
    /// away, since a retried attempt re-streams its deltas from the start —
    /// and dropped on `Error`/`Complete`/a new prompt. Middle-out capped at
    /// `OUTPUT_BUDGET` so an unbounded stream can't grow per-frame render
    /// cost; the authoritative `Text` entry is never capped by this.
    pub streaming_text: String,
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
    /// A stage boundary marker (`triage`, `plan`, `execute`, …).
    Stage(StageKind),
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
    },
    /// A tool invocation finished — `ok` is `false` for a typed tool error.
    ToolResult {
        call_id: String,
        /// Resolved from the matching `ToolStart` at fold time so call and
        /// result rows read as one aligned pair.
        name: String,
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
        /// For a *successful* file-mutating tool
        /// (`write_file`/`edit_file`/`delete_file`), the reference the
        /// renderer uses to show this call's diff inline. `None` for reads,
        /// non-file tools, and failed calls — which gates the inline diff to
        /// mutations that actually happened. The diff itself is never stored
        /// here (L-T5: one event-borne diff path).
        diff: Option<InlineDiffRef>,
    },
    /// A model call was retried (surfaced only once the step commits — the
    /// engine defers these, L-E10).
    Retry { attempt: u32, reason: String },
    /// A compaction pass ran.
    Compaction {
        before_tokens: u64,
        after_tokens: u64,
        evicted: usize,
        deduped: usize,
    },
    /// A spend tick (also folded into [`Hud`]); kept on the transcript as a
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
    /// Context recall completed; frames are cited by human label, never raw
    /// id (L-C4).
    ContextRecall {
        frames: usize,
        tokens: u32,
        labels: Vec<String>,
    },
    /// Context write-back completed.
    ContextWrite {
        provider: String,
        upserts: u32,
        superseded: u32,
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
    /// ladder from a model judge (L-E11).
    JudgeVerdict {
        passed: bool,
        summary: String,
        deterministic: bool,
    },
    /// A goal-check verdict from the judge loop (`AgentEvent::GoalVerdict`) —
    /// the symmetric scrollback row to [`Self::JudgeVerdict`]. `met` is the
    /// pass/fail; `round` is the judge iteration it settled on.
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
    /// [`SessionModel::pending_scope_review`]; this line is the scrollback
    /// record of it).
    ScopeReview {
        summary: String,
        steps: usize,
        estimated_files: u32,
    },
    /// An `ask_user` question was presented (the actionable card is driven off
    /// [`SessionModel::pending_ask_user`]; this line is the scrollback record).
    AskUser { question: String, options: usize },
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
    Complete { model: String, cost_usd: f64 },
}

/// A mutating tool result's handle on the diff it may render inline: the
/// path into [`SessionModel::files`] plus the value of that file's `changes`
/// counter when the result folded. The renderer shows the inline diff only
/// while the counter still matches — a later mutation of the same path bumps
/// it, so a historical entry can never display a diff its call didn't
/// produce. Only the *reference* lives here; the diff bytes stay on the
/// single event-borne path (L-T5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineDiffRef {
    /// The key into [`SessionModel::files`].
    pub path: String,
    /// [`FileState::changes`] at fold time — stale (hidden) once it differs.
    pub seq: u32,
}

/// Live HUD numbers, all folded from the event stream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Hud {
    /// Spend as the budget guard reports it on every `BudgetTick`.
    ///
    /// Note this is **cumulative for the life of the guard**, not per turn: the
    /// deck builds one `BudgetGuard` for the whole session and never calls
    /// `begin_turn`, so this only ever rises. Use [`Hud::turn_spent_usd`] for
    /// the number a reader would call "what this turn cost".
    pub spent_usd: f64,
    pub limit_usd: Option<f64>,
    pub budget_mode: Option<BudgetMode>,
    pub stage: Option<StageKind>,
    pub model: Option<String>,
    /// [`Hud::spent_usd`] as it stood when the current turn began, so live turn
    /// cost is the difference. Snapshotted in [`SessionModel::push_user_prompt`]
    /// — the earliest signal a turn has started.
    pub turn_start_spent_usd: f64,
    /// The final turn cost, set once a `Complete` event lands.
    pub final_cost_usd: Option<f64>,
    pub complete: bool,
}

impl Hud {
    /// What the turn in flight has cost so far.
    ///
    /// Once the turn settles this yields to `Complete`'s own `cost_usd`, which
    /// is authoritative — the driver totals it directly rather than differencing
    /// a cumulative gauge, so it also catches spend that never produced a tick.
    pub fn turn_spent_usd(&self) -> f64 {
        self.final_cost_usd
            .unwrap_or_else(|| (self.spent_usd - self.turn_start_spent_usd).max(0.0))
    }
}

impl SessionModel {
    /// A fresh, empty model — the seq-0 state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one event into the model. This is the **only** mutator; every
    /// panel's state is a pure function of the sequence of `apply` calls, so
    /// replaying the same log yields an identical model (L-T1).
    pub fn apply(&mut self, event: &AgentEvent) {
        match event {
            // An event from a newer stella. The fold stays a pure function of
            // events it understands: guessing at state from an undecodable
            // payload would make the model disagree with the transcript that
            // rendered it. The transcript still shows the event (see
            // `textline::unknown_event`), so nothing is hidden — the model
            // just declines to invent state for it.
            AgentEvent::Unknown { .. } => {}
            AgentEvent::Stage { name } => {
                // A stage after a Complete means a new turn has started —
                // clear the completion flag so the progress bar and HUD read
                // fresh (otherwise the bar stays frozen at full-green and
                // `final_cost_usd` is stale). Within a single turn, complete
                // is never set until the very end, so this is a no-op there.
                //
                // That same transition is the only honest reset point for the
                // proof rail. Proof is per-turn — last turn's flip says nothing
                // about this turn's work — but it must survive to be READ after
                // the verdict lands, so it cannot clear on `Complete`. Clearing
                // on the first stage of the NEXT turn keeps the finished rail on
                // screen for as long as the turn it describes is the current
                // one, and reconstructs identically on replay.
                if self.hud.complete {
                    self.proof = crate::proof::ProofState::default();
                }
                self.hud.complete = false;
                self.hud.final_cost_usd = None;
                self.hud.stage = Some(*name);
                // Any stage that isn't the scope-review gate itself means the
                // engine has moved past a pending gate (approved → execute,
                // or a later plan/verify stage) — clear it. Kept event-driven
                // so the pending state reconstructs on replay.
                if *name != StageKind::ScopeReview {
                    self.pending_scope_review = None;
                }
                self.transcript.push(TranscriptEntry::Stage(*name));
            }
            AgentEvent::Text { delta } => {
                // The authoritative step text replaces any streamed preview
                // outright — merging would duplicate (or, after a retry,
                // garble) what the deltas already showed.
                self.streaming_text.clear();
                self.push_text(delta)
            }
            AgentEvent::TextDelta { text } => self.push_streaming_delta(text),
            AgentEvent::Reasoning { delta } => self.push_reasoning(delta),
            AgentEvent::ToolStart { call } => {
                self.transcript.push(TranscriptEntry::ToolStart {
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                    input: format_tool_input(&call.name, &call.input),
                    raw: cap_input_json(&call.input, INPUT_BUDGET),
                    path: tool_input_path(&call.input),
                });
            }
            AgentEvent::ToolResult {
                call_id,
                output,
                duration_ms,
                speculated,
            } => {
                // ANSI escapes are stripped here, at fold time, and nowhere
                // else: the fold cache would retain them if the renderer
                // stripped per frame, and ratatui renders everything after
                // the ESC byte literally (#934).
                let (ok, summary, full) = match output {
                    ToolOutput::Ok { content } => {
                        let content = strip_ansi(content);
                        (
                            true,
                            summarize(&content),
                            cap_middle(&content, OUTPUT_BUDGET),
                        )
                    }
                    ToolOutput::Error { message } => {
                        let message = strip_ansi(message);
                        (
                            false,
                            summarize(&message),
                            cap_middle(&message, OUTPUT_BUDGET),
                        )
                    }
                };
                // Resolve the tool's name from its start entry (results only
                // carry the call id on the wire).
                let name = self
                    .transcript
                    .iter()
                    .rev()
                    .find_map(|e| match e {
                        TranscriptEntry::ToolStart {
                            call_id: cid, name, ..
                        } if cid == call_id => Some(name.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| "tool".to_string());
                // Only a *successful* mutation gets an inline-diff reference —
                // a failed call produced no `FileChange`, and rendering the
                // path's previous diff under its ✗ would attribute a change
                // the call never made. The engine's `FileChangeTap` emits the
                // `FileChange` during the tool's execution, so by the time
                // this result folds, `files[path].changes` already counts this
                // call's own change — that value is the freshness tag.
                let diff = if ok {
                    self.mutated_path_for(call_id).map(|path| {
                        let seq = self
                            .files
                            .iter()
                            .find(|f| f.path == path)
                            .map_or(0, |f| f.changes);
                        InlineDiffRef { path, seq }
                    })
                } else {
                    None
                };
                self.transcript.push(TranscriptEntry::ToolResult {
                    call_id: call_id.clone(),
                    name,
                    ok,
                    summary,
                    full,
                    duration_ms: *duration_ms,
                    speculated: *speculated,
                    diff,
                });
                // The answer to an `ask_user` question comes back as this very
                // tool result (correlated by id) — there is no separate answer
                // event — so a matching result clears the pending question.
                if self
                    .pending_ask_user
                    .as_ref()
                    .is_some_and(|p| p.id == *call_id)
                {
                    self.pending_ask_user = None;
                }
            }
            AgentEvent::Retry { attempt, reason } => {
                self.transcript.push(TranscriptEntry::Retry {
                    attempt: *attempt,
                    reason: reason.clone(),
                });
            }
            AgentEvent::Steered { text } => {
                // A steered message IS a user message — it entered the
                // conversation mid-turn; the prefix is what tells the
                // reader (and a replay) it landed at a step boundary.
                self.transcript
                    .push(TranscriptEntry::User(format!("(steered mid-turn) {text}")));
            }
            AgentEvent::Compaction {
                before_tokens,
                after_tokens,
                evicted,
                deduped,
                ..
            } => {
                self.transcript.push(TranscriptEntry::Compaction {
                    before_tokens: *before_tokens,
                    after_tokens: *after_tokens,
                    evicted: *evicted,
                    deduped: *deduped,
                });
            }
            AgentEvent::BudgetTick {
                spent_usd,
                limit_usd,
                mode,
                ..
            } => {
                // Gauge only — deliberately *not* pushed to the transcript.
                //
                // A tick fires after every model call that spends money, which
                // for one ordinary turn means four or five of them, each
                // printing a number that differs from the last by a fraction of
                // a cent and none of which is the turn's cost (see
                // `Hud::spent_usd` — this gauge is cumulative). The transcript
                // is the record of what *happened*; a budget gauge moving is
                // not an event, it is a reading. It belongs where a reading
                // belongs — live next to the composer, updating in place — and
                // the transcript gets the one line that is actually news: the
                // settled cost, once, at `Complete`.
                //
                // The `TranscriptEntry::BudgetTick` variant is kept: a log
                // replayed from an older stella may still carry the rows, and
                // dropping the variant would silently reshape that history.
                self.hud.spent_usd = *spent_usd;
                self.hud.limit_usd = *limit_usd;
                self.hud.budget_mode = Some(*mode);
            }
            AgentEvent::ProviderFallback { from, to, reason } => {
                self.transcript.push(TranscriptEntry::ProviderFallback {
                    from: from.clone(),
                    to: to.clone(),
                    reason: reason.clone(),
                });
            }
            // `added`/`removed` are the ledger's business (`deck::FileLedger`);
            // this read-model only keeps the diff text for the panel.
            AgentEvent::FileChange {
                path,
                kind,
                added,
                removed,
                diff,
            } => self.touch_file(path, *kind, *added, *removed, diff),
            AgentEvent::ContextRecall {
                frames,
                provider_mix: _,
                tokens,
                ..
            } => {
                let labels = frames.iter().map(|f| f.citation_label.clone()).collect();
                self.transcript.push(TranscriptEntry::ContextRecall {
                    frames: frames.len(),
                    tokens: *tokens,
                    labels,
                });
            }
            AgentEvent::ContextWrite {
                provider,
                upserts,
                superseded,
            } => {
                self.transcript.push(TranscriptEntry::ContextWrite {
                    provider: provider.clone(),
                    upserts: *upserts,
                    superseded: *superseded,
                });
            }
            AgentEvent::MediaProgress {
                artifact_id,
                kind,
                state,
            } => {
                self.transcript.push(TranscriptEntry::MediaProgress {
                    artifact_id: artifact_id.clone(),
                    kind: *kind,
                    state: state.clone(),
                });
            }
            AgentEvent::MediaComplete { artifact } => {
                self.transcript.push(TranscriptEntry::MediaComplete {
                    label: artifact.label.clone(),
                    path: artifact.path.clone(),
                    kind: artifact.kind,
                });
            }
            // Folded into the rail as well as the transcript: the rail is the
            // live view and scrolls away with nothing, the transcript is the
            // record. Same event, two jobs.
            AgentEvent::Proof { step } => self.proof.apply(step),
            AgentEvent::JudgeVerdict { passed, evidence } => {
                self.proof.apply_verdict(*passed, evidence);
                self.transcript.push(TranscriptEntry::JudgeVerdict {
                    passed: *passed,
                    summary: evidence.summary.clone(),
                    deterministic: evidence.deterministic,
                });
            }
            AgentEvent::ScopeReview { proposal } => {
                self.transcript.push(TranscriptEntry::ScopeReview {
                    summary: proposal.summary.clone(),
                    steps: proposal.steps.len(),
                    estimated_files: proposal.estimated_files,
                });
                self.pending_scope_review = Some(proposal.clone());
            }
            AgentEvent::AskUser {
                id,
                question,
                options,
            } => {
                self.transcript.push(TranscriptEntry::AskUser {
                    question: question.clone(),
                    options: options.len(),
                });
                self.pending_ask_user = Some(AskUserPrompt {
                    id: id.clone(),
                    question: question.clone(),
                    options: options.clone(),
                });
            }
            AgentEvent::Commit { sha, message } => {
                self.transcript.push(TranscriptEntry::Commit {
                    sha: sha.clone(),
                    message: message.clone(),
                });
            }
            AgentEvent::Pr {
                url,
                status,
                number,
                ci,
            } => {
                self.transcript.push(TranscriptEntry::Pr {
                    url: url.clone(),
                    status: *status,
                    number: *number,
                    ci: *ci,
                });
            }
            AgentEvent::TaskUpdate { tasks } => {
                // The board is snapshot state (rendered as a pinned
                // checklist card); the transcript gets a one-line digest so
                // scrollback shows *when* the board moved.
                self.tasks = tasks.clone();
                self.transcript.push(TranscriptEntry::TaskUpdate {
                    done: tasks
                        .iter()
                        .filter(|t| t.status == TaskStatus::Completed)
                        .count(),
                    total: tasks.len(),
                    active: tasks
                        .iter()
                        .find(|t| t.status == TaskStatus::InProgress)
                        .map(|t| t.subject.clone()),
                });
            }
            AgentEvent::GoalVerdict {
                met, round, reasoning, ..
            } => {
                // Symmetric to `JudgeVerdict` above — a scrollback row. The
                // event's own `cost_usd` is already accounted against the
                // budget when it fires, so it is dropped here (folding it would
                // double-count the HUD spend, which `BudgetTick` drives).
                self.transcript.push(TranscriptEntry::GoalVerdict {
                    met: *met,
                    round: *round,
                    reasoning: reasoning.clone(),
                });
            }
            AgentEvent::SubAgent { phase } => {
                // Both phases are scrollback rows: a child can run for
                // minutes with only its forwarded tool calls visible, so
                // without the start row a reader cannot tell whose calls
                // those are. Spend is deliberately not folded into the HUD
                // here — the engine re-ticks the parent's own post-settlement
                // numbers, and folding this too would double-count.
                let entry = match phase {
                    SubAgentPhase::Started {
                        agent_id,
                        instruction_preview,
                        write_access,
                        ..
                    } => TranscriptEntry::SubAgent {
                        agent_id: agent_id.clone(),
                        finished: None,
                        instruction_preview: instruction_preview.clone(),
                        write_access: *write_access,
                    },
                    SubAgentPhase::Finished {
                        agent_id,
                        status,
                        cost_usd,
                        steps,
                        absorbed_messages,
                        reason,
                        ..
                    } => TranscriptEntry::SubAgent {
                        agent_id: agent_id.clone(),
                        finished: Some(SubAgentSummary {
                            status: *status,
                            cost_usd: *cost_usd,
                            steps: *steps,
                            absorbed_messages: *absorbed_messages,
                            reason: reason.clone(),
                        }),
                        instruction_preview: String::new(),
                        write_access: false,
                    },
                };
                self.transcript.push(entry);
            }
            // `StepUsage` is a metering/billing record consumed by
            // `stella-store`; the HUD's live spend is driven by `BudgetTick`,
            // so folding it here would double-count.
            AgentEvent::StepUsage { .. }
            | AgentEvent::UsageIncomplete { .. }
            // Context receipts (spec §4/§5) are consumed by the store/inspector,
            // not folded into TUI panel state — the model stays a pure function
            // of the user-visible event sequence.
            | AgentEvent::BlockRegistered { .. }
            | AgentEvent::StepManifest { .. } => {}
            AgentEvent::Error { message, retryable } => {
                // A terminal error ends the turn without a `Complete`, so the
                // rail has to close here too — an aborted run is exactly when
                // a reader most needs to know what was and was not proven, and
                // exactly when rows would otherwise hang on `pending` forever.
                // A retryable error is a warning mid-flight; the turn goes on.
                if !*retryable {
                    self.proof.finish();
                }
                self.pending_scope_review = None;
                self.pending_ask_user = None;
                // An aborted model call never commits its text — without
                // this the un-committed preview would linger indefinitely.
                self.streaming_text.clear();
                // One failure, one row — see `error_rows` for why the same
                // error can arrive twice.
                if !error_rows::repeats_the_last_row(&self.transcript, message, *retryable) {
                    self.transcript.push(TranscriptEntry::Error {
                        message: message.clone(),
                        retryable: *retryable,
                    });
                }
            }
            AgentEvent::Complete { model, cost_usd } => {
                self.hud.stage = Some(StageKind::Complete);
                self.hud.model = Some(model.clone());
                self.hud.final_cost_usd = Some(*cost_usd);
                self.hud.complete = true;
                // Close the proof rail: anything the run never reported now
                // says so, rather than reading `pending` on a turn that is
                // over. See `crate::proof` — this is the half of the invariant
                // that does not depend on the pipeline cooperating.
                self.proof.finish();
                self.pending_scope_review = None;
                self.pending_ask_user = None;
                self.streaming_text.clear();
                self.transcript.push(TranscriptEntry::Complete {
                    model: model.clone(),
                    cost_usd: *cost_usd,
                });
            }
            // Internal accounting for read-only speculation that never
            // committed — no visible model state to update.
            AgentEvent::SpeculationDiscarded { .. } => {}
            // Typed decision twins (receipts spec §6.3/§6.4) — the prose
            // events they mirror already updated the visible state.
            AgentEvent::LoopDetected { .. }
            | AgentEvent::BudgetDenied { .. }
            | AgentEvent::RetriesExhausted { .. }
            | AgentEvent::PolicyDecision { .. } => {}
        }
        self.evict_transcript_overflow();
    }

    /// Fold an entire log at once — the replay entry point.
    pub fn replay(events: &[AgentEvent]) -> Self {
        let mut model = Self::new();
        for event in events {
            model.apply(event);
        }
        model
    }

    /// Append a streaming text delta, coalescing into the trailing `Text`
    /// entry when the last thing emitted was also assistant text.
    fn push_text(&mut self, delta: &str) {
        if let Some(TranscriptEntry::Text(buf)) = self.transcript.last_mut() {
            buf.push_str(delta);
        } else {
            self.transcript
                .push(TranscriptEntry::Text(delta.to_string()));
        }
    }

    /// Append one best-effort answer fragment to the streaming preview,
    /// re-capping middle-out at [`OUTPUT_BUDGET`] once it overflows — the
    /// cap keeps the per-frame re-fold of the live tail bounded no matter
    /// how long the model streams. Still a pure fold: the capped buffer is
    /// a deterministic function of the delta sequence.
    fn push_streaming_delta(&mut self, text: &str) {
        self.streaming_text.push_str(text);
        if self.streaming_text.len() > OUTPUT_BUDGET {
            self.streaming_text = cap_middle(&self.streaming_text, OUTPUT_BUDGET);
        }
    }

    /// Append a streaming reasoning delta, coalescing like [`Self::push_text`].
    fn push_reasoning(&mut self, delta: &str) {
        if let Some(TranscriptEntry::Reasoning(buf)) = self.transcript.last_mut() {
            buf.push_str(delta);
        } else {
            self.transcript
                .push(TranscriptEntry::Reasoning(delta.to_string()));
        }
    }

    /// Push a user-submitted prompt into the transcript. This is **not** an
    /// `AgentEvent` fold — the deck driver calls this when `PromptStarted`
    /// arrives so user messages appear inline in the conversational scrollback.
    /// It is also the earliest signal that a new turn has begun, so it clears
    /// any completion state from the prior turn — the progress bar and HUD
    /// reset immediately on prompt submission rather than waiting for the
    /// first `Stage` event of the new turn.
    pub fn push_user_prompt(&mut self, text: &str) {
        self.hud.complete = false;
        self.hud.final_cost_usd = None;
        // The rail belongs to one turn; a new prompt is the earliest moment the
        // previous turn's proof stops describing anything on screen.
        self.proof = crate::proof::ProofState::default();
        // Rebase the live turn-cost readout. `spent_usd` is cumulative for the
        // session, so without this the composer's cost cell would open every
        // turn already showing the session total.
        self.hud.turn_start_spent_usd = self.hud.spent_usd;
        // A preview surviving into the next turn could only be stale — the
        // prior turn either committed (cleared on `Text`) or aborted.
        self.streaming_text.clear();
        // Also drop the prior turn's stage, or the progress bar would resume
        // frozen at that stale position (e.g. verify → 83%) instead of restarting
        // at the new turn's beginning. A model turn's first `Stage` event resets
        // this anyway; a driver command (which emits no stages) relies on it.
        self.hud.stage = None;
        self.transcript
            .push(TranscriptEntry::User(text.to_string()));
        self.evict_transcript_overflow();
    }

    /// Total transcript entries evicted by the retention cap so far.
    /// Monotonic — a pass absorbs any prior marker and adds at least one —
    /// so it serves as the invalidation generation for caches keyed on the
    /// retained window's front (see the deck's `SessionFold`).
    pub fn evicted_entries(&self) -> usize {
        match self.transcript.first() {
            Some(TranscriptEntry::Evicted { count }) => *count,
            _ => 0,
        }
    }

    /// Enforce [`MAX_TRANSCRIPT_ENTRIES`]: at the cap, drop the oldest
    /// [`TRANSCRIPT_EVICTION_CHUNK`] entries and stand a single
    /// [`TranscriptEntry::Evicted`] marker in their place, absorbing a prior
    /// marker's count so the tally stays total, not per-pass. Runs inside
    /// every transcript-growing mutator — the retained window is part of the
    /// deterministic fold, never a render-time concern. Only the front is
    /// drained, so streaming coalescing into the tail entry is unaffected.
    fn evict_transcript_overflow(&mut self) {
        if self.transcript.len() < MAX_TRANSCRIPT_ENTRIES {
            return;
        }
        let evicted: usize = self
            .transcript
            .drain(..TRANSCRIPT_EVICTION_CHUNK)
            .map(|entry| match entry {
                TranscriptEntry::Evicted { count } => count,
                _ => 1,
            })
            .sum();
        self.transcript
            .insert(0, TranscriptEntry::Evicted { count: evicted });
    }

    /// If tool call `call_id` was a file mutation, the path it touched —
    /// recovered by correlating back to its `ToolStart` (which is already on
    /// the transcript by the time the result folds). `None` for reads and
    /// non-file tools, which is what gates the transcript's inline diff to
    /// mutations. The diff itself is *not* looked up here — the renderer reads
    /// it from [`SessionModel::files`] at draw time (L-T5).
    fn mutated_path_for(&self, call_id: &str) -> Option<String> {
        self.transcript
            .iter()
            .rev()
            .find_map(|entry| match entry {
                TranscriptEntry::ToolStart {
                    call_id: cid,
                    name,
                    path,
                    ..
                } if cid == call_id => Some((name.clone(), path.clone())),
                _ => None,
            })
            .and_then(|(name, path)| is_file_mutation(&name).then_some(path).flatten())
    }

    /// Record a file touch, retaining the latest diff for the path (L-T5).
    /// A read on an already-tracked path only grows its read count — the
    /// mutation kind, diff, and `changes` (the inline-diff freshness tag)
    /// stay exactly as the last mutation left them.
    fn touch_file(
        &mut self,
        path: &str,
        kind: FileChangeKind,
        added: u32,
        removed: u32,
        diff: &Option<String>,
    ) {
        self.file_touch_seq += 1;
        let touched_seq = self.file_touch_seq;
        if let Some(existing) = self.files.iter_mut().find(|f| f.path == path) {
            existing.touched_seq = touched_seq;
            if kind.is_mutation() {
                existing.kind = kind;
                existing.latest_diff = diff.clone();
                existing.changes += 1;
                existing.added += added;
                existing.removed += removed;
                existing.remember_diff(diff, added, removed);
            } else {
                existing.reads += 1;
            }
        } else {
            if self.files.len() >= MAX_TRACKED_FILES && file_state::evict_lru(&mut self.files) {
                self.files_evicted += 1;
            }
            let mutation = kind.is_mutation();
            let mut state = FileState {
                path: path.to_string(),
                kind,
                latest_diff: diff.clone(),
                added: if mutation { added } else { 0 },
                removed: if mutation { removed } else { 0 },
                recent_diffs: VecDeque::new(),
                changes: mutation as u32,
                reads: !mutation as u32,
                touched_seq,
            };
            if mutation {
                state.remember_diff(diff, added, removed);
            }
            self.files.push(state);
        }
    }
}

/// Compact a tool-call input `Value` to a single-line JSON string. Falls back
/// to the empty string on the (impossible for `Value`) serialization error so
/// the model never panics on a tool card.
fn compact_json(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// Char budget for a tool call's retained compact-JSON arguments.
pub(crate) const INPUT_BUDGET: usize = 4_096;
/// Char budget for a tool result's retained output (outputs are already
/// capped upstream by the tools; this bounds transcript memory).
pub(crate) const OUTPUT_BUDGET: usize = 16_384;

/// Middle-out char cap preserving head and tail (first error + final summary
/// both matter), on char boundaries.
fn cap_middle(text: &str, budget: usize) -> String {
    cap_middle_with(text, budget, "\n[… truncated …]\n")
}

/// [`cap_middle`] with a caller-chosen elision marker. Slices at
/// `char_indices` boundaries instead of materializing a `Vec<char>`, so a
/// multi-megabyte payload costs no allocation beyond the capped result.
fn cap_middle_with(text: &str, budget: usize, marker: &str) -> String {
    // Byte length bounds char count, so an in-budget payload returns without
    // scanning; an over-budget one probes just past the boundary instead.
    if text.len() <= budget || text.char_indices().nth(budget).is_none() {
        return text.to_string();
    }
    let keep = budget.saturating_sub(marker.chars().count());
    let head = keep / 2;
    let tail = keep - head;
    let head_end = text.char_indices().nth(head).map_or(text.len(), |(i, _)| i);
    let tail_start = if tail == 0 {
        text.len()
    } else {
        text.char_indices().nth_back(tail - 1).map_or(0, |(i, _)| i)
    };
    format!("{}{marker}{}", &text[..head_end], &text[tail_start..])
}

/// Per-leaf caps for [`cap_input_json`]: generous enough to keep any one
/// argument readable, small enough that leaf capping alone usually lands the
/// whole object under [`INPUT_BUDGET`].
const INPUT_STR_CAP: usize = 512;
const INPUT_ARR_CAP: usize = 32;

/// Cap a tool call's retained arguments **inside** the JSON: long string
/// leaves are middle-capped and oversized arrays elided, so the compact form
/// stays *valid* JSON and ctrl+o can still pretty-print it. Only a
/// pathological object that remains oversized after leaf capping falls back
/// to the raw char cap (which the renderer shows as wrapped plain text).
fn cap_input_json(value: &serde_json::Value, budget: usize) -> String {
    let compact = compact_json(value);
    if compact.len() <= budget {
        return compact;
    }
    let mut capped = value.clone();
    cap_json_leaves(&mut capped);
    cap_middle(&compact_json(&capped), budget)
}

/// Recursively shrink the leaves of `value` in place (strings middle-capped
/// on one line, arrays truncated with a `+N more` marker) without disturbing
/// the object structure.
fn cap_json_leaves(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => {
            if s.len() > INPUT_STR_CAP {
                *s = cap_middle_with(s, INPUT_STR_CAP, " […] ");
            }
        }
        serde_json::Value::Array(items) => {
            if items.len() > INPUT_ARR_CAP {
                let dropped = items.len() - INPUT_ARR_CAP;
                items.truncate(INPUT_ARR_CAP);
                items.push(serde_json::Value::String(format!("[… +{dropped} more …]")));
            }
            for item in items.iter_mut() {
                cap_json_leaves(item);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values_mut() {
                cap_json_leaves(item);
            }
        }
        _ => {}
    }
}

/// Format a tool-call input as a human-readable one-liner. Instead of raw
/// JSON, this extracts the most relevant field(s) per tool name so the
/// transcript reads naturally — `path` for file tools, `cmd` for shell, the
/// query for search tools, and so on.
fn format_tool_input(name: &str, input: &serde_json::Value) -> String {
    let str_field = |key: &str| -> Option<String> {
        input
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };

    // apply_edits carries its paths in a batch; surface them instead of the
    // raw-JSON fallback so the transcript row reads like other file tools.
    if name == "apply_edits"
        && let Some(edits) = input.get("edits").and_then(|v| v.as_array())
    {
        let mut paths: Vec<&str> = Vec::new();
        for edit in edits {
            if let Some(p) = edit.get("path").and_then(|v| v.as_str())
                && !paths.contains(&p)
            {
                paths.push(p);
            }
        }
        if !paths.is_empty() {
            let labels: Vec<String> = paths.iter().map(|p| truncate_field(p, 48)).collect();
            return if paths.len() == 1 {
                labels.into_iter().next().unwrap()
            } else {
                format!("{}  ({} files)", labels.join(", "), paths.len())
            };
        }
    }

    // Primary field per tool — the one the user cares about at a glance.
    if let Some(p) = str_field("path").or_else(|| str_field("file_path")) {
        return match name {
            // Just the path. The old/new strings used to ride along here,
            // truncated to 40 characters each — long enough to fill the row,
            // never long enough to read, and describing exactly the change the
            // inline diff below renders properly.
            "edit_file" => p,
            "write_file" => {
                let lines = str_field("content").map(|c| c.lines().count()).unwrap_or(0);
                format!("{p}  ({lines} lines)")
            }
            _ => p,
        };
    }

    if let Some(cmd) = str_field("cmd").or_else(|| str_field("command")) {
        return truncate_field(&cmd, 120);
    }

    if let Some(query) = str_field("query")
        .or_else(|| str_field("pattern"))
        .or_else(|| str_field("symbol"))
    {
        return truncate_field(&query, 80);
    }

    if let Some(prompt) = str_field("question").or_else(|| str_field("prompt")) {
        return truncate_field(&prompt, 80);
    }

    // Fallback: compact JSON, summarized.
    summarize(&compact_json(input))
}

/// Truncate a field value to `max` chars with an ellipsis. Cuts *before*
/// flattening: the newline→space replacement is one char in, one char out, so
/// slicing the raw text first yields the identical result without copying a
/// multi-megabyte `content`/`old_string` argument in full (the same reason
/// [`cap_middle_with`] walks `char_indices` instead of materializing chars).
fn truncate_field(s: &str, max: usize) -> String {
    // `nth(max)` is `None` exactly when the text is `max` chars or shorter.
    if s.char_indices().nth(max).is_none() {
        return s.replace(['\n', '\r'], " ");
    }
    let head_end = s
        .char_indices()
        .nth(max.saturating_sub(1))
        .map_or(s.len(), |(i, _)| i);
    format!("{}…", s[..head_end].replace(['\n', '\r'], " "))
}

/// The workspace-relative path a file tool targets. Every built-in file tool
/// (`read_file`/`write_file`/`edit_file`/`delete_file`) takes its path under
/// the `path` key, and the engine emits `FileChange` for that same path — so
/// this is the join key between a tool result and its diff.
fn tool_input_path(input: &serde_json::Value) -> Option<String> {
    if let Some(path) = input
        .get("path")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
    {
        return Some(path);
    }
    // apply_edits carries its paths in a batch, not at the top level; the
    // first edit's path stands in so a single-file batch still renders an
    // inline diff under its result row.
    input
        .get("edits")
        .and_then(serde_json::Value::as_array)
        .and_then(|edits| edits.first())
        .and_then(|e| e.get("path"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// Whether a tool name is one of the file-*mutating* built-ins — the only
/// tools whose result should carry an inline diff (reads must not). Must
/// stay in lockstep with `file_change_of` in stella-cli's `command_deck.rs`,
/// the `FileChange` emitter that owns this list.
fn is_file_mutation(name: &str) -> bool {
    matches!(
        name,
        "write_file" | "edit_file" | "apply_edits" | "delete_file"
    )
}

/// Truncate a summary to [`SUMMARY_BUDGET`] chars with a middle-out elision —
/// the head and tail both matter for a failing tool result (L-S3), so we keep
/// both rather than head-truncating away the error tail.
///
/// Caps *before* flattening. `text` here is a raw tool payload that can be
/// megabytes (the caller also passes it to [`cap_middle`]); replacing newlines
/// first would copy the whole thing just to throw all but 200 chars away, and
/// the replacement is one char in / one char out, so the two orders agree.
fn summarize(text: &str) -> String {
    cap_middle_with(text, SUMMARY_BUDGET, "...").replace(['\n', '\r'], " ")
}

#[cfg(test)]
#[path = "model/tests.rs"]
mod tests;
