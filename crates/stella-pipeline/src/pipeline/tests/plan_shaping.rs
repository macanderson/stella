//! #2416: the plan call's wire shape and its share of `agents.worker`.
//!
//! Two defects, one call site. The planner was the last raw pipeline role
//! sending one undifferentiated user message, so its fixed opener had nowhere
//! cache-markable to ride; and `plan_stage` carried a comment claiming plan
//! "rides the worker's settings" while handing `metered_raw_call` a
//! `RoleCallOverrides::default()`, so the model routing rode the worker's
//! settings and none of the request shaping did.
//!
//! `prompt` is the field that matters, and the reason is structural: every
//! other knob (`temperature`, `effort`, `reasoning`, `params`) also reaches
//! the plan call through `PipelineConfig::engine`, which the CLI already
//! builds from the worker's own tuning — but an `EngineConfig` has no
//! system-message seat, so operator prose reached worker turns and stopped at
//! the planner that writes the worker's work order.

use super::*;

/// The messages of the Nth provider call, as `(role, text)` pairs.
fn call(provider: &ScriptedProvider, index: usize) -> Vec<(stella_protocol::MessageRole, String)> {
    provider
        .shapes()
        .get(index)
        .unwrap_or_else(|| panic!("call {index} was never made"))
        .clone()
}

/// Drive one multi-step run through triage → plan → a one-step walk, with a
/// tracked test command so verification never spends a model call the
/// assertions would have to account for. `overrides` shapes the plan row the
/// stage reads, and the worker row it is resolved from.
async fn plan_call_with(
    overrides: RoleCallOverrides,
    plan_reply: CompletionResult,
    extra: Vec<CompletionResult>,
) -> ScriptedProvider {
    let mut script = vec![text_result("multi"), plan_reply];
    script.extend(extra);
    script.push(text_result("done"));
    let provider = ScriptedProvider::new(script);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![false, true, true], "@@ -1 +1 @@\n-old\n+new");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, _rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig {
            test_command: Some("cargo test -p x".into()),
            diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
            role_overrides: PipelineRoleOverrides {
                // Both rows, the same value — which is what the CLI hands down
                // for an unconfigured `agents.plan`: the plan row is resolved
                // as `agents.plan` over `agents.worker` field by field, so
                // #2416's property survives research and plan getting rows of
                // their own (#2374).
                plan: overrides.clone(),
                worker: overrides,
                ..PipelineRoleOverrides::default()
            },
            ..PipelineConfig::default()
        },
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    pipeline
        .run(
            "Install and verify nginx end to end",
            &mut messages,
            &mut budget,
        )
        .await
        .expect("run succeeds");
    drop(pipeline);
    provider
}

/// The witness for both halves at once. On `main` the plan call is a single
/// `user` message and the configured worker prompt is nowhere in it.
#[tokio::test]
async fn the_plan_call_splits_its_instructions_and_carries_the_worker_prompt() {
    let provider = plan_call_with(
        RoleCallOverrides {
            prompt: Some("SENTINEL-WORKER-PROMPT: this repo uses pnpm, never npm.".into()),
            ..RoleCallOverrides::default()
        },
        text_result(r#"["do the thing"]"#),
        Vec::new(),
    )
    .await;

    let plan = call(&provider, 1);
    let roles: Vec<_> = plan.iter().map(|(role, _)| *role).collect();
    assert_eq!(
        roles,
        vec![
            stella_protocol::MessageRole::System,
            stella_protocol::MessageRole::System,
            stella_protocol::MessageRole::User,
        ],
        "plan dispatches as [override, instructions, payload]; got {plan:#?}"
    );
    assert!(
        plan[0].1.contains("SENTINEL-WORKER-PROMPT"),
        "`agents.worker.prompt` must reach the planner it writes work orders for"
    );
    assert!(
        plan[1].1.contains("You are the planner"),
        "the override PREPENDS — it must never replace the built-in instructions, \
         which carry the JSON-array contract `parse_plan` reads"
    );
    assert!(plan[1].1.contains("JSON array"));
    assert!(
        plan[2].1.contains("Install and verify nginx end to end"),
        "the goal is volatile and rides the user half"
    );
    for (role, text) in &plan {
        if *role == stella_protocol::MessageRole::System {
            assert!(
                !text.contains("Install and verify nginx end to end"),
                "per-call bytes in the system half would defeat the cacheable prefix"
            );
        }
    }
}

/// The repair call is the plan call's shape, not a regression back to one
/// user message: it re-bills the same fixed instruction block on every
/// repair, so it needs the same stable prefix — and the same operator prose,
/// since it is authoring the same plan.
#[tokio::test]
async fn the_plan_repair_call_keeps_the_same_split_and_override() {
    let provider = plan_call_with(
        RoleCallOverrides {
            prompt: Some("SENTINEL-WORKER-PROMPT".into()),
            ..RoleCallOverrides::default()
        },
        // Prose: `parse_plan` rejects it, so the stage spends its one repair.
        text_result("I will install nginx and then verify it."),
        vec![text_result(r#"["do the thing"]"#)],
    )
    .await;

    let repair = call(&provider, 2);
    let roles: Vec<_> = repair.iter().map(|(role, _)| *role).collect();
    assert_eq!(
        roles,
        vec![
            stella_protocol::MessageRole::System,
            stella_protocol::MessageRole::System,
            stella_protocol::MessageRole::User,
        ],
        "plan repair dispatches as [override, instructions, payload]; got {repair:#?}"
    );
    assert!(repair[0].1.contains("SENTINEL-WORKER-PROMPT"));
    assert!(repair[1].1.contains("could not be parsed as a plan"));
    assert!(
        repair[2]
            .1
            .contains("I will install nginx and then verify it."),
        "the echoed response is per-call and rides the user half"
    );
}

/// An unconfigured worker row leaves the plan call at exactly two messages —
/// the split alone, no empty system message manufactured from a `None`.
#[tokio::test]
async fn an_unconfigured_worker_row_dispatches_two_messages() {
    let provider = plan_call_with(
        RoleCallOverrides::default(),
        text_result(r#"["do the thing"]"#),
        Vec::new(),
    )
    .await;
    let roles: Vec<_> = call(&provider, 1).iter().map(|(role, _)| *role).collect();
    assert_eq!(
        roles,
        vec![
            stella_protocol::MessageRole::System,
            stella_protocol::MessageRole::User,
        ]
    );
}

/// The conversational fast path deliberately does NOT take the worker row: it
/// replaces the engineering persona with `CONVERSATIONAL_SYSTEM_PROMPT`, and
/// prepending the operator's worker prose would re-arm exactly the behaviour
/// that replacement suppresses on a turn that has no task.
#[tokio::test]
async fn the_conversational_path_does_not_carry_the_worker_prompt() {
    // "hi" short-circuits triage deterministically, so the only model call is
    // the chat reply itself.
    let provider = ScriptedProvider::new(vec![text_result("Hello! What can I help with?")]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, _rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig {
            role_overrides: PipelineRoleOverrides {
                worker: RoleCallOverrides {
                    prompt: Some("SENTINEL-WORKER-PROMPT".into()),
                    ..RoleCallOverrides::default()
                },
                ..PipelineRoleOverrides::default()
            },
            ..PipelineConfig::default()
        },
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    pipeline
        .run("hi", &mut messages, &mut budget)
        .await
        .expect("run succeeds");
    drop(pipeline);

    for (_, text) in call(&provider, 0) {
        assert!(
            !text.contains("SENTINEL-WORKER-PROMPT"),
            "the chat path must not re-arm the engineering persona it just replaced"
        );
    }
}

/// #2932 witness: a plan that parses cleanly but names an absolute path
/// spends one bounded repair call restating it declaratively, and the
/// repaired plan — not the original path-naming one — is what the worker
/// sees. Fails on `main`, where a step naming `/app/pyknotid` reaches the
/// worker verbatim with no repair call in between.
#[tokio::test]
async fn a_plan_step_naming_an_absolute_path_is_repaired_before_the_worker_sees_it() {
    let provider = plan_call_with(
        RoleCallOverrides::default(),
        text_result(r#"["Clone the repository into /app/pyknotid"]"#),
        vec![text_result(r#"["Clone the pyknotid repository"]"#)],
    )
    .await;

    let repair = call(&provider, 2);
    let roles: Vec<_> = repair.iter().map(|(role, _)| *role).collect();
    assert_eq!(
        roles,
        vec![
            stella_protocol::MessageRole::System,
            stella_protocol::MessageRole::User,
        ],
        "the path-repair call carries the same split shape as the plan call; got {repair:#?}"
    );
    assert!(
        repair[0].1.contains("no tool access")
            && repair[0].1.contains("absolute filesystem path"),
        "the repair instructions must explain why the path may be wrong: {}",
        repair[0].1
    );
    assert!(
        repair[1].1.contains("/app/pyknotid"),
        "the repair payload must echo the flagged step: {}",
        repair[1].1
    );

    let worker_step = call(&provider, 3);
    let worker_text: String = worker_step.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        !worker_text.contains("/app/pyknotid"),
        "the worker must see the repaired step, not the original path-naming one: {worker_text}"
    );
    assert!(
        worker_text.contains("Clone the pyknotid repository"),
        "the worker must see the repaired step's text: {worker_text}"
    );
}

/// The other direction: a plan with no absolute path spends no extra call —
/// the repair retry is conditional, not a tax on every plan.
#[tokio::test]
async fn a_plan_with_no_absolute_path_skips_the_repair_call() {
    let provider = plan_call_with(
        RoleCallOverrides::default(),
        text_result(r#"["Install and start nginx"]"#),
        Vec::new(),
    )
    .await;
    assert_eq!(
        provider.prompts().len(),
        3,
        "triage, plan, and the one step turn — no path-repair call: {:?}",
        provider.prompts()
    );
}
