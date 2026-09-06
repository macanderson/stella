// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The plugin arm of the steering plane.
//!
//! `stella_core::steering::plugins` makes the choice.
//! `stella_runtime::wrapper` measures. This is the adapter between them. It
//! reads the allowance a workspace names. It hands that to the dispatcher
//! that drives the plugins. It names on stderr what the allowance refused.
//!
//! It is [`crate::tool_lean`]'s sibling, one source over. The shape is the
//! same on purpose. Both spend the number `context.steering.max_tokens`
//! names. Both go through the ledger that number is shared in. Both report
//! their cuts through `memory::report_steering_drops`, the one writer every
//! steering source uses.
//!
//! # A cut is a cost measure, not a change in what a plugin may do
//!
//! A plugin whose text the allowance could not hold still ran. Its scope and
//! witness lists still stand. What it published still reached the stage after
//! it. Its `after_turn` is still asked. What it lost is prompt bytes. The drop
//! report says so, and says how to buy them back.

use stella_core::steering::plugins::ContextAllowance;

use crate::config::Config;

/// What this session may spend on a plugin's text, or `None` when the
/// workspace has switched the steering plane off.
///
/// It reads the settings chain at bind time, as
/// [`crate::wrapper_plugin::standing_pipeline`] beside it does. A wrapper is
/// bound once per run, so the answer cannot move under a turn. The ledger it
/// carries is the session's. So the block a turn renders before the plugins
/// are asked has already spent from what this hands them.
pub(crate) fn allowance(cfg: &Config) -> Option<ContextAllowance> {
    let declared = crate::settings::Settings::load(&cfg.workspace_root)
        .unwrap_or_default()
        .plugin_context_budget()?;
    Some(ContextAllowance::new(
        declared,
        std::sync::Arc::clone(&cfg.steering_ledger),
    ))
}

/// Name what this round's allowance refused.
///
/// It goes through `memory::report_steering_drops`, so a plugin's cut reads
/// like a record's or a skill's, in the same place and the same shape. It is
/// silent when nothing was cut, which is every turn a workspace runs inside
/// its allowance.
pub(crate) fn report_drops(prelude: &stella_runtime::wrapper::TurnPrelude) {
    use colored::Colorize;

    // The memory budget this writer takes shapes the recall line alone, and a
    // set built by the wrapper socket holds plugin drops only.
    crate::memory::report_steering_drops(prelude.steering(), 0, |message| {
        eprintln!("  {} {message}", "!".yellow());
    });
}
