// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The seat plane: which model a session serves each *plugin-declared* role
//! from.
//!
//! `doc:roleless-core` is the target state this implements the first slice of:
//! a core that knows exactly one role name (`default`) and resolves every other
//! one opaquely. Read its invariants before changing anything here — several of
//! them are properties of this file specifically.
//!
//! A session has always had exactly one provider, and every child it spawned —
//! a `delegate` sub-agent, a plugin's `child_turn` — was handed that same
//! provider. So a plugin could describe a process with several participants and
//! get one model for all of them, silently.
//!
//! # The rule this module exists to hold
//!
//! **Core does not know what any seat means.** A seat is identified by a string
//! the *plugin* chose and the *user* assigned a model to. Nothing here matches
//! on `"planner"`, `"reviewer"`, `"witness"` or any other word, and nothing
//! here carries a list of the roles a plugin is allowed to want. The engine's
//! side of a role is exactly two facts: a name it received, and a model the
//! user mapped that name to.
//!
//! That is not tidiness — it is the capability. The moment core enumerates the
//! roles, the set of processes a plugin can express is capped at the set core
//! anticipated, and "a plugin can fundamentally change how a turn runs" becomes
//! "a plugin can rearrange the five stages we already shipped". The staged
//! pipeline was the second kind, and deleting it (#3865) is what freed the
//! socket to be the first.
//!
//! # The key is `<plugin-id>/<role>`, and the host writes the prefix
//!
//! A seat name is always plugin-qualified — `vera/verifier`,
//! `stella-plan/planner` — with no bare form and no precedence ladder
//! (`doc:roleless-core` §8.4). Two properties follow, and both are why the
//! qualification is not merely tidy:
//!
//! - **No silent inheritance.** A bare `planner` assignment would be picked up
//!   by the next plugin that happens to declare a role of that name, handing it
//!   a spending decision made about a different plugin weeks earlier.
//! - **The namespace is non-forgeable.** The plugin declares only its bare local
//!   role name and never writes the prefix; the host applies it. So a hostile
//!   plugin cannot declare `vera/verifier` and capture Vera's assignment.
//!
//! Note this is a rule about the *shape* of the key, not a licence to read it:
//! the lookup below still compares whole strings and never splits one.
//!
//! # Who decides what
//!
//! - **The plugin** declares the roles its process needs, in its manifest. It
//!   is describing a process, not requesting a model: a plan plugin says "my
//!   process has a planner", never "my planner must be GPT-5.5".
//! - **The user** decides whether a declared role runs on a model of its own.
//!   An unassigned seat resolves to the session's model, so a plugin's process
//!   is single-model until someone says otherwise — which means installing a
//!   multi-participant plugin never silently multiplies anyone's bill.
//! - **Core** resolves the name to a provider and forgets. It never
//!   substitutes a default of its own for an unassigned seat, because a
//!   sensible-looking default here is core having an opinion about a role it
//!   was supposed to be ignorant of.
//!
//! # What this deliberately does not do
//!
//! It does not touch [`stella_protocol::ModelCallRole`]. That enum is the
//! *receipt* vocabulary — it groups spend by job for a cost report — and it is
//! closed and core-owned for good reasons. Conflating it with routing is what
//! produced the current state, where `stella-runtime`'s `ChildTurns` maps a
//! fixed table of role words onto it and a plugin naming anything else is
//! refused. Retiring that table, and the four-language contract pinning those
//! words (`scripts/check-role-names.sh`), is #3906 and #3910, under epic
//! #3903.

use std::collections::BTreeMap;
use std::sync::Arc;

use stella_protocol::Provider;

/// The models a session serves named seats from.
///
/// A miss is not an error and must never become one: it means "run this on the
/// session's own model", which is what every child did before seats existed and
/// what an unassigned seat must keep doing.
///
/// Keys are opaque. They are compared, never parsed, and never matched against
/// a literal anywhere in this workspace.
#[derive(Default)]
pub(crate) struct SeatProviders(BTreeMap<String, Arc<dyn Provider>>);

impl SeatProviders {
    /// The empty plane: every seat runs on the session's own model.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self(BTreeMap::new())
    }

    /// Record the adapter serving `seat`, replacing any previous answer.
    pub(crate) fn insert(&mut self, seat: impl Into<String>, provider: Arc<dyn Provider>) {
        self.0.insert(seat.into(), provider);
    }

    /// The adapter serving `seat`, or `None` for "the session's own model".
    #[must_use]
    pub(crate) fn get(&self, seat: &str) -> Option<&Arc<dyn Provider>> {
        self.0.get(seat)
    }

    /// The seat names that resolved to a model of their own.
    pub(crate) fn seats(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }
}

impl std::fmt::Debug for SeatProviders {
    /// Names the seats and deliberately not the adapters: a [`Provider`] has no
    /// `Debug`, and printing one would put a credential's neighbourhood in a
    /// log line nobody audited.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.seats()).finish()
    }
}

/// Build the adapters for every seat the user has assigned a model to.
///
/// `assignments` is the user's map from an opaque seat name to a model string,
/// in `--model` spelling (`provider/slug`, or a bare seeded slug). Every entry
/// that resolves gets its own adapter; every entry that does not is **skipped
/// with a returned notice**, never guessed at and never fatal:
///
/// - a model string naming no configured provider — the user assigned a seat a
///   model they have no key for,
/// - a provider whose credential did not discover,
/// - an adapter that failed to build.
///
/// Under [`EnginePosture::Configured`] a skipped seat falls back to the
/// session's model, which is the same answer an unassigned seat gets. That is
/// deliberate and it is the safe direction *for a person at a terminal*: the
/// alternative — refusing the session because one seat could not be built —
/// would let a stale line in a settings file stop a user working, and the
/// alternative to *that* — silently substituting some other model — would
/// spend money on a model nobody chose. Saying so out loud and carrying on is
/// the third option, and the notices are why the caller can say it.
///
/// Under [`EnginePosture::Trusted`] the same degradation is **wrong**, and the
/// run is refused instead. See that variant for why.
///
/// Returns the plane plus one human-readable notice per skipped seat.
pub(crate) fn resolve_seat_models(
    assignments: &BTreeMap<String, String>,
    configured: &[crate::config::ConfiguredProvider],
    posture: EnginePosture,
) -> Result<(SeatProviders, Vec<String>), String> {
    let is_provider = |id: &str| configured.iter().any(|c| c.config.id == id);

    let mut seats = SeatProviders::new();
    let mut notices = Vec::new();

    for (seat, requested) in assignments {
        let Some(spec) = crate::engine_config::parse_model_spec(requested, &is_provider) else {
            notices.push(format!(
                "seat `{seat}`: `{requested}` names no configured provider — this seat runs on \
                 the session's model. Configure that provider's key, or correct the model string."
            ));
            continue;
        };
        let Some(entry) = configured.iter().find(|c| c.config.id == spec.provider) else {
            notices.push(format!(
                "seat `{seat}`: no credential resolved for provider `{}` — this seat runs on the \
                 session's model.",
                spec.provider
            ));
            continue;
        };
        match crate::agent::build_provider_parts(
            &entry.config,
            &spec.model,
            entry.api_key.clone(),
            entry.config.base_url.to_string(),
            None,
            entry.aux.clone(),
            // A seat's calls land in bursts within a run; the 5-minute window
            // is the right ask regardless of surface (#1839).
            stella_model::CacheTtl::default(),
        ) {
            Ok(provider) => seats.insert(seat.clone(), Arc::from(provider)),
            Err(error) => notices.push(format!(
                "seat `{seat}`: could not build `{}/{}` ({error}) — this seat runs on the \
                 session's model.",
                spec.provider, spec.model
            )),
        }
    }

    if posture == EnginePosture::Trusted && !notices.is_empty() {
        return Err(format!(
            "the engine config was supplied by the trusted launcher seam, so a seat it pins is \
             a published claim and may not quietly ride the session's model:\n  {}",
            notices.join("\n  ")
        ));
    }

    Ok((seats, notices))
}

/// Where the engine config this session runs under came from.
///
/// The distinction decides what an unbuildable seat costs, and it is the whole
/// of #1147. A seat that cannot be built normally falls back to the session's
/// model with a notice — right for a person at a terminal, whose alternative
/// is not working at all.
///
/// It is wrong for the trusted launcher seam. That seam replaces the engine
/// config ATOMICALLY, and its contract is that the object is the frozen,
/// disclosed system under test: a benchmark arm publishes its hash and then
/// reports numbers against it. A seat the object pins is therefore a PUBLISHED
/// claim, and degrading it produces a number that misdescribes the posture —
/// which is worse than a crash, because nothing about the run looks wrong. So
/// the run is refused and nobody publishes anything.
///
/// The refusal itself is not new. It lived in `stella-pipeline` against that
/// crate's verifier pin, went with the crate in #3865, and left
/// `Config::engine_settings_trusted` written by two call sites, read by none,
/// and still carrying a doc comment promising it (#3937). The pinned role it
/// used to protect is gone (#3908); the published-claim property is not, and
/// `seat_models` is where it lands now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnginePosture {
    /// The engine config came from the settings scope chain — an ordinary
    /// session, whose seats degrade with a notice.
    Configured,
    /// The engine config came from the trusted launcher seam
    /// (`STELLA_ENGINE_CONFIG_JSON`), whose seats may not degrade.
    Trusted,
}

impl EnginePosture {
    /// The posture a resolved [`crate::config::Config`] is running under.
    pub(crate) fn of(cfg: &crate::config::Config) -> Self {
        if cfg.engine_settings_trusted {
            Self::Trusted
        } else {
            Self::Configured
        }
    }
}

/// One installed plugin's declared roles, as the seat list needs them.
///
/// Deliberately not a `PluginManifest`: what the seat list needs is a name and
/// the role names, and taking the whole manifest would let a later edit reach
/// for a field — a tier, a grade, a participation level — and make the seat
/// list depend on something that is not the user's assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeclaredSeats {
    /// The plugin's name — its identity, the namespace half of every key it
    /// contributes, and the word the "from" column shows.
    ///
    /// One field rather than a separate id and display name, because a
    /// `PluginManifest` carries one `name` and `Roster::get` looks a plugin up
    /// by it. Splitting it here would be inventing a distinction the manifest
    /// does not make.
    ///
    /// **The plugin never writes the qualified key**; the caller supplies this
    /// from the install record and [`declared_seats`] joins it, which is what
    /// makes the namespace unforgeable (`doc:roleless-core` §8.4).
    pub plugin: String,
    /// The bare role names from the manifest's `[roles.<name>]` keys.
    pub roles: Vec<String>,
}

/// Every seat an installed set declares: `(key, plugin display name)`, ordered.
///
/// Ordered by `(plugin_id, role)` rather than by the order plugins happened to
/// be installed, for the reason `doc:roleless-core` §8.3 gives for stage order:
/// install order is machine-local and not reproducible across clones, so a
/// surface that reads in that order shows two people different screens for the
/// same configuration.
///
/// A duplicate key cannot arise from two plugins — the id is in the key — but
/// can from one plugin declaring the same role twice, which a manifest map
/// makes impossible and this deduplicates anyway rather than rendering a row
/// the user cannot tell apart from the one above it.
#[must_use]
pub(crate) fn declared_seats(plugins: &[DeclaredSeats]) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = plugins
        .iter()
        .flat_map(|plugin| {
            plugin
                .roles
                .iter()
                .map(move |role| (format!("{}/{}", plugin.plugin, role), plugin.plugin.clone()))
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows.dedup_by(|a, b| a.0 == b.0);
    rows
}

/// The seats every installed plugin declares, read from the roster.
///
/// The one place this crate turns "what is installed" into "what can be
/// assigned a model". Returns `(seat key, plugin name)` — the key already
/// joined, because the deck must never split one back apart to find out where
/// it came from (`doc:roleless-core` §8.4; the deck's own rule in
/// `stella_tui::envelope::SeatRow`).
///
/// A plugin with no `[roles]` block contributes nothing rather than an empty
/// group: it has not asked for a participant, so there is nothing to assign a
/// model to. Roster load notices are dropped here on purpose — this is a
/// read for a settings pane, and the install and run paths already surface
/// them where a human can act (the reasoning `plugin_cmd::package`'s
/// `session_roster` gives for the same drop).
///
/// Loads `Settings` itself rather than taking one, for that function's reason:
/// the trust gate lives inside `PluginRoster::load`, so every caller reaching
/// the roster through the same two arguments is what keeps a cloned
/// repository's plugins from loading on one path and not another (#3509).
#[must_use]
pub(crate) fn installed_seats(workspace_root: &std::path::Path) -> Vec<(String, String)> {
    let settings = crate::settings::Settings::load(workspace_root).unwrap_or_default();
    let (roster, _notices) =
        crate::plugin_cmd::roster::PluginRoster::load(workspace_root, &settings);
    let declared: Vec<DeclaredSeats> = roster
        .plugins()
        .iter()
        .filter_map(|installed| {
            let roles = installed.manifest.roles.as_ref()?;
            Some(DeclaredSeats {
                plugin: installed.manifest.name.clone(),
                roles: roles.keys().cloned().collect(),
            })
        })
        .collect();
    declared_seats(&declared)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plane is a lookup and nothing more. This is the property that keeps
    /// core ignorant: a name it has never seen resolves exactly as well as one
    /// it has, because it never had a list to check against.
    #[test]
    fn any_name_at_all_is_a_valid_seat() {
        let mut seats = SeatProviders::new();
        assert!(seats.get("planner").is_none());
        assert!(seats.get("wholly-invented-role").is_none());
        assert_eq!(seats.seats().count(), 0);

        // Inserting under an arbitrary name and reading it back is the entire
        // contract; nothing validates, normalizes or rejects the string.
        let named: Vec<&str> = {
            seats.insert("second-opinion", crate::agent::seats::tests::stub());
            seats.seats().collect()
        };
        assert_eq!(named, ["second-opinion"]);
        assert!(seats.get("second-opinion").is_some());
    }

    /// An unassigned seat is a miss, and a miss means the session's model.
    /// Core must not invent a default for a role it does not understand.
    #[test]
    fn no_assignments_resolves_no_seats() {
        let (seats, notices) =
            resolve_seat_models(&BTreeMap::new(), &[], EnginePosture::Configured).unwrap();
        assert_eq!(seats.seats().count(), 0, "resolved: {seats:?}");
        assert!(notices.is_empty(), "{notices:?}");
    }

    /// A seat assigned a model the user has no key for is skipped **and said
    /// out loud**. Silence here would be a settings line that reads like a
    /// capability and is not one.
    #[test]
    fn an_unbuildable_seat_is_skipped_with_a_notice() {
        let assignments = BTreeMap::from([(
            "planner".to_string(),
            "no-such-provider/no-such-model".to_string(),
        )]);
        let (seats, notices) =
            resolve_seat_models(&assignments, &[], EnginePosture::Configured).unwrap();
        assert_eq!(seats.seats().count(), 0);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].contains("planner"), "{notices:?}");
        assert!(
            notices[0].contains("session's model"),
            "the notice must say what happens instead: {notices:?}"
        );
    }

    /// **Witness (#1147, #3937).** The same unbuildable seat that degrades for
    /// an ordinary session **refuses** one whose engine config came from the
    /// trusted launcher seam.
    ///
    /// Both arms in one test, deliberately: the degrading arm is what makes
    /// the refusing arm evidence of a decision rather than of a broken
    /// resolver. Fails on the base commit at the signature —
    /// `resolve_seat_models` had no posture to be told and no way to refuse,
    /// which is why `Config::engine_settings_trusted` was written twice, read
    /// nowhere, and still carrying a doc comment promising this.
    #[test]
    fn a_trusted_posture_refuses_the_seat_an_ordinary_session_degrades() {
        let assignments = BTreeMap::from([(
            "planner".to_string(),
            "no-such-provider/no-such-model".to_string(),
        )]);

        // The control: an ordinary session carries on, on the session's model.
        let (_, notices) =
            resolve_seat_models(&assignments, &[], EnginePosture::Configured).unwrap();
        assert_eq!(notices.len(), 1, "premise: this seat degrades: {notices:?}");

        let refusal = resolve_seat_models(&assignments, &[], EnginePosture::Trusted)
            .expect_err("a pinned seat that cannot be built must refuse the run");
        assert!(
            refusal.contains("planner"),
            "the refusal names the seat: {refusal}"
        );
        assert!(
            refusal.contains("published claim"),
            "and says why a frozen posture may not degrade: {refusal}"
        );
    }

    fn declared(plugin: &str, roles: &[&str]) -> DeclaredSeats {
        DeclaredSeats {
            plugin: plugin.to_string(),
            roles: roles.iter().map(|r| (*r).to_string()).collect(),
        }
    }

    /// The host builds the key, so the namespace is the install record's and
    /// not the plugin's. Two plugins declaring the same bare role name get two
    /// distinct seats — which is the whole reason §8.4 rejected bare names.
    #[test]
    fn two_plugins_declaring_the_same_role_get_two_seats() {
        let seats = declared_seats(&[
            declared("stella-plan", &["planner"]),
            declared("acme-plan", &["planner"]),
        ]);
        let keys: Vec<&str> = seats.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["acme-plan/planner", "stella-plan/planner"]);
    }

    /// Ordered by key, never by install order: install order is machine-local,
    /// so ordering by it would show two people different screens for the same
    /// configuration (`doc:roleless-core` §8.3's reasoning, applied to a list).
    #[test]
    fn seats_order_by_key_not_by_install_order() {
        let installed_one_way =
            declared_seats(&[declared("zulu", &["b", "a"]), declared("alpha", &["z"])]);
        let installed_the_other_way =
            declared_seats(&[declared("alpha", &["z"]), declared("zulu", &["a", "b"])]);
        assert_eq!(installed_one_way, installed_the_other_way);
        assert_eq!(
            installed_one_way
                .iter()
                .map(|(k, _)| k.as_str())
                .collect::<Vec<_>>(),
            ["alpha/z", "zulu/a", "zulu/b"]
        );
    }

    /// No plugins, or plugins declaring no roles, is an empty list — not a row
    /// apologising for itself. The pane renders the default model and nothing
    /// more.
    #[test]
    fn nothing_declared_is_no_seats() {
        assert!(declared_seats(&[]).is_empty());
        assert!(declared_seats(&[declared("quiet", &[])]).is_empty());
    }

    /// A `Provider` stand-in for the lookup tests, which never make a call.
    pub(super) fn stub() -> Arc<dyn Provider> {
        Arc::new(StubProvider)
    }

    struct StubProvider;

    #[async_trait::async_trait]
    impl Provider for StubProvider {
        fn id(&self) -> &str {
            "stub"
        }

        async fn complete_ref(
            &self,
            _req: stella_protocol::CompletionRequestRef<'_>,
        ) -> Result<stella_protocol::CompletionResult, stella_protocol::ProviderError> {
            unreachable!("the seat lookup tests never make a model call")
        }
    }
}
