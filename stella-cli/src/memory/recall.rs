//! Recall — the read side of session memory: the volatile recalled-context
//! block a prompt is given, the A/B control that suppresses it, the frame
//! query behind both that block and the pipeline's [`ContextRecallPort`],
//! and the injection that lands the block at the conversation tail.
//!
//! Split verbatim out of `memory.rs` — no behavior change — so the recall
//! plane and the learning loop (`memory/learning.rs`) can evolve
//! independently instead of contending for one file.

use colored::Colorize;
use stella_context::ContextQuery;
use stella_core::skills::{self, SelectionConfig};
use stella_pipeline::{ContextRecallPort, Recall, RecalledFrame};
use stella_protocol::{CompletionMessage, MessageRole};

use super::projection::{is_suppressed_local_frame, project_recalled_frame};
use super::{RECALL_MARKER, SessionMemory};

/// The rendered recalled-context block, and the structured recall it was
/// rendered from. See [`SessionMemory::recall_block_reported`].
#[derive(Debug, Default)]
pub struct RecalledBlock {
    /// The message to inject, or `None` when nothing relevant surfaced (an
    /// empty block would only burn cache) or the turn is an A/B control.
    pub text: Option<String>,
    /// What recall actually returned and what it cost — the material the
    /// `ContextRecall` event is built from.
    pub recall: Recall,
}

impl RecalledBlock {
    /// This block's recall telemetry, ready to send. `None` when no frame was
    /// recalled — a turn whose block is only skills has no frames to report.
    #[must_use]
    pub fn telemetry_event(&self) -> Option<stella_protocol::AgentEvent> {
        self.recall.telemetry_event()
    }
}

impl SessionMemory {
    /// Hand this session the volatile record channel resolved at assembly.
    ///
    /// Called once, from the session driver that already has the rule registry, so
    /// recall never re-walks the rule directories or re-runs the truth sweep.
    pub(crate) fn set_record_channel(&mut self, block: String) {
        self.record_channel = (!block.trim().is_empty()).then_some(block);
    }

    /// Build the volatile recalled-context block for a prompt: relevant
    /// memories (similarity + domain overlap + recency via the context
    /// store) and relevant skills (lexical + domain selection). `None` when
    /// nothing relevant surfaced — an empty block would only burn cache.
    ///
    /// **Quarantine filter (Proposal 3):** frames whose memory id (`nod_…`)
    /// appears in the session's `quarantined_ids` set are dropped before
    /// rendering. These are memories cited untruthful ≥ 2 times — surfacing
    /// them is active harm.
    ///
    /// **A/B control (Proposal 4):** when `ab_suppressed` is true (set by a
    /// deterministic coin flip), recall returns `None` so the turn runs
    /// without context — the outcome is then comparable to recalled turns.
    ///
    /// Test-only since Phase 2 (#713): every production caller now takes
    /// [`Self::recall_block_reported`], because a caller that only wants the
    /// string is a caller that emits no recall telemetry — which is exactly
    /// the defect deliverable 3 closes. Keeping the convenience form reachable
    /// from production would let the next recall site reintroduce it silently.
    #[cfg(test)]
    pub async fn recall_block(&self, prompt: &str) -> Option<String> {
        self.recall_block_reported(prompt).await.text
    }

    /// The recalled-context block **without throwing the recall away**.
    ///
    /// Phase 2 (#713) deliverable 3. `recall_block` rendered a `String` and
    /// dropped the structured [`Recall`] — frames, provider mix, and the CGP
    /// usage report — on the floor. That discard is the whole reason the
    /// one-shot run, the interactive REPL, `/goal`, and the Command Deck
    /// emitted no `ContextRecall` event: they had nothing left to emit. The
    /// pipeline was the only surface that reported its recall, and it is the
    /// surface real users touch least.
    ///
    /// So the block and the recall behind it now travel together, for the same
    /// reason [`Recall`] itself carries frames and usage together: they are
    /// answers to different questions about one request, and separating them
    /// means either re-running recall to report it or losing it entirely.
    pub async fn recall_block_reported(&self, prompt: &str) -> RecalledBlock {
        if self.ab_suppressed {
            return RecalledBlock::default();
        }
        let mut sections: Vec<String> = Vec::new();
        let recall = self.recalled_frames(prompt).await;
        if let Some(section) = render_context_section(&recall.frames) {
            sections.push(section);
        }

        let all_skills = self.load_skills();
        let selected = skills::select_skills(
            &all_skills,
            prompt,
            &self.domains.names(),
            &SelectionConfig::default(),
        );
        if !selected.is_empty() {
            sections.push(skills::render_skills_section(&selected));
        }

        // In-progress workspace maps belong here, not in the cached system
        // prefix: a draft line names the producing pid and its liveness, so
        // it differs per process and flips mid-session. Rendering it in the
        // prefix is what made the prefix non-byte-stable (#639).
        if let Some(section) = stella_tools::exploration::render_draft_claims(&self.workspace_root)
        {
            sections.push(section);
        }

        // The volatile context-record channel (epic #897). Same channel as the
        // memories above and for the same reason: a fact about a staging URL costs
        // tokens on every turn and is worth them on almost none.
        if let Some(section) = self.record_channel.as_deref().filter(|s| !s.is_empty()) {
            sections.push(section.trim_start().to_string());
        }

        RecalledBlock {
            // Skills and draft claims can produce a block with no frames behind
            // it; frames can be recalled and then filtered out of the render by
            // the citation-label rule. The two fields are therefore reported
            // independently rather than one gating the other — a provider still
            // spent the tokens for a frame the host later dropped, and a cost
            // that vanishes because the render discarded it is exactly the
            // unmeterable cost this event exists to surface.
            text: (!sections.is_empty())
                .then(|| format!("{RECALL_MARKER}\n\n{}", sections.join("\n\n"))),
            recall,
        }
    }

    /// A/B recall control (Proposal 4): suppress recall for this turn on a
    /// deterministic `1/rate` schedule, returning whether recall was
    /// suppressed. A rate of 0 (or 1) never suppresses.
    ///
    /// The flag is turn-scoped but only *reset* by this call, so every driver
    /// that recalls must call it once per turn or inherit the previous turn's
    /// arm. Today only the interactive REPL's plain prompts do
    /// (`agent::run_interactive`); the outcome is attributed by tagging the
    /// episode summary `[ab-control]` (see `agent::record_turn_episode`), which
    /// is the whole readout — there is no reporting command yet, so the
    /// comparison is an offline query over the episode store.
    ///
    /// Suppression is driven by a per-session **turn counter**, not a wall
    /// clock. A previous implementation seeded off `SystemTime` nanoseconds
    /// and tested `ns % rate == 0`; on any host whose realtime clock is
    /// coarser than nanoseconds (macOS keeps it in microseconds, so `ns` is
    /// always a multiple of 1000) that predicate is true on *every* turn for
    /// any `rate` dividing 1000 — silently disabling recall entirely. A plain
    /// counter makes exactly every `rate`-th turn a control turn, on every OS.
    pub fn maybe_suppress_recall(&mut self, rate: u32) -> bool {
        if rate == 0 || rate == 1 {
            self.ab_suppressed = false;
            return false;
        }
        self.ab_turn = self.ab_turn.wrapping_add(1);
        self.ab_suppressed = ab_control_turn(self.ab_turn, rate);
        self.ab_suppressed
    }

    /// Whether recall was suppressed this turn (for outcome attribution).
    pub fn recall_was_suppressed(&self) -> bool {
        self.ab_suppressed
    }

    /// Authoritative prompt and pipeline recall, including a fresh quarantine
    /// read so prior-turn feedback applies immediately.
    async fn recalled_frames(&self, goal: &str) -> Recall {
        self.recalled_frames_reporting(goal, |message| eprintln!("  {} {message}", "!".yellow()))
            .await
    }
    /// Recall with an injectable diagnostic sink to avoid global stderr capture in tests.
    pub(super) async fn recalled_frames_reporting(
        &self,
        goal: &str,
        mut report: impl FnMut(String),
    ) -> Recall {
        if self.ab_suppressed {
            return Recall::default();
        }
        let query = ContextQuery {
            goal: goal.to_string(),
            query_text: Some(goal.to_string()),
            embedding: None,
            kinds: vec![],
            // Workspace files the goal names verbatim, as anchors: the
            // code-graph provider answers anchors with each file's graph
            // NEIGHBORHOOD (symbols + imports + importers), not just
            // goal-token definition lookups — deterministic localization
            // into both the planner prompt and the worker's recall block,
            // instead of hoping the model's first move is a graph_query
            // (#342 seam 3).
            anchors: goal_path_anchors(goal, &self.workspace_root),
            // Settings-backed since #712 deliverable 8; the defaults are the
            // literals that used to be here.
            max_frames: self.retrieval.max_frames,
            max_tokens: self.retrieval.max_tokens,
            as_of: None,
            representation_preferences: vec![],
        };
        // The suppression sets, read together and applied as one. This is a
        // redundant net behind the provider-internal read — and it goes through
        // the SAME reader rather than spelling the union out again, because two
        // copies of "what is suppressed" are two things that can disagree. When
        // retirement joined the union (#715 deliverable 5) the inline copy that
        // used to live here would have silently kept serving retired records.
        //
        // Fail-closed: if the state cannot be read, surfacing everything is the
        // one outcome that is definitely wrong.
        let quarantined = match super::suppression::suppression_reader(
            &self.workspace_root,
            self.store.clone(),
        )() {
            Ok(ids) => ids,
            Err(error) => {
                report(format!(
                    "memory recall disabled: suppression state unavailable: {error}"
                ));
                return Recall::default();
            }
        };
        // The usage report accounts for the fan-out that produced this turn's
        // context, so it is captured even when quarantine or the citation-label
        // filter later drops every frame: a provider still spent the tokens,
        // and a cost that vanishes because the host discarded the frames is
        // exactly the unmeterable cost #452 exists to surface.
        // #875: recall is on the first-token path of every turn, so the one
        // number that says whether context retrieval is why a turn felt slow
        // is measured here — around the host fan-out itself, which is the
        // work. Everything before this point is query construction and
        // suppression-set reads; everything after is projection. Timing the
        // whole function would blame recall for both.
        let started = std::time::Instant::now();
        let recalled = crate::contextgraph::recall_via_host(&self.host, &query).await;
        let latency_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
        // Phase 2 (#713): the host's cross-provider merge is a second budget
        // pass and reported nothing at all, so anything it cut vanished
        // without a trace — a silent truncation `L-C5` bans. A required item
        // it could not honor is the loudest case and is named first: the
        // caller pointed at that file, and being told it did not fit is the
        // whole difference between a budget and a lie.
        for drop in &recalled.dropped {
            if drop.reason == stella_context::DropReason::RequiredOverBudget {
                report(format!(
                    "recall could not fit an anchored frame: {} ({} tokens) exceeds the \
                     {}-token budget — raise context.retrieval.max_tokens to include it",
                    drop.citation_label, drop.token_cost, query.max_tokens
                ));
            }
        }
        Recall {
            frames: recalled
                .frames
                .into_iter()
                .filter_map(project_recalled_frame)
                .filter(|frame| !is_suppressed_local_frame(frame, &quarantined))
                .collect(),
            usage: Some(recalled.usage),
            latency_ms,
            // The host fan-out does not carry the accelerator flag across the
            // provider-result boundary, and reporting `false` would read as
            // "the index never fires" rather than "nobody said".
            used_ann_index: None,
        }
    }
}

/// Is the `turn`-th turn (1-based) an A/B control turn at the given `rate`?
/// Every `rate`-th turn is a control turn; `rate` of 0 or 1 never controls.
/// Pure so the schedule is property-testable independent of the (heavy)
/// [`SessionMemory`] it lives on.
pub(super) fn ab_control_turn(turn: u32, rate: u32) -> bool {
    rate > 1 && turn.is_multiple_of(rate)
}

/// Workspace files the goal names verbatim (`src/driver.rs`,
/// `stella-core/src/bus.rs`), for use as recall anchors (#342 seam 3).
/// Deterministic and cheap: path-shaped tokens (must contain `/`) that
/// resolve to real files under the workspace root. Escapes are rejected,
/// duplicates collapse, and the list is capped — anchors fan out into
/// whole graph neighborhoods, so a pathological goal must not turn recall
/// into a full-graph walk.
pub(super) fn goal_path_anchors(goal: &str, root: &std::path::Path) -> Vec<String> {
    const MAX_ANCHORS: usize = 4;
    let mut seen = std::collections::HashSet::new();
    let mut anchors = Vec::new();
    'tokens: for token in goal.split(|c: char| c.is_whitespace() || "\"'`()[]{}<>,;!?".contains(c))
    {
        // Strip trailing sentence punctuation (`fix src/a.rs.`) without
        // touching the extension dot; a `src/a.rs:42` file:line spelling
        // anchors the file.
        let token = token.trim_end_matches(['.', ':']);
        for candidate in [token, token.split(':').next().unwrap_or(token)] {
            if !candidate.contains('/')
                || candidate.len() > 256
                || candidate.split('/').any(|seg| seg == "..")
                || candidate.starts_with('/')
            {
                continue;
            }
            if root.join(candidate).is_file() && seen.insert(candidate.to_string()) {
                anchors.push(candidate.to_string());
                if anchors.len() == MAX_ANCHORS {
                    break 'tokens;
                }
                break;
            }
        }
    }
    anchors
}

/// The pipeline's context-recall port over the workspace memory store: the
/// split-context planner (L-E6) receives the same durable lessons the
/// worker's injected recall block carries, as structured frames instead of a
/// rendered string. Frames without a citation label are dropped (L-C4), and
/// failed recall, including quarantine verification, degrades to no frames (L-C6).
#[async_trait::async_trait]
impl ContextRecallPort for SessionMemory {
    async fn recall(&self, goal: &str) -> Recall {
        self.recalled_frames(goal).await
    }
}

/// Render recalled frames as the "Relevant context" section of the recall
/// block. Memory-kind frames carry their stable `[nod_…]` id inline — the
/// handle the `cite_memory` tool ties feedback to — and their presence
/// appends the citation instruction, so the model is asked to cite exactly
/// when there is something citable. Other frame kinds (code-graph hits,
/// episodes) keep the plain label form: they are grounding, not memories,
/// and never enter the citation → promotion loop. `None` when no frame has
/// a citation label (L-C4 filters the rest).
pub(super) fn render_context_section(frames: &[RecalledFrame]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut citable = false;
    for f in frames {
        let label = &f.citation_label;
        match (f.kind.as_str(), &f.id) {
            // A memory with an id: citable, and the id is what the model
            // hands back to `cite_memory`.
            ("memory", Some(id)) => {
                citable = true;
                lines.push(format!("- [{id}] {label} — {}", f.content.trim()));
            }
            // A memory WITHOUT an id still has content worth recalling, and
            // the recall budget has already been spent fetching it.
            //
            // This arm used to be missing: an id-less memory flipped `citable`
            // and then pushed no line at all, so its content vanished from the
            // block while the model was still told to cite `[nod_…]` lines
            // that might not exist anywhere in it. `RecalledFrame` documents
            // `id: None` as a legitimate state for a not-yet-materialized
            // frame (`stella-pipeline/src/ports.rs`), so this was reachable by
            // contract even though the only production projection happens to
            // always set `Some`. Render it as grounding — it cannot be cited,
            // so it must not claim to be.
            ("memory", None) => lines.push(format!("- {label} — {}", f.content.trim())),
            _ => lines.push(format!("- {label} — {}", f.content.trim())),
        }
    }
    if lines.is_empty() {
        return None;
    }
    let mut section = format!("Relevant context:\n{}", lines.join("\n"));
    if citable {
        section.push_str(
            "\n\nWhen a memory above (a [nod_…]-tagged line) actually informs your work this \
             turn, call cite_memory with that id once you can judge it: useful_score 1-5 for \
             how much it helped the actual work, truthful for whether its content still holds \
             (verify against the workspace, don't assume), and a one-sentence remark. Cite \
             only memories you genuinely used — no courtesy citations.",
        );
    }
    Some(section)
}

/// Land the recalled-context message for this turn at the conversation
/// TAIL — just before the turn's prompt when the prompt is already present
/// (one-shot paths), appended otherwise (interactive paths push the prompt
/// right after) — leaving previous turns' blocks in place as durable
/// history. Rewriting or removing an early message every turn (the old
/// index-1 refresh) byte-changed the front of the replayed history, which
/// reduced the provider cache's reusable prefix to the system message
/// alone for the whole session — the exact full-rate re-bill L-E8 exists
/// to prevent. Durability's cost is bounded: an unchanged block is not
/// re-appended (the model already sees it), so only genuinely new recall
/// content adds tokens, and it rides the cached prefix from the next turn
/// on. `None` (nothing relevant, or an A/B-suppressed turn) adds nothing
/// and touches nothing.
pub fn inject_recall_block(messages: &mut Vec<CompletionMessage>, block: Option<String>) {
    let is_marker =
        |m: &CompletionMessage| m.role == MessageRole::User && m.content.starts_with(RECALL_MARKER);
    let Some(content) = block else { return };
    if messages
        .iter()
        .rev()
        .find(|m| is_marker(m))
        .is_some_and(|m| m.content == content)
    {
        return;
    }
    let message = CompletionMessage {
        role: MessageRole::User,
        content,
        tool_calls: vec![],
        tool_results: vec![],
        attachments: Vec::new(),
    };
    // Context precedes the question: when the turn's prompt is already the
    // final message, slot the block just before it.
    let at = match messages.last() {
        Some(last)
            if last.role == MessageRole::User
                && !is_marker(last)
                && last.tool_results.is_empty() =>
        {
            messages.len() - 1
        }
        _ => messages.len(),
    };
    messages.insert(at, message);
}
