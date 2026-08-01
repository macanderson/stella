//! Harness profiles — one word that re-tunes every engine role at once.
//!
//! A profile answers "how hard should this machine try?" in the vocabulary a
//! person actually thinks in — `fast`, `balanced`, `pro`, `ultra` — and lowers
//! that into the settings the engine already reads: a model per role, a
//! reasoning effort per role, and the response-shape knobs that ride along.
//!
//! # Why price is the tier signal
//!
//! [`CatalogEntry`](stella_model::catalog::CatalogEntry) carries no tier,
//! rank, or latency field, so a profile has to infer capability from
//! something. It uses **list output price**, the same proxy
//! [`crate::engine_config::auto_judge_spec`] already ranks by. That choice is
//! deliberate: a hardcoded table of model names would be stale the week after
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
//! `effort_auto` is a convenience that pins judge=high, worker=medium,
//! triage=low and **overrides any per-agent effort**. Left on, it would cap
//! `/profile ultra`'s worker at medium — the profile would print one thing and
//! run another. So applying a profile turns `effort_auto`, `reasoning_auto`,
//! and `auto_mode` off and writes the values explicitly. What the confirmation
//! prints is what the next session runs.
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
    AgentEngineAgent, AgentEngineAgents, AgentEngineConfig, AgentEngineParams, EngineAgentKind,
    Toggle,
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
    /// The daily driver: mid-priced worker, a stronger cross-family judge,
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
    ///
    /// Triage sits below the worker in every profile: it is a short
    /// classification under a latency ceiling, and spending the session's most
    /// expensive model on it buys nothing.
    ///
    /// The judge's fraction is measured over a **filtered** list — see
    /// [`plan`] — so it reads as "how far up the qualifying judges to reach",
    /// not as a position in the full catalog.
    fn percentile(self, kind: EngineAgentKind) -> f64 {
        match (self, kind) {
            (Profile::Fast, _) => 0.0,
            (Profile::Balanced, EngineAgentKind::Triage) => 0.0,
            (Profile::Balanced, EngineAgentKind::Judge) => 0.75,
            (Profile::Balanced, _) => 0.50,
            (Profile::Pro, EngineAgentKind::Triage) => 0.25,
            (Profile::Pro, EngineAgentKind::Judge) => 1.0,
            (Profile::Pro, _) => 0.85,
            (Profile::Ultra, EngineAgentKind::Triage) => 0.50,
            (Profile::Ultra, _) => 1.0,
        }
    }

    /// The reasoning effort this profile asks for, before the provider clamp.
    fn effort(self, kind: EngineAgentKind) -> ReasoningEffort {
        match (self, kind) {
            (Profile::Fast, _) => ReasoningEffort::Low,
            (Profile::Balanced, EngineAgentKind::Triage) => ReasoningEffort::Low,
            (Profile::Balanced, EngineAgentKind::Judge) => ReasoningEffort::High,
            (Profile::Balanced, _) => ReasoningEffort::Medium,
            (Profile::Pro, EngineAgentKind::Triage) => ReasoningEffort::Low,
            (Profile::Pro, EngineAgentKind::Judge) => ReasoningEffort::Xhigh,
            (Profile::Pro, _) => ReasoningEffort::High,
            (Profile::Ultra, EngineAgentKind::Triage) => ReasoningEffort::Medium,
            (Profile::Ultra, _) => ReasoningEffort::Max,
        }
    }

    /// Whether thinking mode is on for a role. `fast` never thinks; triage
    /// only thinks under `ultra`.
    fn reasoning(self, kind: EngineAgentKind) -> bool {
        match (self, kind) {
            (Profile::Fast, _) => false,
            (Profile::Ultra, _) => true,
            (_, EngineAgentKind::Triage) => false,
            _ => true,
        }
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
    /// Family, for the judge's cross-family preference.
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

/// What a profile decided for one role.
#[derive(Debug, Clone, PartialEq)]
pub struct RolePick {
    pub kind: EngineAgentKind,
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
    /// One entry per [`EngineAgentKind`], in [`EngineAgentKind::ALL`] order.
    pub picks: Vec<RolePick>,
    pub verbosity: Verbosity,
    pub service_tier: ServiceTier,
    /// How many distinct models the ranking had to choose from. One (or zero)
    /// means every role landed on the same model and the profiles differ only
    /// by effort — worth saying out loud rather than letting it look broken.
    pub ranked_count: usize,
    /// `true` when the judge had to share the worker's model family because no
    /// reachable model was both independent of it and at least as capable. The
    /// review is then correlated with the work it grades; the summary reports
    /// it rather than letting the compromise pass unseen.
    pub judge_shares_worker_family: bool,
}

impl Plan {
    /// The pick for one role.
    pub fn pick(&self, kind: EngineAgentKind) -> Option<&RolePick> {
        self.picks.iter().find(|p| p.kind == kind)
    }
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

    // What the worker landed on decides the judge, so resolve it first.
    let worker_index = index_at(pool.len(), profile.percentile(EngineAgentKind::Worker));
    let worker = worker_index.and_then(|i| pool.get(i).copied());
    let worker_family = worker.map(|c| c.family.as_str()).unwrap_or_default();
    let worker_price = worker.map(|c| c.output_usd_per_mtok).unwrap_or_default();

    // Choosing the judge.
    //
    // Two goals pull against each other. A judge in the worker's own family
    // lets a shared blind spot pass review twice; a judge well below the
    // worker's tier cannot follow the work it grades, and rubber-stamps. No
    // single ordering satisfies both on every catalog, so this resolves in
    // three steps, preferring the option that gives up the least:
    //
    //   1. Independent AND at least as capable — no compromise at all.
    //   2. Nothing clears the floor, but the strongest independent model is
    //      only one rung below the worker. That is a near miss, not an
    //      outmatched judge, so independence is worth the single rung.
    //   3. The strongest independent model is further down than that. Now
    //      capability wins, and `Plan` records the correlation so the summary
    //      says it out loud instead of absorbing it silently.
    //
    // Step 2 is what separates this from a plain capability floor: losing bias
    // resistance over a rounding-error price gap was overcorrection. Anyone who
    // wants independence unconditionally still has `auto_mode`.
    let cross_family: Vec<&Candidate> = pool
        .iter()
        .copied()
        .filter(|c| c.family != worker_family)
        .collect();
    let at_or_above: Vec<&Candidate> = cross_family
        .iter()
        .copied()
        .filter(|c| c.output_usd_per_mtok >= worker_price)
        .collect();
    // `pool` is cheapest-first, so the last cross-family entry is the strongest
    // independent option, and its rank decides whether the gap is a near miss.
    let near_miss = at_or_above.is_empty()
        && match (worker_index, cross_family.last()) {
            (Some(worker_index), Some(best)) => pool
                .iter()
                .rposition(|c| std::ptr::eq(*c, *best))
                .is_some_and(|rank| rank + 1 >= worker_index),
            _ => false,
        };
    // Steps 1 and 2 both end with an independent judge; only step 3 gives that
    // up, and that is the case worth reporting.
    let judge_is_independent = !at_or_above.is_empty() || near_miss;

    let picks = EngineAgentKind::ALL
        .iter()
        .map(|&kind| {
            let wanted = profile.effort(kind);
            // The judge's three steps, in order; every other role just takes
            // its fraction of the whole pool.
            let chosen = match kind {
                EngineAgentKind::Judge if !at_or_above.is_empty() => {
                    at_percentile_expressive(&at_or_above, profile.percentile(kind), wanted)
                }
                // A near-miss judge was chosen for being the strongest
                // independent option there is, so it takes the top of that list
                // rather than the profile's usual fraction.
                EngineAgentKind::Judge if near_miss => cross_family.last().copied(),
                _ => at_percentile_expressive(pool, profile.percentile(kind), wanted),
            };
            let effort = chosen.and_then(|c| clamp_effort(wanted, c.effort_levels));
            // A catalog-confirmed non-reasoning model must not carry a
            // thinking switch: the request path drops it anyway, and writing
            // it into settings would show a state that never reaches the API.
            let can_think = chosen.is_none_or(|c| c.supports_reasoning != Some(false));
            RolePick {
                kind,
                model: chosen.cloned(),
                effort,
                reasoning: can_think.then(|| profile.reasoning(kind)),
                effort_downgraded_from: match effort {
                    Some(got) if got != wanted => Some(wanted),
                    _ => None,
                },
            }
        })
        .collect();

    Plan {
        profile,
        picks,
        verbosity: profile.verbosity(),
        service_tier: profile.service_tier(),
        ranked_count: pool.len(),
        // Only a compromise when there was a worker to be independent OF, and
        // when no independent option survived either of the first two steps.
        judge_shares_worker_family: !judge_is_independent && worker.is_some(),
    }
}

/// Write a plan into an existing engine config, in place.
///
/// **Preserves what the profile does not own.** A hand-written judge prompt, a
/// pinned provider, a chosen temperature or seed all survive: the profile
/// claims the per-role model, effort, reasoning, verbosity, and service tier,
/// and nothing else. It also leaves `allowed_models` alone — silently
/// narrowing the model picker is the kind of surprise that makes a
/// convenience command untrustworthy.
pub fn apply(plan: &Plan, engine: &mut AgentEngineConfig) {
    // The profile is now the authority on effort and judge selection; the
    // auto switches would override it. See the module docs.
    engine.auto_mode = Some(Toggle::Off);
    engine.effort_auto = Some(Toggle::Off);
    engine.reasoning_auto = Some(Toggle::Off);

    let agents = engine.agents.get_or_insert_with(AgentEngineAgents::default);
    for pick in &plan.picks {
        if let Some(chosen) = &pick.model {
            let qualified = Some(chosen.qualified());
            match pick.kind {
                EngineAgentKind::Default => engine.default_model = qualified,
                EngineAgentKind::Worker => engine.pipeline_worker_model = qualified,
                EngineAgentKind::Judge => engine.pipeline_judge_model = qualified,
                EngineAgentKind::Triage => engine.pipeline_triage_model = qualified,
            }
        }
        let agent = agents
            .get_mut(pick.kind)
            .get_or_insert_with(AgentEngineAgent::default);
        // The flat `pipeline_*_model` keys are the canonical simple form and a
        // per-agent `model` outranks them, so a stale one here would quietly
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
/// goes back to choosing them (judge high, worker medium, triage low).
///
/// It deliberately leaves **model** choices alone. The switches are what a
/// profile took away; a model pin may well predate it — set by `/model` or by
/// hand — and clearing that would be destroying something this command never
/// owned. `auto_mode` re-picks the judge on its own regardless.
pub fn restore_auto(engine: &mut AgentEngineConfig) {
    engine.auto_mode = Some(Toggle::On);
    engine.effort_auto = Some(Toggle::On);
    engine.reasoning_auto = Some(Toggle::On);
    let Some(agents) = engine.agents.as_mut() else {
        return;
    };
    for kind in EngineAgentKind::ALL {
        let slot = agents.get_mut(kind);
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
    }
    let no_agents_left = *agents == AgentEngineAgents::default();
    if no_agents_left {
        engine.agents = None;
    }
}

/// Whether this config has the auto switches on and no per-role effort pinned
/// — the state [`restore_auto`] leaves behind, and the honest answer to "which
/// profile is this?" when none is.
pub fn is_auto(engine: &AgentEngineConfig) -> bool {
    if !(engine.auto_mode_on() && engine.effort_auto_on() && engine.reasoning_auto_on()) {
        return false;
    }
    engine.agents.as_ref().is_none_or(|agents| {
        EngineAgentKind::ALL.iter().all(|&kind| {
            agents
                .get(kind)
                .is_none_or(|a| a.effort.is_none() && a.reasoning.is_none())
        })
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
