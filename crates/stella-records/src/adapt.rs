// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What the record channel picked, as steering candidates.
//!
//! An adapter maps what the channel already chose. It does not choose, rank,
//! or draw again. Each cost is read off the bytes [`crate::records::render`] will
//! emit, through that same call. So the cost the budget sees and the bytes
//! the prompt pays for cannot drift.
//!
//! The skill adapters live in `stella_cli::memory::steering`, next to the
//! code they read. These two moved here with the record plane. They
//! were the last thing that made the engine name a record type.

use stella_core::steering::{DroppedCandidate, SteeringCandidate, SteeringSource};

use crate::records::{Registry, RenderedChannel, render};

#[cfg(test)]
mod tests;

/// What this turn's volatile channel drew, as candidates.
///
/// `score` flattens the rank `render::survivors` cuts by: `force` first, then
/// `precedence`. So the ledger's order in this source matches the order the
/// channel's own budget would cut in. The flat form loses nothing: `strength`
/// is a `u8` and `precedence` a `u32`, so `strength * 2^32 + precedence` is
/// exact in an `f64` and sorts as the pair does.
///
/// A `handle` the `registry` cannot look up is skipped. `rendered` came from
/// this same `registry`, so such a `handle` means the caller mixed two of
/// them. A free candidate for it would spoil the ledger it feeds.
pub fn record_candidates(
    registry: &Registry,
    rendered: &RenderedChannel,
) -> Vec<SteeringCandidate> {
    rendered
        .rendered
        .iter()
        .filter_map(|handle| {
            let entry = registry.by_handle(handle)?;
            let input = entry.render_input();
            let force = render::effective_force(input.record, input.disposition);
            Some(SteeringCandidate {
                source: SteeringSource::Record,
                handle: handle.clone(),
                score: f64::from(force.strength()) * 4_294_967_296.0
                    + f64::from(input.record.record.precedence()),
                why: format!(
                    "record channel selected ^{handle} for this turn (force {}, precedence {})",
                    force.as_str(),
                    input.record.record.precedence()
                ),
                est_tokens: stella_protocol::estimate_tokens(&render::bullet(&input)),
            })
        })
        .collect()
}

/// What the channel's own budget cut, as ledger rows. This is the named-drop
/// rule `stella_core::steering::SteeringSet::dropped` holds, spread to every
/// source.
///
/// A `handle` the `registry` cannot look up keeps its name and costs zero.
/// For a cut row the name is the whole report, and the cost is not paid.
pub fn record_drops(registry: &Registry, rendered: &RenderedChannel) -> Vec<DroppedCandidate> {
    rendered
        .dropped
        .iter()
        .map(|handle| DroppedCandidate {
            source: SteeringSource::Record,
            handle: handle.clone(),
            est_tokens: registry
                .by_handle(handle)
                .map(|entry| {
                    stella_protocol::estimate_tokens(&render::bullet(&entry.render_input()))
                })
                .unwrap_or(0),
        })
        .collect()
}
