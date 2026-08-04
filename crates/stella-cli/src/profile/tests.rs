//! Profile planner tests.
//!
//! Every test here feeds [`plan`] a synthetic candidate list rather than the
//! real catalog: the planner is pure by construction, so a test never needs a
//! credential, a network call, or a particular `stella models refresh` state.

use super::*;

/// The five-rung ladder, for providers that expose all of it.
const FULL: &[&str] = &["low", "medium", "high", "xhigh", "max"];
/// What Gemini and Vertex expose.
const LOW_HIGH: &[&str] = &["low", "high"];
/// What the OpenAI shapes expose.
const OPENAI: &[&str] = &["low", "medium", "high"];

fn candidate(provider: &str, model: &str, family: &str, price: f64) -> Candidate {
    Candidate {
        provider: provider.to_string(),
        model: model.to_string(),
        family: family.to_string(),
        output_usd_per_mtok: price,
        supports_reasoning: Some(true),
        effort_levels: FULL,
    }
}

/// A spread of five models across four families, cheapest to priciest. The
/// two same-family entries sit at opposite ends on purpose, so the verifier's
/// independence rule has to reach past one of them.
fn spread() -> Vec<Candidate> {
    vec![
        candidate("deepseek", "deepseek-chat", "deepseek", 1.0),
        candidate("zai", "glm-5.2", "glm", 5.0),
        candidate("anthropic", "claude-sonnet-5", "anthropic", 15.0),
        candidate("openai", "gpt-5.5", "gpt", 40.0),
        candidate("anthropic", "claude-fable-5", "anthropic", 75.0),
    ]
}

fn model_of(plan: &Plan, kind: EngineAgentKind) -> String {
    plan.pick(kind)
        .and_then(|p| p.model.as_ref())
        .map(Candidate::qualified)
        .unwrap_or_default()
}

#[test]
fn profile_names_round_trip_and_reject_anything_else() {
    for profile in Profile::ALL {
        assert_eq!(Profile::parse(profile.name()), Some(profile));
    }
    // Case-insensitive, whitespace-tolerant — a user typing `/profile Ultra`
    // means the same thing.
    assert_eq!(Profile::parse("  ULTRA "), Some(Profile::Ultra));
    assert_eq!(Profile::parse("turbo"), None);
    assert_eq!(Profile::parse(""), None);
}

#[test]
fn the_ladder_runs_cheapest_to_most_capable() {
    let models = spread();
    let picks: Vec<f64> = Profile::ALL
        .into_iter()
        .map(|p| {
            plan(p, &models)
                .pick(EngineAgentKind::Default)
                .and_then(|pick| pick.model.as_ref())
                .map(|c| c.output_usd_per_mtok)
                .unwrap_or_default()
        })
        .collect();
    // Monotonically non-decreasing across fast → balanced → pro → ultra.
    assert!(
        picks.windows(2).all(|w| w[0] <= w[1]),
        "profile ladder is not monotonic in price: {picks:?}"
    );
    assert_eq!(picks[0], 1.0, "fast should take the cheapest model");
    assert_eq!(picks[3], 75.0, "ultra should take the priciest model");
}

/// Where a role's pick sits in the price-sorted candidate list.
fn rank_of(models: &[Candidate], plan: &Plan, kind: EngineAgentKind) -> usize {
    let mut prices: Vec<f64> = models.iter().map(|c| c.output_usd_per_mtok).collect();
    prices.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let price = plan
        .pick(kind)
        .and_then(|p| p.model.as_ref())
        .map(|c| c.output_usd_per_mtok)
        .unwrap();
    prices.iter().position(|p| *p == price).unwrap()
}

#[test]
fn triage_never_outspends_the_worker_and_the_verifier_stays_within_one_rung() {
    for profile in Profile::ALL {
        let models = spread();
        let p = plan(profile, &models);
        let price = |kind| {
            p.pick(kind)
                .and_then(|pick| pick.model.as_ref())
                .map(|c| c.output_usd_per_mtok)
                .unwrap_or_default()
        };
        assert!(
            price(EngineAgentKind::Triage) <= price(EngineAgentKind::Worker),
            "{}: triage outspent the worker",
            profile.name()
        );
        // The verifier may sit one rung below the worker, and only one — that is
        // the price of keeping it independent when nothing stronger is. Two
        // rungs down is an outmatched verifier, and capability takes over.
        let worker = rank_of(&models, &p, EngineAgentKind::Worker);
        let verifier = rank_of(&models, &p, EngineAgentKind::Verifier);
        assert!(
            verifier + 1 >= worker,
            "{}: verifier sat {} rungs below the worker",
            profile.name(),
            worker - verifier
        );
    }
}

#[test]
fn the_verifier_prefers_a_family_the_worker_is_not_in() {
    // Balanced puts the worker on `anthropic/claude-sonnet-5`; the only model
    // that is both stronger and from another family is `openai/gpt-5.5`.
    let p = plan(Profile::Balanced, &spread());
    let worker = p.pick(EngineAgentKind::Worker).unwrap();
    let verifier = p.pick(EngineAgentKind::Verifier).unwrap();
    assert_ne!(
        worker.model.as_ref().unwrap().family,
        verifier.model.as_ref().unwrap().family,
        "a verifier in the worker's own family lets a shared blind spot pass twice"
    );
    assert!(!p.verifier_shares_worker_family);
}

#[test]
fn a_verifier_one_rung_below_the_worker_keeps_its_independence() {
    // Ultra puts the worker on the priciest model, so nothing is both stronger
    // AND independent. The strongest independent model is one rung down, which
    // is a near miss rather than an outmatched verifier — losing bias resistance
    // over a single rung would be overcorrection.
    let models = spread();
    let p = plan(Profile::Ultra, &models);
    assert_eq!(
        model_of(&p, EngineAgentKind::Worker),
        "anthropic/claude-fable-5"
    );
    assert_eq!(model_of(&p, EngineAgentKind::Verifier), "openai/gpt-5.5");
    assert_ne!(
        p.pick(EngineAgentKind::Worker)
            .unwrap()
            .model
            .as_ref()
            .unwrap()
            .family,
        p.pick(EngineAgentKind::Verifier)
            .unwrap()
            .model
            .as_ref()
            .unwrap()
            .family
    );
    assert!(
        !p.verifier_shares_worker_family,
        "independence was kept, so there is no compromise to report"
    );
}

#[test]
fn a_verifier_more_than_one_rung_below_loses_to_capability_and_is_recorded() {
    // One cheap outsider and a wall of same-family models. The only
    // independent option is three rungs down — far enough that it could not
    // follow the work — so capability takes over and the correlation is
    // reported rather than absorbed.
    let models = vec![
        candidate("deepseek", "deepseek-chat", "deepseek", 1.0),
        candidate("anthropic", "claude-a", "anthropic", 15.0),
        candidate("anthropic", "claude-b", "anthropic", 40.0),
        candidate("anthropic", "claude-c", "anthropic", 75.0),
    ];
    let p = plan(Profile::Ultra, &models);
    assert_eq!(model_of(&p, EngineAgentKind::Worker), "anthropic/claude-c");
    assert_eq!(model_of(&p, EngineAgentKind::Verifier), "anthropic/claude-c");
    assert!(
        p.verifier_shares_worker_family,
        "a correlated verifier must be reported, not absorbed"
    );
}

#[test]
fn a_single_reachable_family_still_yields_a_verifier() {
    // One provider, one family — the cross-family preference has nothing to
    // reach for and must not leave the verifier unset.
    let only = vec![candidate("anthropic", "claude-fable-5", "anthropic", 75.0)];
    let p = plan(Profile::Ultra, &only);
    assert_eq!(
        model_of(&p, EngineAgentKind::Verifier),
        "anthropic/claude-fable-5"
    );
    assert_eq!(p.ranked_count, 1);
}

#[test]
fn unpriced_rows_are_excluded_rather_than_read_as_free() {
    let mut models = spread();
    // A gateway row that bills at whatever it routes to. Reading its zero as
    // "cheapest" would hand `fast` a model of genuinely unknown cost.
    models.push(Candidate {
        output_usd_per_mtok: 0.0,
        ..candidate("openrouter", "openrouter/auto", "openrouter", 0.0)
    });
    let p = plan(Profile::Fast, &models);
    assert_eq!(
        model_of(&p, EngineAgentKind::Default),
        "deepseek/deepseek-chat",
        "fast took the unpriced gateway row instead of the cheapest known model"
    );
    assert_eq!(p.ranked_count, 5, "the unpriced row should not be ranked");
}

#[test]
fn an_all_unpriced_catalog_still_produces_a_plan() {
    // Nothing is priced — the planner must still tune effort rather than
    // leaving every role unset.
    let models = vec![
        candidate("local", "some-local-model", "local", 0.0),
        candidate("openrouter", "openrouter/auto", "openrouter", 0.0),
    ];
    let p = plan(Profile::Ultra, &models);
    assert!(p.pick(EngineAgentKind::Default).unwrap().model.is_some());
    assert_eq!(
        p.pick(EngineAgentKind::Default).unwrap().effort,
        Some(ReasoningEffort::Max)
    );
}

#[test]
fn no_candidates_at_all_leaves_every_model_unset() {
    let p = plan(Profile::Pro, &[]);
    assert_eq!(p.ranked_count, 0);
    for pick in &p.picks {
        assert!(pick.model.is_none());
    }
    // With no model chosen there is nothing to clamp against, so effort stays
    // unset too rather than guessing a level.
    assert!(p.picks.iter().all(|p| p.effort.is_none()));
}

#[test]
fn effort_is_clamped_down_to_the_rungs_the_provider_exposes() {
    // Gemini exposes only low/high. `ultra` asks for max; writing `max` here
    // would record a level the request can never express.
    let gemini = vec![Candidate {
        effort_levels: LOW_HIGH,
        ..candidate("gemini", "gemini-3-pro", "google", 10.0)
    }];
    let pick = plan(Profile::Ultra, &gemini)
        .pick(EngineAgentKind::Default)
        .cloned()
        .unwrap();
    assert_eq!(pick.effort, Some(ReasoningEffort::High));
    assert_eq!(pick.effort_downgraded_from, Some(ReasoningEffort::Max));

    // The OpenAI shapes stop at high, so `pro`'s xhigh verifier lands on high.
    let openai = vec![Candidate {
        effort_levels: OPENAI,
        ..candidate("openai", "gpt-5.5", "gpt", 10.0)
    }];
    let verifier = plan(Profile::Pro, &openai)
        .pick(EngineAgentKind::Verifier)
        .cloned()
        .unwrap();
    assert_eq!(verifier.effort, Some(ReasoningEffort::High));
    assert_eq!(verifier.effort_downgraded_from, Some(ReasoningEffort::Xhigh));
}

#[test]
fn a_same_tier_model_that_can_express_the_effort_wins_the_tie() {
    // Two models at the same price, one of which cannot go above `high`.
    // Picking on price alone would hand `ultra` the narrow-ladder model and
    // clamp `max` away, when an equally-priced sibling could have expressed it.
    let models = vec![
        Candidate {
            effort_levels: LOW_HIGH,
            ..candidate("gemini", "gemini-3-pro", "google", 75.0)
        },
        Candidate {
            effort_levels: FULL,
            ..candidate("anthropic", "claude-fable-5", "anthropic", 75.0)
        },
    ];
    let p = plan(Profile::Ultra, &models);
    let pick = p.pick(EngineAgentKind::Default).unwrap();
    assert_eq!(
        pick.model.as_ref().unwrap().qualified(),
        "anthropic/claude-fable-5"
    );
    assert_eq!(pick.effort, Some(ReasoningEffort::Max));
    assert!(pick.effort_downgraded_from.is_none());
}

#[test]
fn the_expressiveness_tiebreak_never_leaves_the_price_tier() {
    // `balanced` lands on a two-rung provider and wants `medium`, which that
    // provider cannot express. The only model that can costs six times as
    // much — buying the knob there would blow the profile's own price promise,
    // so the pick stands and the effort is clamped instead.
    let models = vec![
        Candidate {
            effort_levels: LOW_HIGH,
            ..candidate("gemini", "gemini-3-flash", "google", 10.0)
        },
        Candidate {
            effort_levels: LOW_HIGH,
            ..candidate("gemini", "gemini-3-pro", "google", 11.0)
        },
        Candidate {
            effort_levels: FULL,
            ..candidate("anthropic", "claude-fable-5", "anthropic", 60.0)
        },
    ];
    let pick = plan(Profile::Balanced, &models)
        .pick(EngineAgentKind::Default)
        .cloned()
        .unwrap();
    assert_eq!(pick.model.as_ref().unwrap().provider, "gemini");
    assert_eq!(pick.effort, Some(ReasoningEffort::Low));
    assert_eq!(pick.effort_downgraded_from, Some(ReasoningEffort::Medium));
}

#[test]
fn the_tiebreak_prefers_the_cheapest_expressive_model_in_the_band() {
    // The priciest model is the one that cannot express `max`, and two models
    // inside the same-tier band can. The pick must be the CHEAPER of them —
    // a tiebreak buys back the missing rung, it does not go shopping.
    let models = vec![
        Candidate {
            effort_levels: LOW_HIGH,
            ..candidate("gemini", "gemini-3-pro", "google", 105.0)
        },
        Candidate {
            effort_levels: FULL,
            ..candidate("anthropic", "claude-cheap", "anthropic", 95.0)
        },
        Candidate {
            effort_levels: FULL,
            ..candidate("openai", "gpt-dear", "gpt", 100.0)
        },
    ];
    let pick = plan(Profile::Ultra, &models)
        .pick(EngineAgentKind::Default)
        .cloned()
        .unwrap();
    assert_eq!(
        pick.model.as_ref().unwrap().qualified(),
        "anthropic/claude-cheap"
    );
    assert_eq!(pick.effort, Some(ReasoningEffort::Max));
}

#[test]
fn clamping_never_raises_an_effort() {
    // `fast` asks for low everywhere; a provider offering the full ladder must
    // not be nudged up to it.
    let pick = plan(Profile::Fast, &spread())
        .pick(EngineAgentKind::Default)
        .cloned()
        .unwrap();
    assert_eq!(pick.effort, Some(ReasoningEffort::Low));
    assert!(
        pick.effort_downgraded_from.is_none(),
        "an un-clamped effort must not report a downgrade"
    );
}

#[test]
fn a_provider_with_no_effort_knob_gets_no_effort_but_keeps_thinking() {
    // Z.ai has no request-level effort control, but its thinking switch is
    // still on/off — the two must not be conflated.
    let zai = vec![Candidate {
        effort_levels: &[],
        ..candidate("zai", "glm-5.2", "glm", 5.0)
    }];
    let pick = plan(Profile::Ultra, &zai)
        .pick(EngineAgentKind::Default)
        .cloned()
        .unwrap();
    assert_eq!(pick.effort, None);
    assert_eq!(pick.reasoning, Some(true));
}

#[test]
fn a_model_that_cannot_think_carries_neither_effort_nor_a_thinking_switch() {
    let no_thinking = vec![Candidate {
        supports_reasoning: Some(false),
        effort_levels: &[],
        ..candidate("openai", "gpt-classic", "gpt", 10.0)
    }];
    let pick = plan(Profile::Ultra, &no_thinking)
        .pick(EngineAgentKind::Default)
        .cloned()
        .unwrap();
    assert_eq!(pick.effort, None);
    assert_eq!(
        pick.reasoning, None,
        "a confirmed non-reasoning model must not be handed a thinking switch"
    );
}

#[test]
fn ranking_is_reproducible_when_two_models_cost_the_same() {
    let tied = vec![
        candidate("openai", "b-model", "gpt", 10.0),
        candidate("anthropic", "a-model", "anthropic", 10.0),
    ];
    let first = plan(Profile::Fast, &tied);
    let reversed: Vec<Candidate> = tied.iter().rev().cloned().collect();
    let second = plan(Profile::Fast, &reversed);
    assert_eq!(
        model_of(&first, EngineAgentKind::Default),
        model_of(&second, EngineAgentKind::Default),
        "a price tie must break deterministically, not on catalog order"
    );
}

// Applying a plan to settings

#[test]
fn applying_a_profile_turns_the_auto_switches_off() {
    // `effort_auto: on` overrides every per-agent effort with verifier=high /
    // worker=medium / triage=low. Left on, `/profile ultra` would print `max`
    // and run `medium` — the profile must be the authority.
    let mut engine = AgentEngineConfig {
        auto_mode: Some(Toggle::On),
        effort_auto: Some(Toggle::On),
        reasoning_auto: Some(Toggle::On),
        ..AgentEngineConfig::default()
    };
    apply(&plan(Profile::Ultra, &spread()), &mut engine);
    assert_eq!(engine.effort_auto, Some(Toggle::Off));
    assert_eq!(engine.reasoning_auto, Some(Toggle::Off));
    assert_eq!(engine.auto_mode, Some(Toggle::Off));
    let worker = engine.agents.as_ref().unwrap().worker.as_ref().unwrap();
    assert_eq!(worker.effort, Some(ReasoningEffort::Max));
}

#[test]
fn applying_a_profile_writes_the_flat_model_keys_and_clears_per_agent_pins() {
    let mut engine = AgentEngineConfig {
        agents: Some(AgentEngineAgents {
            worker: Some(AgentEngineAgent {
                // A stale per-agent pin outranks the flat key, so it would
                // quietly beat the model the profile just chose.
                model: Some("openai/gpt-classic".to_string()),
                ..AgentEngineAgent::default()
            }),
            ..AgentEngineAgents::default()
        }),
        ..AgentEngineConfig::default()
    };
    apply(&plan(Profile::Fast, &spread()), &mut engine);
    assert_eq!(
        engine.pipeline_worker_model.as_deref(),
        Some("deepseek/deepseek-chat")
    );
    assert!(
        engine
            .agents
            .as_ref()
            .unwrap()
            .worker
            .as_ref()
            .unwrap()
            .model
            .is_none(),
        "the per-agent pin must be cleared so the flat key decides"
    );
    assert!(engine.default_model.is_some());
    assert!(engine.pipeline_verifier_model.is_some());
    assert!(engine.pipeline_triage_model.is_some());
}

#[test]
fn applying_a_profile_preserves_what_it_does_not_own() {
    let mut engine = AgentEngineConfig {
        allowed_models: Some(vec!["anthropic/claude-fable-5".to_string()]),
        headless_scope_bypass: Some(Toggle::On),
        agents: Some(AgentEngineAgents {
            verifier: Some(AgentEngineAgent {
                prompt: Some("You are a strict reviewer.".to_string()),
                provider: Some("openrouter".to_string()),
                params: Some(AgentEngineParams {
                    temperature: Some(0.2),
                    seed: Some(42),
                    ..AgentEngineParams::default()
                }),
                ..AgentEngineAgent::default()
            }),
            ..AgentEngineAgents::default()
        }),
        ..AgentEngineConfig::default()
    };
    apply(&plan(Profile::Pro, &spread()), &mut engine);
    let verifier = engine.agents.as_ref().unwrap().verifier.as_ref().unwrap();
    assert_eq!(verifier.prompt.as_deref(), Some("You are a strict reviewer."));
    assert_eq!(verifier.provider.as_deref(), Some("openrouter"));
    let params = verifier.params.as_ref().unwrap();
    assert_eq!(params.temperature, Some(0.2));
    assert_eq!(params.seed, Some(42));
    // The profile does own verbosity and service tier.
    assert_eq!(params.verbosity, Some(Verbosity::Medium));
    assert_eq!(params.service_tier, Some(ServiceTier::Priority));
    // And it must not touch the model vocabulary or unrelated switches.
    assert_eq!(
        engine.allowed_models.as_deref(),
        Some(["anthropic/claude-fable-5".to_string()].as_slice())
    );
    assert_eq!(engine.headless_scope_bypass, Some(Toggle::On));
}

#[test]
fn applying_the_same_profile_twice_changes_nothing_the_second_time() {
    let models = spread();
    let mut once = AgentEngineConfig::default();
    apply(&plan(Profile::Balanced, &models), &mut once);
    let mut twice = once.clone();
    apply(&plan(Profile::Balanced, &models), &mut twice);
    assert_eq!(once, twice, "applying a profile must be idempotent");
}

#[test]
fn switching_profiles_fully_replaces_the_previous_ones_effort() {
    // Going ultra → fast must not leave `max` behind on any role.
    let models = spread();
    let mut engine = AgentEngineConfig::default();
    apply(&plan(Profile::Ultra, &models), &mut engine);
    apply(&plan(Profile::Fast, &models), &mut engine);
    let agents = engine.agents.as_ref().unwrap();
    for agent in [
        &agents.default,
        &agents.worker,
        &agents.verifier,
        &agents.triage,
    ] {
        let agent = agent.as_ref().unwrap();
        assert_eq!(agent.effort, Some(ReasoningEffort::Low));
        assert_eq!(agent.reasoning, Some(Toggle::Off));
    }
}

#[test]
fn detect_names_the_profile_a_config_was_written_by() {
    let models = spread();
    for profile in Profile::ALL {
        let mut engine = AgentEngineConfig::default();
        apply(&plan(profile, &models), &mut engine);
        assert_eq!(
            detect(&engine, &models),
            Some(profile),
            "a config written by `{}` should read back as `{}`",
            profile.name(),
            profile.name()
        );
    }
}

#[test]
fn detect_reports_a_hand_edited_config_as_no_profile() {
    let models = spread();
    let mut engine = AgentEngineConfig::default();
    apply(&plan(Profile::Balanced, &models), &mut engine);
    engine
        .agents
        .as_mut()
        .unwrap()
        .worker
        .as_mut()
        .unwrap()
        .effort = Some(ReasoningEffort::Max);
    assert_eq!(
        detect(&engine, &models),
        None,
        "a config edited away from its profile must read as customized"
    );
}

#[test]
fn detect_is_none_on_an_untouched_config() {
    assert_eq!(detect(&AgentEngineConfig::default(), &spread()), None);
}

// The way out of a profile

#[test]
fn restore_auto_undoes_what_a_profile_claimed() {
    // Applying a profile has to switch the auto modes off or they would
    // override it. Without a way back that is a one-way door.
    let mut engine = AgentEngineConfig::default();
    apply(&plan(Profile::Ultra, &spread()), &mut engine);
    assert!(!is_auto(&engine));

    restore_auto(&mut engine);
    assert!(is_auto(&engine));
    assert_eq!(engine.effort_auto, Some(Toggle::On));
    assert_eq!(engine.reasoning_auto, Some(Toggle::On));
    assert_eq!(engine.auto_mode, Some(Toggle::On));
    // Everything a profile wrote into `agents` is gone — and the emptied
    // objects go with it rather than leaving `"params": {}` husks in the file.
    assert_eq!(engine.agents, None);
    // No profile matches once the dials are handed back.
    assert_eq!(detect(&engine, &spread()), None);
}

#[test]
fn restore_auto_keeps_a_params_object_that_still_has_something_in_it() {
    // Dropping emptied objects must not become "drop the object": a seed the
    // profile never owned has to survive, and its `params` with it.
    let mut engine = AgentEngineConfig {
        agents: Some(AgentEngineAgents {
            worker: Some(AgentEngineAgent {
                params: Some(AgentEngineParams {
                    seed: Some(7),
                    ..AgentEngineParams::default()
                }),
                ..AgentEngineAgent::default()
            }),
            ..AgentEngineAgents::default()
        }),
        ..AgentEngineConfig::default()
    };
    apply(&plan(Profile::Ultra, &spread()), &mut engine);
    restore_auto(&mut engine);
    let worker = engine
        .agents
        .as_ref()
        .expect("the worker still has a seed, so `agents` must survive")
        .worker
        .as_ref()
        .unwrap();
    let params = worker.params.as_ref().unwrap();
    assert_eq!(params.seed, Some(7));
    // …but the fields the profile did own are gone.
    assert_eq!(params.verbosity, None);
    assert_eq!(params.service_tier, None);
    assert_eq!(worker.effort, None);
}

#[test]
fn restore_auto_leaves_model_choices_alone() {
    // A model pin may well predate the profile — set by `/model` or by hand.
    // Clearing it would destroy something this command never owned.
    let mut engine = AgentEngineConfig::default();
    apply(&plan(Profile::Ultra, &spread()), &mut engine);
    let models = (
        engine.default_model.clone(),
        engine.pipeline_worker_model.clone(),
        engine.pipeline_verifier_model.clone(),
        engine.pipeline_triage_model.clone(),
    );
    restore_auto(&mut engine);
    assert_eq!(
        (
            engine.default_model.clone(),
            engine.pipeline_worker_model.clone(),
            engine.pipeline_verifier_model.clone(),
            engine.pipeline_triage_model.clone(),
        ),
        models
    );
}

#[test]
fn restore_auto_preserves_a_custom_prompt_and_temperature() {
    let mut engine = AgentEngineConfig {
        agents: Some(AgentEngineAgents {
            verifier: Some(AgentEngineAgent {
                prompt: Some("You are a strict reviewer.".to_string()),
                params: Some(AgentEngineParams {
                    temperature: Some(0.2),
                    ..AgentEngineParams::default()
                }),
                ..AgentEngineAgent::default()
            }),
            ..AgentEngineAgents::default()
        }),
        ..AgentEngineConfig::default()
    };
    apply(&plan(Profile::Pro, &spread()), &mut engine);
    restore_auto(&mut engine);
    let verifier = engine.agents.as_ref().unwrap().verifier.as_ref().unwrap();
    assert_eq!(verifier.prompt.as_deref(), Some("You are a strict reviewer."));
    assert_eq!(verifier.params.as_ref().unwrap().temperature, Some(0.2));
}

#[test]
fn is_auto_is_false_while_a_profile_holds_the_dials() {
    // An untouched config has the switches unset, not on — that is "no
    // opinion", which is not the same as having asked for auto.
    assert!(!is_auto(&AgentEngineConfig::default()));
    for profile in Profile::ALL {
        let mut engine = AgentEngineConfig::default();
        apply(&plan(profile, &spread()), &mut engine);
        assert!(!is_auto(&engine), "{} read as auto", profile.name());
    }
}

#[test]
fn a_saved_profile_reloads_as_the_same_profile() {
    // The persistence half, end to end and hermetically: apply → `save_to` →
    // read the file back → `detect`. `save_to` is the same writer the SETTINGS
    // tab and `/model` call, so a shape it cannot round-trip would be a real
    // break, not a test artifact.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    // A sibling key that must survive: `save_to` replaces the whole
    // `agent_engine_config` block, and a profile has no business touching
    // anything outside it.
    std::fs::write(&path, "{\"enable_recap\": true}\n").unwrap();

    let models = spread();
    let mut engine = AgentEngineConfig::default();
    apply(&plan(Profile::Ultra, &models), &mut engine);
    engine.save_to(&path).expect("profile should persist");

    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        raw.get("enable_recap"),
        Some(&serde_json::Value::Bool(true)),
        "an unrelated settings key must survive a profile write"
    );
    let reloaded: AgentEngineConfig =
        serde_json::from_value(raw.get("agent_engine_config").unwrap().clone()).unwrap();
    assert_eq!(detect(&reloaded, &models), Some(Profile::Ultra));
    // And the switch that would have overridden it is off on disk.
    assert_eq!(
        raw.pointer("/agent_engine_config/effort_auto"),
        Some(&serde_json::Value::String("off".to_string()))
    );
}
