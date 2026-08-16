// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The steering plane — one seam for "what could help here?" (#3243).
//!
//! # Why this exists
//!
//! Stella already asks that question four times per turn, from four unrelated
//! engines, over corpora that overlap: recall frames (RRF fuse + MMR),
//! context records (`applies_to` glob matching), skills (lexical coverage +
//! domain boost), and tool advertisement (unfiltered, or `tool_search`'s rank
//! under lean mode). They fire at the same moment, from the same trigger, and
//! **not one of them knows the other three exist** — four relevance engines,
//! four budgets, four failure postures, three on/off switches. A skill the
//! host declines to inject can be the top `skill_search` hit, because those
//! are different rankers with different thresholds and different tokenizers.
//!
//! This module is the shape they collapse into. It is deliberately **pure
//! decision logic over owned data** (invariant 2): the plane is a port, the
//! adapters do the I/O, and that is what lets the ranking and the budgeting
//! be property-tested without a filesystem, a provider, or a store.
//!
//! # What it is not, yet
//!
//! The four existing selectors are not migrated onto it in this change. The
//! types and the budgeter land first so the flag has something to gate and
//! the migration has somewhere to land; moving each selector across is a pure
//! refactor whose review is worth having on its own.
//!
//! # The signal is the point
//!
//! Every selector today takes a `&str` prompt and nothing else, and fires
//! once, at turn start. A forty-step turn that opens on "fix the flaky test"
//! and ends in a provider adapter is still selecting against its first
//! sentence. [`TurnSignal`] is what those selectors never had: what the turn
//! has *become*.

pub mod adapt;

use std::collections::BTreeMap;

/// What is happening in this turn, as a selector needs to see it.
///
/// Borrowed rather than owned so assembling one costs nothing — it is built
/// per query, potentially at every step boundary, and a selector that made
/// the caller clone the turn's history to ask a question would be paid for in
/// exactly the hot loop it is meant to help.
#[derive(Debug, Clone, Copy, Default)]
pub struct TurnSignal<'a> {
    /// The turn's goal, as the operator wrote it. What every selector has
    /// today, and on its own it is what makes selection go stale by step 40.
    pub prompt: &'a str,
    /// Tools the turn has actually called, most recent last. The cheapest
    /// available evidence that the work has moved: a turn that opened on a
    /// test failure and is now running `read_file` over an adapter is asking
    /// a different question than the one it was given.
    pub recent_tool_calls: &'a [String],
    /// Workspace paths the turn has touched, from file-tool receipts.
    ///
    /// Distinct from the prompt's path anchors in one way that matters: an
    /// anchor must resolve to an existing file, so a goal naming a file to be
    /// **created** contributes none. A path the turn has since written *is*
    /// evidence, and it is the only kind that arrives after the goal.
    pub touched_paths: &'a [String],
    /// The domains this turn is working in, already resolved from the paths
    /// above. Never the whole workspace taxonomy — that mistake is what made
    /// every domain-tagged skill match every prompt (#3243 D1).
    pub active_domains: &'a [String],
    /// Step index within the turn, and how many steps since this plane was
    /// last queried. The pair is the hysteresis input: re-querying every step
    /// re-pays for the volatile block every step, so a query is bought by a
    /// change in signal, never by a counter alone.
    pub step: u32,
    pub since_last_query: u32,
    /// Diagnostic codes the turn has seen. "We are stuck, and this is how" is
    /// the strongest retrieval cue available, and it is the one currently
    /// thrown away entirely.
    pub errors_seen: &'a [&'a str],
}

/// Which plane a candidate came from.
///
/// `Plugin` is declared now and produced by nothing yet, deliberately: #3246
/// sequences plugins last, and the whole cost of a fifth source arriving
/// later is whether this enum and [`SteeringSet`] anticipated it. A variant
/// costs nothing today and a second seam costs a migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SteeringSource {
    /// A context record — a rule, fact, or directive (`.stella/rules/`).
    Record,
    /// A recalled frame: memory, episode, or code-graph symbol.
    Memory,
    /// A workspace skill.
    Skill,
    /// A tool schema competing for advertisement space.
    Tool,
    /// A plugin-contributed candidate. Nothing produces one yet (#3246).
    Plugin,
}

impl SteeringSource {
    /// Stable lowercase tag for telemetry and drop reports. Named rather than
    /// derived from `Debug`, which is not a wire contract.
    pub fn tag(self) -> &'static str {
        match self {
            Self::Record => "record",
            Self::Memory => "memory",
            Self::Skill => "skill",
            Self::Tool => "tool",
            Self::Plugin => "plugin",
        }
    }
}

/// One thing the plane believes could help, with the evidence for it.
#[derive(Debug, Clone, PartialEq)]
pub struct SteeringCandidate {
    pub source: SteeringSource,
    /// Stable identity within its source — a record `^handle`, a `nod_…`, a
    /// skill name, a tool name. This is what a receipt joins on, so it is the
    /// candidate's identity and never its rendered text.
    pub handle: String,
    /// Relevance, comparable **only within a source**. Cross-source ranking
    /// reads [`SteeringCandidate::priority`], never this: four engines that
    /// each normalize differently do not produce one number, and pretending
    /// they do is how a rule loses to a tool schema.
    pub score: f64,
    /// Why this was selected, in words a human reads in a drop report.
    pub why: String,
    /// What injecting it would cost. The single unit the budgeter works in —
    /// today there are three packing passes over recall alone, in two
    /// different units (tokens, then chars).
    pub est_tokens: u64,
}

/// How the budgeter orders sources when it cannot afford all of them.
///
/// Deliberately a fixed precedence rather than a tuned weight per source.
/// The four engines' scores are not commensurable, so any single blended
/// number would be a fabricated comparison — and a fabricated comparison
/// between a `must` rule and a skill is a governance failure, not a ranking
/// nit. Within a source, `score` decides; across sources, this does.
fn source_rank(source: SteeringSource) -> u8 {
    match source {
        // A published record is the repository's own steering policy; it
        // outranks anything inferred.
        SteeringSource::Record => 0,
        // A tool the model cannot see is a capability it does not have,
        // which fails harder than missing know-how.
        SteeringSource::Tool => 1,
        SteeringSource::Skill => 2,
        SteeringSource::Memory => 3,
        // Last until #3246 gives it an authority story.
        SteeringSource::Plugin => 4,
    }
}

/// A candidate the budget could not afford, kept so the drop is reportable.
///
/// Every existing plane drops silently except the record channel, which
/// names what it dropped — and that is the one whose drops anybody has ever
/// been able to debug.
#[derive(Debug, Clone, PartialEq)]
pub struct DroppedCandidate {
    pub source: SteeringSource,
    pub handle: String,
    pub est_tokens: u64,
}

/// What the plane decided for one query.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SteeringSet {
    pub selected: Vec<SteeringCandidate>,
    pub dropped: Vec<DroppedCandidate>,
}

impl SteeringSet {
    /// Total cost of what was selected.
    pub fn est_tokens(&self) -> u64 {
        self.selected.iter().map(|c| c.est_tokens).sum()
    }

    /// One line per source: how many were selected and how many dropped.
    /// `BTreeMap` so the report is ordered and therefore diffable.
    pub fn by_source(&self) -> BTreeMap<&'static str, (usize, usize)> {
        let mut out: BTreeMap<&'static str, (usize, usize)> = BTreeMap::new();
        for c in &self.selected {
            out.entry(c.source.tag()).or_default().0 += 1;
        }
        for d in &self.dropped {
            out.entry(d.source.tag()).or_default().1 += 1;
        }
        out
    }
}

/// The port. One implementation per host; adapters supply the candidates.
pub trait SteeringPlane {
    /// Everything that could help, already budgeted.
    fn query(&self, signal: &TurnSignal<'_>) -> SteeringSet;
}

/// Fit `candidates` into `budget_tokens`, reporting what did not fit.
///
/// One budget for every source, which is the point: today a rule, a skill and
/// a recalled frame are packed by three passes that cannot see each other, so
/// "the volatile block is too big" is a question no single piece of code can
/// answer.
///
/// Ordering is [`source_rank`] first, then `score` descending, then `handle`
/// — the last is not a nicety. Selection feeds a prompt, prompt bytes feed
/// the cache, and a tie broken by hash order would reorder the block between
/// two otherwise identical turns and re-bill the tail (invariant 7).
///
/// A candidate that does not fit is **skipped, not terminal**: a cheap
/// low-ranked candidate still lands after an expensive one is refused. The
/// alternative — stopping at the first thing that does not fit — makes one
/// oversized record silently evict every skill behind it.
pub fn pack_to_budget(mut candidates: Vec<SteeringCandidate>, budget_tokens: u64) -> SteeringSet {
    candidates.sort_by(|a, b| {
        source_rank(a.source)
            .cmp(&source_rank(b.source))
            .then_with(|| b.score.total_cmp(&a.score))
            .then_with(|| a.handle.cmp(&b.handle))
    });

    let mut set = SteeringSet::default();
    let mut spent: u64 = 0;
    for candidate in candidates {
        if spent + candidate.est_tokens <= budget_tokens {
            spent += candidate.est_tokens;
            set.selected.push(candidate);
        } else {
            set.dropped.push(DroppedCandidate {
                source: candidate.source,
                handle: candidate.handle,
                est_tokens: candidate.est_tokens,
            });
        }
    }
    set
}

#[cfg(test)]
mod tests;
