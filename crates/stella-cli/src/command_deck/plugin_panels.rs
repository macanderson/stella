// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Seating an installed plugin's panels, and keeping them fed — the driver
//! half of SPEC 12.2's three placements.
//!
//! [`stella_tui::panel_deck`] holds the frames and draws them; this decides
//! which panels exist and asks each one for its next frame. The split is the
//! same one `plugin_panel` and `panel_host` already have: the deck owns
//! rectangles, and the driver owns processes and grants.
//!
//! # The reserved-name check lives here because the reserved list does
//!
//! `stella-plugin` validates a `[panel] command` as a slug and stops there
//! ([`stella_plugin::PanelGrant::command`]): it is a near-leaf that
//! takes `stella-protocol` and nothing else, and the built-in table is
//! [`super::skills::DECK_BUILTINS`], here. So the collision is asked here, and
//! **out loud**. A colliding name is refused with a notice naming the plugin
//! and the name it wanted — never the silent drop
//! `CustomExtensions::slash_entries` performs for a `.stella/commands/*.toml`
//! row, because a `.toml` a user wrote is theirs to re-read and a signed
//! manifest a human accepted at install must not contain a line that quietly
//! does nothing (#5055).
//!
//! The refusal is per name, not per plugin: a plugin whose popup name collides
//! keeps its `settings` pane and its `overlay` block, because those are three
//! separate things a person agreed to and only one of them is unavailable.

use std::collections::HashSet;

use stella_plugin::{PanelLease, PanelSurface};
use stella_tui::envelope::PanelSeat;

use super::skills::deck_reserved;
use crate::plugin_cmd::roster::PluginPanelRoute;

/// The frame budget every panel is leased, in milliseconds.
///
/// 33ms is SPEC 12.4's worked example — 30fps equivalent — and it is a
/// deadline for the *tag*, never for the process: a panel that overruns keeps
/// its last good frame and is marked, and is killed only by
/// `[panel.process] timeout_secs`, which is the plugin author's own number.
pub(super) const FRAME_BUDGET_MS: u32 = 33;

/// What the deck should seat, and what the operator should be told.
pub(super) struct Seating {
    /// The panels that may draw, in route order.
    pub(super) seats: Vec<PanelSeat>,
    /// One line per refused slash name, ready for [`super::system_notice`].
    pub(super) refusals: Vec<String>,
}

/// Decide which of `routes` may draw, and refuse every slash name that
/// collides with one of the deck's own commands.
///
/// `reserved` is the deck's own vocabulary, taken as an argument rather than
/// read from [`deck_reserved`] so the refusal can be witnessed against a table
/// a test wrote — the collision rule is what is under test, not the contents of
/// the built-in list on the day the test was written.
pub(super) fn seat_with(routes: &[PluginPanelRoute], reserved: &[&str]) -> Seating {
    let reserved: HashSet<&str> = reserved
        .iter()
        .map(|name| name.strip_prefix('/').unwrap_or(name))
        .collect();
    // A second plugin asking for a name the first already took is the same
    // failure with a different owner, and gets the same visible refusal.
    let mut taken: HashSet<String> = HashSet::new();
    let mut seats = Vec::with_capacity(routes.len());
    let mut refusals = Vec::new();
    for route in routes {
        let command = match (route.surface, route.command.as_deref()) {
            (PanelSurface::Command, Some(name)) if reserved.contains(name) => {
                refusals.push(format!(
                    "! the plugin `{}` asked for /{name}, which is one of stella's own commands — \
                     its popup is not installed. Rename `[panel] command` in its manifest to open it.",
                    route.plugin
                ));
                None
            }
            (PanelSurface::Command, Some(name)) if !taken.insert(name.to_string()) => {
                refusals.push(format!(
                    "! the plugin `{}` asked for /{name}, which another installed plugin already \
                     opens — its popup is not installed. Rename `[panel] command` in its manifest \
                     to open it.",
                    route.plugin
                ));
                None
            }
            (PanelSurface::Command, name) => name.map(str::to_string),
            _ => None,
        };
        // A command route whose name was refused is not seated at all: the
        // popup has no way to be opened, and a seat nothing can reach is the
        // silent drop this module exists to avoid, one layer down.
        if route.surface == PanelSurface::Command && command.is_none() {
            continue;
        }
        seats.push(PanelSeat {
            plugin: route.plugin.clone(),
            surface: route.surface,
            command,
        });
    }
    Seating { seats, refusals }
}

/// [`seat_with`] against the deck's live vocabulary.
pub(super) fn seat(routes: &[PluginPanelRoute]) -> Seating {
    seat_with(routes, &deck_reserved())
}

/// Spawn the exchange for one frame request and answer the deck with exactly
/// one panel envelope.
///
/// Spawned rather than awaited, so neither the driver's loop nor the deck's
/// draw waits on somebody else's process. **Exactly one answer always goes
/// back** — [`stella_tui::envelope::Inbound::PanelSilent`] for a tick that
/// produced nothing — because the seat is rearmed by the answer, and a request
/// that went unanswered would leave the panel waiting for the rest of the
/// session.
///
/// A `slot` naming no route sends nothing and starts nothing: the seat list and
/// the route list are built from the same `panel_routes()` in the same order,
/// so an index outside it is a message from a deck that has been reseated and
/// there is no plugin it could mean.
pub(super) fn spawn_tick(
    routes: &[PluginPanelRoute],
    slot: usize,
    tick: u64,
    cols: u16,
    rows: u16,
    tx: &tokio::sync::mpsc::UnboundedSender<stella_tui::envelope::Inbound>,
) {
    let Some(route) = routes.get(slot) else {
        return;
    };
    let lease = PanelLease::new(
        route.plugin.clone(),
        route.surface,
        tick,
        stella_plugin::PanelRect::new(cols, rows),
        FRAME_BUDGET_MS,
    );
    // The child's whole environment, resolved here rather than inside the
    // task. `stella-runtime` may not read process-global state — N sessions in
    // one process would all see the same value, which is the property
    // `no_ambient_reads` protects — so `resolve_env` applies the manifest's
    // policy over a lookup its caller supplies, and this binary is the caller
    // that owns the ambient world. Before the spawn, so the read happens at one
    // stated point on one thread instead of on every panel task at once.
    let env =
        stella_runtime::panel_host::resolve_env(&route.process, |name| std::env::var(name).ok());
    let route = route.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        let answer = tick_once(slot, &route, lease, &env)
            .await
            .unwrap_or(stella_tui::envelope::Inbound::PanelSilent { slot });
        let _ = tx.send(answer);
    });
}

/// Ask one panel for its next frame, off the draw path, and report what to
/// land in the deck's state.
///
/// `async` and spawned by [`spawn_tick`]: SPEC 12.4 says a panel is asked for a
/// frame between draws and never during one, because the deck's repaint is a
/// pure projection and a repaint that waits on somebody else's process is a
/// terminal that has frozen.
///
/// A missed budget is [`stella_tui::envelope::Inbound::PanelThrottled`], which
/// keeps whatever frame is on screen — the panel is late, not wrong. A frame
/// the lease refuses is neither: `PanelLease::admits` answers the surface and
/// the tick before geometry, so a frame for another rectangle is dropped here
/// rather than blitted into this one.
async fn tick_once(
    slot: usize,
    route: &PluginPanelRoute,
    lease: PanelLease,
    env: &[(String, String)],
) -> Option<stella_tui::envelope::Inbound> {
    use stella_tui::envelope::Inbound;

    let budget_ms = lease.budget_ms;
    let outcome = stella_runtime::panel_host::ask(&route.process, lease.clone(), env).await;
    let tick = match outcome {
        Ok(tick) => tick,
        // Every failure is the same thing to the deck: no new frame. The
        // typed error is the driver's to log, not the panel's to render — a
        // plugin's stack trace is not chrome a reader asked for.
        Err(_) => return None,
    };
    let elapsed_ms = u64::try_from(tick.elapsed.as_millis()).unwrap_or(u64::MAX);
    if elapsed_ms > u64::from(budget_ms) {
        return Some(Inbound::PanelThrottled {
            slot,
            elapsed_ms,
            budget_ms,
        });
    }
    let frame = tick.frame?;
    lease.admits(&frame).ok()?;
    Some(Inbound::PanelFrame {
        slot,
        frame: Box::new(frame),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_core::ports::Principal;
    use stella_plugin::Runtime;

    fn route(plugin: &str, surface: PanelSurface, command: Option<&str>) -> PluginPanelRoute {
        PluginPanelRoute {
            plugin: plugin.to_string(),
            principal: Principal::Plugin(plugin.to_string()),
            surface,
            command: command.map(str::to_string),
            process: Runtime {
                argv: vec!["true".to_string()],
                timeout_secs: 2,
                env: Vec::new(),
            },
        }
    }

    /// **The refusal witness.** A plugin whose slash name is one of the deck's
    /// own is refused out loud: no seat, and a notice naming both the plugin
    /// and the name. The silent drop is what `slash_entries` does to a
    /// `.stella/commands/*.toml` row, and a signed manifest gets the opposite.
    #[test]
    fn a_name_colliding_with_a_builtin_is_refused_out_loud() {
        let routes = vec![route("impostor", PanelSurface::Command, Some("model"))];
        let seating = seat_with(&routes, &["/model", "/help"]);

        assert!(
            seating.seats.is_empty(),
            "a refused name seats no popup: {:?}",
            seating.seats
        );
        assert_eq!(seating.refusals.len(), 1, "{:?}", seating.refusals);
        let notice = &seating.refusals[0];
        assert!(notice.contains("impostor"), "names the plugin: {notice}");
        assert!(notice.contains("/model"), "names the name: {notice}");
    }

    /// The refusal is per name. The plugin keeps the two placements that did
    /// not collide, because they are separate things the operator agreed to.
    #[test]
    fn a_refused_popup_does_not_cost_the_plugin_its_other_panels() {
        let routes = vec![
            route("impostor", PanelSurface::Settings, None),
            route("impostor", PanelSurface::Overlay, None),
            route("impostor", PanelSurface::Command, Some("help")),
        ];
        let seating = seat_with(&routes, &["/help"]);

        let surfaces: Vec<_> = seating.seats.iter().map(|seat| seat.surface).collect();
        assert_eq!(
            surfaces,
            vec![PanelSurface::Settings, PanelSurface::Overlay]
        );
        assert_eq!(seating.refusals.len(), 1);
    }

    /// A name a built-in does not claim is seated, and carries the name a
    /// person types.
    #[test]
    fn an_uncontested_name_is_seated_under_the_name_it_asked_for() {
        let routes = vec![route("hello", PanelSurface::Command, Some("hello"))];
        let seating = seat_with(&routes, &["/model"]);

        assert!(seating.refusals.is_empty(), "{:?}", seating.refusals);
        assert_eq!(seating.seats.len(), 1);
        assert_eq!(seating.seats[0].command.as_deref(), Some("hello"));
    }

    /// Two plugins wanting one name is the same failure with a different
    /// owner: the first keeps it, the second is told.
    #[test]
    fn a_name_a_second_plugin_already_opens_is_refused_too() {
        let routes = vec![
            route("first", PanelSurface::Command, Some("shared")),
            route("second", PanelSurface::Command, Some("shared")),
        ];
        let seating = seat_with(&routes, &[]);

        assert_eq!(seating.seats.len(), 1, "{:?}", seating.seats);
        assert_eq!(seating.seats[0].plugin, "first");
        assert_eq!(seating.refusals.len(), 1);
        assert!(
            seating.refusals[0].contains("second"),
            "{:?}",
            seating.refusals
        );
    }

    /// The deck's live table is what production is held to, and it does claim
    /// the names this rule is about — so the guard is not vacuous.
    #[test]
    fn the_live_reserved_table_refuses_a_builtin_name() {
        let routes = vec![route("impostor", PanelSurface::Command, Some("settings"))];
        let seating = seat(&routes);
        assert!(seating.seats.is_empty(), "{:?}", seating.seats);
        assert_eq!(seating.refusals.len(), 1, "{:?}", seating.refusals);
    }

    /// A plugin whose panel process leaves a mark on disk the instant it is
    /// started, so "was it asked for a frame?" is answerable by looking rather
    /// than by trusting a route list.
    #[cfg(unix)]
    fn panel_manifest(name: &str, marker: &std::path::Path) -> String {
        format!(
            "name = \"{name}\"\n\
             description = \"a panel that reports having been started\"\n\
             \n\
             [panel]\n\
             surfaces = [\"settings\"]\n\
             denies = [\"network\", \"write-outside-sandbox\"]\n\
             \n\
             [panel.process]\n\
             argv = [\"/bin/sh\", \"-c\", \"touch '{}'\"]\n\
             timeout_secs = 2\n\
             env = []\n",
            marker.display()
        )
    }

    /// Plant that plugin into `tier`, optionally with the consent receipt
    /// `stella plugin install` writes.
    #[cfg(unix)]
    fn plant_panel(tier: &std::path::Path, name: &str, marker: &std::path::Path, consented: bool) {
        let dir = tier.join(name);
        std::fs::create_dir_all(&dir).expect("fixture plugin dir");
        let text = panel_manifest(name, marker);
        std::fs::write(dir.join(crate::plugin_cmd::roster::MANIFEST_FILE), &text)
            .expect("fixture manifest");
        if consented {
            crate::plugin_cmd::receipt::record(
                tier,
                crate::plugin_cmd::roster::PluginScope::User,
                name,
                name,
                text.as_bytes(),
            )
            .expect("fixture receipt");
        }
    }

    /// **The gate witness.** No panel frame is requested before the install
    /// grant — asserted on the *process*, not on the rendering.
    ///
    /// The shape is `stella-mcp`'s
    /// `no_tool_call_reaches_an_ungranted_server_before_the_grant`: the
    /// security property is that nothing went out, so it is asserted by looking
    /// at what the other side saw. Here the other side is a program on
    /// somebody's machine, and the marker file is its wire log — a test that
    /// only checked `panel_routes().is_empty()` would pass against a build that
    /// still spawned the plugin from somewhere else.
    ///
    /// The refusal proves nothing unless the same bytes are shown to run once
    /// the grant is on record, so the second half is here for the reason that
    /// test carries its own.
    #[cfg(unix)]
    #[tokio::test]
    async fn no_panel_frame_is_requested_before_the_install_grant() {
        use crate::plugin_cmd::roster::{PluginRoster, PluginScope};

        // The lock, but **not** `paths::test_user_home`: nothing here reads the
        // ambient home — `read_tier`, `receipt::record` and
        // `resolve_user_plugins_dir` are all handed the tier explicitly — and
        // moving `HOME` for a test that does not need it moved is a collision
        // waiting for whichever concurrent test does read it. This one did:
        // `subsession`'s resumed-lane test failed three runs in eight beside
        // it, minting `req:1` against a home that was not the operator's. The
        // lock is still taken, because `spawn_tick` resolves the panel's
        // environment with `std::env::var` and a concurrent `setenv` racing a
        // `getenv` is UB on POSIX.
        let _env = crate::test_env::lock();
        let root = std::env::temp_dir().join(format!("stella-panel-grant-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp root");
        let tier = stella_home::resolve_user_plugins_dir(Some(root.join(".stella")))
            .expect("an explicit stella root resolves its plugins tier");
        let marker = root.join("the-panel-process-ran");

        // Planted, complete, and never consented to.
        plant_panel(&tier, "unconsented", &marker, false);
        let ungranted = PluginRoster::compose(
            crate::plugin_cmd::roster::read_tier(&tier, PluginScope::User, &mut Vec::new()),
            Vec::new(),
            &std::collections::BTreeMap::new(),
        );
        let routes = ungranted.panel_routes();
        assert!(routes.is_empty(), "an ungranted plugin has no panel route");

        // The deck asks anyway — a request naming a slot the roster never
        // produced is exactly what a stale deck sends — and the driver starts
        // nothing.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        spawn_tick(&routes, 0, 1, 40, 8, &tx);
        drop(tx);
        assert!(rx.recv().await.is_none(), "nothing was even answered");

        // THE property, asserted about the other side of the pipe: the panel
        // process was never started, so a third party's code did not run.
        assert!(
            !marker.exists(),
            "a panel process ran before the operator granted it: {}",
            marker.display()
        );

        // Anti-vacuity: the same bytes run once the grant is on record.
        plant_panel(&tier, "consented", &marker, true);
        let granted = PluginRoster::compose(
            crate::plugin_cmd::roster::read_tier(&tier, PluginScope::User, &mut Vec::new()),
            Vec::new(),
            &std::collections::BTreeMap::new(),
        );
        let routes = granted.panel_routes();
        assert_eq!(routes.len(), 1, "a granted plugin's panel routes");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        spawn_tick(&routes, 0, 1, 40, 8, &tx);
        // The fixture draws no frame, so the answer is whichever of the two
        // no-frame envelopes the exchange earned. **Both are correct here and
        // pinning one is a race**: the fixture starts a real process, and a
        // loaded machine takes longer than the 33ms budget often enough to
        // matter — one run in eight, measured. What the seat needs is that one
        // of the three arrives at all, because an unanswered request would
        // leave the panel waiting for the rest of the session.
        let got = rx.recv().await;
        assert!(
            matches!(
                got,
                Some(
                    stella_tui::envelope::Inbound::PanelSilent { slot: 0 }
                        | stella_tui::envelope::Inbound::PanelThrottled { slot: 0, .. }
                )
            ),
            "a frameless tick still rearms the seat: {got:?}"
        );
        assert!(marker.exists(), "the granted plugin's process did run");

        let _ = std::fs::remove_dir_all(&root);
    }
}
