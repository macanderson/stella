//! Accounted LLM authoring operations surfaced by the command deck.

use stella_core::BudgetGuard;
use stella_model::provider::Provider;
use stella_protocol::{CompletionMessage, CompletionRequest};
use stella_tools::ToolRegistry;
use stella_tui::{AgentScope, Inbound};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::Config;
use crate::memory::{ReflectionReport, SessionMemory, turn_warrants_reflection};

use super::LEAD;

// ── Installed-agents snapshots ─────────────────────────────────────────────

/// Build an [`Inbound::AgentsList`] from the definitions on disk at both
/// scopes. `status`, when set, replaces the pane's hint line. `creating` is
/// false: every snapshot built here is a settled state (a parked create
/// announces itself via [`agents_list_creating`] instead), and `created` is
/// unset (a completed create attaches the name via [`agents_list_created`]).
pub(super) fn agents_list_inbound(
    workspace_root: &std::path::Path,
    status: Option<String>,
) -> Inbound {
    agents_list_created(workspace_root, status, None)
}

/// [`agents_list_inbound`] with the just-created agent's name attached — the
/// completion form a successful `WorkspaceInput::AgentCreate` answers with, so
/// the deck's create dialog can open the detail preview on that entry.
fn agents_list_created(
    workspace_root: &std::path::Path,
    status: Option<String>,
    created: Option<String>,
) -> Inbound {
    let project = crate::agents_installed::project_agents_dir(workspace_root);
    let user = crate::agents_installed::user_agents_dir();
    Inbound::AgentsList {
        entries: crate::agents_installed::discover(user.as_deref(), &project),
        status,
        creating: false,
        created,
    }
}

/// An [`Inbound::AgentsList`] snapshot with `creating: true` — sent when an
/// LLM-assisted agent creation is accepted but still in flight (parked behind
/// a running turn), so the deck's create dialog keeps its spinner up rather
/// than reading the interim snapshot as completion.
pub(super) fn agents_list_creating(
    workspace_root: &std::path::Path,
    status: Option<String>,
) -> Inbound {
    match agents_list_inbound(workspace_root, status) {
        Inbound::AgentsList {
            entries, status, ..
        } => Inbound::AgentsList {
            entries,
            status,
            creating: true,
            created: None,
        },
        other => other,
    }
}

pub(super) fn forward_reflection_events(
    in_tx: &UnboundedSender<Inbound>,
    report: ReflectionReport,
) {
    for event in report.events {
        let _ = in_tx.send(Inbound::Event {
            agent: LEAD.to_string(),
            event,
        });
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn record_and_reflect_turn(
    memory: &mut Option<SessionMemory>,
    prompt: &str,
    outcome: &Result<(), crate::failure::CliFailure>,
    registry: &ToolRegistry,
    files_before: usize,
    started_unix: i64,
    messages: &[CompletionMessage],
    reflect_start: usize,
    provider: &dyn Provider,
    cfg: &Config,
    budget: &mut BudgetGuard,
    in_tx: &UnboundedSender<Inbound>,
) {
    crate::agent::record_turn_episode(
        memory,
        prompt,
        outcome,
        registry,
        files_before,
        started_unix,
        &messages[reflect_start..],
    )
    .await;
    if outcome.is_err() || !turn_warrants_reflection(&messages[reflect_start..]) {
        return;
    }
    let Some(memory) = memory else { return };
    let mut report = memory
        .reflect_and_record(
            provider,
            &cfg.model_id,
            messages,
            true,
            true,
            crate::agent::remaining_budget(budget),
        )
        .await;
    crate::agent::settle_reflection_budget(&mut report, budget);
    forward_reflection_events(in_tx, report);
}

pub(super) async fn handle_agent_create(
    description: &str,
    scope: AgentScope,
    cfg: &Config,
    provider: &dyn Provider,
    budget_limit: Option<f64>,
    in_tx: &UnboundedSender<Inbound>,
) {
    let (status, created) =
        match create_agent(description, scope, cfg, provider, budget_limit).await {
            Ok((status, name)) => (status, Some(name)),
            Err(error) => (format!("agent creation failed: {error}"), None),
        };
    let _ = in_tx.send(agents_list_created(
        &cfg.workspace_root,
        Some(status),
        created,
    ));
}

/// Draft + install an agent from a description. Returns
/// `(status line, created agent name)` on success.
async fn create_agent(
    description: &str,
    scope: AgentScope,
    cfg: &Config,
    provider: &dyn Provider,
    budget_limit: Option<f64>,
) -> Result<(String, String), String> {
    let request = CompletionRequest {
        messages: crate::agents_installed::creation_messages(description),
        max_output_tokens: Some(1200),
        temperature: Some(0.2),
        effort: None,
        tools: Vec::new(),
        reasoning: None,
        params: None,
    };
    let accounted = crate::accounted_call::complete_standalone(
        &cfg.workspace_root,
        provider,
        stella_protocol::ModelCallRole::AgentAuthor,
        "agent_author",
        &cfg.model_id,
        budget_limit,
        request,
    )
    .await
    .map_err(|error| {
        format!(
            "draft call failed: {} (${:.6})",
            error.message, error.cost_usd
        )
    })?;
    let agent = crate::agents_installed::parse_generated_agent(&accounted.result.text)?;
    let dir = crate::agents_installed::agents_dir_for(scope, &cfg.workspace_root)?;
    let path = crate::agents_installed::install_new_agent(&dir, &agent)?;
    Ok((
        format!(
            "created {} ({} scope) at {} — v1 pinned (${:.6})",
            agent.name,
            scope.label(),
            path.display(),
            accounted.cost_usd,
        ),
        agent.name,
    ))
}
