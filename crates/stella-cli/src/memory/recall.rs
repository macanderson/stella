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
use stella_learn::skills::{self, SelectionConfig};
use stella_protocol::{CompletionMessage, ContextRecallPort, MessageRole, Recall, RecalledFrame};
use stella_records::records::RenderedChannel;

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
    /// carried — SPEC 6.3's `✦ skill` rows.
    ///
    /// One event per skill rather than one carrying a list, because each
    /// becomes one transcript row with its own head, subject and cost; a list
    /// would make the renderer split what the emitter had already separated.
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
/// shares with the tool array. Measured over the injected bytes, so what the
/// plane records and what the provider is sent are one number; and charged
/// only when the block is genuinely appended, since a block the dedup refuses
/// is one an earlier turn already paid for and already spent here.
pub(crate) fn inject_opening_recall(
    messages: &mut Vec<CompletionMessage>,
    recalled: RecalledBlock,
    ledger: &stella_core::steering::ledger::SteeringLedger,
) -> OpeningRecall {
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

/// The name the A/B recall control's durable turn counter is filed under in
/// the context store (#1221). A name rather than an implicit singleton row so
/// a second experiment can be scheduled later without renumbering this one —
/// and so a reader of `ab_control_counter` can tell what the row counts.
pub(super) const AB_RECALL_EXPERIMENT: &str = "recall_suppression";

/// The name the per-artifact holdout's durable turn counter is filed under.
///
/// Its own row, not a share of [`AB_RECALL_EXPERIMENT`]'s: the two schedules
/// count different populations, since a plane-control turn advances the first
/// and is skipped by the second.
pub(super) const ARTIFACT_HOLDOUT_EXPERIMENT: &str = "artifact_holdout";

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
        let RecalledFrames {
            recall,
            dropped: frame_drops,
        } = self
            .recalled_frames_anchored(prompt, self.anchors_for(prompt, touched), |message| {
                eprintln!("  {} {message}", "!".yellow())
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
            eprintln!("  {} {message}", "!".yellow())
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
        }
    }

    /// The volatile block for a turn that has DRIFTED from its opening
    /// prompt (#3243 Phase 3) — the same three selectors as
    /// [`Self::recall_block_reported`], queried against what the turn has
    /// become instead of what it was asked.
    ///
    /// The signal's touched paths do the work the prompt could not: they
    /// join the recall anchors (when they resolve to real files — a created
    /// file qualifies the moment it exists), they widen the domain scope
    /// skills are selected in, and they join the record channel's
    /// `applies_to` path facts. The prompt still carries the lexical query —
    /// drift changes *where* the turn is, not what it was asked to do.
    ///
    /// One deliberate omission against the pre-turn block: no date section
    /// (the turn-opening block already carries today, and repeating it
    /// mid-turn buys nothing). The `Recall` travels back with the text for
    /// the same reason it does in [`Self::recall_block_reported`] — a
    /// re-query is a full fan-out with provider spend behind it, and the
    /// adapter that called it reports that spend into the turn's event
    /// stream (#3366). `RecalledBlock::text` is `None` when nothing
    /// surfaced, when the turn is an A/B control, or when steering is off —
    /// the same gates, for the same reasons.
    ///
    /// `produced` is what this turn's earlier blocks already rendered, and
    /// every frame, skill and record in it is left out of this one (#4236,
    /// records since #4498). Drift is
    /// incremental — a re-query answering `{A, B, C, D}` after one that
    /// answered `{A, B, C}` differs by one frame — so a block deduped by its
    /// bytes alone is a block that always differs and is therefore always
    /// injected, whole. These are `User` messages that only the overflow
    /// summarizer can ever reclaim, so each repeat is permanent in the paid
    /// prefix for the rest of the session.
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
        report_steering_drops(&set, self.retrieval.max_tokens, |message| {
            eprintln!("  {} {message}", "!".yellow())
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

    /// Arm this turn's A/B recall control at the workspace's configured rate
    /// (`context.retrieval.ab_recall_rate`), returning whether this turn is a
    /// control turn.
    ///
    /// **Every driver calls this once per turn, before it recalls anything.**
    /// The flag is turn-scoped but only *reset* here, so a driver that skips it
    /// inherits the previous turn's arm — and, more to the point, a driver that
    /// skips it produces no control turns at all, which is what made the
    /// measurement structurally impossible everywhere except the interactive
    /// REPL's plain prompts (#1221). It takes no rate argument for the same
    /// reason [`SessionMemory::open_for_session`] takes no "and also attach the
    /// records" flag: a per-driver copy of the rate is a per-driver way to get
    /// the schedule wrong.
    ///
    /// A "turn" here is one user-supplied prompt — not one internal round. The
    /// goal loop's rounds and the pipeline's stages all belong to the turn that
    /// armed them, which is also the unit the episode is recorded in, so the
    /// arm and its attribution describe the same thing.
    pub fn arm_recall_control(&mut self) -> bool {
        self.arm_controls(
            self.retrieval.ab_recall_rate,
            self.retrieval.artifact_holdout_rate,
        )
    }

    /// Arm both of this turn's controls, plane first, and report whether the
    /// plane one fired.
    ///
    /// Order is the whole point. The per-artifact holdout is skipped on a turn
    /// that injects nothing anyway, and whether this is one is not settled
    /// until the plane arm is.
    fn arm_controls(&mut self, recall_rate: u32, holdout_rate: u32) -> bool {
        // The join and the holdout pick belong to the turn that armed them.
        self.reset_context_trials();
        let suppressed = self.maybe_suppress_recall(recall_rate);
        self.arm_artifact_holdout(holdout_rate);
        suppressed
    }

    /// Arm this turn's per-artifact holdout
    /// (`context.retrieval.artifact_holdout_rate`).
    ///
    /// Its own durable counter, under its own experiment name, so the two
    /// schedules can be read apart afterwards.
    ///
    /// **A turn that injects nothing claims no number here.** Both counters
    /// advance once per turn, so two schedules counting every turn stay in
    /// lockstep: a plane rate of 10 and a holdout rate of 20 would land every
    /// holdout on a turn where the plane had already withheld everything, and
    /// the per-artifact arm would carry no measurement the plane arm did not
    /// already carry. Skipping instead means this counter advances only over
    /// turns the holdout could act on — which rules out a session with
    /// steering switched off for the same reason.
    ///
    /// Degrades exactly as [`Self::maybe_suppress_recall`] does: a store that
    /// cannot hand out a number falls back to the in-session tally, which on a
    /// one-turn process means no holdout. A lost holdout costs one sample;
    /// holding a skill back on a turn the schedule did not choose costs the
    /// user that skill for nothing.
    fn arm_artifact_holdout(&mut self, rate: u32) {
        self.holdout_ordinal = None;
        if rate <= 1 || self.injection_suppressed() {
            return;
        }
        self.holdout_turn = match self.store.next_ab_control_turn(ARTIFACT_HOLDOUT_EXPERIMENT) {
            Ok(turn) => turn,
            Err(_) => self.holdout_turn.wrapping_add(1),
        };
        self.holdout_ordinal = stella_learn::holdout::ordinal(self.holdout_turn, rate);
    }

    /// A/B recall control (Proposal 4): suppress recall for this turn on a
    /// deterministic `1/rate` schedule, returning whether recall was
    /// suppressed. A rate of 0 (or 1) never suppresses.
    ///
    /// The schedule is driven by a **turn counter**, not a wall clock. A
    /// previous implementation seeded off `SystemTime` nanoseconds and tested
    /// `ns % rate == 0`; on any host whose realtime clock is coarser than
    /// nanoseconds (macOS keeps it in microseconds, so `ns` is always a
    /// multiple of 1000) that predicate is true on *every* turn for any `rate`
    /// dividing 1000 — silently disabling recall entirely. A plain counter
    /// makes exactly every `rate`-th turn a control turn, on every OS.
    ///
    /// That counter is **durable** (#1221): it is claimed from the workspace's
    /// context store, so it survives the process that claimed it. A per-session
    /// counter cannot schedule anything on the surfaces that matter most —
    /// `stella run`, a fleet task and a `/goal` are one turn per process, so
    /// the session counter is 1 on every one of them and no control turn ever
    /// happens. Two processes against one workspace each claim a distinct
    /// number, so the arms interleave across surfaces instead of each surface
    /// running its own private schedule.
    ///
    /// A store that cannot hand out a number degrades to the in-session
    /// counter, which on a one-turn process means "not a control turn". That
    /// direction is deliberate: a lost control turn costs the experiment one
    /// sample, while suppressing recall on a turn the schedule did not choose
    /// costs the user their memory for no measurement at all.
    fn maybe_suppress_recall(&mut self, rate: u32) -> bool {
        if rate == 0 || rate == 1 {
            self.ab_suppressed = false;
            return false;
        }
        self.ab_turn = match self.store.next_ab_control_turn(AB_RECALL_EXPERIMENT) {
            Ok(turn) => turn,
            Err(_) => self.ab_turn.wrapping_add(1),
        };
        self.ab_suppressed = ab_control_turn(self.ab_turn, rate);
        self.ab_suppressed
    }

    /// Arm the control at an explicit rate, for tests that must not depend on
    /// a workspace's settings file. Production arms through
    /// [`Self::arm_recall_control`], which is the only door that reads the
    /// configured rate.
    #[cfg(test)]
    pub(crate) fn arm_recall_control_at(&mut self, rate: u32) -> bool {
        self.arm_controls(rate, 0)
    }

    /// Arm both controls at explicit rates, for the same reason
    /// [`Self::arm_recall_control_at`] exists. A test that wants only the
    /// per-artifact holdout passes `0` for the plane rate.
    #[cfg(test)]
    pub(crate) fn arm_controls_at(&mut self, recall_rate: u32, holdout_rate: u32) -> bool {
        self.arm_controls(recall_rate, holdout_rate)
    }

    /// Whether recall was suppressed this turn.
    ///
    /// Test-gated: outcome attribution used to read this from `agent.rs` and
    /// compose the `[ab-control]` tag itself, which is exactly the arrangement
    /// that left three of the four episode-writing surfaces untagged.
    /// [`SessionMemory::record_episode`] now reads the flag directly, so a
    /// production caller of this is a caller re-deriving attribution beside the
    /// one place that owns it. Drop the gate if a surface ever needs to *show*
    /// a control turn rather than record one.
    #[cfg(test)]
    pub(crate) fn recall_was_suppressed(&self) -> bool {
        self.ab_suppressed
    }

    /// Authoritative prompt and pipeline recall, including a fresh quarantine
    /// read so prior-turn feedback applies immediately.
    /// What this turn is *about*, as paths: what the prompt names, then what
    /// the conversation has already touched.
    ///
    /// Capped so a busy turn cannot turn one query into a full-graph walk, and
    /// every anchor must be a file that exists — an anchor naming nothing
    /// scopes nothing.
    ///
    /// The second half is what the per-turn recall was missing, and it is why a
    /// turn could pull five frames of unrelated Python into a Rust task. A
    /// prompt like *"Upstream renamed `_model` to `model` and added a PR
    /// strip"* names no path at all, so the anchor set came out empty and
    /// retrieval ran unscoped across the whole index — where the word `model`
    /// matches a benchmark harness's `model.py` exactly as well as the file the
    /// turn is editing. Unscoped, lexical similarity has nothing to lose
    /// against, so it returns its top-k however weak the top is.
    ///
    /// The paths a turn has touched are the strongest available statement of
    /// what it is working on. `signal_recall_block` has composed anchors this
    /// way since #3243; this is that composition extracted, so the two callers
    /// cannot drift into disagreeing about what "about" means.
    pub(super) fn anchors_for(&self, prompt: &str, touched: &[String]) -> Vec<String> {
        const MAX_ANCHORS: usize = 8;
        let mut anchors = goal_path_anchors(prompt, &self.workspace_root);
        for path in touched {
            if anchors.len() >= MAX_ANCHORS {
                break;
            }
            if !anchors.contains(path) && self.workspace_root.join(path).is_file() {
                anchors.push(path.clone());
            }
        }
        anchors
    }

    async fn recalled_frames(&self, goal: &str) -> RecalledFrames {
        let anchors = goal_path_anchors(goal, &self.workspace_root);
        let max_tokens = self.retrieval.max_tokens;
        self.recalled_frames_anchored_reporting(goal, anchors, max_tokens, |message| {
            eprintln!("  {} {message}", "!".yellow())
        })
        .await
    }
    /// Recall with an injectable diagnostic sink to avoid global stderr capture in tests.
    ///
    /// Test-only, and gated as such: every caller is inside a `#[cfg(test)]`
    /// module, so the shipping binary carries no reference to it and
    /// `clippy -D warnings` reads it as dead code in that build.
    #[cfg(test)]
    pub(super) async fn recalled_frames_reporting(
        &self,
        goal: &str,
        report: impl FnMut(String),
    ) -> Recall {
        let anchors = goal_path_anchors(goal, &self.workspace_root);
        let max_tokens = self.retrieval.max_tokens;
        self.recalled_frames_anchored_reporting(goal, anchors, max_tokens, report)
            .await
            .recall
    }

    /// Recall with an injectable diagnostic sink AND the anchor set chosen by
    /// the caller — the seam the proactive re-query (#3243 Phase 3) queries
    /// through, because a drifted turn's best anchors are the paths it has
    /// TOUCHED, which the goal string cannot name.
    pub(super) async fn recalled_frames_anchored(
        &self,
        goal: &str,
        anchors: Vec<String>,
        report: impl FnMut(String),
    ) -> RecalledFrames {
        let max_tokens = self.retrieval.max_tokens;
        self.recalled_frames_anchored_reporting(goal, anchors, max_tokens, report)
            .await
    }

    /// The one frame-query body every recall path funnels through, with the
    /// turn's retrieval budget explicit: the budget-pressure summary the
    /// query emits names this number, so it is a parameter rather than a
    /// second read of `self.retrieval` the caller could disagree with.
    async fn recalled_frames_anchored_reporting(
        &self,
        goal: &str,
        anchors: Vec<String>,
        max_tokens: u32,
        mut report: impl FnMut(String),
    ) -> RecalledFrames {
        // Both withholding switches live HERE, at the frame query the rendered
        // block, the pipeline's `ContextRecallPort`, and the mid-turn re-query
        // all go through — not at the block renders alone. A control turn
        // whose port still answered would feed frames to the goal message, the
        // planner, and the witness author while the block above showed none,
        // which is not a control arm at all: the turn would be measured as
        // frameless while running on recalled context (#1221). Steering off is
        // the same leak with an operator behind it instead of an experiment:
        // gated only at the renders, the port kept injecting recalled frames
        // into pipeline, fleet, and resumed turns after the org said no
        // (#3243).
        if self.ab_suppressed || !self.steering_enabled {
            return RecalledFrames::default();
        }
        let query = ContextQuery {
            goal: goal.to_string(),
            query_text: Some(goal.to_string()),
            embedding: None,
            kinds: vec![],
            // Workspace files as anchors: the code-graph provider answers
            // anchors with each file's graph NEIGHBORHOOD (symbols + imports
            // + importers), not just goal-token definition lookups —
            // deterministic localization into both the planner prompt and
            // the worker's recall block, instead of hoping the model's first
            // move is a graph_query (#342 seam 3). The default set is what
            // the goal names verbatim; a re-query passes the paths the turn
            // has since touched.
            anchors,
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
                return RecalledFrames::default();
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
        // without a trace — a silent truncation `L-C5` bans.
        report_budget_drops(&recalled.dropped, max_tokens, &mut report);
        // ...and the same report goes to the ledger, which is the half that
        // used to end here (#3358): the stderr line above is a warning a human
        // sees once, on one of the several surfaces that recall, while
        // `SteeringSet::dropped` is what the plane can be *queried* about.
        let dropped = recalled.dropped.iter().map(frame_drop).collect();
        RecalledFrames {
            recall: Recall {
                frames: recalled
                    .frames
                    .into_iter()
                    .filter_map(project_recalled_frame)
                    .filter(|frame| !is_suppressed_local_frame(frame, &quarantined))
                    .collect(),
                usage: Some(recalled.usage),
                latency_ms,
                // The host fan-out does not carry the accelerator flag across
                // the provider-result boundary, and reporting `false` would
                // read as "the index never fires" rather than "nobody said".
                used_ann_index: None,
            },
            dropped,
        }
    }
}

/// The host-merge drop report: a single summary line per class rather than
/// one line per frame. Five evicted memories are one fact — the budget is too
/// small for this turn — said once, with the remedy and the budget the turn
/// actually ran with, and without the internal handle a user cannot act on.
/// A required item the merge could not honor is the loudest case and is named
/// first: the caller pointed at that file, and being told it did not fit is
/// the whole difference between a budget and a lie.
pub(super) fn report_budget_drops(
    dropped: &[crate::contextgraph::HostDroppedFrame],
    max_tokens: u32,
    report: &mut impl FnMut(String),
) {
    let required_over = dropped
        .iter()
        .filter(|d| d.reason == stella_context::DropReason::RequiredOverBudget)
        .count();
    if required_over > 0 {
        report(format!(
            "recall could not fit {required_over} anchored frame(s) — each exceeds the \
             {max_tokens}-token budget on its own; raise context.retrieval.max_tokens in \
             stella.toml to include them"
        ));
    }
    let budget_drops = dropped
        .iter()
        .filter(|d| d.reason == stella_context::DropReason::TokenBudget)
        .count();
    if budget_drops > 0 {
        report(format!(
            "{budget_drops} memories did not fit this turn's {max_tokens}-token retrieval \
             budget — raise context.retrieval.max_tokens in stella.toml to include them"
        ));
    }
}

/// One turn's frame recall **and** what the host's merge could not fit.
///
/// A sibling channel rather than two more fields on [`Recall`]: `Recall` is a
/// `stella-protocol` boundary type and the drop report is the CLI's own
/// host-merge type over a `stella-context` reason, and `stella-protocol`
/// does not depend on the context crate (`recall.rs`'s module doc states
/// that direction deliberately). Mapping to `DroppedCandidate` here — at the
/// one call site that has all of them in scope — keeps that boundary intact
/// and still gets the drops to the plane.
#[derive(Debug, Default)]
pub(super) struct RecalledFrames {
    pub recall: Recall,
    /// The host merge's evictions, already in ledger shape.
    pub dropped: Vec<stella_core::steering::DroppedCandidate>,
}

/// A host-merge eviction as a ledger entry.
///
/// The handle follows [`super::steering::frame_handle`]'s precedence — the
/// stable `nod_…` id when the frame has one, its citation label otherwise —
/// so a drop and a later selection of the same frame join on one identity.
///
/// `est_tokens` is the **host's** `token_cost`, not an estimate over
/// [`frame_recall_line`]: a dropped frame never reached this process with a
/// body, so there is no recall line to measure. The host's number is the one
/// the budget actually refused, which is also what a caller sizing
/// `context.retrieval.max_tokens` needs.
pub(super) fn frame_drop(
    drop: &crate::contextgraph::HostDroppedFrame,
) -> stella_core::steering::DroppedCandidate {
    stella_core::steering::DroppedCandidate {
        source: stella_core::steering::SteeringSource::Memory,
        handle: if drop.id.is_empty() {
            drop.citation_label.clone()
        } else {
            drop.id.clone()
        },
        est_tokens: u64::from(drop.token_cost),
    }
}

/// Is the `turn`-th turn (1-based) an A/B control turn at the given `rate`?
/// Every `rate`-th turn is a control turn; `rate` of 0 or 1 never controls.
/// Pure so the schedule is property-testable independent of the (heavy)
/// [`SessionMemory`] it lives on.
pub(super) fn ab_control_turn(turn: u64, rate: u32) -> bool {
    stella_learn::holdout::is_scheduled(turn, rate)
}

/// Path-shaped tokens the prompt names (`deny.toml`, `src/api/mod.rs`), for
/// `applies_to` path matching. Unlike [`goal_path_anchors`] below this does
/// NOT require the file to exist: a record scoped to `deny.toml` is about the
/// *topic* the turn raises, and the turn that says "add an MIT dep to
/// deny.toml" is exactly when it applies — whether or not the working
/// directory is the workspace root, and even before the file exists. A
/// token qualifies with a `/` (a path) or an interior `.` (a file name);
/// noise tokens cost nothing because they only matter when a record's
/// pattern matches them, and a pattern matching "v0.1.2" is a pattern the
/// author wrote to match it. `file:line` spellings anchor the file, a
/// leading `./` is normalized off, and escapes are rejected.
pub(super) fn turn_path_tokens(goal: &str) -> Vec<String> {
    const MAX_TOKENS: usize = 32;
    let mut seen = std::collections::HashSet::new();
    let mut tokens = Vec::new();
    for token in goal.split(|c: char| c.is_whitespace() || "\"'`()[]{}<>,;!?".contains(c)) {
        let token = token.trim_end_matches(['.', ':']);
        let token = token.split(':').next().unwrap_or(token);
        let token = token.strip_prefix("./").unwrap_or(token);
        let path_shaped = token.contains('/')
            || token
                .find('.')
                .is_some_and(|at| at > 0 && at + 1 < token.len());
        if !path_shaped
            || token.len() > 256
            || token.split('/').any(|seg| seg == "..")
            || token.starts_with('/')
        {
            continue;
        }
        if seen.insert(token.to_string()) {
            tokens.push(token.to_string());
            if tokens.len() == MAX_TOKENS {
                break;
            }
        }
    }
    tokens
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
/// failed recall, including quarantine verification, degrades to no frames
/// (L-C6). An A/B control turn returns no frames either, for the reason the
/// frame query itself gives: the port is how a pipeline-driven turn gets its
/// context, so a control arm that stopped at the rendered block would be
/// suppressed in name only.
#[async_trait::async_trait]
impl ContextRecallPort for SessionMemory {
    async fn recall(&self, goal: &str) -> Recall {
        self.recalled_frames(goal).await.recall
    }
}

/// Render recalled frames as the "Relevant context" section of the recall
/// block. Memory-kind frames carry their stable `[nod_…]` id inline — the
/// durable handle a reader (or a later promotion sweep) can resolve back to
/// the record. Other frame kinds (code-graph hits, episodes) keep the plain
/// label form: they are grounding, not memories. `None` when no frame has a
/// citation label (L-C4 filters the rest).
pub fn render_context_section(frames: &[RecalledFrame]) -> Option<String> {
    let lines: Vec<String> = frames.iter().map(frame_recall_line).collect();
    if lines.is_empty() {
        return None;
    }
    Some(format!("Relevant context:\n{}", lines.join("\n")))
}

/// The recall line ONE frame contributes to the section above.
///
/// Only a label that says something the content does not earns its bytes:
/// memory (and episode) nodes mint theirs FROM the content, so rendering both
/// shipped the same sentence twice into a recall budget the packer had
/// already spent on the content alone (#2476). A memory with an id keeps the
/// id visible — it names the record, and it is the join key
/// `receipts::parse_recall_item` reads back. A memory WITHOUT one still has
/// content worth recalling and the budget has already been spent fetching it;
/// `RecalledFrame` documents `id: None` as a legitimate state for a
/// not-yet-materialized frame (`crates/stella-protocol/src/recall.rs`), so it
/// renders as grounding.
///
/// Split out of the section loop so the steering plane's frame adapter
/// (`super::steering::frame_candidates`) estimates cost over these exact
/// bytes — one producer, per #3334.
pub(super) fn frame_recall_line(f: &RecalledFrame) -> String {
    stella_core::receipts::render_recall_line(&stella_core::receipts::RecallLine {
        id: (f.kind == "memory").then_some(f.id.as_deref()).flatten(),
        label: f.distinct_label(),
        body: f.content.trim(),
        source: None,
    })
}

/// One turn's gathered candidates, packed once — the production
/// [`stella_core::steering::SteeringPlane`] query every context source now
/// goes through (#3349). The frame slice is empty on pipeline-driven turns,
/// whose frames travel on the pipeline's own recall port.
pub(super) fn query_gathered_plane(
    signal: &stella_core::steering::TurnSignal<'_>,
    frames: &[RecalledFrame],
    frame_drops: &[stella_core::steering::DroppedCandidate],
    selected: &skills::SkillSelection,
    record: Option<&(stella_records::records::Registry, RenderedChannel)>,
) -> stella_core::steering::SteeringSet {
    use stella_core::steering::SteeringPlane;

    let mut candidates = super::steering::frame_candidates(frames);
    candidates.extend(super::steering::skill_candidates(&selected.selected));
    let mut source_drops = frame_drops.to_vec();
    source_drops.extend(super::steering::skill_drops(selected));
    if let Some((registry, rendered)) = record {
        candidates.extend(stella_records::adapt::record_candidates(registry, rendered));
        source_drops.extend(stella_records::adapt::record_drops(registry, rendered));
    }
    let plane = super::steering::GatheredSteering {
        candidates,
        source_drops,
    };
    plane.query(signal)
}

/// The frames the plane kept, in recall's own order, ready for
/// [`render_context_section`]. Identity under this slice's authorized budget
/// — the filter is where a *bound* budget's decision will land, so the render
/// follows the ledger rather than trusting it.
fn kept_frames(
    frames: &[RecalledFrame],
    set: &stella_core::steering::SteeringSet,
) -> Vec<RecalledFrame> {
    frames
        .iter()
        .filter(|frame| {
            let handle = super::steering::frame_handle(frame);
            set.selected.iter().any(|c| {
                c.source == stella_core::steering::SteeringSource::Memory && c.handle == handle
            })
        })
        .cloned()
        .collect()
}

/// The skills the plane kept, in selection order — same contract as
/// [`kept_frames`].
fn kept_skills(
    selected: &[skills::SelectedSkill],
    set: &stella_core::steering::SteeringSet,
) -> Vec<skills::SelectedSkill> {
    selected
        .iter()
        .filter(|sel| {
            set.selected.iter().any(|c| {
                c.source == stella_core::steering::SteeringSource::Skill
                    && c.handle == sel.skill.name
            })
        })
        .cloned()
        .collect()
}

/// The record channel's section, from its rendered bytes. `None` for a blank
/// render — an empty section would only burn tokens.
fn record_section_text(rendered: RenderedChannel) -> Option<String> {
    let text = rendered.text;
    (!text.trim().is_empty()).then(|| text.trim_start().to_string())
}

/// The eviction report for one dropped candidate — the single producer every
/// source emits through (#3437).
///
/// One sentence shape for all of them, the record channel's:
/// *what applied, which budget refused it, its handle, and the remedy*. The
/// remedy is the half that differs, and it has to: telling a user whose skill
/// lost its seat to "raise its precedence" is advice for a different channel.
///
/// `still_selected` is the section-budget class, and it is why this takes the
/// whole ledger rather than a handle. A skill can be in `selected` *and*
/// `dropped` by design — top-k kept it and `skills::section_fit` then left it
/// out of the rendered section (`steering::skill_drops`' own doc). Both classes
/// genuinely miss the prompt, so both are reported; only the remedy differs,
/// because `SKILLS_SECTION_TOKEN_BUDGET` is a constant and nothing
/// configurable widens it until #3243 Phase 4 collapses the two budgets.
///
/// Memory drops return `None`: the frame query already reported them as ONE
/// summary line naming the budget and the remedy, and repeating the same
/// advice once per evicted memory — with an internal id a user cannot act on
/// — is the noise that line exists to replace.
///
/// A tool drop names the tool and the allowance that refused it. The remedy
/// is the allowance rather than the tool: withholding one is never a
/// capability change (`crate::tool_lean`), so the advice is to widen what the
/// session may spend on schemas, or to turn the lever off.
///
/// `None` for a plugin-contributed candidate, which nothing produces yet.
/// Silence there is the absence of a producer, not a withheld report.
fn drop_message(
    drop: &stella_core::steering::DroppedCandidate,
    still_selected: bool,
) -> Option<String> {
    use stella_core::steering::SteeringSource;
    let handle = &drop.handle;
    match drop.source {
        SteeringSource::Record => Some(format!(
            "a record applying to this turn did not fit the {RECORD_CHANNEL_BUDGET}-char \
             record budget: ^{handle} — raise its precedence, or trim the records that \
             outrank it"
        )),
        SteeringSource::Memory => None,
        SteeringSource::Skill if still_selected => Some(format!(
            "a skill matching this turn did not fit the skills section's token budget: \
             {handle} — nothing configurable widens that budget yet (#3243)"
        )),
        SteeringSource::Skill => Some(format!(
            "a skill matching this turn did not fit the skill budget: {handle} — raise \
             `skills.max_skills`"
        )),
        SteeringSource::Tool => Some(format!(
            "a tool did not fit what this turn's records, skills and frames left of the \
             steering allowance, and was not advertised: {handle} — it still runs if it \
             is called; raise `context.steering.max_tokens`, or set \
             `context.steering.tools.lean` false to advertise every tool"
        )),
        SteeringSource::Plugin => None,
    }
}

/// Report every candidate the ledger says was dropped, whatever its source.
///
/// #3358 completed the *ledger* across records, skills and frames; this is the
/// human-facing half (#3437). Before it, a skill that lost its seat every turn
/// and a frame the recall host's merge evicted were queryable and said nothing
/// to the person watching the run — the #2709 observability gap in its other
/// half.
///
/// Memory drops are summarized, not enumerated: `memory_budget` is the
/// `context.retrieval.max_tokens` the turn ran with, and the report is one
/// line — how many memories missed the budget, the budget itself, and the
/// knob that widens it — instead of one line per memory repeating the same
/// remedy under an internal id.
///
/// Two recall-side filters are deliberately **not** reported here, and that is
/// a decision rather than an omission. `project_recalled_frame` drops a frame
/// the citation-label rule cannot name, and `is_suppressed_local_frame` drops
/// one the session quarantined. Neither is a budget eviction: the first is a
/// frame this process could not cite, and the second is deliberate
/// suppression of a memory cited untruthful twice. Reporting either as
/// `DroppedCandidate` would tell a user their budget was too small when it was
/// not, and quarantine in particular wants its own vocabulary rather than a
/// line advising a bigger retrieval budget. The provider's spend on both is
/// already accounted for by the usage report captured above the filters.
pub(crate) fn report_steering_drops(
    set: &stella_core::steering::SteeringSet,
    memory_budget: u32,
    mut report: impl FnMut(String),
) {
    let memory_drops = set
        .dropped
        .iter()
        .filter(|d| d.source == stella_core::steering::SteeringSource::Memory)
        .count();
    if memory_drops > 0 {
        report(format!(
            "{memory_drops} memories did not fit this turn's {memory_budget}-token retrieval \
             budget — raise context.retrieval.max_tokens in stella.toml to include them"
        ));
    }
    for drop in &set.dropped {
        let still_selected = set
            .selected
            .iter()
            .any(|c| c.source == drop.source && c.handle == drop.handle);
        if let Some(message) = drop_message(drop, still_selected) {
            report(message);
        }
    }
}

/// The wall clock's current instant, in Unix seconds. The one `SystemTime`
/// read in this module — everything downstream of it (`render_today_section`)
/// takes the value as a parameter instead of reading the clock itself, so a
/// test can inject two different instants without racing real time.
fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Render today's date for the VOLATILE recall block: the second operand of
/// the knowledge-cutoff staleness clause in
/// `crate::agent::prompt::append_session_environment`, which names a cutoff
/// but never a "now" to measure it against (#2901) — a model handed one
/// endpoint of an interval cannot reason about its width.
///
/// This is why it lives here and not beside the cutoff it completes. The
/// cutoff is session-constant (a model card fact, fixed at catalog load) and
/// rides the byte-stable system prefix. The date changes once a day, possibly
/// mid-session, so putting it in the prefix would be a guaranteed
/// prompt-cache miss at every UTC midnight for every session alive across it
/// (invariant #7 / L-E8). `the_assembled_prompt_names_the_session_environment`
/// (`crates/stella-cli/src/agent/prompt.rs`) would start failing the moment a
/// test ran across one. It belongs with the other genuinely volatile facts —
/// selected skills, per-turn records — that ride this per-turn block instead.
///
/// Takes Unix seconds rather than reading the clock itself so it stays a pure
/// function of its input: two calls a day apart must render different text,
/// and neither call may touch `SystemTime::now()` for that difference to be
/// provable without racing real time.
pub(super) fn render_today_section(unix_secs: i64) -> String {
    let today = &stella_context::format_rfc3339(unix_secs)[..10];
    format!(
        "Today's date: {today} UTC — measure the knowledge-cutoff staleness clause in the \
         session environment above against this."
    )
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
/// Answers whether the block was appended, which is what
/// [`inject_opening_recall`] charges the steering ledger on: a block the
/// dedup below refuses costs this turn nothing, because the model is already
/// carrying it.
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
