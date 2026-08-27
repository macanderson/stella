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

/// The panel routes this session is driving, and which composition of the
/// roster they came from.
///
/// # Why the driver holds a generation at all (#5253)
///
/// The panels are seated from the roster, and the roster changes under a live
/// session: a plugin is installed, removed, or retracted with
/// `plugins.<name> = "off"`, and `/reload` is the command whose whole job is to
/// pick that up. Reseating is wholesale — `PanelDeck::reseat` replaces every
/// slot — so a slot index means one plugin before a reseat and another after
/// it.
///
/// That is survivable on the **answer** leg, which `PanelSlot::settle` already
/// handles by checking the surface and the tick a frame carries. It is not
/// survivable on the **request** leg: a `PanelFrameWanted` raised before a
/// reseat and handled after one names a slot that still resolves, and starting
/// the program it now resolves to is a third party's code running against a
/// lease nobody granted it. So the seating rides on the request, the driver
/// refuses a mismatch, and the two lists cannot be read against each other at
/// all.
#[derive(Default)]
pub(super) struct PanelPlane {
    /// Which composition these routes came from. Counted up by
    /// [`PanelPlane::reseat`] and never reused, so a wrapped or repeated value
    /// cannot make a stale request look current.
    generation: u64,
    /// Every panel a host may ask for a frame, in the order the deck seats
    /// them.
    routes: Vec<PluginPanelRoute>,
}

/// Everything one reseat owes the session: the seats, the refusals, the
/// handshakes, and which seating they belong to.
pub(super) struct Reseated {
    pub(super) generation: u64,
    pub(super) seats: Vec<PanelSeat>,
    pub(super) refusals: Vec<String>,
    pub(super) handshakes: Vec<String>,
}

impl PanelPlane {
    /// Recompose the roster from disk and replace the plane with what it
    /// admits.
    ///
    /// The whole panel half of a session's boot, and of every `/reload`, in one
    /// call — so the two paths cannot come to differ, which is the shape #5253
    /// found: the seating existed in exactly one place, and `/reload` had no
    /// way to reach it.
    ///
    /// Through `PluginRoster::load` and nowhere else: that is where the #3509
    /// project-tier trust gate lives (`plugin_hooks`'s
    /// `no_other_production_site_reads_the_plugins_tier`), and a second reader
    /// would be a second place it can be forgotten.
    pub(super) fn reseat(&mut self, workspace_root: &std::path::Path) -> Reseated {
        let settings = crate::settings::Settings::load(workspace_root).unwrap_or_default();
        let (roster, _) = crate::plugin_cmd::roster::PluginRoster::load(workspace_root, &settings);
        let routes = roster.panel_routes();
        let seating = seat(&routes);
        let handshakes = handshakes(&roster);
        self.generation = self.generation.saturating_add(1);
        self.routes = routes;
        Reseated {
            generation: self.generation,
            seats: seating.seats,
            refusals: seating.refusals,
            handshakes,
        }
    }
}

#[cfg(test)]
impl PanelPlane {
    /// A plane over routes a test composed, at a stated seating.
    ///
    /// `#[cfg(test)]` rather than a `pub(super)` constructor: production has
    /// exactly one way to build a plane, and that is
    /// [`PanelPlane::reseat`] reading the roster off disk. A second door in the
    /// shipping binary would be a second place the trust gate can be bypassed
    /// (CLAUDE.md, "`#[cfg(test)]` is the better answer").
    fn at(generation: u64, routes: Vec<PluginPanelRoute>) -> Self {
        Self { generation, routes }
    }
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

/// The handshake blocks a session owes its operator before any panel draws —
/// SPEC 12.4's first sentence (#5056).
///
/// One block per installed plugin that declares a `[panel]`, in roster order,
/// and none for a workspace with no panel plugins at all.
///
/// # Why an allowed panel still says something, and why it is one line
///
/// SPEC 12.4 asks for the handshake *before any panel*, not before the first
/// one ever. A rectangle drawn by somebody else's program on every boot is a
/// standing grant, and a standing grant that is never restated becomes
/// invisible — which is the same failure the install receipt exists to catch,
/// one surface later. But the full document is what you read to *decide*, and
/// re-reading it at every boot for a decision already made would train a reader
/// to skip the block that matters. So an allowed panel gets the two facts that
/// change — what it draws, and the signature the grant covers — and the verb
/// that withdraws it.
///
/// A panel that is **not** allowed gets the whole document, because that
/// reader has a decision in front of them.
///
/// # The signature is re-read from disk
///
/// Not digested from the roster's own parse: the grant on disk is keyed to the
/// bytes `read_tier` will hash on the next load, and a signature computed from
/// anything else would show a reader a number that decides nothing. A manifest
/// that has become unreadable since the roster was composed yields no block
/// rather than a block with a fabricated signature in it.
pub(super) fn handshakes(roster: &crate::plugin_cmd::roster::PluginRoster) -> Vec<String> {
    let mut blocks = Vec::new();
    for plugin in roster.plugins() {
        if plugin.manifest.panel.is_none() {
            continue;
        }
        let Ok(Some((_, text))) = crate::plugin_cmd::roster::read_manifest(&plugin.dir) else {
            continue;
        };
        let signature = format!(
            "sha256:{}",
            crate::plugin_cmd::receipt::digest(text.as_bytes())
        );
        let name = &plugin.manifest.name;
        if plugin.panel_grant.admits() {
            let surfaces: Vec<&str> = PanelSurface::all()
                .iter()
                .filter(|surface| {
                    plugin
                        .manifest
                        .panel
                        .as_ref()
                        .is_some_and(|panel| panel.draws(**surface))
                })
                .map(|surface| surface.as_str())
                .collect();
            blocks.push(format!(
                "◳ panel · {name} — you allowed this plugin to draw on your screen \
                 ({}), under manifest signature {signature}. \
                 `stella plugin panel {name}` withdraws it.",
                surfaces.join(", ")
            ));
            continue;
        }
        let mut block = String::new();
        if let Some(handshake) = stella_plugin::panel_handshake_text(&plugin.manifest, &signature) {
            block.push_str(&handshake);
            block.push('\n');
        }
        if let Some(notice) = plugin.panel_grant.notice(name) {
            block.push('\n');
            block.push_str(&notice);
        }
        if !block.is_empty() {
            blocks.push(block);
        }
    }
    blocks
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
///
/// **A `generation` naming an earlier seating is the same refusal, for the case
/// the index check cannot see** (#5253). A request minted before a reseat and
/// answered after one carries a slot index that still resolves — to a
/// *different plugin's* route — so an index-only check would start somebody
/// else's program against a lease the operator never granted it. The deck
/// stamps the seating on the request and this drops any that is not the one
/// in force.
pub(super) fn spawn_tick(
    plane: &PanelPlane,
    generation: u64,
    slot: usize,
    tick: u64,
    cols: u16,
    rows: u16,
    tx: &tokio::sync::mpsc::UnboundedSender<stella_tui::envelope::Inbound>,
) {
    if generation != plane.generation {
        return;
    }
    let Some(route) = plane.routes.get(slot) else {
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
    /// `stella plugin install` writes and the panel grant `stella plugin
    /// panel` writes.
    ///
    /// The two are separate arguments because they are separate transactions
    /// (#5056): a package can be installed and its panel denied, and the whole
    /// point of the second gate is that the first does not answer for it.
    #[cfg(unix)]
    fn plant_panel(
        tier: &std::path::Path,
        name: &str,
        marker: &std::path::Path,
        consented: bool,
        panel: Option<crate::plugin_cmd::panel_grant::PanelVerdict>,
    ) {
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
        if let Some(verdict) = panel {
            crate::plugin_cmd::panel_grant::record(
                tier,
                crate::plugin_cmd::roster::PluginScope::User,
                name,
                name,
                text.as_bytes(),
                verdict,
            )
            .expect("fixture panel grant");
        }
    }

    /// Compose the user tier as a session would, and report its panel routes.
    #[cfg(unix)]
    fn panel_routes_of(tier: &std::path::Path) -> Vec<PluginPanelRoute> {
        use crate::plugin_cmd::roster::{PluginRoster, PluginScope};

        PluginRoster::compose(
            crate::plugin_cmd::roster::read_tier(tier, PluginScope::User, &mut Vec::new()),
            Vec::new(),
            &std::collections::BTreeMap::new(),
        )
        .panel_routes()
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
        plant_panel(&tier, "unconsented", &marker, false, None);
        let routes = panel_routes_of(&tier);
        assert!(routes.is_empty(), "an ungranted plugin has no panel route");

        // The deck asks anyway — a request naming a slot the roster never
        // produced is exactly what a stale deck sends — and the driver starts
        // nothing.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        spawn_tick(&PanelPlane::at(1, routes.clone()), 1, 0, 1, 40, 8, &tx);
        drop(tx);
        assert!(rx.recv().await.is_none(), "nothing was even answered");

        // THE property, asserted about the other side of the pipe: the panel
        // process was never started, so a third party's code did not run.
        assert!(
            !marker.exists(),
            "a panel process ran before the operator granted it: {}",
            marker.display()
        );

        // Anti-vacuity: the same bytes run once both grants are on record.
        plant_panel(
            &tier,
            "consented",
            &marker,
            true,
            Some(crate::plugin_cmd::panel_grant::PanelVerdict::Allow),
        );
        let routes = panel_routes_of(&tier);
        assert_eq!(routes.len(), 1, "a granted plugin's panel routes");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        spawn_tick(&PanelPlane::at(1, routes.clone()), 1, 0, 1, 40, 8, &tx);
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

    /// **The panel-grant witness (#5056).** An installed, consented, perfectly
    /// loadable plugin whose panel handshake was answered `deny` — or never
    /// answered at all — does not have its panel program started.
    ///
    /// Asserted on the marker file rather than on `panel_routes()` for the
    /// reason the install-grant witness above gives, and the reason is sharper
    /// here: the install grant withholds the whole package, so a build that
    /// ignored it would fail in a dozen visible ways. A panel grant withholds
    /// one rectangle from a package that is otherwise fully in force — its
    /// hooks fire, its tools are registered, its process is a program the host
    /// already knows how to start — so "the route list is empty" and "nobody's
    /// code ran" are genuinely different claims, and only the second one is the
    /// security property.
    ///
    /// The three states are asserted together because they share one marker
    /// path: if any of them started a process, the file exists, and the test
    /// cannot pass by having checked the wrong one.
    #[cfg(unix)]
    #[tokio::test]
    async fn no_panel_frame_is_requested_when_the_panel_grant_does_not_allow_it() {
        use crate::plugin_cmd::panel_grant::PanelVerdict;

        // `test_env::lock` for `spawn_tick`'s `std::env::var`, on the sibling
        // witness's reasoning; the home is never read, so it is never moved.
        let _env = crate::test_env::lock();
        let root = std::env::temp_dir().join(format!("stella-panel-deny-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp root");
        let tier = stella_home::resolve_user_plugins_dir(Some(root.join(".stella")))
            .expect("an explicit stella root resolves its plugins tier");
        let marker = root.join("the-panel-process-ran");

        // Installed and consented to, both of them. One was answered `deny`;
        // the other was never asked.
        plant_panel(&tier, "denied", &marker, true, Some(PanelVerdict::Deny));
        plant_panel(&tier, "unasked", &marker, true, None);

        let routes = panel_routes_of(&tier);

        // The deck asks for both slots regardless — a request naming a slot the
        // roster never produced is exactly what a stale deck sends.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for slot in [0, 1] {
            spawn_tick(&PanelPlane::at(1, routes.clone()), 1, slot, 1, 40, 8, &tx);
        }
        drop(tx);
        // Drained before anything is asserted, and that is the synchronisation
        // rather than a check: every sender clone lives inside a spawned task,
        // so `None` means each task has finished — which is the only moment at
        // which "the marker does not exist" distinguishes "no process was
        // started" from "one was started and has not touched the file yet".
        let answered = rx.recv().await;

        // THE property first, so a flip fails on somebody's code having run
        // rather than on the route list that is only evidence about it
        // (`a_frame_in_flight_across_a_reseat_lands_nowhere`'s ordering).
        assert!(
            !marker.exists(),
            "a panel process ran without an allow: {}",
            marker.display()
        );
        assert!(
            routes.is_empty(),
            "and no route existed for it to run from: {routes:?}"
        );
        assert!(
            answered.is_none(),
            "nothing was even answered: {answered:?}"
        );

        // Anti-vacuity, and the reason the gate is the *panel* grant rather
        // than the install one: the same package, the same receipt, the same
        // bytes — answered `allow` — does run.
        plant_panel(&tier, "allowed", &marker, true, Some(PanelVerdict::Allow));
        let routes = panel_routes_of(&tier);
        assert_eq!(routes.len(), 1, "only the allowed panel is routed");
        assert_eq!(routes[0].plugin, "allowed");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        spawn_tick(&PanelPlane::at(1, routes.clone()), 1, 0, 1, 40, 8, &tx);
        // Either no-frame envelope, on the sibling witness's measured reason:
        // the fixture starts a real process and a loaded machine overruns the
        // 33ms budget often enough that pinning one of them is a race.
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
        assert!(marker.exists(), "the allowed plugin's process did run");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The `/reload` witness (#5253).** A reseat composes the roster again
    /// from disk, so a plugin retracted since the last one loses its seat and
    /// its route — and the seating moves, which is what tells the two apart.
    ///
    /// Driven through [`PanelPlane::reseat`] rather than through the driver
    /// loop, because that function is the whole of what `/reload` now does to
    /// the panels: `DeckCommand::Reloaded` calls it and hands what it returns
    /// to `announce_panels`. What is under test is that composing twice gives
    /// two different answers, which is the thing that used to be impossible —
    /// the seating happened once, at session open, and nothing could reach it
    /// again.
    #[cfg(unix)]
    #[test]
    fn a_reseat_drops_a_plugin_retracted_since_the_last_one() {
        use crate::plugin_cmd::panel_grant::PanelVerdict;

        let root = std::env::temp_dir().join(format!("stella-panel-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let workspace = root.join("ws");
        std::fs::create_dir_all(&workspace).expect("temp workspace");
        // The user tier, moved wholesale by `test_user_home` — which takes the
        // process-wide env lock, so nothing here reads the developer's own
        // `~/.stella/plugins` and nothing concurrent sees a half-moved home.
        // The project tier would need `STELLA_TRUST_PROJECT` as well, and a
        // retraction that could be confused with that gate is a worse witness.
        let _home = crate::paths::test_user_home(root.join("home"));
        let tier = stella_home::resolve_user_plugins_dir(crate::paths::user_extension_root())
            .expect("the redirected home resolves its plugins tier");
        std::fs::create_dir_all(&tier).expect("user tier");
        let marker = root.join("unused");
        plant_panel(&tier, "alpha", &marker, true, Some(PanelVerdict::Allow));

        let mut plane = PanelPlane::default();
        let first = plane.reseat(&workspace);
        assert_eq!(first.generation, 1);
        let seated: Vec<&str> = first
            .seats
            .iter()
            .map(|seat| seat.plugin.as_str())
            .collect();
        assert_eq!(seated, vec!["alpha"], "the installed panel is seated");

        // The retraction an operator writes by hand, and the whole reason
        // `/reload` exists: withdraw the panel without deleting anything.
        std::fs::create_dir_all(workspace.join(".stella")).expect("settings dir");
        std::fs::write(
            workspace.join(".stella").join("settings.json"),
            "{\"plugins\": {\"alpha\": \"off\"}}",
        )
        .expect("settings");

        let second = plane.reseat(&workspace);
        assert!(
            second.seats.is_empty(),
            "the retracted plugin lost its seat: {:?}",
            second.seats
        );
        assert_eq!(
            second.generation, 2,
            "and the seating moved, so a request raised against the first one is \
             refusable rather than merely out of range"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The reseat witness (#5253).** A frame request minted before a reseat
    /// starts nothing after one — asserted on the *process*, because that is
    /// what the seating generation exists to stop.
    ///
    /// `PanelSlot::settle` already refuses a stale *frame*
    /// (`a_frame_in_flight_across_a_reseat_lands_nowhere`), and that is a
    /// different property one leg later: by the time there is a frame to
    /// refuse, the plugin that now holds the slot index has already been
    /// started, with its environment resolved and its argv run. A slot index is
    /// all a request carries otherwise, and after a reseat slot 0 is somebody
    /// else — so an index-only check cannot tell "the panel you asked for" from
    /// "the panel that inherited its number".
    ///
    /// Two plugins with **separate markers**, so a green run says which program
    /// ran rather than that something did.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_frame_request_from_a_previous_seating_starts_nothing() {
        use crate::plugin_cmd::panel_grant::PanelVerdict;

        let _env = crate::test_env::lock();
        let root = std::env::temp_dir().join(format!("stella-panel-reseat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp root");
        let tier = stella_home::resolve_user_plugins_dir(Some(root.join(".stella")))
            .expect("an explicit stella root resolves its plugins tier");
        let alpha_ran = root.join("alpha-ran");
        let beta_ran = root.join("beta-ran");

        plant_panel(&tier, "alpha", &alpha_ran, true, Some(PanelVerdict::Allow));
        plant_panel(&tier, "beta", &beta_ran, true, Some(PanelVerdict::Allow));

        // Seating 1 holds both, in name order, so `alpha` is slot 0.
        let both = panel_routes_of(&tier);
        assert_eq!(both.len(), 2, "{both:?}");
        assert_eq!(both[0].plugin, "alpha");

        // `alpha` is retracted and the driver reseats: seating 2 holds `beta`
        // alone, which makes `beta` slot 0.
        let plane = PanelPlane::at(2, vec![both[1].clone()]);
        assert_eq!(plane.routes[0].plugin, "beta");

        // The request the deck raised under seating 1, arriving now.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        spawn_tick(&plane, 1, 0, 1, 40, 8, &tx);
        drop(tx);
        let answered = rx.recv().await;

        assert!(
            !beta_ran.exists(),
            "a stale request started the plugin that inherited its slot: {}",
            beta_ran.display()
        );
        assert!(
            !alpha_ran.exists(),
            "and it did not resurrect the retracted one either: {}",
            alpha_ran.display()
        );
        assert!(
            answered.is_none(),
            "a request naming a seating that is gone is answered by nobody, because \
             there is no seat left to rearm: {answered:?}"
        );

        // Anti-vacuity: the same slot, under the seating that is in force, does
        // start `beta` and only `beta`.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        spawn_tick(&plane, 2, 0, 1, 40, 8, &tx);
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
        assert!(beta_ran.exists(), "beta drew under its own seating");
        assert!(!alpha_ran.exists(), "and alpha stayed retracted");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The visible-handshake witness (#5056).** Before any panel is seated,
    /// the session says which plugin is about to draw on the screen and under
    /// what grant — the full SPEC 12.4 document with the `[a]llow [d]eny` ask
    /// where the decision is still open, and the standing grant plus the way to
    /// withdraw it where it is not.
    #[cfg(unix)]
    #[test]
    fn the_first_seat_is_preceded_by_a_handshake_for_every_panel_plugin() {
        use crate::plugin_cmd::panel_grant::PanelVerdict;
        use crate::plugin_cmd::roster::{PluginRoster, PluginScope};

        let _env = crate::test_env::lock();
        let root = std::env::temp_dir().join(format!("stella-panel-shake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp root");
        let tier = stella_home::resolve_user_plugins_dir(Some(root.join(".stella")))
            .expect("an explicit stella root resolves its plugins tier");
        let marker = root.join("unused");

        plant_panel(&tier, "granted", &marker, true, Some(PanelVerdict::Allow));
        plant_panel(&tier, "pending", &marker, true, None);
        // A plugin with no `[panel]` at all, so the block list is about panels
        // rather than about the roster.
        let quiet = tier.join("quiet");
        std::fs::create_dir_all(&quiet).expect("fixture plugin dir");
        let quiet_text = "name = \"quiet\"\n";
        std::fs::write(
            quiet.join(crate::plugin_cmd::roster::MANIFEST_FILE),
            quiet_text,
        )
        .expect("fixture manifest");
        crate::plugin_cmd::receipt::record(
            &tier,
            PluginScope::User,
            "quiet",
            "quiet",
            quiet_text.as_bytes(),
        )
        .expect("fixture receipt");

        let roster = PluginRoster::compose(
            crate::plugin_cmd::roster::read_tier(&tier, PluginScope::User, &mut Vec::new()),
            Vec::new(),
            &std::collections::BTreeMap::new(),
        );
        let blocks = handshakes(&roster);
        assert_eq!(
            blocks.len(),
            2,
            "one block per panel plugin, and none for `quiet`: {blocks:?}"
        );

        // Roster order is name order: `granted`, then `pending`.
        let granted = &blocks[0];
        assert!(granted.contains("◳ panel · granted"), "{granted}");
        assert!(granted.contains("sha256:"), "the signature: {granted}");
        assert!(
            granted.contains("stella plugin panel granted"),
            "and the way to withdraw it: {granted}"
        );
        assert!(
            !granted.contains(stella_plugin::PANEL_GRANT_ASK),
            "a decided grant is not asked again: {granted}"
        );

        let pending = &blocks[1];
        assert!(
            pending.contains(stella_plugin::PANEL_GRANT_ASK),
            "an undecided panel is asked: {pending}"
        );
        assert!(pending.contains("Manifest signature: sha256:"), "{pending}");
        assert!(
            pending.contains("stella plugin panel pending"),
            "and named the verb that answers: {pending}"
        );
        assert!(
            pending.contains("nobody has been asked yet"),
            "and why it is not drawing: {pending}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A denial takes the rectangle and nothing else: the package still loads,
    /// and its hook dispatches are untouched.
    ///
    /// Without this, "deny" could be implemented by dropping the package from
    /// the roster and every assertion above would still pass — while the
    /// operator who wanted to stop a panel drawing had silently lost the
    /// plugin's tools and hooks as well.
    #[cfg(unix)]
    #[test]
    fn a_denied_panel_costs_the_plugin_nothing_but_the_rectangle() {
        use crate::plugin_cmd::panel_grant::PanelVerdict;
        use crate::plugin_cmd::roster::{PluginRoster, PluginScope};

        let _env = crate::test_env::lock();
        let root = std::env::temp_dir().join(format!("stella-panel-kept-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp root");
        let tier = stella_home::resolve_user_plugins_dir(Some(root.join(".stella")))
            .expect("an explicit stella root resolves its plugins tier");

        let dir = tier.join("hooked");
        std::fs::create_dir_all(&dir).expect("fixture plugin dir");
        let text = "name = \"hooked\"\n\
                    \n\
                    [loop]\n\
                    participation = \"steering\"\n\
                    hooks = [\"PreToolUse\"]\n\
                    \n\
                    [runtime]\n\
                    argv = [\"/bin/true\"]\n\
                    timeout_secs = 2\n\
                    env = []\n\
                    \n\
                    [panel]\n\
                    surfaces = [\"settings\"]\n\
                    denies = [\"network\", \"write-outside-sandbox\"]\n\
                    \n\
                    [panel.process]\n\
                    argv = [\"/bin/true\"]\n\
                    timeout_secs = 2\n\
                    env = []\n";
        std::fs::write(dir.join(crate::plugin_cmd::roster::MANIFEST_FILE), text)
            .expect("fixture manifest");
        crate::plugin_cmd::receipt::record(
            &tier,
            PluginScope::User,
            "hooked",
            "hooked",
            text.as_bytes(),
        )
        .expect("fixture receipt");
        crate::plugin_cmd::panel_grant::record(
            &tier,
            PluginScope::User,
            "hooked",
            "hooked",
            text.as_bytes(),
            PanelVerdict::Deny,
        )
        .expect("fixture panel grant");

        let roster = PluginRoster::compose(
            crate::plugin_cmd::roster::read_tier(&tier, PluginScope::User, &mut Vec::new()),
            Vec::new(),
            &std::collections::BTreeMap::new(),
        );
        assert_eq!(roster.plugins().len(), 1, "the package still loads");
        assert_eq!(
            roster.hook_routes().len(),
            1,
            "and its hook dispatch is untouched"
        );
        assert!(
            roster.panel_routes().is_empty(),
            "only the rectangle is gone"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
