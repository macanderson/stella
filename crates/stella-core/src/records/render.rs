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
//! [cited]: super::super::context_record::context_use::ContextUseKind::Cited
//! [nr]: super::super::context_record::context_use::MissingContextKind

use super::super::ingest::record::Force;
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
    pub dropped: Vec<String>,
}

/// Which channel a record renders into, or `None` when it does not render at all.
///
/// A sweep demotion outranks the declared force: a record whose freshness is in
/// question cannot sit in a block that must stay byte-identical, because the reason
/// it was demoted is per-turn information even when the renderer declines to print
/// it.
pub fn channel_of(force: Force, disposition: &Disposition) -> Option<Channel> {
    if !disposition.is_selected() {
        return None;
    }
    if disposition.forces_volatile() || !force.is_always_injected() {
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
pub fn render_channel(
    inputs: &[RenderInput<'_>],
    channel: Channel,
    budget_chars: Option<usize>,
) -> RenderedChannel {
    let mine: Vec<&RenderInput<'_>> = inputs
        .iter()
        .filter(|input| {
            channel_of(
                effective_force(input.record, input.disposition),
                input.disposition,
            ) == Some(channel)
        })
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
fn render_cached(inputs: &[&RenderInput<'_>], budget_chars: Option<usize>) -> RenderedChannel {
    let mut out = RenderedChannel {
        text: CACHED_HEADING.to_string(),
        ..RenderedChannel::default()
    };
    for (force, heading) in [(Force::Must, "### Must"), (Force::Should, "### Should")] {
        let group: Vec<&&RenderInput<'_>> = inputs
            .iter()
            .filter(|input| effective_force(input.record, input.disposition) == force)
            .collect();
        if group.is_empty() {
            continue;
        }
        let mut section = format!("\n\n{heading}");
        let mut wrote_any = false;
        for input in group {
            let line = format!("\n{}", bullet(input));
            if over_budget(&out.text, &section, &line, budget_chars) {
                out.dropped.push(input.record.handle.clone());
                continue;
            }
            section.push_str(&line);
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
    for input in inputs {
        let line = format!("\n{}", bullet(input));
        if over_budget(&out.text, "", &line, budget_chars) {
            out.dropped.push(input.record.handle.clone());
            continue;
        }
        out.text.push_str(&line);
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

/// Whether appending `line` would exceed the channel budget.
fn over_budget(text: &str, pending: &str, line: &str, budget_chars: Option<usize>) -> bool {
    budget_chars.is_some_and(|budget| text.len() + pending.len() + line.len() > budget)
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
