//! The frame query. Every recall path asks the context host through here:
//! the block, the mid-turn re-query, and the pipeline's recall port.

use colored::Colorize;
use stella_context::ContextQuery;
use stella_learn::skills;
use stella_protocol::{ContextRecallPort, Recall, RecalledFrame};
use stella_records::records::RenderedChannel;

use crate::memory::SessionMemory;
use crate::memory::projection::{is_suppressed_local_frame, project_recalled_frame};

impl SessionMemory {
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
    pub(in crate::memory) fn anchors_for(&self, prompt: &str, touched: &[String]) -> Vec<String> {
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

    /// Authoritative prompt and pipeline recall, including a fresh quarantine
    /// read so prior-turn feedback applies immediately.
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
    pub(in crate::memory) async fn recalled_frames_reporting(
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
    pub(in crate::memory) async fn recalled_frames_anchored(
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
            // Workspace files as anchors. The code-graph provider answers an
            // anchor with the file's graph neighborhood: its symbols, its
            // imports and its importers. That places the planner and the
            // worker in the right code without waiting for the model to ask
            // (#342 seam 3). By default the anchors are the files the goal
            // names. A re-query passes the paths the turn has touched since.
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
        let quarantined = match crate::memory::suppression::suppression_reader(
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
        // The usage report covers the fan-out behind this turn's context. It
        // is kept even when quarantine or the citation-label filter later
        // drops every frame. A provider still spent those tokens, and a cost
        // that vanishes with the frames is the hidden cost #452 is about.
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
pub(in crate::memory) fn report_budget_drops(
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
pub(in crate::memory) struct RecalledFrames {
    pub recall: Recall,
    /// The host merge's evictions, already in ledger shape.
    pub dropped: Vec<stella_core::steering::DroppedCandidate>,
}

/// A host-merge eviction as a ledger entry.
///
/// The handle follows [`crate::memory::steering::frame_handle`]'s precedence — the
/// stable `nod_…` id when the frame has one, its citation label otherwise —
/// so a drop and a later selection of the same frame join on one identity.
///
/// `est_tokens` is the **host's** `token_cost`, not an estimate over
/// [`super::frame_recall_line`]: a dropped frame never reached this process with a
/// body, so there is no recall line to measure. The host's number is the one
/// the budget actually refused, which is also what a caller sizing
/// `context.retrieval.max_tokens` needs.
pub(in crate::memory) fn frame_drop(
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

/// Path-shaped tokens the prompt names (`deny.toml`, `src/api/mod.rs`), for
/// `applies_to` path matching.
///
/// Unlike [`goal_path_anchors`] below, the file need not exist. A record
/// scoped to `deny.toml` is about the *topic* the turn raises. The turn that
/// says "add an MIT dep to deny.toml" is when it applies, whatever the
/// working directory is and even before the file exists.
///
/// A token counts if it has a `/` (a path) or a `.` inside it (a file name).
/// Noise tokens cost nothing: they matter only when a record's pattern
/// matches them, and a pattern that matches "v0.1.2" was written to. A
/// `file:line` spelling names the file, a leading `./` is dropped, and an
/// escape is refused.
pub(in crate::memory) fn turn_path_tokens(goal: &str) -> Vec<String> {
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
pub(in crate::memory) fn goal_path_anchors(goal: &str, root: &std::path::Path) -> Vec<String> {
    const MAX_ANCHORS: usize = 4;
    let mut seen = std::collections::HashSet::new();
    let mut anchors = Vec::new();
    'tokens: for token in goal.split(|c: char| c.is_whitespace() || "\"'`()[]{}<>,;!?".contains(c))
    {
        // Strip trailing sentence punctuation (`fix src/a.rs.`) without
        // touching the extension dot. A `path:line` spelling, a file with a
        // line number after a colon, anchors the file.
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

/// One turn's gathered candidates, packed once — the production
/// [`stella_core::steering::SteeringPlane`] query every context source now
/// goes through (#3349). The frame slice is empty on pipeline-driven turns,
/// whose frames travel on the pipeline's own recall port.
pub(in crate::memory) fn query_gathered_plane(
    signal: &stella_core::steering::TurnSignal<'_>,
    frames: &[RecalledFrame],
    frame_drops: &[stella_core::steering::DroppedCandidate],
    selected: &skills::SkillSelection,
    record: Option<&(stella_records::records::Registry, RenderedChannel)>,
) -> stella_core::steering::SteeringSet {
    use stella_core::steering::SteeringPlane;

    let mut candidates = crate::memory::steering::frame_candidates(frames);
    candidates.extend(crate::memory::steering::skill_candidates(
        &selected.selected,
    ));
    let mut source_drops = frame_drops.to_vec();
    source_drops.extend(crate::memory::steering::skill_drops(selected));
    if let Some((registry, rendered)) = record {
        candidates.extend(stella_records::adapt::record_candidates(registry, rendered));
        source_drops.extend(stella_records::adapt::record_drops(registry, rendered));
    }
    let plane = crate::memory::steering::GatheredSteering {
        candidates,
        source_drops,
    };
    plane.query(signal)
}

/// The frames the plane kept, in recall's own order, ready for
/// [`super::render_context_section`]. Identity under this slice's authorized budget
/// — the filter is where a *bound* budget's decision will land, so the render
/// follows the ledger rather than trusting it.
pub(super) fn kept_frames(
    frames: &[RecalledFrame],
    set: &stella_core::steering::SteeringSet,
) -> Vec<RecalledFrame> {
    frames
        .iter()
        .filter(|frame| {
            let handle = crate::memory::steering::frame_handle(frame);
            set.selected.iter().any(|c| {
                c.source == stella_core::steering::SteeringSource::Memory && c.handle == handle
            })
        })
        .cloned()
        .collect()
}

/// The skills the plane kept, in selection order — same contract as
/// [`kept_frames`].
pub(super) fn kept_skills(
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
