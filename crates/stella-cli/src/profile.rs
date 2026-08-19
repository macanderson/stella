//! Harness profiles — one word that re-tunes the engine.
//!
//! A profile answers "how hard should this machine try?" in the vocabulary a
//! person actually thinks in — `fast`, `balanced`, `pro`, `ultra` — and lowers
//! that into the settings the engine already reads: a model, a reasoning
//! effort, and the response-shape knobs that ride along.
//!
//! # One role since #3908
//!
//! It used to lower a profile onto six roles at once, including a three-step
//! cross-family search that kept the verifier independent of the worker. Four
//! of those roles were the staged pipeline's (#3865) and their settings keys
//! had stopped resolving, so the search was spending real ranking effort to
//! write a key nothing read — and `/profile` was printing a per-role table for
//! models that would never run.
//!
//! The independence *idea* is not gone: `stella goal`'s
//! `resolve_cross_family_verifier` still groups by family at the point of use.
//! What went is the attempt to pre-commit that choice in a settings file, for
//! a role core no longer has. A profile that should tune a plugin's seat is
//! #3936.
//!
//! # Why price is the tier signal
//!
//! [`CatalogEntry`](stella_model::catalog::CatalogEntry) carries no tier,
//! rank, or latency field, so a profile has to infer capability from
//! something. It uses **list output price**. That choice is deliberate: a hardcoded table of model names would be stale the week after
//! it merged, whereas price ranking picks up every `stella models refresh`
//! for free. It is a proxy, not a truth — a cheap frontier model ranks low
//! until its price says otherwise — and the confirmation text names the models
//! it chose, so the ranking is never silent.
//!
//! Rows priced at zero are **excluded** from the ranking rather than read as
//! cheapest. A zero rate means "the catalog does not know" (gateway-priced
//! rows like `openrouter/auto` bill at whatever they route to), and treating
//! that as free would hand every `fast` profile a model whose real cost is
//! unknown. They remain a last-resort fallback when nothing else is reachable.
//!
//! # Why the auto switches go off
//!
//! `effort_auto` is a convenience that pins a middle rung and **overrides any
//! per-agent effort**. Left on, it would cap `/profile ultra` at medium — the
//! profile would print one thing and run another. So applying a profile turns
//! `effort_auto`, `reasoning_auto`, and `auto_mode` off and writes the values
//! explicitly. What the confirmation prints is what the next session runs.
//!
//! That would be a one-way door on its own, so [`restore_auto`] is the way
//! back: it switches all three on again and drops the per-role pins, handing
//! the dials to the engine's own ladder.
//!
//! # Effort is clamped to what the provider actually exposes
//!
//! The five-rung ladder is Stella's vocabulary, not every provider's: Gemini
//! and Vertex expose only `low`/`high`, the OpenAI shapes stop at `high`, and
//! Z.ai has no effort knob at all (its thinking switch is on/off). A profile
//! that wrote `max` onto a Gemini pick would be recording a level the request
//! can never express, so each pick is clamped down to the highest rung its own
//! provider supports.
//!
//! Clamping alone would flatten the ladder, though: on a two-rung provider
//! `fast` and `balanced` both land on `low`, and two of the four profiles stop
//! meaning anything. So the missing rung is bought back on the axis that can
//! still carry it — [`at_percentile_expressive`] prefers a model whose provider
//! can express the requested level, among models the same price. It is a
//! tiebreak and only a tiebreak: the search never leaves the price band, so a
//! profile cannot overspend its tier to buy a knob. Where nothing in the band
//! can express it, the clamp stands and the summary says which levels moved.

use crate::settings::{
    AgentEngineAgent, AgentEngineAgents, AgentEngineConfig, AgentEngineParams, Toggle,
};
use stella_protocol::completion::{ReasoningEffort, ServiceTier, Verbosity};

/// The canonical effort ladder, weakest first. Position in this list is what
/// [`clamp_effort`] compares; the per-provider vocabularies from
/// [`crate::engine_config::effort_levels`] are always subsets of it.
const EFFORT_LADDER: [ReasoningEffort; 5] = [
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::Xhigh,
    ReasoningEffort::Max,
];

/// The four profiles, cheapest to most capable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Cheapest reachable model everywhere, thinking off, terse replies.
    Fast,
    /// The daily driver: mid-priced worker, a stronger cross-family verifier,
    /// the cheapest model on triage.
    Balanced,
    /// Near the top of what the keys can reach, with real deliberation.
    Pro,
    /// The most capable model every key can reach, at the ceiling of effort.
    Ultra,
}

impl Profile {
    /// Every profile, cheapest first — the order `/profile` lists them in.
    pub const ALL: [Profile; 4] = [
        Profile::Fast,
        Profile::Balanced,
        Profile::Pro,
        Profile::Ultra,
    ];

    /// Parse a profile name, case-insensitively. `None` is an unknown name;
    /// the caller turns that into usage text rather than a model call.
    pub fn parse(raw: &str) -> Option<Profile> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "fast" => Some(Profile::Fast),
            "balanced" => Some(Profile::Balanced),
            "pro" => Some(Profile::Pro),
            "ultra" => Some(Profile::Ultra),
            _ => None,
        }
    }

    /// The name as typed.
    pub fn name(self) -> &'static str {
        match self {
            Profile::Fast => "fast",
            Profile::Balanced => "balanced",
            Profile::Pro => "pro",
            Profile::Ultra => "ultra",
        }
    }

    /// The one-line "vibe", shown in the menu and the summary.
    pub fn tagline(self) -> &'static str {
        match self {
            Profile::Fast => "cheapest models, thinking off, terse replies",
            Profile::Balanced => "good enough for daily work without burning the budget",
            Profile::Pro => "strong models, real deliberation, priority capacity",
            Profile::Ultra => "the most capable models your keys can reach, effort at the ceiling",
        }
    }

    /// Where in the price-sorted candidate list this role should land, as a
    /// fraction from 0.0 (cheapest reachable) to 1.0 (most expensive).
    fn percentile(self) -> f64 {
        match self {
            Profile::Fast => 0.0,
            Profile::Balanced => 0.50,
            Profile::Pro => 0.85,
            Profile::Ultra => 1.0,
        }
    }

    /// The reasoning effort this profile asks for, before the provider clamp.
    fn effort(self) -> ReasoningEffort {
        match self {
            Profile::Fast => ReasoningEffort::Low,
            Profile::Balanced => ReasoningEffort::Medium,
            Profile::Pro => ReasoningEffort::High,
            Profile::Ultra => ReasoningEffort::Max,
        }
    }

    /// Whether thinking mode is on. `fast` never thinks.
    fn reasoning(self) -> bool {
        !matches!(self, Profile::Fast)
    }

    /// Response detail. `fast` buys latency and cost back by asking for less
    /// prose; `ultra` asks for the fullest answer the provider will give.
    fn verbosity(self) -> Verbosity {
        match self {
            Profile::Fast => Verbosity::Low,
            Profile::Balanced | Profile::Pro => Verbosity::Medium,
            Profile::Ultra => Verbosity::High,
        }
    }

    /// Provider service tier.
    ///
    /// `fast` takes `Default`, not `Flex`: flex capacity is *cheaper but
    /// slower*, and this is the profile that promised low latency. Paying for
    /// `Priority` starts at `pro`, where budget is no longer the binding
    /// constraint.
    fn service_tier(self) -> ServiceTier {
        match self {
            Profile::Fast => ServiceTier::Default,
            Profile::Balanced => ServiceTier::Auto,
            Profile::Pro | Profile::Ultra => ServiceTier::Priority,
        }
    }
}

/// One model the current credentials can actually reach, flattened out of the
/// catalog so the planner never has to touch I/O.
///
/// Deliberately carries no key material: [`candidates`] reads
/// [`crate::config::discover_configured_providers`], which hands back live
/// `ApiKey`s, and drops them here.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Provider id (`"anthropic"`, `"openrouter"`).
    pub provider: String,
    /// Provider-native slug, never re-qualified with the provider (#259).
    pub model: String,
    /// Family, for the verifier's cross-family preference.
    pub family: String,
    /// List output price. Zero means the catalog does not know.
    pub output_usd_per_mtok: f64,
    /// `Some(false)` is a catalog-confirmed "this model cannot think".
    pub supports_reasoning: Option<bool>,
    /// The effort rungs this model's provider actually exposes, from
    /// [`crate::engine_config::effort_levels_for_spec`]. Empty means the
    /// provider has no request-level effort control at all (Z.ai), which is
    /// not the same as being unable to think.
    pub effort_levels: &'static [&'static str],
}

impl Candidate {
    /// The `provider/slug` string the settings file stores.
    pub fn qualified(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }

    /// Whether the catalog knows what this model costs. Unpriced rows are
    /// excluded from ranking — see the module docs.
    fn is_priced(&self) -> bool {
        self.output_usd_per_mtok > 0.0
    }
}

/// Clamp `wanted` down to the strongest rung `levels` actually offers.
///
/// `None` when the provider exposes no effort control at all. Never clamps
/// *up*: a profile asking for `low` on a provider that also offers `max`
/// still gets `low`.
fn clamp_effort(wanted: ReasoningEffort, levels: &[&str]) -> Option<ReasoningEffort> {
    let wanted_rank = EFFORT_LADDER.iter().position(|&e| e == wanted)?;
    EFFORT_LADDER
        .iter()
        .enumerate()
        .filter(|(rank, effort)| {
            *rank <= wanted_rank && levels.contains(&crate::engine_config::effort_to_str(**effort))
        })
        .map(|(_, effort)| *effort)
        // The ladder is weakest-first, so the last survivor is the strongest
        // rung at or below what was asked for.
        .next_back()
}

/// What a profile decided for the session's role.
#[derive(Debug, Clone, PartialEq)]
pub struct RolePick {
    /// The chosen model, or `None` when no candidate was reachable at all —
    /// the role then keeps whatever it already had.
    pub model: Option<Candidate>,
    /// The clamped effort, or `None` when this provider has no effort knob.
    pub effort: Option<ReasoningEffort>,
    /// Thinking on/off, or `None` when the model cannot think at all.
    pub reasoning: Option<bool>,
    /// The effort the profile asked for before the provider clamp — `Some`
    /// only when the clamp actually moved it, so the summary can say why.
    pub effort_downgraded_from: Option<ReasoningEffort>,
}

/// A profile lowered against a concrete set of reachable models.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub profile: Profile,
    /// The session role's pick.
    ///
    /// One, since #3908. A `Vec` rather than a bare `RolePick` because the
    /// entries come back as plugin-declared seats in #3909, and every consumer
    /// here already iterates.
    pub picks: Vec<RolePick>,
    pub verbosity: Verbosity,
    pub service_tier: ServiceTier,
    /// How many distinct models the ranking had to choose from. One (or zero)
    /// means every role landed on the same model and the profiles differ only
    /// by effort — worth saying out loud rather than letting it look broken.
    pub ranked_count: usize,
}

/// Order candidates cheapest-first on list output price, dropping unpriced
/// rows. Ties break on `provider/slug` so a plan is reproducible run to run
/// (catalog install order is not stable enough to rank on).
fn ranked(candidates: &[Candidate]) -> Vec<&Candidate> {
    let mut priced: Vec<&Candidate> = candidates.iter().filter(|c| c.is_priced()).collect();
    priced.sort_by(|a, b| {
        a.output_usd_per_mtok
            .partial_cmp(&b.output_usd_per_mtok)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.qualified().cmp(&b.qualified()))
    });
    priced
}

/// The index at `percentile` of a list of `len` entries.
fn index_at(len: usize, percentile: f64) -> Option<usize> {
    let last = len.checked_sub(1)?;
    // `round` rather than truncate, so 1.0 reaches the final entry and 0.5
    // lands mid-list rather than one below it.
    Some(((last as f64) * percentile.clamp(0.0, 1.0)).round() as usize)
}

/// The entry sitting at `percentile` of a cheapest-first list.
fn at_percentile<'a>(ranked: &[&'a Candidate], percentile: f64) -> Option<&'a Candidate> {
    let index = index_at(ranked.len(), percentile)?;
    ranked
        .get(index.min(ranked.len().saturating_sub(1)))
        .copied()
}

/// How far apart two list prices can be and still count as the same tier.
///
/// List price is a coarse signal, and the gap between neighbouring models in a
/// vendor's line-up is far wider than this. Two models within 10% of each other
/// are indistinguishable as far as price-as-tier can tell, which makes the band
/// safe to reorder inside: [`at_percentile_expressive`] only ever swaps within
/// it, so a tiebreak can never move a pick to a different tier.
const SAME_TIER_PRICE_TOLERANCE: f64 = 0.10;

/// Pick at `percentile`, then prefer a same-tier model whose provider can
/// actually express `wanted` effort.
///
/// Effort is only one of the dials a profile turns, and it is the one most
/// likely to be missing: Gemini and Vertex expose two of the five rungs, and
/// Z.ai exposes none. Picking purely on price lets `/profile ultra` land on a
/// model that cannot go above `high` while an equally-priced sibling could have
/// gone to `max` — and worse, it collapses the ladder, because `fast` and
/// `balanced` both clamp to `low` on a two-rung provider.
///
/// So expressiveness breaks ties, and only ties. The search is confined to
/// [`SAME_TIER_PRICE_TOLERANCE`] around the model price already chose, and it
/// takes the CHEAPEST expressive candidate in that band, so a profile can never
/// overspend its tier to buy a knob.
fn at_percentile_expressive<'a>(
    ranked: &[&'a Candidate],
    percentile: f64,
    wanted: ReasoningEffort,
) -> Option<&'a Candidate> {
    let target = at_percentile(ranked, percentile)?;
    if clamp_effort(wanted, target.effort_levels) == Some(wanted) {
        return Some(target);
    }
    let price = target.output_usd_per_mtok;
    let (lo, hi) = (
        price * (1.0 - SAME_TIER_PRICE_TOLERANCE),
        price * (1.0 + SAME_TIER_PRICE_TOLERANCE),
    );
    ranked
        .iter()
        .copied()
        // `ranked` is cheapest-first, so the first hit is the cheapest way to
        // buy back the missing rung.
        .find(|c| {
            (lo..=hi).contains(&c.output_usd_per_mtok)
                && clamp_effort(wanted, c.effort_levels) == Some(wanted)
        })
        .or(Some(target))
}

/// Lower a profile against the models these credentials can reach.
///
/// Pure: no catalog read, no credential read, no clock. Feed it a synthetic
/// candidate list and the plan is fully determined.
pub fn plan(profile: Profile, candidates: &[Candidate]) -> Plan {
    let ranked_list = ranked(candidates);
    // Every priced row was filtered out (a catalog with no pricing at all, or
    // only gateway rows). Fall back to whatever is reachable, in the order
    // discovery returned it, so a profile still tunes effort.
    let fallback: Vec<&Candidate> = candidates.iter().collect();
    let pool: &[&Candidate] = if ranked_list.is_empty() {
        &fallback
    } else {
        &ranked_list
    };

    let wanted = profile.effort();
    let chosen = at_percentile_expressive(pool, profile.percentile(), wanted);
    let effort = chosen.and_then(|c| clamp_effort(wanted, c.effort_levels));
    // A catalog-confirmed non-reasoning model must not carry a thinking
    // switch: the request path drops it anyway, and writing it into settings
    // would show a state that never reaches the API.
    let can_think = chosen.is_none_or(|c| c.supports_reasoning != Some(false));

    Plan {
        profile,
        picks: vec![RolePick {
            model: chosen.cloned(),
            effort,
            reasoning: can_think.then(|| profile.reasoning()),
            effort_downgraded_from: match effort {
                Some(got) if got != wanted => Some(wanted),
                _ => None,
            },
        }],
        verbosity: profile.verbosity(),
        service_tier: profile.service_tier(),
        ranked_count: pool.len(),
    }
}

/// Write a plan into an existing engine config, in place.
///
/// **Preserves what the profile does not own.** A hand-written prompt, a
/// pinned provider, a chosen temperature or seed all survive: the profile
/// claims the model, effort, reasoning, verbosity, and service tier, and
/// nothing else. It also leaves `allowed_models` alone — silently
/// narrowing the model picker is the kind of surprise that makes a
/// convenience command untrustworthy.
pub fn apply(plan: &Plan, engine: &mut AgentEngineConfig) {
    // The profile is now the authority on effort and model selection; the
    // auto switches would override it. See the module docs.
    engine.auto_mode = Some(Toggle::Off);
    engine.effort_auto = Some(Toggle::Off);
    engine.reasoning_auto = Some(Toggle::Off);

    let agents = engine.agents.get_or_insert_with(AgentEngineAgents::default);
    for pick in &plan.picks {
        if let Some(chosen) = &pick.model {
            engine.default_model = Some(chosen.qualified());
        }
        let agent = agents.default.get_or_insert_with(AgentEngineAgent::default);
        // The flat `default_model` key is the canonical simple form and a
        // per-agent `model` outranks it, so a stale one here would quietly
        // beat the model this profile just chose.
        if pick.model.is_some() {
            agent.model = None;
        }
        agent.effort = pick.effort;
        agent.reasoning = pick
            .reasoning
            .map(|on| if on { Toggle::On } else { Toggle::Off });
        let params = agent.params.get_or_insert_with(AgentEngineParams::default);
        params.verbosity = Some(plan.verbosity);
        params.service_tier = Some(plan.service_tier);
    }
}

/// Hand the intensity dials back to Stella's own ladder — the inverse of
/// [`apply`], and the way out of a profile.
///
/// Applying a profile has to switch `effort_auto`, `reasoning_auto` and
/// `auto_mode` off, or they would override the levels it just chose. Without
/// this that is a one-way door: nothing puts a preference back once a profile
/// has claimed it. This restores all three and drops the per-role effort,
/// thinking, verbosity and service-tier pins a profile writes, so the engine
/// goes back to choosing them (verifier high, worker medium, triage low).
///
/// It deliberately leaves **model** choices alone. The switches are what a
/// profile took away; a model pin may well predate it — set by `/model` or by
/// hand — and clearing that would be destroying something this command never
/// owned.
pub fn restore_auto(engine: &mut AgentEngineConfig) {
    engine.auto_mode = Some(Toggle::On);
    engine.effort_auto = Some(Toggle::On);
    engine.reasoning_auto = Some(Toggle::On);
    let Some(agents) = engine.agents.as_mut() else {
        return;
    };
    let slot = &mut agents.default;
    if let Some(agent) = slot.as_mut() {
        agent.effort = None;
        agent.reasoning = None;
        if let Some(params) = agent.params.as_mut() {
            params.verbosity = None;
            params.service_tier = None;
            // Clearing the last field a profile owned must not leave a
            // `"params": {}` husk behind. Same rule as `settings_from_state`
            // — an emptied object is dropped, so the file stays minimal.
            if *params == AgentEngineParams::default() {
                agent.params = None;
            }
        }
    }
    if slot.as_ref() == Some(&AgentEngineAgent::default()) {
        *slot = None;
    }
    if *agents == AgentEngineAgents::default() {
        engine.agents = None;
    }
}

/// Whether this config has the auto switches on and no effort pinned
/// — the state [`restore_auto`] leaves behind, and the honest answer to "which
/// profile is this?" when none is.
pub fn is_auto(engine: &AgentEngineConfig) -> bool {
    if !(engine.auto_mode_on() && engine.effort_auto_on() && engine.reasoning_auto_on()) {
        return false;
    }
    engine.agents.as_ref().is_none_or(|agents| {
        agents
            .default
            .as_ref()
            .is_none_or(|a| a.effort.is_none() && a.reasoning.is_none())
    })
}

/// Which profile an engine config currently matches, if any.
///
/// Compares against each profile's plan for the SAME candidate set, so the
/// answer stays honest when the reachable models change: a config written by
/// `/profile ultra` before a new key was added no longer matches `ultra` and
/// reads as customized — which is exactly what it is.
pub fn detect(engine: &AgentEngineConfig, candidates: &[Candidate]) -> Option<Profile> {
    Profile::ALL.into_iter().find(|&profile| {
        let mut probe = engine.clone();
        apply(&plan(profile, candidates), &mut probe);
        probe == *engine
    })
}

/// Every model the current credentials can reach, from the merged catalog.
///
/// The credential side is [`crate::config::discover_configured_providers`] —
/// the same chain `Config::load` uses, so a provider is reachable here exactly
/// when a real run could have selected it. The resolved `ApiKey`s it returns
/// are dropped immediately; only provider ids survive into the result.
pub fn candidates() -> Vec<Candidate> {
    let reachable: Vec<String> = crate::config::discover_configured_providers()
        .into_iter()
        .map(|p| p.config.id.to_string())
        .collect();
    stella_model::catalog::Catalog::current()
        .entries()
        .iter()
        .filter(|entry| reachable.iter().any(|id| id == &entry.provider))
        .map(|entry| Candidate {
            provider: entry.provider.clone(),
            model: entry.id.clone(),
            family: entry.family.clone(),
            output_usd_per_mtok: entry.pricing.output_usd_per_mtok,
            supports_reasoning: entry.supports_reasoning,
            effort_levels: crate::engine_config::effort_levels_for_spec(&entry.provider, &entry.id),
        })
        .collect()
}

#[cfg(test)]
mod tests;
