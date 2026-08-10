//! The projection: the bytes the model actually receives.
//!
//! Governance artifacts are large — `docs/spec/adaptive-context/context-record-examples/07-agent-projection.md`
//! measures the ten live example records at 627 bytes each, and their projection at
//! 101. The agent sees about 16% of a record, and this module decides which 16%.
//!
//! # Two blocks, because `force` decides the channel
//!
//! `must` and `should` are unconditional, so they belong in the system prefix that
//! is built once per session and reused verbatim under the prompt-cache contract.
//! `may` and `info` are facts — only worth tokens when they apply — so they are
//! relevance-selected per turn and ride the volatile block beside memories, which
//! already behave that way.
//!
//! That split is not a stylistic preference; it is forced. Anything selected per
//! turn cannot live in the cached prefix without rebuilding the cache every turn.
//! Which is also why nothing here carries a clock: no timestamps, no "verified 3
//! weeks ago", no confidence that drifts as citations accumulate. Staleness is
//! resolved by [`super::sweep`] *before* rendering — a record arrives fresh at its
//! stated force, demoted to the volatile channel, or dropped. **The decision
//! reaches the prompt; the reasoning does not.**
//!
//! # Grouping, so the renderer stops overstating
//!
//! The prior renderer put every rule under one header — *"Workspace rules (binding
//! — follow exactly; guarded rules are hard-blocked)"* — which presented a fact
//! about a staging URL as something to follow exactly. Grouping by force amortises
//! the force marker across many records instead of repeating it per line, and says
//! only what is true of each group.
//!
//! # `^handle`, the attribution key
//!
//! Every rendered record carries its handle. That is the whole reason
//! [`ContextUseKind::Cited`][cited] becomes reachable: the model has a token it can
//! name. [`RenderedChannel`] also reports which handles were **dropped by the
//! budget**, because a record chosen by the selector and then dropped looks
//! identical, from the ledger, to one that was never chosen — that gap is
//! [`MissingContextKind::NotRendered`][nr], and it can only be recorded by whoever
//! did the dropping.
//!
//! # Order and scarcity are two questions
//!
//! Records render in the order the caller gave them, which for the cached channel
//! must not vary across turns. *Which* records render is asked only when a budget
//! binds, and answered by declared precedence rather than by position — a record
//! is not worth less for having loaded later. `survivors` resolves that ahead of
//! rendering, so the two answers cannot drift into each other; with budget to
//! spare it admits everything and the block is byte-for-byte what load order alone
//! would have produced.
//!
//! [cited]: super::super::context_record::context_use::ContextUseKind::Cited
//! [nr]: super::super::context_record::context_use::MissingContextKind

use super::super::ingest::record::{Force, Tier};
use super::LoadedRecord;
use super::sweep::Disposition;

/// The cached block's heading, including the one instruction that makes attribution
/// happen rather than merely be possible.
///
/// The handle is the attribution key, but a key nobody is asked to use stays unused:
/// a model handed `^pkg-manager` and no instruction produces prose about package
/// managers and cites nothing, so `ContextUseKind::Cited` would remain empty for a
/// reason that has nothing to do with whether the record helped.
///
/// It lives in the heading rather than on its own line for a reason worth stating:
/// the heading is amortised across every record in the block, so the instruction
/// costs a handful of tokens once instead of a clause per bullet — the same argument
/// that made grouping by force worth doing.
const CACHED_HEADING: &str = "\n## Workspace rules (cite the ^handle of any you apply)";

/// The default budget for the cached record channel, in characters (#2709).
///
/// One constant, consumed by BOTH the prompt assembler (which caps the
/// `## Workspace rules` block it bakes into the prefix) and the ingest-time
/// pinned-footprint diagnostic (which warns when an ingest's pinned records
/// cannot all fit) — a second copy of this number is how the warning and the
/// truncation would drift apart. Sized to match the workspace-memory budget
/// that shares the same prefix: generous enough that no real record set has
/// hit it, small enough that a runaway ingest cannot flood every future
/// prompt.
pub const CACHED_RECORD_BUDGET_CHARS: usize = 16_000;

/// Which of the two prompt channels a record renders into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// The byte-stable system prefix, built once per session. `must` and `should`.
    Cached,
    /// The per-turn block after the prefix, beside recalled memories. `may`, `info`,
    /// and anything the sweep demoted.
    Volatile,
}

/// One record and what the sweep decided about it.
#[derive(Debug, Clone, Copy)]
pub struct RenderInput<'a> {
    /// The loaded record, with its handle already assigned.
    pub record: &'a LoadedRecord,
    /// The sweep's verdict on whether — and how — it may steer.
    pub disposition: &'a Disposition,
    /// Whether a Tier-2 guard is **actually armed** for this record.
    ///
    /// Decided by the caller, not re-derived here. Two reasons: the blocking gate
    /// needs facts this crate has no access to (a human approval, a suspending
    /// conflict), and legacy markdown rules reach the renderer with a guard that
    /// predates the gate entirely — recomputing would disarm them in the marker
    /// while they stayed armed at the tool boundary, so the prompt would understate
    /// what actually happens.
    pub enforced: bool,
}

/// A rendered channel: the bytes, and the ledger facts about them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderedChannel {
    /// The block to inject. Empty when nothing rendered — the caller appends
    /// nothing rather than an empty heading.
    pub text: String,
    /// Handles that reached the bytes, in render order. These are the records the
    /// ledger may record as `rendered`.
    pub rendered: Vec<String>,
    /// Handles the budget dropped after selection. These are `selected` but **not**
    /// `rendered` — the silent gap `MissingContextKind::NotRendered` names.
    ///
    /// Chosen by ascending precedence, not by position — the least important
    /// record loses its place, not the last-loaded one.
    pub dropped: Vec<String>,
}

/// Which channel a record renders into, or `None` when it does not render at all.
///
/// The promotion tier decides (#2709): `pinned` rides the cached prefix,
/// `scoped` and `retrieved` ride the volatile block. A record with no explicit
/// tier derives one from `force` + `applies_to` ([`Record::tier`][t]), which
/// reproduces the pre-tier contract exactly — `must`/`should` cached,
/// `may`/`info` volatile.
///
/// A sweep demotion outranks the tier, exactly as it outranked the declared
/// force: a record whose freshness is in question cannot sit in a block that
/// must stay byte-identical, because the reason it was demoted is per-turn
/// information even when the renderer declines to print it. A stale-demoted
/// pinned record therefore never enters the stable prefix.
///
/// [t]: super::super::ingest::record::Record::tier
pub fn channel_of(tier: Tier, disposition: &Disposition) -> Option<Channel> {
    if !disposition.is_selected() {
        return None;
    }
    if disposition.forces_volatile() || tier != Tier::Pinned {
        return Some(Channel::Volatile);
    }
    Some(Channel::Cached)
}

/// The force a record renders under, after any demotion.
fn effective_force(record: &LoadedRecord, disposition: &Disposition) -> Force {
    let declared = record
        .record
        .steering
        .as_ref()
        .map_or(Force::Info, |steering| steering.force);
    if disposition.forces_volatile() && declared.is_always_injected() {
        // Demoted out of the cached channel. `may` is the strongest force the
        // volatile channel carries, so that is where it lands — weaker than
        // declared, which is the point.
        return Force::May;
    }
    declared
}

/// Render one channel from every record the selector chose.
///
/// `budget_chars` caps the block; `None` means no cap. The cap applies **per
/// channel** and is checked before a record is appended, so a block never
/// half-renders a record — a truncated statement is a different instruction from
/// the one that was reviewed.
///
/// Records are emitted in the order given. The caller controls that order, and for
/// the cached channel it must be stable across turns (see [`super::assign_handles`]
/// on why handles do not depend on load order).
///
/// **Which records are emitted is a separate question from their order**, and the
/// budget is the only thing that ever asks it. With budget to spare the two
/// questions collapse and every record renders where it was given; under pressure
/// the record with the lowest declared `precedence` loses its place rather than
/// the last-loaded one.
pub fn render_channel(
    inputs: &[RenderInput<'_>],
    channel: Channel,
    budget_chars: Option<usize>,
) -> RenderedChannel {
    let mine: Vec<&RenderInput<'_>> = inputs
        .iter()
        .filter(|input| channel_of(input.record.record.tier(), input.disposition) == Some(channel))
        .collect();
    if mine.is_empty() {
        return RenderedChannel::default();
    }

    match channel {
        Channel::Cached => render_cached(&mine, budget_chars),
        Channel::Volatile => render_volatile(&mine, budget_chars),
    }
}

/// `## Workspace rules`, grouped `### Must` then `### Should`.
///
/// Force outranks precedence here, and the grouping is what makes that true: the
/// `must` section is laid out first and so meets the budget first, leaving
/// `should` to compete for the remainder. Precedence then ranks within a group,
/// which is the only place it has anything left to decide.
fn render_cached(inputs: &[&RenderInput<'_>], budget_chars: Option<usize>) -> RenderedChannel {
    let mut out = RenderedChannel {
        text: CACHED_HEADING.to_string(),
        ..RenderedChannel::default()
    };
    for (forces, heading) in [
        (&[Force::Must][..], "### Must"),
        (&[Force::Should][..], "### Should"),
        // Explicitly-pinned records below `should` strength (#2709). Only an
        // explicit `tier = "pinned"` can put a `may`/`info` record in this
        // channel, so the heading never appears for a pre-tier record set and
        // every existing prompt keeps its exact bytes.
        (&[Force::May, Force::Info][..], "### Pinned"),
    ] {
        let group: Vec<&RenderInput<'_>> = inputs
            .iter()
            .copied()
            .filter(|input| forces.contains(&effective_force(input.record, input.disposition)))
            .collect();
        if group.is_empty() {
            continue;
        }
        let mut section = format!("\n\n{heading}");
        let lines = bullet_lines(&group);
        // The heading counts against the budget whether or not the section is
        // ultimately written — a group that fits nothing has already been
        // charged for it, which is the conservative direction.
        let keep = survivors(&group, &lines, out.text.len() + section.len(), budget_chars);
        let mut wrote_any = false;
        for ((input, line), keep) in group.iter().zip(&lines).zip(keep) {
            if !keep {
                out.dropped.push(input.record.handle.clone());
                continue;
            }
            section.push_str(line);
            out.rendered.push(input.record.handle.clone());
            wrote_any = true;
        }
        if wrote_any {
            out.text.push_str(&section);
        }
    }
    if out.rendered.is_empty() {
        // Every record was dropped: emit no heading rather than an empty section
        // claiming rules exist.
        out.text.clear();
    }
    out
}

/// `## Relevant context` — one flat list. `may` and `info` are both "context that
/// applies right now"; a second subheading would distinguish them for the model
/// without giving it anything to do differently.
fn render_volatile(inputs: &[&RenderInput<'_>], budget_chars: Option<usize>) -> RenderedChannel {
    let mut out = RenderedChannel {
        text: "\n## Relevant context".to_string(),
        ..RenderedChannel::default()
    };
    let header_len = out.text.len();
    let lines = bullet_lines(inputs);
    let keep = survivors(inputs, &lines, header_len, budget_chars);
    for ((input, line), keep) in inputs.iter().zip(&lines).zip(keep) {
        if !keep {
            out.dropped.push(input.record.handle.clone());
            continue;
        }
        out.text.push_str(line);
        out.rendered.push(input.record.handle.clone());
    }
    if out.text.len() == header_len {
        out.text.clear();
    }
    out
}

/// `- <statement> ^<handle>[ [enforced]]`.
///
/// `[enforced]` marks a record that actually blocks at the tool boundary — the
/// guard is armed, so the marker is a fact rather than an aspiration. A record
/// whose blocking was refused ([`super::BlockingRefusal`]) does not get it: telling
/// the model a call will be blocked when it will not is the same lie the blank-guard
/// bug used to tell.
fn bullet(input: &RenderInput<'_>) -> String {
    let statement = input.record.record.statement.trim();
    let suffix = if input.enforced { " [enforced]" } else { "" };
    format!("- {statement} ^{}{suffix}", input.record.handle)
}

/// The rendered line for each input, in the order given.
///
/// Materialized up front because [`survivors`] has to weigh the lines in one
/// order and the caller emits them in another; formatting twice would be the
/// kind of duplication that lets the two orders disagree about a record's size.
fn bullet_lines(inputs: &[&RenderInput<'_>]) -> Vec<String> {
    inputs
        .iter()
        .map(|input| format!("\n{}", bullet(input)))
        .collect()
}

/// Which records fit, ranked by importance — the answer parallel to `inputs`.
///
/// # Why this is not just the emit loop with a length check
///
/// It used to be, and that made the budget's victim a function of **load
/// order**: the first-loaded records took the space and whatever came last was
/// ledgered as `dropped`, so a `precedence = 80` record could lose its place to
/// a `precedence = 40` one from an earlier file (#2299). The drop was honest —
/// [`RenderedChannel::dropped`] recorded it — and still wrong, because nothing
/// about arriving later makes a record worth less.
///
/// So scarcity is resolved here, ahead of and apart from rendering. The caller
/// then emits the survivors in the order it was given: importance settles *who*,
/// never *where*, which is the same split `pack_to_budget` draws in
/// `stella-context` — a band decides nothing until something has to be dropped.
///
/// # Importance is force, then precedence
///
/// **Force first** ([`Force::strength`][s]), because force is the coarser claim
/// and a `may` record outranks an `info` one whatever numbers they declared.
/// This is free for the cached channel, whose grouping already spends the budget
/// on `must` before `should` — it exists for the volatile channel, which had no
/// such guarantee and where the sole production budget actually binds. Leaving
/// that asymmetry in place would have made the drop order hostage to every
/// authoring path agreeing with the force ordering, and one already did not:
/// `precedence_for` in `stella-cli`'s ingest stamped `may = 15` below
/// `info = 20`, which was inert while precedence only fed conflict detection and
/// would have started evicting the stronger record the moment it also ranked
/// scarcity.
///
/// **Precedence within a force**, which is where an author's own ranking
/// applies. The sort is **stable**, so records that made no competing claim
/// (equal on both — including the `0` that [`Record::precedence`][p] gives the
/// ones that declared none) keep load order and the block stays deterministic.
///
/// # A record that cannot fit is skipped, not terminal
///
/// The walk continues past it, so a less important record still renders in space
/// the more important one could not have used anyway. That keeps the budget from
/// being held open for nothing; the skipped record is ledgered like any other
/// drop. Note this is the one case where a lower-ranked record outlives a higher-
/// ranked one, and it is a statement about size, not about worth.
///
/// [p]: super::super::ingest::record::Record::precedence
/// [s]: super::super::ingest::record::Force::strength
fn survivors(
    inputs: &[&RenderInput<'_>],
    lines: &[String],
    spent: usize,
    budget_chars: Option<usize>,
) -> Vec<bool> {
    let Some(budget) = budget_chars else {
        return vec![true; inputs.len()];
    };
    let mut by_importance: Vec<usize> = (0..inputs.len()).collect();
    by_importance.sort_by_key(|&i| {
        let input = inputs[i];
        std::cmp::Reverse((
            effective_force(input.record, input.disposition).strength(),
            input.record.record.precedence(),
        ))
    });

    let mut keep = vec![false; inputs.len()];
    let mut used = spent;
    for i in by_importance {
        let Some(after) = used
            .checked_add(lines[i].len())
            .filter(|len| *len <= budget)
        else {
            continue;
        };
        used = after;
        keep[i] = true;
    }
    keep
}

#[cfg(test)]
mod tests {
    use super::super::tests::{loaded_from, record_named, with_force};
    use super::*;

    fn input<'a>(record: &'a LoadedRecord, disposition: &'a Disposition) -> RenderInput<'a> {
        RenderInput {
            record,
            disposition,
            enforced: false,
        }
    }

    fn enforced_input<'a>(
        record: &'a LoadedRecord,
        disposition: &'a Disposition,
    ) -> RenderInput<'a> {
        RenderInput {
            record,
            disposition,
            enforced: true,
        }
    }

    fn at(force: Force, lineage: &str, statement: &str) -> LoadedRecord {
        let mut record = with_force(record_named(lineage), force);
        record.statement = statement.to_string();
        let mut loaded = loaded_from(record);
        loaded.handle = lineage.rsplit('.').next().unwrap_or(lineage).to_string();
        loaded
    }

    /// [`at`] with a declared precedence — the claim that decides who survives a
    /// budget. `at` leaves every record at the builder's default, so the tests
    /// above exercise the equal-precedence path and must be unaffected by any of
    /// this.
    fn ranked(force: Force, lineage: &str, statement: &str, precedence: u32) -> LoadedRecord {
        let mut loaded = at(force, lineage, statement);
        loaded
            .record
            .steering
            .as_mut()
            .expect("`at` builds every record with a steering block")
            .precedence = Some(precedence);
        loaded
    }

    /// Three same-length bullets, so a budget expressed in characters admits a
    /// count rather than a particular record, and the tests never hardcode a
    /// byte total.
    fn three_equal_length() -> (LoadedRecord, LoadedRecord, LoadedRecord) {
        (
            ranked(Force::Info, "ctx.a.b.aa", "Alpha statement.", 10),
            ranked(Force::Info, "ctx.a.b.bb", "Bravo statement.", 50),
            ranked(Force::Info, "ctx.a.b.cc", "Cocoa statement.", 90),
        )
    }

    /// The budget that fits exactly `n` of the same-length bullets.
    fn budget_for(n: usize, records: &[&LoadedRecord]) -> usize {
        let select = Disposition::Select;
        let inputs: Vec<RenderInput<'_>> = records
            .iter()
            .take(n)
            .map(|record| input(record, &select))
            .collect();
        render_channel(&inputs, Channel::Volatile, None).text.len()
    }

    #[test]
    fn the_cached_block_groups_by_force_and_names_every_record() {
        let must = at(
            Force::Must,
            "ctx.a.b.pkg-manager",
            "This repository uses pnpm exclusively.",
        );
        let also_must = at(
            Force::Must,
            "ctx.a.b.node-version",
            "Development runs on Node 22.x.",
        );
        let should = at(
            Force::Should,
            "ctx.a.b.pr-descriptions",
            "A PR description states why.",
        );
        let select = Disposition::Select;
        let out = render_channel(
            &[
                input(&must, &select),
                input(&also_must, &select),
                input(&should, &select),
            ],
            Channel::Cached,
            None,
        );
        assert_eq!(
            out.text,
            "\n## Workspace rules (cite the ^handle of any you apply)\
             \n\n### Must\
             \n- This repository uses pnpm exclusively. ^pkg-manager\
             \n- Development runs on Node 22.x. ^node-version\
             \n\n### Should\
             \n- A PR description states why. ^pr-descriptions"
        );
        assert_eq!(
            out.rendered,
            vec!["pkg-manager", "node-version", "pr-descriptions"]
        );
        assert!(out.dropped.is_empty());
    }

    #[test]
    fn the_volatile_block_is_one_flat_relevant_context_list() {
        let may = at(
            Force::May,
            "ctx.a.b.capability-contract-location",
            "Contracts live in contracts/.",
        );
        let info = at(
            Force::Info,
            "ctx.a.b.capability-contract-format",
            "A contract is YAML.",
        );
        let select = Disposition::Select;
        let out = render_channel(
            &[input(&may, &select), input(&info, &select)],
            Channel::Volatile,
            None,
        );
        assert_eq!(
            out.text,
            "\n## Relevant context\
             \n- Contracts live in contracts/. ^capability-contract-location\
             \n- A contract is YAML. ^capability-contract-format"
        );
    }

    #[test]
    fn a_must_record_never_appears_in_the_volatile_channel_and_vice_versa() {
        let must = at(Force::Must, "ctx.a.b.one", "Must statement.");
        let info = at(Force::Info, "ctx.a.b.two", "Info statement.");
        let select = Disposition::Select;
        let inputs = [input(&must, &select), input(&info, &select)];
        assert_eq!(
            render_channel(&inputs, Channel::Cached, None).rendered,
            vec!["one"]
        );
        assert_eq!(
            render_channel(&inputs, Channel::Volatile, None).rendered,
            vec!["two"]
        );
    }

    // Byte-stability — the hard constraint.

    #[test]
    fn the_cached_block_is_byte_identical_across_turns() {
        let must = at(
            Force::Must,
            "ctx.a.b.pkg-manager",
            "This repository uses pnpm exclusively.",
        );
        let should = at(
            Force::Should,
            "ctx.a.b.pr-descriptions",
            "A PR description states why.",
        );
        let select = Disposition::Select;
        let inputs = [input(&must, &select), input(&should, &select)];
        let first = render_channel(&inputs, Channel::Cached, None);
        for turn in 2..=25 {
            let again = render_channel(&inputs, Channel::Cached, None);
            assert_eq!(
                again, first,
                "turn {turn} produced different prefix bytes — the prompt cache is gone"
            );
        }
        assert!(
            !first.text.contains("verified")
                && !first.text.contains("checked")
                && !first
                    .text
                    .chars()
                    .any(|c| c.is_ascii_digit() && first.text.contains("20")),
            "no clock may enter the cached block: {}",
            first.text
        );
    }

    #[test]
    fn a_stale_record_is_demoted_out_of_the_cached_block_without_saying_why() {
        let must = at(
            Force::Must,
            "ctx.a.b.node-version",
            "Development runs on Node 20.",
        );
        let stale = Disposition::SelectStale {
            reason: "its truth probe no longer holds".to_string(),
        };
        let inputs = [input(&must, &stale)];
        assert!(
            render_channel(&inputs, Channel::Cached, None)
                .text
                .is_empty(),
            "a demoted record must not sit in the byte-stable prefix"
        );
        let volatile = render_channel(&inputs, Channel::Volatile, None);
        assert_eq!(volatile.rendered, vec!["node-version"]);
        assert!(
            !volatile.text.contains("no longer holds"),
            "the decision reaches the prompt; the reasoning does not: {}",
            volatile.text
        );
    }

    #[test]
    fn a_dropped_or_blocked_record_renders_in_neither_channel() {
        let must = at(Force::Must, "ctx.a.b.gone", "Refuted statement.");
        for disposition in [
            Disposition::Drop {
                reason: "refuted".to_string(),
            },
            Disposition::Block {
                reason: "refuted, and on_expiry = block".to_string(),
            },
        ] {
            let inputs = [input(&must, &disposition)];
            assert!(
                render_channel(&inputs, Channel::Cached, None)
                    .text
                    .is_empty()
            );
            assert!(
                render_channel(&inputs, Channel::Volatile, None)
                    .text
                    .is_empty()
            );
        }
    }

    #[test]
    fn unfalsifiable_still_renders_at_its_declared_force() {
        let must = at(Force::Must, "ctx.a.b.tone", "Write in plain language.");
        let inputs = [input(&must, &Disposition::SelectUnfalsifiable)];
        assert_eq!(
            render_channel(&inputs, Channel::Cached, None).rendered,
            vec!["tone"],
            "nothing being able to check a decree is not a reason to demote it"
        );
    }

    // The selected/rendered gap.

    #[test]
    fn budget_drops_are_reported_rather_than_silently_lost() {
        let first = at(Force::Info, "ctx.a.b.kept", "Short.");
        let second = at(
            Force::Info,
            "ctx.a.b.dropped",
            "A considerably longer statement that will not fit.",
        );
        let select = Disposition::Select;
        let out = render_channel(
            &[input(&first, &select), input(&second, &select)],
            Channel::Volatile,
            Some(45),
        );
        assert_eq!(out.rendered, vec!["kept"]);
        assert_eq!(
            out.dropped,
            vec!["dropped"],
            "a record the budget dropped is `selected` but not `rendered` — the ledger \
             cannot tell those apart unless the renderer says so"
        );
    }

    // Who the budget drops (#2299). Precedence answers that and nothing else —
    // the order records render in is still the order they were given.

    #[test]
    fn the_budget_drops_the_least_important_record_not_the_last_one() {
        // The shape from the field: the weaker claim loads first and used to
        // take the space on that alone.
        let weak = ranked(Force::Info, "ctx.a.b.weak", "Weaker claim.", 40);
        let strong = ranked(Force::Info, "ctx.a.b.strong", "Stronger claim.", 80);
        let select = Disposition::Select;
        let out = render_channel(
            &[input(&weak, &select), input(&strong, &select)],
            Channel::Volatile,
            Some(budget_for(1, &[&strong])),
        );
        assert_eq!(out.rendered, vec!["strong"]);
        assert_eq!(
            out.dropped,
            vec!["weak"],
            "arriving earlier is not a reason to outrank a record that declared \
             itself more important"
        );
    }

    #[test]
    fn survivors_render_in_load_order_not_in_precedence_order() {
        // Load order and precedence order disagree about the two survivors, so
        // this fails if selection is allowed to leak into layout.
        let (low, mid, high) = three_equal_length();
        let select = Disposition::Select;
        let out = render_channel(
            &[
                input(&low, &select),
                input(&mid, &select),
                input(&high, &select),
            ],
            Channel::Volatile,
            Some(budget_for(2, &[&low, &mid])),
        );
        assert_eq!(
            out.rendered,
            vec!["bb", "cc"],
            "precedence chose the survivors; the caller's order places them"
        );
        assert_eq!(out.dropped, vec!["aa"]);
    }

    #[test]
    fn an_unbudgeted_block_is_byte_identical_whatever_the_precedences() {
        // The claim that makes this change safe to ship: precedence decides
        // nothing until something has to be dropped, so a block under no
        // pressure is what load order alone would have produced.
        let (low, mid, high) = three_equal_length();
        let flat = [
            at(Force::Info, "ctx.a.b.aa", "Alpha statement."),
            at(Force::Info, "ctx.a.b.bb", "Bravo statement."),
            at(Force::Info, "ctx.a.b.cc", "Cocoa statement."),
        ];
        let select = Disposition::Select;
        let ranked_block = render_channel(
            &[
                input(&low, &select),
                input(&mid, &select),
                input(&high, &select),
            ],
            Channel::Volatile,
            None,
        );
        let flat_block = render_channel(
            &[
                input(&flat[0], &select),
                input(&flat[1], &select),
                input(&flat[2], &select),
            ],
            Channel::Volatile,
            None,
        );
        assert_eq!(ranked_block, flat_block);
    }

    #[test]
    fn equal_precedence_still_loses_in_load_order() {
        // Records that made no competing claim must break their tie the way
        // they always did, or the block stops being deterministic.
        let first = at(Force::Info, "ctx.a.b.aa", "Alpha statement.");
        let second = at(Force::Info, "ctx.a.b.bb", "Bravo statement.");
        let third = at(Force::Info, "ctx.a.b.cc", "Cocoa statement.");
        let select = Disposition::Select;
        let out = render_channel(
            &[
                input(&first, &select),
                input(&second, &select),
                input(&third, &select),
            ],
            Channel::Volatile,
            Some(budget_for(2, &[&first, &second])),
        );
        assert_eq!(out.rendered, vec!["aa", "bb"]);
        assert_eq!(out.dropped, vec!["cc"]);
    }

    #[test]
    fn a_record_too_long_to_fit_does_not_starve_the_ones_that_do() {
        // Outranking everything is not the same as fitting. The walk skips the
        // record it cannot place and keeps going, so the budget buys context
        // instead of being held open for something that was never going to fit.
        let huge = ranked(
            Force::Info,
            "ctx.a.b.huge",
            "A statement far too long to fit in the space this budget leaves.",
            99,
        );
        let small = ranked(Force::Info, "ctx.a.b.small", "Short.", 1);
        let select = Disposition::Select;
        let out = render_channel(
            &[input(&huge, &select), input(&small, &select)],
            Channel::Volatile,
            Some(budget_for(1, &[&small])),
        );
        assert_eq!(out.rendered, vec!["small"]);
        assert_eq!(
            out.dropped,
            vec!["huge"],
            "the record that could not fit is ledgered, not silently skipped"
        );
    }

    #[test]
    fn the_cached_budget_drops_by_precedence_too() {
        // The same defect lived in both renderers; fixing one would have left a
        // must-record losing its place in the prefix for having loaded first.
        let weak = ranked(Force::Must, "ctx.a.b.weak", "Weaker claim.", 40);
        let strong = ranked(Force::Must, "ctx.a.b.strong", "Stronger claim.", 80);
        let select = Disposition::Select;
        let full = render_channel(
            &[input(&weak, &select), input(&strong, &select)],
            Channel::Cached,
            None,
        );
        let out = render_channel(
            &[input(&weak, &select), input(&strong, &select)],
            Channel::Cached,
            Some(full.text.len() - 1),
        );
        assert_eq!(out.rendered, vec!["strong"]);
        assert_eq!(out.dropped, vec!["weak"]);
    }

    #[test]
    fn force_outranks_precedence_in_the_volatile_block_too() {
        // The cached channel gets this from its grouping; the volatile channel
        // has no grouping, so it has to be ranked in. These are the exact
        // numbers `precedence_for` stamped on extracted records — `may` below
        // `info` — which is what makes force-first a guarantee rather than a
        // hope that every authoring path agrees with the force ordering.
        let trivia = ranked(Force::Info, "ctx.a.b.trivia", "Informational.", 20);
        let actionable = ranked(Force::May, "ctx.a.b.actionable", "Do the thing.", 15);
        let select = Disposition::Select;
        let out = render_channel(
            &[input(&trivia, &select), input(&actionable, &select)],
            Channel::Volatile,
            Some(budget_for(1, &[&actionable])),
        );
        assert_eq!(
            out.rendered,
            vec!["actionable"],
            "a `may` record must outrank an `info` one whatever precedence each declared"
        );
        assert_eq!(out.dropped, vec!["trivia"]);
    }

    #[test]
    fn a_demoted_record_competes_at_the_force_it_renders_under() {
        // A demoted `must` renders as `may`, so it must rank as `may` — reading
        // the declared force here would let it outrank the volatile channel's
        // own records on a claim the sweep already took away from it.
        let demoted = ranked(Force::Must, "ctx.a.b.demoted", "Was binding.", 10);
        let native = ranked(Force::May, "ctx.a.b.native", "Still relevant.", 90);
        let stale = Disposition::SelectStale {
            reason: "its truth probe no longer holds".to_string(),
        };
        let select = Disposition::Select;
        let out = render_channel(
            &[input(&demoted, &stale), input(&native, &select)],
            Channel::Volatile,
            Some(budget_for(1, &[&native])),
        );
        assert_eq!(
            out.rendered,
            vec!["native"],
            "a demoted record ranks at its effective force, not its declared one"
        );
        assert_eq!(out.dropped, vec!["demoted"]);
    }

    #[test]
    fn a_budget_nothing_reaches_changes_nothing() {
        // The unbudgeted test proves `None` is inert; this proves a `Some` large
        // enough to bind on nothing is inert too. Those are different code paths
        // — `None` short-circuits, a generous `Some` walks the whole ranking —
        // and only the second one is what production actually passes.
        let (low, mid, high) = three_equal_length();
        let select = Disposition::Select;
        let inputs = [
            input(&low, &select),
            input(&mid, &select),
            input(&high, &select),
        ];
        let unbudgeted = render_channel(&inputs, Channel::Volatile, None);
        let generous = render_channel(&inputs, Channel::Volatile, Some(usize::MAX));
        assert_eq!(generous, unbudgeted);
        assert_eq!(generous.rendered, vec!["aa", "bb", "cc"]);
        assert!(generous.dropped.is_empty());
    }

    #[test]
    fn force_outranks_precedence_in_the_cached_block() {
        // Precedence ranks within a force group, never across one: `must` is
        // the coarser statement of importance and the grouping already spends
        // the budget on it first.
        let should = ranked(Force::Should, "ctx.a.b.should", "Advisory claim.", 90);
        let must = ranked(Force::Must, "ctx.a.b.must", "Binding claim.", 10);
        let select = Disposition::Select;
        let full = render_channel(
            &[input(&should, &select), input(&must, &select)],
            Channel::Cached,
            None,
        );
        let out = render_channel(
            &[input(&should, &select), input(&must, &select)],
            Channel::Cached,
            Some(full.text.len() - 1),
        );
        assert_eq!(
            out.rendered,
            vec!["must"],
            "a precedence-90 advisory must not displace a binding rule"
        );
        assert_eq!(out.dropped, vec!["should"]);
    }

    #[test]
    fn a_budget_that_fits_nothing_emits_no_heading_at_all() {
        let record = at(Force::Must, "ctx.a.b.one", "Statement.");
        let out = render_channel(
            &[input(&record, &Disposition::Select)],
            Channel::Cached,
            Some(4),
        );
        assert!(
            out.text.is_empty(),
            "an empty heading claims rules exist that the model cannot see: {}",
            out.text
        );
        assert_eq!(out.dropped, vec!["one"]);
    }

    #[test]
    fn no_records_at_all_renders_nothing() {
        assert_eq!(
            render_channel(&[], Channel::Cached, None),
            RenderedChannel::default()
        );
        assert_eq!(
            render_channel(&[], Channel::Volatile, None),
            RenderedChannel::default()
        );
    }

    // The [enforced] marker

    #[test]
    fn only_an_armed_guard_earns_the_enforced_marker() {
        let record = at(Force::Must, "ctx.a.b.no-force-push", "Never force-push.");
        let select = Disposition::Select;

        let armed = render_channel(&[enforced_input(&record, &select)], Channel::Cached, None);
        assert!(
            armed.text.contains("^no-force-push [enforced]"),
            "{}",
            armed.text
        );

        let advisory = render_channel(&[input(&record, &select)], Channel::Cached, None);
        assert!(
            !advisory.text.contains("[enforced]"),
            "promising a block that will not happen is the blank-guard bug again: {}",
            advisory.text
        );
        assert!(
            advisory.text.contains("^no-force-push"),
            "{}",
            advisory.text
        );
    }
}

#[cfg(test)]
mod tier_tests {
    use super::super::tests::{loaded_from, record_named, with_tier};
    use super::*;
    use crate::ingest::record::Tier;

    fn input<'a>(record: &'a LoadedRecord, disposition: &'a Disposition) -> RenderInput<'a> {
        RenderInput {
            record,
            disposition,
            enforced: false,
        }
    }

    /// Witness for #2709: an explicitly pinned `may`-strength record rides the
    /// cached prefix — under its own `### Pinned` heading, so the Must/Should
    /// bytes of every pre-tier record set are untouched — and leaves the
    /// volatile channel entirely.
    #[test]
    fn an_explicitly_pinned_may_record_joins_the_cached_block() {
        let record = loaded_from(with_tier(
            record_named("ctx.acme.web.style"),
            Force::May,
            Tier::Pinned,
        ));
        let select = Disposition::Select;
        let inputs = [input(&record, &select)];
        let cached = render_channel(&inputs, Channel::Cached, None);
        assert!(
            cached.text.contains("### Pinned"),
            "a pinned may-record earns the cached block: {}",
            cached.text
        );
        assert!(
            render_channel(&inputs, Channel::Volatile, None)
                .text
                .is_empty(),
            "and no longer rides the volatile channel"
        );
    }

    /// Witness for #2709: an explicitly scoped `must` record leaves the cached
    /// prefix — its seat there was the whole cost the tier exists to avoid —
    /// and rides the volatile channel, where per-turn selection gates it.
    #[test]
    fn an_explicitly_scoped_must_record_leaves_the_cached_block() {
        let record = loaded_from(with_tier(
            record_named("ctx.acme.web.api-only"),
            Force::Must,
            Tier::Scoped,
        ));
        let select = Disposition::Select;
        let inputs = [input(&record, &select)];
        assert!(
            render_channel(&inputs, Channel::Cached, None)
                .text
                .is_empty(),
            "a scoped must vacates the prefix"
        );
        let volatile = render_channel(&inputs, Channel::Volatile, None);
        assert!(
            volatile.text.contains("pnpm"),
            "and renders in the volatile channel instead: {}",
            volatile.text
        );
    }

    /// The sweep constraint #2709 inherits: a stale-demoted pinned record
    /// never enters the stable prefix — demotion outranks the tier exactly as
    /// it outranked the declared force, because the reason for the demotion is
    /// per-turn information the byte-stable block cannot carry.
    #[test]
    fn a_demoted_pinned_record_stays_out_of_the_cached_block() {
        let record = loaded_from(with_tier(
            record_named("ctx.acme.web.stale-pin"),
            Force::May,
            Tier::Pinned,
        ));
        let stale = Disposition::SelectStale {
            reason: "ttl expired".to_string(),
        };
        let inputs = [input(&record, &stale)];
        assert!(
            render_channel(&inputs, Channel::Cached, None)
                .text
                .is_empty(),
            "a demoted pinned record must never enter the stable prefix"
        );
        assert!(
            !render_channel(&inputs, Channel::Volatile, None)
                .text
                .is_empty(),
            "it demotes to the volatile channel rather than vanishing"
        );
    }
}
