//! Accounted LLM authoring operations surfaced by the command deck.

use stella_model::provider::Provider;
use stella_protocol::CompletionRequest;
use stella_tui::{AgentScope, Inbound};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::Config;
use crate::memory::ReflectionReport;

use super::{LEAD, agents_list_inbound};

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

pub(super) async fn handle_agent_create(
    description: &str,
    scope: AgentScope,
    cfg: &Config,
    provider: &dyn Provider,
    budget_limit: Option<f64>,
    in_tx: &UnboundedSender<Inbound>,
) {
    let status = match create_agent(description, scope, cfg, provider, budget_limit).await {
        Ok(status) => status,
        Err(error) => format!("agent creation failed: {error}"),
    };
    let _ = in_tx.send(agents_list_inbound(&cfg.workspace_root, Some(status)));
}

async fn create_agent(
    description: &str,
    scope: AgentScope,
    cfg: &Config,
    provider: &dyn Provider,
    budget_limit: Option<f64>,
) -> Result<String, String> {
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
    Ok(format!(
        "created {} ({} scope) at {} — v1 pinned (${:.6})",
        agent.name,
        scope.label(),
        path.display(),
        accounted.cost_usd,
    ))
}
