// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The session's hook plane: the operator's own hooks, plus the routes the
//! installed plugins are permitted to be dispatched at.
//!
//! # The gap this closes
//!
//! [`PluginRoster::hook_routes`] has produced "the set of dispatches a host
//! is permitted to make" since #3482, and until now its only consumer
//! **printed** them: a manifest could declare `hooks = ["Stop"]`, `stella
//! plugin list` would advertise it, and the hook never fired (#4417, #3521).
//! An emitted signal with no consumer is invariant 10's shape, and one that
//! a user has been *told about* is worse than one nobody can see.
//!
//! # Routes come from the roster and from nowhere else
//!
//! [`session_hook_plane`] reaches the project tier only through
//! [`PluginRoster::load`], because that is where the #3509 trust gate lives
//! (`plugin_cmd::roster::read_project_tier`). A host that reached for
//! `stella_home::resolve_project_plugins_dir` directly, or read the tier
//! before composing a roster, would re-open arbitrary code execution on
//! `git clone` with nothing failing — the gate is only as good as the
//! chokepoint being the only door. `no_other_production_site_reads_the_plugins_tier`
//! pins that the chokepoint *is* the only door, and
//! `an_untrusted_project_tier_contributes_no_hook_route` pins the gate at
//! this dispatch site rather than only at the roster.
//!
//! The roster is recomputed from disk on every load by design, so nothing
//! here caches: a plugin removed between two sessions is gone from the second
//! one, with nothing to clean up and nothing to forget to clean up.
//!
//! # Why the plane is assembled and never written to a settings file
//!
//! `plugin_cmd::roster`'s module doc gives the argument in full: a
//! [`HookMatcher`] carries no owner, so once several plugins' entries and the
//! operator's own have been concatenated by `settings::merge` there is no
//! expression that removes one plugin's entries and nobody else's. That
//! argument is about *persistence*, and it is why this fold happens in memory
//! at [`Config`](crate::config::Config) assembly, from a roster read seconds
//! earlier, rather than at install time into `settings.json`. Uninstall a
//! plugin and its routes are simply not built next session.
//!
//! Identity survives the fold even so:
//! [`HookAction::plugin`](stella_core::hooks::HookAction::plugin) carries the
//! manifest name onto every action, which is what lets a notice say *which*
//! plugin held a turn open, and what a later `max_holds` binding (#3487) will
//! key off.
//!
//! # The combination rule when several plugins declare the same event
//!
//! Every route runs. They are ordered `(plugin name, event)` by
//! [`PluginRoster::hook_routes`] — install order is machine-local and would
//! show two clones a different dispatch order for one configuration — and the
//! operator's own matchers stay ahead of every plugin's, so a plugin can
//! never displace a hook the user wrote.
//!
//! Where several plugins declare a **bound** rather than an action, the
//! combination is the **minimum** of the declared bounds, never the sum and
//! never the last one read. `LoopGrant::max_holds` is the case in hand: it is
//! an *ask* (`doc:wrapper-socket` §5), the host already clamps it to
//! `max_stop_hook_holds`, and two plugins asking for 3 and 5 holds compose to
//! 3. Any other rule lets a plugin widen a bound by installing beside another
//! one, which is the self-escalation shape #3514 is about wearing a second
//! face. Nothing binds `max_holds` yet — #3487 is the issue that does, and it
//! was blocked on this module existing.

use std::path::Path;

use stella_core::hooks::{HookAction, HookEvent, HookMatcher, Hooks, PluginHookOrigin};

use crate::plugin_cmd::roster::{PluginHookRoute, PluginRoster};
use crate::settings::Settings;

/// The hook plane a session runs with: the merged settings chain's hooks,
/// with every installed plugin's declared routes folded in.
///
/// `None` under the trusted launcher's filesystem-isolation boundary, which
/// closes filesystem-configured extensions of every kind — the same answer
/// `PluginRoster::load` gives on its own side, so a plugin cannot be the one
/// extension that stays on inside a boundary drawn to exclude executable
/// ones.
///
/// Roster load notices are dropped here for `agent::seats::installed_seats`'
/// reason: this is config assembly, and the install and `--pipeline` paths
/// already surface them where a human can act on them.
pub(crate) fn session_hook_plane(workspace_root: &Path, settings: &Settings) -> Option<Hooks> {
    if crate::enterprise_telemetry::process_free_authority_active() {
        return None;
    }
    let (roster, _notices) = PluginRoster::load(workspace_root, settings);
    fold_plugin_routes(settings.hooks.clone(), &roster.hook_routes())
}

/// Fold routes into the operator's hooks. Pure — the caller supplies what it
/// read, which is what lets the trust gate be witnessed without a process
/// environment.
///
/// A plugin route carries no matcher, so a `PreToolUse` grant fires for every
/// tool: a manifest has no matcher vocabulary to narrow it with, and
/// inventing one here would be a grant no consent text ever showed.
pub(crate) fn fold_plugin_routes(
    operator: Option<Hooks>,
    routes: &[PluginHookRoute],
) -> Option<Hooks> {
    if routes.is_empty() {
        // Not `Some(Hooks::default())`: a hook-free session must carry no
        // hooks handle at all, so the engine takes its pre-hooks path
        // (`settings::merge`'s `concat_hooks`).
        return operator;
    }
    let mut hooks = operator.unwrap_or_default();
    for route in routes {
        let action = HookAction::from_plugin(
            PluginHookOrigin {
                plugin: route.plugin.clone(),
                argv: route.argv.clone(),
                env_allowlist: route.env_allowlist.clone(),
            },
            // Seconds on the manifest, milliseconds in the engine.
            // `HookAction::effective_timeout_ms` clamps to
            // `MAX_HOOK_TIMEOUT_MS` from here, so a manifest cannot buy
            // itself an unbounded hook any more than an unbounded loop.
            route.timeout_secs.saturating_mul(1_000),
        );
        slot_for(&mut hooks, route.event).push(HookMatcher {
            matcher: None,
            hooks: vec![action],
        });
    }
    Some(hooks)
}

/// The list one event's matchers live in, created empty if this is the first.
///
/// Exhaustive with no rest pattern, for [`crate::settings::completeness`]'
/// reason: a new event added to `Hooks` stops this compiling until its author
/// says whether a plugin may be routed at it.
fn slot_for(hooks: &mut Hooks, event: HookEvent) -> &mut Vec<HookMatcher> {
    let slot = match event {
        HookEvent::SessionStart => &mut hooks.session_start,
        HookEvent::PreToolUse => &mut hooks.pre_tool_use,
        HookEvent::PostToolUse => &mut hooks.post_tool_use,
        HookEvent::Stop => &mut hooks.stop,
        HookEvent::PreCompact => &mut hooks.pre_compact,
        // Unreachable through the roster: `EVENT_ORDER` omits the
        // loop-lifecycle pair and `ManifestError::HookNotAvailableToPlugins`
        // refuses a manifest that declares one. Named rather than
        // `unreachable!` — invariant 5 — so a future route lands somewhere
        // real instead of panicking a session.
        HookEvent::PreIssueWork => &mut hooks.pre_issue_work,
        HookEvent::PostIssueWork => &mut hooks.post_issue_work,
    };
    slot.get_or_insert_with(Vec::new)
}

// `pub(crate)` rather than private: `config::tests::resolution` drives the
// same fixture through `Config::load_with_settings` to witness the wiring,
// and it is not a descendant of this module.
#[cfg(test)]
pub(crate) mod tests;
