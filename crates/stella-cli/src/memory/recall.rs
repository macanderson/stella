//! Recall is the read side of session memory. This file builds the block of
//! recalled context a prompt is given, and lands it at the end of the
//! conversation.
//!
//! Three submodules hold the rest. `control` runs the A/B schedule that
//! withholds the block. `frames` asks the context host for frames, for the
//! block and for the pipeline's recall port. `render` turns each part into text.

mod control;
mod frames;
mod render;

use stella_learn::skills::{self, SelectionConfig};
use stella_protocol::{CompletionMessage, MessageRole, Recall};

#[cfg(test)]
pub(super) use control::{AB_RECALL_EXPERIMENT, ARTIFACT_HOLDOUT_EXPERIMENT, ab_control_turn};
pub(super) use frames::{
    RecalledFrames, goal_path_anchors, query_gathered_plane, turn_path_tokens,
};
#[cfg(test)]
pub(super) use frames::{frame_drop, report_budget_drops};
use frames::{kept_frames, kept_skills};
#[cfg(test)]
use render::drop_message;
pub use render::render_context_section;
pub(crate) use render::report_steering_drops;
pub(super) use render::{frame_recall_line, render_today_section};
use render::{now_unix_secs, record_section_text};

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
    /// The frames and skills that reached [`Self::text`], by steering handle.
    ///
    /// The render's own answer to "what has the model now been shown", which
    /// is a different question from what recall returned: the plane's budget
    /// and the citation-label rule both cut between the two. A mid-turn
    /// re-query carries this forward so the next block renders only what is
    /// new (#4236).
    pub produced: super::steering::ProducedSteering,
    /// The skills [`Self::text`] carried, in rendered order — the material
    /// each `SkillInjected` event is built from.
    ///
    /// Held rather than re-derived downstream because selection is not free
    /// and, more to the point, not stable to re-run: the block's own cut is
    /// the only place that knows which ranked skills the section's token
    /// budget actually admitted.
    pub injected_skills: Vec<skills::InjectedSkill>,
    /// The turn scopes the directive-carrying skills among
    /// [`Self::injected_skills`] ask for, in the same rendered order.
    ///
    /// An auto-selected skill that declares invoke directives expands
    /// exactly as an explicit `/slug` invocation does: its `allowed-tools`
    /// grant narrows the turn and its `effort` is honored. Safe without a
    /// human in the loop because narrowing is the only power a scope has —
    /// `skill_plane` enforces the grant as `operator ∧ grant`, so a scope can
    /// restrict the surface the operator configured, never widen it. Empty
    /// for the common turn whose skills carry no directive.
    pub skill_scopes: Vec<crate::extensions::SkillTurnScope>,
    /// What the turn's steering budgets refused, one advisory line each —
    /// the material each `SteeringDropped` event is built from.
    ///
    /// Carried out rather than printed. A `SessionMemory` under the deck
    /// shares its stderr with a `ratatui` frame, so a line written here lands
    /// inside the drawn screen and scrolls it out from under the renderer's
    /// diff. The caller owns a channel; this layer owns none.
    pub dropped: Vec<String>,
}

impl RecalledBlock {
    /// This block's recall telemetry, ready to send. `None` when no frame was
    /// recalled — a turn whose block is only skills has no frames to report.
    #[must_use]
    pub fn telemetry_event(&self) -> Option<stella_protocol::AgentEvent> {
        self.recall.telemetry_event()
    }

    /// Everything this block leaves for the turn runner's channel, in send
    /// order: the recall telemetry, then one `SkillInjected` per skill it
    /// carried — SPEC 6.3's `✦ skill` rows — and last one `SteeringDropped`
    /// per candidate the turn's budgets refused.
    ///
    /// One event each rather than one carrying a list, because each becomes
    /// one transcript row with its own head, subject and cost; a list would
    /// make the renderer split what the emitter had already separated. The
    /// refusals come last: one is only legible once the reader has seen what
    /// did get a seat.
    #[must_use]
    pub fn telemetry_events(&self) -> Vec<stella_protocol::AgentEvent> {
        self.telemetry_event()
            .into_iter()
            .chain(self.injected_skills.iter().map(|s| {
                stella_protocol::AgentEvent::SkillInjected {
                    name: s.name.clone(),
                    summary: s.summary.clone(),
                    tokens: s.tokens,
                    trigger: stella_protocol::SkillTrigger::Auto,
                }
            }))
            .chain(self.dropped.iter().map(|advisory| {
                stella_protocol::AgentEvent::SteeringDropped {
                    advisory: advisory.clone(),
                }
            }))
            .collect()
    }
}

/// What a turn-opening recall leaves behind for the turn runner: the
/// telemetry to put on the channel the runner opens, and the handles the
/// injected block rendered, which seed the mid-turn re-query so its first
/// answer does not repeat the opening block (#4498).
///
/// They travel together because they are residues of one injection — carrying
/// only the events was exactly how the seed got lost: the block's `produced`
/// died at the call site while its event rode on.
#[derive(Debug, Default)]
pub(crate) struct OpeningRecall {
    /// This turn's `ContextRecall` if recall ran, then one `SkillInjected`
    /// per skill the block carried — emitted by the turn runner, which owns
    /// the event channel the block precedes.
    ///
    /// **In send order**, which is the producer's to decide: recall and the
    /// skills entered the prompt in one block, and a reader who saw a skill
    /// ahead of the recall it rode with would place it in the wrong turn.
    pub(crate) events: Vec<stella_protocol::AgentEvent>,
    /// The frames, skills and records the opening block rendered, by steering
    /// handle — the re-query adapter's seed.
    pub(crate) produced: super::steering::ProducedSteering,
    /// The turn scopes the directive-carrying skills of this turn ask for —
    /// the grant narrowing and effort override the turn driver applies while
    /// the invocations are live. Every scope reaches the turn the same way,
    /// whether a human typed `/slug` or recall selected the skill:
    /// each is mounted as its own span on the invocation plane, so the tool
    /// surface is the intersection of every live grant. Empty for the common
    /// turn that carries no directive-carrying skill.
    ///
    /// Ordered with the explicitly invoked skill first, then the
    /// auto-selected ones by rank — the order [`Self::skill_effort`] reads.
    pub(crate) skill_scopes: Vec<crate::extensions::SkillTurnScope>,
}

impl OpeningRecall {
    /// Mount every scope of this turn on `plane`, returning the guards that
    /// hold the spans open. Drop them together with the turn's tool stack:
    /// every narrowing lifts structurally when the guards go.
    ///
    /// The one place the scopes become spans, so no driver can mount the
    /// invoked skill's grant and forget the auto-selected ones.
    #[must_use = "the spans end (and their narrowing lifts) the moment these guards drop"]
    pub(crate) fn mount_skill_spans(
        &self,
        plane: &stella_tools::skill_plane::SkillInvocationPlane,
    ) -> Vec<stella_tools::skill_plane::SkillSpanGuard> {
        self.skill_scopes
            .iter()
            .map(|scope| plane.begin(&scope.slug, scope.allowed_tools.as_deref()))
            .collect()
    }

    /// The reasoning-effort override this turn runs under, when any scope
    /// declares one: the invoked skill's wins over an auto-selected skill's,
    /// and among auto-selected skills the higher-ranked one wins — the same
    /// order the scopes are kept in, read to the first declaration.
    #[must_use]
    pub(crate) fn skill_effort(&self) -> Option<stella_protocol::ReasoningEffort> {
        self.skill_scopes.iter().find_map(|scope| scope.effort)
    }
    /// Report a skill the user invoked by name, on the same channel the
    /// auto-selected ones ride (#5232).
    ///
    /// A `/slug` expansion happens before this turn's recall and outside it —
    /// the skill's body is the prompt rather than a section of the steering
    /// block — so it has no [`RecalledBlock`] to be carried by, and until it
    /// had one of these it reached the prompt without producing an event at
    /// all. It goes after the block's own skills so every `✦ skill` row of a
    /// turn stays contiguous.
    pub(crate) fn note_invoked_skill(&mut self, skill: Option<crate::extensions::InvokedSkill>) {
        if let Some(skill) = skill {
            // The scope rides beside the event rather than inside it: the
            // event is the transcript's record that a skill was injected,
            // and the scope is a turn-driver instruction the transcript
            // never needs. First, ahead of the auto-selected scopes: a
            // human asked for this one, so its `effort` outranks theirs.
            if let Some(scope) = skill.scope {
                self.skill_scopes.insert(0, scope);
            }
            self.events
                .push(stella_protocol::AgentEvent::SkillInjected {
                    name: skill.name,
                    summary: skill.summary,
                    tokens: skill.tokens,
                    trigger: stella_protocol::SkillTrigger::Command,
                });
        }
    }

    /// The skills this turn was asked for by name, for the usage recorder.
    ///
    /// Read back off the events rather than kept as a second field, so the
    /// two answers cannot disagree: what the transcript shows as invoked and
    /// what `skill_usage` counts as invoked are the same list by
    /// construction.
    pub(crate) fn invoked_skills(&self) -> Vec<&str> {
        self.events
            .iter()
            .filter_map(|event| match event {
                stella_protocol::AgentEvent::SkillInjected {
                    name,
                    trigger: stella_protocol::SkillTrigger::Command,
                    ..
                } => Some(name.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// Inject `recalled`'s block ([`inject_recall_block`]) and keep what the turn
/// runner still needs of it — the one seam through which an opening recall
/// reaches a turn, so no call site can inject the block and drop the seed.
///
/// It is also where the block's cost reaches `ledger`, the allowance it
/// shares with the tool array. The cost is measured over the injected bytes,
/// so what the plane records and what the provider is sent are one number. It
/// is charged only when the block is really appended. A block the dedup
/// refuses costs this turn nothing, since the model already carries it.
///
/// **This is the turn boundary the allowance is measured on.** The allowance
/// is what one turn's volatile block may take. Every driver opens a turn
/// through this seam, so
/// [`stella_core::steering::ledger::SteeringLedger::open_turn`] is called
/// here rather than in each driver. A ledger that added every turn up would
/// charge one turn for what the whole session spent, and far enough in would
/// advertise no tools at all.
pub(crate) fn inject_opening_recall(
    messages: &mut Vec<CompletionMessage>,
    recalled: RecalledBlock,
    ledger: &stella_core::steering::ledger::SteeringLedger,
) -> OpeningRecall {
    ledger.open_turn();
    let events = recalled.telemetry_events();
    let cost = recalled
        .text
        .as_deref()
        .map_or(0, stella_protocol::estimate_tokens);
    if inject_recall_block(messages, recalled.text) {
        ledger.spend(cost);
    }
    OpeningRecall {
        events,
        produced: recalled.produced,
        // The block's directive-carrying skills scope the turn exactly as
        // an explicit invocation would: selection is the trigger,
        // and the grant's only power is to narrow.
        skill_scopes: recalled.skill_scopes,
    }
}

/// The turn scopes of the skills that actually reached the prompt — the
/// same prefix of `kept` [`skills::injected_skills`] announces, so a skill
/// the section budget cut can neither be reported nor narrow anything.
fn auto_skill_scopes(kept: &[skills::SelectedSkill]) -> Vec<crate::extensions::SkillTurnScope> {
    if kept.is_empty() {
        return Vec::new();
    }
    kept[..skills::section_fit(kept)]
        .iter()
        .filter_map(|sel| {
            let directives = crate::extensions::invoke_directives_for(&sel.skill);
            crate::extensions::skill_turn_scope(&sel.skill.name, &directives)
        })
        .collect()
}

/// How many characters of volatile records ride the recall block.
///
/// Bounded for the same reason the baked prefix bounds memories, and it lives
/// here rather than beside that budget because this one bounds the *recall
/// block* — the thing rendered a few lines below — not the system prompt. The
/// volatile channel competes with recalled memories and skills for one turn's
/// attention, so a records set that grew without bound would crowd out the
/// recall it sits beside. Records over the budget are reported as dropped
/// rather than silently lost — that gap is `MissingContextKind::NotRendered`.
pub(super) const RECORD_CHANNEL_BUDGET: usize = 2_000;

impl SessionMemory {
    /// Hand this session the resolved record registry behind the volatile
    /// channel.
    ///
    /// Private to the memory module, and reachable only through
    /// [`SessionMemory::open_for_session`], which passes the rule registry the
    /// driver already resolved — so recall never re-walks the rule directories
    /// or re-runs the truth sweep, and no session surface can open a memory
    /// that skips this step. The registry is stored rather than a rendered
    /// string because rendering happens per turn: `applies_to` selection needs
    /// each turn's prompt to decide which scoped records apply.
    pub(super) fn set_record_registry(&mut self, registry: stella_records::records::Registry) {
        *self.record_registry.get_mut().expect("records lock") =
            (!registry.entries.is_empty()).then_some(registry);
        // Prime the freshness digest, so the first boundary check after open
        // reads "unchanged" instead of reloading what was just handed in.
        *self.records_fingerprint.get_mut().expect("records digest") =
            super::records_refresh::rules_digest(&self.workspace_root);
    }

    /// The record channel's section and eviction report, with an injectable
    /// diagnostic sink — the same split as
    /// [`Self::recalled_frames_reporting`] and for the same reason: the
    /// eviction report must be testable without capturing global stderr. A
    /// record reported here MATCHED this turn's facts — the selector chose it
    /// and the budget evicted it — which is the coverage gap #2709 requires
    /// to be observable rather than silent: a scoped rule that systematically
    /// loses its seat looks exactly like a rule that never applied, unless
    /// someone says otherwise.
    ///
    /// Since #3349 this is a composition over the same pieces the steering
    /// plane uses — and test-gated like [`Self::recall_block`], because the
    /// production path is now the plane's: a production caller reaching for
    /// the record section alone would be a fifth selector the migration just
    /// removed.
    #[cfg(test)]
    pub(super) fn turn_record_section_reporting(
        &self,
        prompt: &str,
        mut report: impl FnMut(String),
    ) -> Option<String> {
        let (registry, rendered) = self.turn_records_for_prompt(prompt)?;
        for drop in stella_records::adapt::record_drops(&registry, &rendered) {
            // A record channel drop is never also selected — the channel's
            // own budget cut it before the plane saw it.
            if let Some(message) = drop_message(&drop, false) {
                report(message);
            }
        }
        record_section_text(rendered)
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
    /// **A/B control (Proposal 4):** when `ab_suppressed` is true (this turn's
    /// number came up on the control's durable schedule — see
    /// [`Self::arm_recall_control`]), recall returns `None` so the turn runs
    /// without context — the outcome is then comparable to recalled turns.
    ///
    /// Test-only since Phase 2 (#713): every production caller now takes
    /// [`Self::recall_block_reported`], because a caller that only wants the
    /// string is a caller that emits no recall telemetry — which is exactly
    /// the defect deliverable 3 closes. Keeping the convenience form reachable
    /// from production would let the next recall site reintroduce it silently.
    #[cfg(test)]
    pub async fn recall_block(&self, prompt: &str) -> Option<String> {
        self.recall_block_reported(prompt, &[]).await.text
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
    /// `touched` is what the conversation has already changed — the anchor
    /// set's second half, and the difference between a scoped recall and one
    /// that matches a word anywhere in the index. Empty is the honest argument
    /// for a turn that has touched nothing yet; callers with a live
    /// conversation should derive it (see
    /// `stella_core::driver::loop_evidence::turn_evidence`).
    pub async fn recall_block_reported(&self, prompt: &str, touched: &[String]) -> RecalledBlock {
        // Two switches, one gate. `ab_suppressed` withholds injection for ONE
        // turn to build a control arm; `steering_enabled` withholds it for the
        // session because an operator or their org asked (#3243). Both stop
        // here, before any provider is queried, so an off session pays nothing
        // — a gate after retrieval would spend the tokens and discard them.
        if self.ab_suppressed || !self.steering_enabled {
            return RecalledBlock::default();
        }
        // A record edited since the last look joins this very block — see
        // `records_refresh` for what a swap can and cannot apply.
        self.refresh_records_if_changed();
        // Every refusal this turn makes, for the block to carry out
        // (`RecalledBlock::dropped`). The two reporters below run in
        // sequence, so one collector serves both and the lines keep the order
        // the plane refused them in.
        let mut dropped: Vec<String> = Vec::new();
        let RecalledFrames {
            recall,
            dropped: frame_drops,
        } = self
            .recalled_frames_anchored(prompt, self.anchors_for(prompt, touched), |message| {
                dropped.push(message);
            })
            .await;

        let all_skills = self.load_skills();
        let mut selected = skills::select_skills_reporting(
            &all_skills,
            prompt,
            &self.active_domains(prompt),
            &SelectionConfig::default(),
        );
        // This turn's holdout, applied to the block the model actually reads.
        // The trial ledger records the same withholding, and the two only
        // agree because both ask `apply_holdout`.
        self.apply_holdout(&mut selected);

        // The volatile context-record channel (epic #897). Same channel as the
        // memories above and for the same reason: a fact about a staging URL costs
        // tokens on every turn and is worth them on almost none — which is why it
        // is selected per turn: a record scoped by `applies_to` renders only when
        // this prompt names a matching path, task, or keyword.
        let record = self.turn_records_for_prompt(prompt);

        let signal = stella_core::steering::TurnSignal {
            prompt,
            ..Default::default()
        };
        let set = query_gathered_plane(
            &signal,
            &recall.frames,
            &frame_drops,
            &selected,
            record.as_ref(),
        );
        report_steering_drops(&set, self.retrieval.max_tokens, |message| {
            dropped.push(message);
        });

        // The holdout's memory arm, and this turn's memory join, in one door.
        let frames = self.withhold_held_memory(&recall.frames, kept_frames(&recall.frames, &set));
        let kept = kept_skills(&selected.selected, &set);
        let record_handles = record
            .as_ref()
            .map(|(_, rendered)| rendered.rendered.clone())
            .unwrap_or_default();
        let produced = super::steering::ProducedSteering::of(&frames, &kept, &record_handles);

        let mut sections: Vec<String> = Vec::new();
        if let Some(section) = render_context_section(&frames) {
            sections.push(section);
        }
        if !kept.is_empty() {
            sections.push(skills::render_skills_section(&kept));
        }
        if let Some(section) = record.and_then(|(_, rendered)| record_section_text(rendered)) {
            sections.push(section);
        }
        // What a mid-session registry swap could not fully apply, said once,
        // first — see `records_refresh`.
        if let Some(note) = self.take_records_note() {
            sections.insert(0, note);
        }

        // The second operand of the knowledge-cutoff staleness clause
        // (#2901): unconditional, unlike the sections above, because the
        // model needs "now" to judge staleness on every turn, not only the
        // ones that also recalled a memory or a record.
        sections.push(render_today_section(now_unix_secs()));

        RecalledBlock {
            // Skills and records can produce a block with no frames behind
            // it; frames can be recalled and then filtered out of the render by
            // the citation-label rule. The two fields are therefore reported
            // independently rather than one gating the other — a provider still
            // spent the tokens for a frame the host later dropped, and a cost
            // that vanishes because the render discarded it is exactly the
            // unmeterable cost this event exists to surface.
            text: (!sections.is_empty())
                .then(|| format!("{RECALL_MARKER}\n\n{}", sections.join("\n\n"))),
            recall,
            produced,
            injected_skills: skills::injected_skills(&kept),
            skill_scopes: auto_skill_scopes(&kept),
            dropped,
        }
    }

    /// The volatile block for a turn that has DRIFTED from its opening
    /// prompt — the same three selectors as
    /// [`Self::recall_block_reported`], queried against what the turn has
    /// become instead of what it was asked.
    ///
    /// The signal's touched paths do the work the prompt could not: they
    /// join the recall anchors once they resolve to real files, they widen
    /// the domain scope skills are selected in, and they join the record
    /// channel's `applies_to` path facts. The prompt still carries the
    /// lexical query — drift changes *where* the turn is, not what it was
    /// asked to do.
    ///
    /// One omission against the pre-turn block: no date section, since the
    /// turn-opening block already carries today. The `Recall` travels back
    /// with the text for the reason it does in
    /// [`Self::recall_block_reported`] — a re-query is a full fan-out with
    /// provider spend behind it, and the adapter that called it reports that
    /// spend into the turn's event stream. `RecalledBlock::text` is `None`
    /// under the same three gates: nothing surfaced, an A/B control turn, or
    /// steering off.
    ///
    /// `produced` is what this turn's earlier blocks rendered, and every
    /// frame, skill and record in it is left out of this one. Drift is
    /// incremental — a re-query answering `{A, B, C, D}` after one that
    /// answered `{A, B, C}` differs by one frame — so a block deduped by its
    /// bytes alone always differs, and is always injected whole. These are
    /// `User` messages that only the overflow summarizer can reclaim, so each
    /// repeat is permanent in the paid prefix for the rest of the session.
    pub async fn signal_recall_block(
        &self,
        signal: &stella_core::steering::TurnSignal<'_>,
        produced: &super::steering::ProducedSteering,
    ) -> RecalledBlock {
        if self.ab_suppressed || !self.steering_enabled {
            return RecalledBlock::default();
        }
        // A no-op when the re-query boundary already refreshed (one digest
        // read); real work only for a direct caller — see `records_refresh`.
        self.refresh_records_if_changed();
        let prompt = signal.prompt;

        let anchors = self.anchors_for(prompt, signal.touched_paths);
        let domains = crate::contextgraph::query_domain_scope(&self.domains, &anchors);

        let RecalledFrames {
            recall,
            dropped: frame_drops,
        } = self.recalled_frames_anchored(prompt, anchors, |_| {}).await;

        let all_skills = self.load_skills();
        let mut selected = skills::select_skills_reporting(
            &all_skills,
            prompt,
            &domains,
            &SelectionConfig::default(),
        );
        // The same holdout the block above applies. This pass scopes its
        // domains by the paths the turn touched rather than by the prompt
        // alone, so its shortlist differs — which is exactly why the pick reads
        // the catalog both passes share.
        self.apply_holdout(&mut selected);

        let mut paths = turn_path_tokens(prompt);
        for path in signal.touched_paths {
            if !paths.contains(path) {
                paths.push(path.clone());
            }
        }
        let facts = stella_records::records::TurnFacts {
            text: prompt,
            paths: &paths,
        };
        // The per-record half of the #4498 dedup, and the holdout's rule arm:
        // the channel renders as one budgeted block, so leaving out what the
        // turn has seen means re-rendering without those records, not
        // filtering a list — the exclusion door is the registry's.
        let record = self.turn_records_held(&facts, produced.records());

        let set = query_gathered_plane(
            signal,
            &recall.frames,
            &frame_drops,
            &selected,
            record.as_ref(),
        );
        let mut dropped: Vec<String> = Vec::new();
        report_steering_drops(&set, self.retrieval.max_tokens, |message| {
            dropped.push(message);
        });

        // The per-frame cut this block exists to make. It runs AFTER the plane
        // has packed, not before the query: the drop is about what the model
        // has already been shown, and the recall telemetry must still report
        // the frame — recall spent the tokens fetching it either way, and a
        // report that quietly forgot the ones the render suppressed would
        // under-report the fan-out the re-query paid for. The trial ledger is
        // a different question and gets a different answer: `produced` frames
        // were shown earlier this turn, so `withhold_held_memory` folds them
        // into the turn's selected set even though this block leaves them out.
        let frames = self.withhold_held_memory(
            &recall.frames,
            kept_frames(&recall.frames, &set)
                .into_iter()
                .filter(|frame| !produced.has_frame(&super::steering::frame_handle(frame)))
                .collect(),
        );
        let kept: Vec<skills::SelectedSkill> = kept_skills(&selected.selected, &set)
            .into_iter()
            .filter(|sel| !produced.has_skill(&sel.skill.name))
            .collect();
        let record_handles = record
            .as_ref()
            .map(|(_, rendered)| rendered.rendered.clone())
            .unwrap_or_default();
        let block_produced = super::steering::ProducedSteering::of(&frames, &kept, &record_handles);

        let mut sections: Vec<String> = Vec::new();
        if let Some(section) = render_context_section(&frames) {
            sections.push(section);
        }
        if !kept.is_empty() {
            sections.push(skills::render_skills_section(&kept));
        }
        if let Some(section) = record.and_then(|(_, rendered)| record_section_text(rendered)) {
            sections.push(section);
        }
        // What a mid-session registry swap could not fully apply, said once,
        // first — see `records_refresh`.
        if let Some(note) = self.take_records_note() {
            sections.insert(0, note);
        }
        RecalledBlock {
            text: (!sections.is_empty())
                .then(|| format!("{RECALL_MARKER}\n\n{}", sections.join("\n\n"))),
            recall,
            produced: block_produced,
            injected_skills: skills::injected_skills(&kept),
            // A drifted turn's re-query rescopes nothing: the spans were
            // mounted when the turn opened, beside the tool stack they
            // narrow, and a grant that appeared mid-loop would change the
            // surface under calls already in flight. A directive-carrying
            // skill that surfaces here is context until the next turn opens
            // with it selected — the same rule an explicit invocation obeys.
            skill_scopes: Vec::new(),
            dropped,
        }
    }

    /// The domains **this turn** is working in, derived from the workspace
    /// paths its prompt names.
    ///
    /// Every `select_skills` call site used to pass `self.domains.names()`,
    /// which is every domain the repository declares, loaded once at session
    /// open and constant for the session. Against that list `matched_domains`
    /// answers "is this skill tagged with a domain this repo has?" — true for
    /// every domain-tagged skill on every turn — rather than "is this domain
    /// active right now?". With `domain_boost` at 0.5 against a `min_score` of
    /// 0.08, and a single domain match satisfying `corroborated`, the effect
    /// was that any domain-tagged skill was injected on every non-control
    /// turn regardless of the prompt, ranked by how MANY tags it carried, so a
    /// 3-tag skill permanently outranked a perfectly-matching 1-tag one and
    /// the top-k was spent on the most-tagged rather than the most-relevant
    /// (#3243 D1).
    ///
    /// Memory recall already derives per-query scope this way (#2333); skill
    /// selection was simply never migrated to it. Anchors require the file to
    /// exist, so a prompt naming no extant path yields an empty scope — the
    /// honest answer, and the reason the lexical score has to carry its own
    /// weight (see `mining::coverage`).
    pub(super) fn active_domains(&self, prompt: &str) -> Vec<String> {
        let anchors = goal_path_anchors(prompt, &self.workspace_root);
        crate::contextgraph::query_domain_scope(&self.domains, &anchors)
    }
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
///
/// Answers whether the block was appended. That is what
/// [`inject_opening_recall`] charges the steering ledger on. A block the dedup
/// below refuses costs this turn nothing, since the model already carries it.
#[must_use]
pub fn inject_recall_block(messages: &mut Vec<CompletionMessage>, block: Option<String>) -> bool {
    let is_marker =
        |m: &CompletionMessage| m.role == MessageRole::User && m.content.starts_with(RECALL_MARKER);
    let Some(content) = block else { return false };
    // Against EVERY prior marker, not just the most recent one.
    //
    // Comparing only the latest made the dedup order-sensitive: an A → B → A
    // recall sequence re-appended A, because by then B was the newest marker.
    // Recall content genuinely does oscillate — it is a function of the
    // prompt, so returning to an earlier subject returns to an earlier block —
    // and each one is up to ~3k tokens that nothing can ever reclaim. These
    // are User messages, and compaction passes 1–4 only rewrite tool results,
    // so only the overflow summarizer can remove them. A 30-turn REPL session
    // with shifting recall accumulated ~90k tokens of superseded blocks,
    // permanently in the paid prefix (#1846).
    //
    // A set membership test rather than superseding the old marker in place:
    // rewriting an earlier message byte-changes the replayed prefix from that
    // point on, which is precisely the full-rate re-bill the index-1 refresh
    // was removed for (see this function's header, L-E8). Skipping an append
    // costs nothing and breaks nothing — the model has already been shown that
    // block, and it is still in history where it can see it.
    if messages
        .iter()
        .any(|m| is_marker(m) && m.content == content)
    {
        return false;
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
    true
}
