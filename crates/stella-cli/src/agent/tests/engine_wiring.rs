//! Which model actually runs, and who wins when several things pin one.
//!
//! Split out of `agent/tests.rs` when it grew past its file-size ceiling
//! (#629). The guard's rule is that new code goes in its own module rather
//! than onto the end of an already-oversized file, and these tests were the
//! growth that tripped it. They are one subject — `resolve_engine_wiring`'s
//! precedence order — so they move together with the two groups they belong
//! with: the `pipeline_worker_model` routing they extend, and the
//! `worker_model_*` fallbacks they share `cfg_with_engine` with.
//!
//! Everything here reaches `cfg_for`, `Config`, `Role`, and `ModelRef` through
//! the parent's `use super::*`, exactly as `usage_completeness` does.

use super::*;

/// A `Config` selecting `provider_id` at its default model, carrying
/// `engine_settings` — the `agent_engine_config` variant of [`cfg_for`] for
/// `resolve_engine_wiring` tests.
fn cfg_with_engine(provider_id: &str, engine_settings_json: &str) -> Config {
    let mut cfg = cfg_for(provider_id);
    cfg.engine_settings =
        Some(serde_json::from_str(engine_settings_json).expect("valid agent_engine_config json"));
    cfg
}

#[test]
fn the_turn_timeout_flag_reaches_every_role_that_can_continue() {
    // The policy it feeds (`driver::truncation::ContinuationBudget`) is inert
    // unless something sets it, and "inert" is indistinguishable from "working"
    // in every test that only exercises the engine crate. This is the wire, so
    // it is what proves the flag is not decorative.
    //
    // Every role, not just the worker: the deadline being guarded belongs to
    // the process, so a worker that declines a continuation while a verifier
    // spends the remaining time past it would defeat the point.
    let mut cfg = cfg_for("zai");
    cfg.turn_timeout = Some(std::time::Duration::from_secs(840));

    assert_eq!(
        crate::agent::engine_config_for(&cfg).turn_budget,
        Some(std::time::Duration::from_secs(840)),
    );
    assert_eq!(
        crate::agent::verifier_engine_config_for(&cfg).turn_budget,
        Some(std::time::Duration::from_secs(840)),
    );

    // And absent by default, so no existing caller starts declining work.
    let plain = cfg_for("zai");
    assert_eq!(crate::agent::engine_config_for(&plain).turn_budget, None);
}

#[test]
fn a_bound_session_checkpoints_from_every_role() {
    // The same argument as the turn budget above, for the same reason: the
    // sink is attached at ONE place (`tuned_engine_config`) precisely so no
    // role can be left out, and "left out" is invisible from inside the engine
    // crate — an unbound sink is indistinguishable from a session that simply
    // never crashed. This is the wire.
    let store = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let cfg = cfg_for("zai");

    // Unbound: no sink, so `persist_checkpoint` does not serialize a whole
    // transcript per step only to drop it.
    assert!(
        crate::agent::engine_config_for(&cfg)
            .checkpoint_sink
            .is_none(),
        "a session with no durable record attaches nothing",
    );

    // `open_in`, never `open`: the latter reads `STELLA_HOME`, and a test that
    // touched a process-global would race its siblings.
    let record = stella_store::work_journal::WorkJournal::open_in(
        store.path(),
        ws.path(),
        "ses-engine-wiring",
    )
    .unwrap();
    cfg.durability.bind(record.clone());

    for (role, engine) in [
        ("default", crate::agent::engine_config_for(&cfg)),
        ("verifier", crate::agent::verifier_engine_config_for(&cfg)),
    ] {
        assert!(
            engine.checkpoint_sink.is_some(),
            "the {role} engine must carry the session's checkpoint sink",
        );
    }

    // And it reaches the record the store reads back — the binding is live,
    // not merely present.
    crate::agent::engine_config_for(&cfg)
        .checkpoint_sink
        .expect("bound")
        .persist("{\"version\":1}");
    assert_eq!(record.checkpoint().as_deref(), Some("{\"version\":1}"));
}

/// Every role of a session shares ONE view of what its providers will fund
/// (#3307), and it is the same cell — not merely an equal one.
///
/// The same argument as the checkpoint sink above: the handle is attached at
/// one place (`tuned_engine_config`) precisely so no role can be left out, and
/// "left out" is invisible from inside the engine crate — a role with its own
/// empty handle behaves exactly like a session that has simply never been
/// refused. The damage is silent and only shows up as latency: each role that
/// misses the handle re-learns a refused ceiling by paying its own 402
/// round-trip, every turn, for as long as the balance stays low.
#[test]
fn every_role_shares_one_session_view_of_affordable_output_ceilings() {
    let cfg = cfg_for("zai");
    let roles = [
        ("default", crate::agent::engine_config_for(&cfg)),
        ("verifier", crate::agent::verifier_engine_config_for(&cfg)),
        (
            "sub-session",
            crate::agent::subsession_engine_config_for(&cfg),
        ),
    ];

    // Empty until something is refused, so a session that never meets a 402
    // asks for exactly what it configured (AGENTS.md invariant 7).
    for (role, engine) in &roles {
        let ceilings = engine
            .session_output_ceilings
            .as_ref()
            .unwrap_or_else(|| panic!("the {role} engine must carry the session's handle"));
        assert_eq!(
            ceilings.narrow("zai", Some(128_000)),
            Some(128_000),
            "the {role} engine must start with nothing standing",
        );
    }

    // One cell, not four: what any role learns, every role sees. Written
    // through the default role's handle and read back through the others,
    // which is the property a per-role `Default` would silently fail.
    roles[0]
        .1
        .session_output_ceilings
        .as_ref()
        .expect("attached")
        .learn("zai", 90_000);
    for (role, engine) in &roles {
        assert_eq!(
            engine
                .session_output_ceilings
                .as_ref()
                .expect("attached")
                .narrow("zai", Some(128_000)),
            Some(90_000),
            "the {role} engine must see what another role already paid to learn",
        );
    }
}

#[test]
fn pipeline_worker_model_is_inert_without_the_fix_but_routes_with_it() {
    // Issue #276: `pipeline_worker_model` (and `agents.worker.*`) must
    // actually change what `Role::Worker` resolves to — previously the
    // worker always rode `worker_ref` (the session default), no matter what
    // this setting said.
    let cfg = cfg_with_engine(
        "zai", // session default: zai/glm-5.2
        r#"{ "pipeline_worker_model": "anthropic/claude-fable-5" }"#,
    );
    let model_ref = ModelRef::new(cfg.provider.id, cfg.model_id.clone());
    let configured = vec![configured_provider("zai"), configured_provider("anthropic")];

    let wiring = resolve_engine_wiring(&cfg, &model_ref, &configured);

    let overridden = ModelRef::new("anthropic", "claude-fable-5");
    assert_eq!(
        wiring.worker_model, overridden,
        "the wiring's own worker_model must reflect the pipeline_worker_model override"
    );
    assert_eq!(
        wiring.pins.get(Role::Worker),
        Some(&overridden),
        "Role::Worker must be pinned to the configured worker model"
    );
    assert_eq!(
        wiring.pins.get(Role::Plan),
        Some(&overridden),
        "Role::Plan shares the worker's tier and must follow the override too, or plan/witness \
         turns would silently keep running on the session default"
    );
    assert!(
        wiring
            .extra_providers
            .iter()
            .any(|(model_ref, _)| *model_ref == overridden),
        "an adapter for the overridden worker model must be built"
    );

    // The full round trip through the actual router (what any host resolving
    // a role provider goes through — `stella-pipeline`'s `resolve_provider`
    // did, before #3865 deleted that crate) must resolve BOTH roles to the
    // override, not just the raw pin table.
    let breaker = CircuitBreaker::new(Box::new(SystemClock::new()));
    let router = Router::new(wiring.pins.clone(), wiring.profiles.clone(), breaker);
    assert_eq!(
        router.resolve(Role::Worker).unwrap().model_ref,
        overridden,
        "the router must actually route worker turns to the override"
    );
    assert_eq!(
        router.resolve(Role::Plan).unwrap().model_ref,
        overridden,
        "the router must actually route plan turns to the override"
    );
}

#[test]
fn an_explicit_model_flag_outranks_pipeline_worker_model() {
    // An explicit `--model` is a per-invocation pin of the WORKER model —
    // that is the flag's whole documented job. Before this,
    // `pipeline_worker_model` was applied unconditionally on top of the
    // already-resolved session default, so `--model zai/glm-5.2` silently ran
    // (and billed for) `anthropic/claude-fable-5` instead.
    let mut cfg = cfg_with_engine(
        "zai", // --model zai/glm-5.2
        r#"{ "pipeline_worker_model": "anthropic/claude-fable-5" }"#,
    );
    cfg.model_pinned_by_flag = true;
    let model_ref = ModelRef::new(cfg.provider.id, cfg.model_id.clone());
    let configured = vec![configured_provider("zai"), configured_provider("anthropic")];

    let wiring = resolve_engine_wiring(&cfg, &model_ref, &configured);

    assert_eq!(
        wiring.worker_model, model_ref,
        "the flag-pinned model must be what the worker role resolves to"
    );
    let overridden = ModelRef::new("anthropic", "claude-fable-5");
    assert_ne!(
        wiring.pins.get(Role::Worker),
        Some(&overridden),
        "settings must not pin the worker over an explicit --model"
    );
    assert!(
        !wiring
            .extra_providers
            .iter()
            .any(|(model_ref, _)| *model_ref == overridden),
        "no adapter for the suppressed worker override should be built"
    );

    // The round trip through the real router is the claim that matters: the
    // pin table being empty is only useful if resolution lands on the flag.
    let breaker = CircuitBreaker::new(Box::new(SystemClock::new()));
    let router = Router::new(wiring.pins.clone(), wiring.profiles.clone(), breaker);
    assert_eq!(
        router.resolve(Role::Worker).unwrap().model_ref,
        model_ref,
        "worker turns must run on the flag-pinned model"
    );
    assert_eq!(
        router.resolve(Role::Plan).unwrap().model_ref,
        model_ref,
        "plan/witness turns ride the worker and must follow the flag too"
    );

    // Silently dropping a configured setting is its own bug — say so.
    assert!(
        wiring
            .notices
            .iter()
            .any(|n| n.contains("anthropic/claude-fable-5") && n.contains("--model")),
        "a notice must explain that --model suppressed the configured worker \
         model, got: {:?}",
        wiring.notices
    );
}

/// The benchmark's two arms, at the layer that decides whether the authored
/// witness can run at all (#1007).
///
/// `Pipeline::can_author_independent_witness` compares the resolved verifier's
/// `model_ref` with the worker's and refuses to let the worker author the test
/// that verifies it. The benchmark posture expresses routing only through
/// `default_model`, so both roles land on one model and the witness tier is
/// structurally off — every Terminal-Bench number before #1007 was measured
/// that way. Adding `pipeline_verifier_model` is what splits them.
///
/// Both arms asserted together because the pair is the point: the control arm
/// must keep collapsing (it is the published baseline) and the treatment arm
/// must actually separate.
#[test]
fn the_benchmark_posture_splits_worker_and_verifier_only_on_the_witness_arm() {
    let worker_ref = ModelRef::new("openrouter", "z-ai/glm-5.1");
    let configured = vec![configured_provider("openrouter")];

    let control = cfg_with_engine(
        "openrouter",
        r#"{ "default_model": "openrouter/z-ai/glm-5.1",
             "allowed_models": ["openrouter/z-ai/glm-5.1"] }"#,
    );
    let control = resolve_engine_wiring(&control, &worker_ref, &configured);
    assert_eq!(
        control.pins.get(Role::Verifier),
        control.pins.get(Role::Worker),
        "the control arm must keep the verifier on the worker's model — that is \
         precisely what leaves the authored witness with no independent author"
    );

    let treatment = cfg_with_engine(
        "openrouter",
        r#"{ "default_model": "openrouter/z-ai/glm-5.1",
             "pipeline_verifier_model": "openrouter/deepseek/deepseek-v4-pro",
             "allowed_models": ["openrouter/z-ai/glm-5.1",
                                "openrouter/deepseek/deepseek-v4-pro"] }"#,
    );
    let treatment = resolve_engine_wiring(&treatment, &worker_ref, &configured);
    let author = ModelRef::new("openrouter", "deepseek/deepseek-v4-pro");
    assert_eq!(
        treatment.pins.get(Role::Verifier),
        Some(&author),
        "the witness arm must pin the verifier to the second model"
    );
    assert_ne!(
        treatment.pins.get(Role::Verifier),
        treatment.pins.get(Role::Worker),
        "worker and verifier must resolve to different models, or the witness \
         tier stays off with a posture hash claiming it is on"
    );
}

/// The same-model degradation is a fact the operator is owed BEFORE a token
/// is spent — the pipeline keeps running (an auto-routing gateway can serve
/// distinct upstream models behind one id, so refusing would punish exactly
/// that setup), but the notice must name the duplicated model and the config
/// key that restores independence.
#[test]
fn a_same_model_posture_is_named_in_a_wiring_notice_not_refused() {
    let same_model_notice = |wiring: &EngineWiring| {
        wiring
            .notices
            .iter()
            .any(|n| n.contains("verifier and worker are both") && n.contains("degraded"))
    };

    // The stock posture: no engine settings at all, every role on the
    // session default.
    let plain = cfg_for("zai");
    let plain_ref = ModelRef::new(plain.provider.id, plain.model_id.clone());
    let wiring = resolve_engine_wiring(&plain, &plain_ref, &[configured_provider("zai")]);
    assert!(
        same_model_notice(&wiring),
        "no engine settings means every role rides `{plain_ref}` — the degradation \
         must be named: {:?}",
        wiring.notices
    );
    assert!(
        wiring
            .notices
            .iter()
            .any(|n| n.contains(&plain_ref.to_string()) && n.contains("pipeline_verifier_model")),
        "the notice must name the duplicated model and the key that fixes it: {:?}",
        wiring.notices
    );

    // The benchmark control-arm shape: settings present, but routing entirely
    // through `default_model`.
    let control = cfg_with_engine(
        "openrouter",
        r#"{ "default_model": "openrouter/z-ai/glm-5.1",
             "allowed_models": ["openrouter/z-ai/glm-5.1"] }"#,
    );
    let worker_ref = ModelRef::new("openrouter", "z-ai/glm-5.1");
    let wiring = resolve_engine_wiring(&control, &worker_ref, &[configured_provider("openrouter")]);
    assert!(
        same_model_notice(&wiring),
        "a settings posture that lands every role on one model is the same fact: {:?}",
        wiring.notices
    );

    // A split posture must NOT raise it.
    let split = cfg_with_engine(
        "openrouter",
        r#"{ "default_model": "openrouter/z-ai/glm-5.1",
             "pipeline_verifier_model": "openrouter/deepseek/deepseek-v4-pro" }"#,
    );
    let wiring = resolve_engine_wiring(&split, &worker_ref, &[configured_provider("openrouter")]);
    assert!(
        !same_model_notice(&wiring),
        "an independent verifier pin is not degraded — no notice: {:?}",
        wiring.notices
    );
}

#[test]
fn an_explicit_model_flag_leaves_triage_and_verifier_pins_alone() {
    // `--model` says nothing about the triage/verifier roles, so their own
    // settings must keep applying — suppressing them too would make the flag
    // a blunt instrument that silently un-configures the rest of the pipeline.
    let mut cfg = cfg_with_engine(
        "zai",
        r#"{ "pipeline_worker_model": "anthropic/claude-fable-5",
             "pipeline_triage_model": "deepseek/deepseek-chat",
             "pipeline_verifier_model": "openai/gpt-5.5" }"#,
    );
    cfg.model_pinned_by_flag = true;
    let model_ref = ModelRef::new(cfg.provider.id, cfg.model_id.clone());
    let configured = vec![
        configured_provider("zai"),
        configured_provider("anthropic"),
        configured_provider("deepseek"),
        configured_provider("openai"),
    ];

    let wiring = resolve_engine_wiring(&cfg, &model_ref, &configured);

    assert_eq!(wiring.worker_model, model_ref, "worker follows the flag");
    assert_eq!(
        wiring.pins.get(Role::Triage),
        Some(&ModelRef::new("deepseek", "deepseek-chat")),
        "the configured triage model must survive an explicit --model"
    );
    assert_eq!(
        wiring.pins.get(Role::Verifier),
        Some(&ModelRef::new("openai", "gpt-5.5")),
        "the configured verifier model must survive an explicit --model"
    );
}

#[test]
fn without_a_model_flag_pipeline_worker_model_still_wins() {
    // The guard must key off the FLAG, not merely the presence of a worker
    // setting: with no `--model`, issue #276's behavior is unchanged.
    let cfg = cfg_with_engine(
        "zai",
        r#"{ "pipeline_worker_model": "anthropic/claude-fable-5" }"#,
    );
    assert!(!cfg.model_pinned_by_flag);
    let model_ref = ModelRef::new(cfg.provider.id, cfg.model_id.clone());
    let configured = vec![configured_provider("zai"), configured_provider("anthropic")];

    let wiring = resolve_engine_wiring(&cfg, &model_ref, &configured);

    assert_eq!(
        wiring.worker_model,
        ModelRef::new("anthropic", "claude-fable-5"),
        "an unpinned run must still honor pipeline_worker_model (#276)"
    );
    assert!(
        wiring.notices.is_empty(),
        "no --model means nothing was suppressed, so no notice: {:?}",
        wiring.notices
    );
}

#[test]
fn worker_model_unset_falls_back_to_the_session_default() {
    // No `pipeline_worker_model`/`agents.worker.*` configured at all: the
    // worker must behave exactly as before this fix — riding the session
    // default, no pin, no extra adapter.
    let cfg = cfg_with_engine(
        "zai",
        r#"{ "pipeline_verifier_model": "anthropic/claude-fable-5" }"#,
    );
    let model_ref = ModelRef::new(cfg.provider.id, cfg.model_id.clone());
    let configured = vec![configured_provider("zai"), configured_provider("anthropic")];

    let wiring = resolve_engine_wiring(&cfg, &model_ref, &configured);

    assert_eq!(
        wiring.worker_model, model_ref,
        "with no worker override, worker_model falls back to the session default"
    );
    assert!(
        wiring.pins.get(Role::Worker).is_none(),
        "no worker pin is recorded when unconfigured"
    );
    assert!(
        wiring.pins.get(Role::Plan).is_none(),
        "no plan pin is recorded when unconfigured"
    );
}

#[test]
fn worker_model_with_no_resolvable_credential_falls_back_and_notices() {
    // The configured worker provider has no credential available: the
    // override must degrade to the session default (soft failure), with a
    // human-readable notice — never a hard error, matching triage/verifier's
    // existing posture. An explicit `agents.worker.provider` pin (rather
    // than a flat-key `provider/slug` string) is what exercises this path:
    // an unconfigured provider named only inside a flat-key string is not
    // even recognized as a provider prefix (`model_spec_for`'s `is_provider`
    // gate), so it never reaches `pin_role`'s credential lookup at all.
    let cfg = cfg_with_engine(
        "zai",
        r#"{ "agents": { "worker": { "provider": "anthropic" } } }"#,
    );
    let model_ref = ModelRef::new(cfg.provider.id, cfg.model_id.clone());
    let configured = vec![configured_provider("zai")]; // anthropic NOT configured

    let wiring = resolve_engine_wiring(&cfg, &model_ref, &configured);

    assert_eq!(
        wiring.worker_model, model_ref,
        "an unroutable worker override must fall back to the session default"
    );
    assert!(wiring.pins.get(Role::Worker).is_none());
    assert!(
        wiring
            .notices
            .iter()
            .any(|n| n.contains("worker") && n.contains("anthropic")),
        "a skipped worker override must be reported: {:?}",
        wiring.notices
    );
}

#[test]
fn worker_override_equal_to_the_session_default_still_pins_without_a_duplicate_adapter() {
    // Configuring the worker to the SAME model the session already defaults
    // to must not build a redundant second adapter (mirrors the existing
    // triage/verifier "same instance" optimization) — but the pin is still
    // recorded, matching that established behavior.
    let cfg = cfg_with_engine("zai", r#"{ "pipeline_worker_model": "zai/glm-5.2" }"#);
    let model_ref = ModelRef::new(cfg.provider.id, cfg.model_id.clone());
    let configured = vec![configured_provider("zai")];

    let wiring = resolve_engine_wiring(&cfg, &model_ref, &configured);

    assert_eq!(wiring.worker_model, model_ref);
    assert_eq!(wiring.pins.get(Role::Worker), Some(&model_ref));
    assert!(
        wiring.extra_providers.is_empty(),
        "no extra adapter is needed when the override equals the session default"
    );
}

#[test]
fn worker_override_shifts_the_verifiers_cross_family_comparison() {
    // Issue #276's router-correctness corollary: once the worker is
    // overridden, auto-mode verifier selection (and the router's own unpinned-
    // verifier cross-family fallback) must compare against the model the
    // worker ACTUALLY resolves to, not the stale session default — else a
    // verifier could silently collapse to the same family as the real worker.
    let cfg = cfg_with_engine(
        "zai",
        r#"{ "pipeline_worker_model": "anthropic/claude-fable-5",
             "auto_mode": "on",
             "allowed_models": ["anthropic/claude-fable-5", "zai/glm-5.2"] }"#,
    );
    let model_ref = ModelRef::new(cfg.provider.id, cfg.model_id.clone());
    let configured = vec![configured_provider("zai"), configured_provider("anthropic")];

    let wiring = resolve_engine_wiring(&cfg, &model_ref, &configured);

    // The worker now runs on Anthropic; auto-mode must pick the cross-family
    // candidate (zai) as verifier, not Anthropic again (which the STALE
    // "worker family = zai" comparison would have wrongly treated as
    // cross-family).
    assert_eq!(
        wiring.pins.get(Role::Verifier),
        Some(&ModelRef::new("zai", "glm-5.2")),
        "auto-mode verifier selection must be cross-family from the OVERRIDDEN worker, not the \
         session default"
    );
}

/// #1147's acceptance criterion, at the layer that decides it: a posture
/// carrying ONLY the flat `pipeline_verifier_model` — no `agents.verifier.model`,
/// no `agents.verifier.provider` — must make `Role::Verifier` resolve to that model
/// through the real router, so `Pipeline::can_author_independent_witness`
/// sees a verifier distinct from the worker.
///
/// The benchmark's witness arm is exactly this shape, and the bug report
/// suspected the flat key never reached role resolution. It does: what the
/// live run actually lost was the *pin*, dropped by `pin_role` when the
/// author's adapter could not be built (see
/// `a_trusted_posture_whose_verifier_pin_cannot_be_built_refuses_the_run`). This
/// test pins the half that works so a future refactor cannot quietly break
/// the half that was never broken.
#[test]
fn the_flat_pipeline_verifier_model_alone_resolves_role_verifier_to_the_witness_author() {
    // The benchmark posture verbatim in shape: one `--model`-pinned worker,
    // per-agent tuning WITHOUT a model, and the author expressed only as the
    // flat root key.
    let mut cfg = cfg_with_engine(
        "anthropic",
        r#"{ "default_model": "anthropic/claude-sonnet-5",
             "pipeline_verifier_model": "anthropic/claude-fable-5",
             "allowed_models": ["anthropic/claude-sonnet-5",
                                "anthropic/claude-fable-5"],
             "auto_mode": "off", "effort_auto": "off", "reasoning_auto": "off",
             "agents": { "verifier": { "effort": "xhigh", "reasoning": "on" } } }"#,
    );
    // `--model anthropic/claude-sonnet-5`, as the adapter passes it. The flag
    // suppresses the WORKER spec only; the verifier key must survive it.
    cfg.model_id = "claude-sonnet-5".to_string();
    cfg.model_pinned_by_flag = true;
    let worker_ref = ModelRef::new("anthropic", "claude-sonnet-5");
    let configured = vec![configured_provider("anthropic")];

    let wiring = resolve_engine_wiring(&cfg, &worker_ref, &configured);

    let author = ModelRef::new("anthropic", "claude-fable-5");
    assert_eq!(
        wiring.pins.get(Role::Verifier),
        Some(&author),
        "the flat key alone must pin the verifier; notices: {:?}",
        wiring.notices
    );

    // The round trip through the real router is the claim that matters —
    // `Pipeline::resolve_provider(Role::Verifier)` calls exactly this.
    let breaker = CircuitBreaker::new(Box::new(SystemClock::new()));
    let router = Router::new(wiring.pins.clone(), wiring.profiles.clone(), breaker);
    assert_eq!(
        router.resolve(Role::Verifier).unwrap().model_ref,
        author,
        "Role::Verifier must route to the flat-key author"
    );
    assert_ne!(
        router.resolve(Role::Verifier).unwrap().model_ref,
        router.resolve(Role::Worker).unwrap().model_ref,
        "worker and verifier must differ, or the authored witness has no author"
    );
    assert!(
        wiring
            .extra_providers
            .iter()
            .any(|(model_ref, _)| *model_ref == author),
        "the author needs its own adapter, or the pin routes to nothing"
    );
}

/// The third of the three coupled ceilings, and until #1211 §6.2 the only one
/// reachable from no configuration at all: the output cap rides
/// `agents.<role>.params.max_tokens` and the turn budget is a CLI flag, but
/// `model_timeout` was an `EngineConfig` constant, so moving it meant a
/// rebuild. On the benchmark protocol that promoted a timeout change into a
/// system-under-test change — a re-freeze of the registered commit rather than
/// a line in a posture.
///
/// Every role, not just the worker: what it bounds belongs to the process, so
/// a verifier left at the default while the worker's cap tripled would stop first
/// on exactly the runs the raise was for.
#[test]
fn the_model_timeout_setting_reaches_every_roles_engine() {
    let mut cfg = cfg_with_engine(
        "anthropic",
        r#"{ "default_model": "anthropic/claude-sonnet-5",
             "model_timeout_secs": 1572 }"#,
    );
    cfg.model_id = "claude-sonnet-5".to_string();

    let expected = Some(std::time::Duration::from_secs(1572));
    for (role, engine) in [
        ("default", crate::agent::engine_config_for(&cfg)),
        ("verifier", crate::agent::verifier_engine_config_for(&cfg)),
    ] {
        assert_eq!(
            engine.model_timeout, expected,
            "{role} must carry the selected silence ceiling"
        );
    }
}

/// An absent key leaves the engine's own default in place — asserted against
/// `EngineConfig::default()` rather than the literal `816`, so the day the
/// engine changes its mind this test follows it instead of pinning the CLI to
/// a default the engine had already abandoned.
#[test]
fn an_absent_model_timeout_leaves_the_engine_default_alone() {
    let mut cfg = cfg_with_engine(
        "anthropic",
        r#"{ "default_model": "anthropic/claude-sonnet-5" }"#,
    );
    cfg.model_id = "claude-sonnet-5".to_string();

    assert_eq!(
        crate::agent::engine_config_for(&cfg).model_timeout,
        EngineConfig::default().model_timeout,
    );
}

/// `0` is a third state, not a spelling of "absent": it asks for no backstop at
/// all, which is the engine's `None`. Reading it as "time out immediately"
/// would fail every generation instantly, and reading it as "unset" would make
/// a deliberate choice indistinguishable from having made none.
#[test]
fn a_zero_model_timeout_disables_the_backstop_rather_than_tripping_instantly() {
    let mut cfg = cfg_with_engine(
        "anthropic",
        r#"{ "default_model": "anthropic/claude-sonnet-5",
             "model_timeout_secs": 0 }"#,
    );
    cfg.model_id = "claude-sonnet-5".to_string();

    assert_eq!(crate::agent::engine_config_for(&cfg).model_timeout, None);
    assert!(
        EngineConfig::default().model_timeout.is_some(),
        "the default must be bounded, or this test proves nothing"
    );
}

/// The context-handling pair (#1285): the compaction budget and the
/// tool-result retention horizon were engine constants a benchmark arm could
/// not move without a rebuild — the same promotion-to-SUT-change shape
/// `model_timeout_secs` closed for the timeout ceiling.
#[test]
fn the_context_shaping_settings_reach_the_engine() {
    let mut cfg = cfg_with_engine(
        "anthropic",
        r#"{ "default_model": "anthropic/claude-sonnet-5",
             "compaction_budget_tokens": 40000,
             "tool_result_horizon_steps": 6 }"#,
    );
    cfg.model_id = "claude-sonnet-5".to_string();

    let engine = crate::agent::engine_config_for(&cfg);
    assert_eq!(engine.compaction_budget_tokens, 40_000);
    assert_eq!(engine.tool_result_horizon_steps, Some(6));
}

/// The settings budget rides under the same window clamp as the default:
/// whatever an operator writes, compaction still fires before the provider's
/// context window would overflow.
#[test]
fn a_settings_budget_cannot_exceed_the_model_window_clamp() {
    let mut cfg = cfg_with_engine(
        "anthropic",
        r#"{ "default_model": "anthropic/claude-sonnet-5",
             "compaction_budget_tokens": 100000000 }"#,
    );
    cfg.model_id = "claude-sonnet-5".to_string();

    let window = stella_model::catalog::Catalog::current()
        .resolve_for("anthropic", "claude-sonnet-5")
        .expect("seeded model resolves")
        .context_window as u64;
    assert!(window > 0, "the seeded row must carry a window");
    assert_eq!(
        crate::agent::engine_config_for(&cfg).compaction_budget_tokens,
        window.saturating_mul(3) / 4,
        "an oversized budget clamps to 3/4 of the window"
    );
}

/// `0` disables the retention pass — a deliberate opt-out, distinct from
/// absent (the engine default) exactly as `model_timeout_secs: 0` is.
#[test]
fn a_zero_horizon_disables_retention_and_absent_keeps_the_default() {
    let mut cfg = cfg_with_engine(
        "anthropic",
        r#"{ "default_model": "anthropic/claude-sonnet-5",
             "tool_result_horizon_steps": 0 }"#,
    );
    cfg.model_id = "claude-sonnet-5".to_string();
    assert_eq!(
        crate::agent::engine_config_for(&cfg).tool_result_horizon_steps,
        None
    );

    let mut absent = cfg_with_engine(
        "anthropic",
        r#"{ "default_model": "anthropic/claude-sonnet-5" }"#,
    );
    absent.model_id = "claude-sonnet-5".to_string();
    assert_eq!(
        crate::agent::engine_config_for(&absent).tool_result_horizon_steps,
        EngineConfig::default().tool_result_horizon_steps,
    );
    assert!(
        EngineConfig::default().tool_result_horizon_steps.is_some(),
        "the default must retain, or the disable test proves nothing"
    );
}

#[test]
fn the_models_own_output_ceiling_replaces_the_global_default() {
    // `EngineConfig::default()` carries one `max_output_tokens` (16384) for
    // every model on every provider, and its own comment named per-model caps
    // as "the eventual refinement". The data was already on the model card —
    // models.dev publishes `limit.output`, the store has the column — and was
    // dropped when the runtime catalog was assembled, so nothing could read it.
    //
    // The cost was measured, not theoretical. On the Terminal-Bench 2.1 gate
    // four trials ended on a step that emitted the cap exactly and made no
    // tool call, while the comparator — same model, same API, same effort —
    // spent 45,001-64,000 output tokens on those same four tasks and finished
    // each one, reward 1.0. Two of its winning steps landed on precisely
    // 64,000, its own ceiling. The model fills whatever budget it is given;
    // the only question is whose number stops it first.
    //
    // Superset of the seed, matching `stella-model`'s own runtime-catalog test:
    // the catalog is a process-global, so a test that installed less than the
    // seed would perturb whatever runs beside it.
    let mut entries = stella_model::catalog::Catalog::seed().entries().to_vec();
    entries.push(
        stella_model::catalog::CatalogEntry::new(
            "test-only-wiring-model",
            "anthropic",
            "claude",
            200_000,
            stella_model::catalog::ToolDialect::AnthropicTools,
            stella_model::catalog::Pricing {
                input_usd_per_mtok: 3.0,
                output_usd_per_mtok: 15.0,
                cached_input_usd_per_mtok: 0.3,
                cache_write_usd_per_mtok: 3.75,
            },
        )
        .with_max_output_tokens(Some(64_000)),
    );
    stella_model::catalog::Catalog::install_runtime(stella_model::catalog::Catalog::with_entries(
        entries,
    ));

    let mut cfg = cfg_for("anthropic");
    cfg.model_id = "test-only-wiring-model".to_string();

    assert_eq!(
        crate::agent::engine_config_for(&cfg).max_output_tokens,
        Some(64_000),
        "the engine must ask for what the model can actually emit, not 16384",
    );

    // A model the catalog has no ceiling for keeps the default, so this
    // widens nothing it does not have data for.
    let mut unknown = cfg_for("anthropic");
    unknown.model_id = "definitely-not-in-any-catalog".to_string();
    assert_eq!(
        crate::agent::engine_config_for(&unknown).max_output_tokens,
        stella_core::driver::EngineConfig::default().max_output_tokens,
    );
}

/// The default is the MODEL'S maximum, with nothing configured (#1290).
///
/// Asserted against the number the provider actually reports rather than
/// "whatever the seed happens to say", and against both of the wrong answers
/// this has historically been: the engine's global 16384, and the 64,000 this
/// row carried for months — which was the comparator's measured stopping
/// point, not Sonnet 5's ceiling.
#[test]
fn an_unconfigured_run_asks_for_the_models_whole_output_budget() {
    let mut cfg = cfg_for("anthropic");
    cfg.model_id = "claude-sonnet-5".to_string();

    let resolved = crate::agent::engine_config_for(&cfg).max_output_tokens;
    assert_eq!(
        resolved,
        Some(128_000),
        "Anthropic's own /v1/models reports max_tokens=128000 for this model",
    );
    assert_ne!(
        resolved,
        stella_core::driver::EngineConfig::default().max_output_tokens,
        "the global default must not be what a catalogued model gets",
    );
}

/// Spending LESS than the model can write is the one direction an operator
/// actually needs, and `[models.output_caps]` is how they ask for it by name.
#[test]
fn a_per_model_cap_lowers_the_ceiling_but_only_for_the_model_named() {
    let mut cfg = cfg_with_engine(
        "anthropic",
        r#"{ "default_model": "anthropic/claude-sonnet-5",
             "model_output_caps": { "anthropic/claude-sonnet-5": 64000 } }"#,
    );
    cfg.model_id = "claude-sonnet-5".to_string();
    assert_eq!(
        crate::agent::engine_config_for(&cfg).max_output_tokens,
        Some(64_000),
    );

    // Same settings, different model: an entry naming one model says nothing
    // about any other, so Fable keeps its own 128,000. A map that leaked
    // across models would be a global cap wearing a per-model spelling.
    let mut other = cfg_with_engine(
        "anthropic",
        r#"{ "default_model": "anthropic/claude-fable-5",
             "model_output_caps": { "anthropic/claude-sonnet-5": 64000 } }"#,
    );
    other.model_id = "claude-fable-5".to_string();
    assert_eq!(
        crate::agent::engine_config_for(&other).max_output_tokens,
        Some(128_000),
    );
}

/// An override ABOVE the model's ceiling clamps instead of being forwarded.
///
/// Asking a provider for more than the model can write is not a longer answer,
/// it is a rejection on every step. The operator who typed an extra digit
/// meant "as much as possible", and the catalog number already is that.
#[test]
fn a_per_model_cap_above_the_models_ceiling_clamps_to_the_ceiling() {
    let mut cfg = cfg_with_engine(
        "anthropic",
        r#"{ "default_model": "anthropic/claude-sonnet-5",
             "model_output_caps": { "anthropic/claude-sonnet-5": 999999 } }"#,
    );
    cfg.model_id = "claude-sonnet-5".to_string();
    assert_eq!(
        crate::agent::engine_config_for(&cfg).max_output_tokens,
        Some(128_000),
        "clamped to what the model can actually emit",
    );
}

/// A bare slug pins the model on every route; a qualified key pins it on one.
/// Both are real requests — capping a model only on the gateway you pay
/// per-token for is a thing people want — so the qualified form wins when both
/// are present rather than one silently shadowing the other.
#[test]
fn a_bare_slug_caps_every_route_and_a_qualified_key_outranks_it() {
    let mut bare = cfg_with_engine(
        "anthropic",
        r#"{ "default_model": "anthropic/claude-sonnet-5",
             "model_output_caps": { "claude-sonnet-5": 32000 } }"#,
    );
    bare.model_id = "claude-sonnet-5".to_string();
    assert_eq!(
        crate::agent::engine_config_for(&bare).max_output_tokens,
        Some(32_000),
    );

    let mut both = cfg_with_engine(
        "anthropic",
        r#"{ "default_model": "anthropic/claude-sonnet-5",
             "model_output_caps": {
                 "claude-sonnet-5": 32000,
                 "anthropic/claude-sonnet-5": 48000
             } }"#,
    );
    both.model_id = "claude-sonnet-5".to_string();
    assert_eq!(
        crate::agent::engine_config_for(&both).max_output_tokens,
        Some(48_000),
        "the provider-qualified entry is the more specific request",
    );
}

/// `--max-output-tokens` outranks BOTH settings surfaces, on every role.
///
/// It is the most specific statement available — this invocation, right now —
/// and it is what an operator reaches for when a configured value is the thing
/// going wrong. A flag a settings file could quietly beat would be useless in
/// exactly that moment.
#[test]
fn the_max_output_tokens_flag_outranks_every_configured_cap() {
    // Both settings surfaces set, and disagreeing with each other.
    let mut cfg = cfg_with_engine(
        "anthropic",
        r#"{ "default_model": "anthropic/claude-sonnet-5",
             "model_output_caps": { "anthropic/claude-sonnet-5": 64000 },
             "agents": { "default": { "params": { "max_tokens": 48000 } },
                         "verifier":   { "params": { "max_tokens": 48000 } } } }"#,
    );
    cfg.model_id = "claude-sonnet-5".to_string();
    cfg.max_output_tokens = Some(24_000);

    assert_eq!(
        crate::agent::engine_config_for(&cfg).max_output_tokens,
        Some(24_000),
        "the flag must beat both the per-model cap and the per-agent tuning",
    );
    // Every role, for the reason the turn budget is every-role: what it bounds
    // is the process's spend. Capping the worker while the verifier kept the
    // model's whole ceiling would move the cost rather than reduce it.
    assert_eq!(
        crate::agent::verifier_engine_config_for(&cfg).max_output_tokens,
        Some(24_000),
    );
}

/// The flag clamps too. It is the likeliest of the three to carry a
/// fat-fingered digit — typed fresh each run, with no file to review it — and
/// over-asking is a rejection on every step, not a longer answer.
#[test]
fn the_max_output_tokens_flag_clamps_to_the_models_ceiling() {
    let mut cfg = cfg_for("anthropic");
    cfg.model_id = "claude-sonnet-5".to_string();
    cfg.max_output_tokens = Some(900_000);

    assert_eq!(
        crate::agent::engine_config_for(&cfg).max_output_tokens,
        Some(128_000),
    );
}

/// Absent flag changes nothing — the model's own maximum still stands. Without
/// this the two tests above would pass just as well if the flag were applied
/// unconditionally with some default.
#[test]
fn no_flag_leaves_the_models_own_ceiling_in_place() {
    let mut cfg = cfg_for("anthropic");
    cfg.model_id = "claude-sonnet-5".to_string();
    assert_eq!(cfg.max_output_tokens, None, "fixture must not set the flag");
    assert_eq!(
        crate::agent::engine_config_for(&cfg).max_output_tokens,
        Some(128_000),
    );
}

/// `0` is a mistake here, not an opt-out — unlike `model_timeout_secs`, where
/// it spells "no ceiling". The field's ABSENCE already means "the model's own
/// maximum", so zero has no meaning left to carry, and honoring it literally
/// would ask the provider for an empty completion on every step.
#[test]
fn a_zero_per_model_cap_is_ignored_rather_than_sent() {
    let mut cfg = cfg_with_engine(
        "anthropic",
        r#"{ "default_model": "anthropic/claude-sonnet-5",
             "model_output_caps": { "anthropic/claude-sonnet-5": 0 } }"#,
    );
    cfg.model_id = "claude-sonnet-5".to_string();
    assert_eq!(
        crate::agent::engine_config_for(&cfg).max_output_tokens,
        Some(128_000),
        "a zero cap falls back to the model's own maximum",
    );
}

/// #1847: post-turn reflection must dispatch on the configured cheap tier.
///
/// It ran on the WORKER model from the day it shipped — with `memory.rs`
/// calling it "one cheap model call" the whole time — because no reflection
/// pin existed anywhere in the role wiring. The route rides the triage
/// declaration (`pipeline_triage_model`/`agents.triage.*`): the posture's one
/// existing statement of "the cheap, fast model", so there is no second knob
/// for the two to drift apart on.
#[test]
fn reflection_routes_to_the_configured_triage_model() {
    let cfg = cfg_with_engine(
        "zai", // session default (the worker): zai/glm-5.2
        r#"{ "pipeline_triage_model": "deepseek/deepseek-chat" }"#,
    );
    let configured = vec![configured_provider("zai"), configured_provider("deepseek")];

    let route = reflection_route(&cfg, &configured)
        .expect("a credentialed triage pin must route reflection off the worker");
    let (model, provider) = route
        .provider
        .as_ref()
        .expect("a different model needs its own adapter");
    assert_eq!(*model, ModelRef::new("deepseek", "deepseek-chat"));
    assert_eq!(
        provider.id(),
        "deepseek",
        "the routed adapter must be the triage provider's, not the worker's"
    );
}

/// #2174 witness: the triage agent's configured thinking posture reaches the
/// reflection request, on both axes.
///
/// Reflection dispatches on the model the triage pin selected (#1847) and then
/// built its request with `reasoning: None`, so `agents.triage.reasoning: off`
/// chose the model for a call it could not reach. On a reasoning model that
/// meant a thinking stream billed against reflection's output cap — the
/// mechanism behind execution 63's `finish_reason: length` at exactly 2,048
/// tokens and a nine-day frozen learning plane.
#[test]
fn the_triage_posture_governs_the_reflection_request() {
    let cfg = cfg_with_engine(
        "zai",
        r#"{ "pipeline_triage_model": "deepseek/deepseek-chat",
             "agents": { "triage": { "reasoning": "off", "effort": "high" } } }"#,
    );
    let configured = vec![configured_provider("zai"), configured_provider("deepseek")];

    let route = reflection_route(&cfg, &configured).expect("routable triage pin");
    assert_eq!(
        route.posture,
        crate::memory::ReflectionPosture {
            reasoning: Some(false),
            effort: Some(stella_protocol::ReasoningEffort::High),
        },
        "the posture the pin declares must ride out with the route"
    );

    // And the same-model case, which builds no second adapter: the pin still
    // selected this model, so its posture still governs the call.
    let same_model = cfg_with_engine(
        "zai",
        r#"{ "pipeline_triage_model": "zai/glm-5.2",
             "agents": { "triage": { "reasoning": "off" } } }"#,
    );
    let route = reflection_route(&same_model, &[configured_provider("zai")])
        .expect("a pin naming the session default is still a triage pin");
    assert!(
        route.provider.is_none(),
        "the worker adapter already serves that exact model — a duplicate \
         instance would buy only a second connection pool"
    );
    assert_eq!(route.posture.reasoning, Some(false));
}

/// Every unroutable shape rides the worker — `None`, the exact pre-#1847
/// dispatch — because reflection is best-effort by contract and a
/// configuration problem must never cost the turn its learning.
#[test]
fn reflection_rides_the_worker_when_the_triage_tier_is_unroutable() {
    // The stock posture: no engine settings at all.
    let stock = cfg_for("zai");
    assert!(reflection_route(&stock, &[configured_provider("zai")]).is_none());

    // A triage pin whose provider has no resolvable credential.
    let uncredentialed = cfg_with_engine(
        "zai",
        r#"{ "pipeline_triage_model": "deepseek/deepseek-chat" }"#,
    );
    assert!(reflection_route(&uncredentialed, &[configured_provider("zai")]).is_none());

    // A triage pin naming the session default model rides the worker ADAPTER
    // and is still a route: no duplicate connection pool, but the pin's
    // posture still governs the call (asserted above, #2174).
    let same_model = cfg_with_engine("zai", r#"{ "pipeline_triage_model": "zai/glm-5.2" }"#);
    let route = reflection_route(&same_model, &[configured_provider("zai")])
        .expect("a pin naming the session default is still a triage pin");
    assert!(route.provider.is_none());
}
