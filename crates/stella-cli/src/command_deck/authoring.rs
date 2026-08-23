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
    started_unix: i64,
    messages: &[CompletionMessage],
    reflect_start: usize,
    friction: &crate::memory::TurnFriction,
    provider: &dyn Provider,
    cfg: &Config,
    budget: &mut BudgetGuard,
    in_tx: &UnboundedSender<Inbound>,
) {
    crate::agent::record_turn_episode(
        memory,
        prompt,
        outcome,
        started_unix,
        &messages[reflect_start..],
    )
    .await;
    let turn = &messages[reflect_start..];
    if outcome.is_err() || !turn_warrants_reflection(turn) {
        return;
    }
    let Some(memory) = memory else { return };
    // Transcript AND the lane's own folded ledger (#3962). The transcript
    // carries every tool call and its typed result, which is what the digest
    // selects on; only the ledger carries what the turn cost, how long each
    // tool took, and whether it retried or looped — none of which any
    // `CompletionMessage` records. It is one ledger because a lead turn is one
    // turn: the several-turn case is `/goal`, and it passes a slice.
    //
    // The transcript is this turn's slice — the same one the gate above read.
    // Handing over the whole session made the deck's reflection describe the
    // session rather than the execution its row is keyed to (#4382), exactly
    // as `agent::reflect::reflect_on_interactive_turn` did.
    let mut report = crate::memory::reflect_routed(
        memory,
        cfg,
        provider,
        crate::memory::TurnEvidence::with_friction(turn, friction, true),
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
        // Unstated on purpose: `AgentAuthor`'s output contract is declared once
        // at the chokepoint (`accounted_call::standalone_bounds`) and arrives
        // with reasoning headroom on top. This call site has no per-call reason
        // for a number — the 1,200 it used to send was one picked in isolation,
        // below the median definition it asks the model to write (#2444).
        max_output_tokens: None,
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

/// The delete path: the canonical definition and every archived version go
/// (`agents_installed::remove_agent`). Returns the pane's status line.
pub(super) fn delete_agent(root: &std::path::Path, name: &str, scope: AgentScope) -> String {
    let dir = match crate::agents_installed::agents_dir_for(scope, root) {
        Ok(dir) => dir,
        Err(e) => return format!("delete failed: {e}"),
    };
    let Some(slug) = crate::agents_installed::find_slug(&dir, name) else {
        return format!(
            "no installed agent named {name} at the {} scope",
            scope.label()
        );
    };
    match crate::agents_installed::remove_agent(&dir, &slug) {
        Ok(true) => format!("deleted {name} ({} scope) and its versions", scope.label()),
        Ok(false) => format!("{name} was already gone"),
        Err(e) => format!("delete failed: {e}"),
    }
}

/// The persona block an assumed agent contributes to the system prompt: the
/// same words a `/name task` invocation opens with, minus the task.
pub(super) fn assumed_persona(
    root: &std::path::Path,
    name: &str,
    scope: AgentScope,
) -> Result<String, String> {
    let dir = crate::agents_installed::agents_dir_for(scope, root)?;
    let entry = crate::agents_installed::discover(
        crate::agents_installed::user_agents_dir().as_deref(),
        &crate::agents_installed::project_agents_dir(root),
    )
    .into_iter()
    .find(|e| e.name == name && e.scope == scope)
    .ok_or_else(|| format!("no installed agent named {name} in {}", dir.display()))?;
    let body = entry
        .content
        .splitn(3, "---")
        .nth(2)
        .unwrap_or(&entry.content)
        .trim()
        .to_string();
    let mut out = format!(
        "You are acting as the following agent for this whole session.\n\n# Agent: {}\n{}\n\n{body}",
        entry.name, entry.description
    );
    if let Some(tools) = &entry.tools {
        out.push_str(&format!(
            "\n\nThis agent's toolbelt is restricted to: {}.",
            tools.join(", ")
        ));
    }
    Ok(out)
}

/// The edit-save path: archive-then-write a NEW version and pin it (see
/// `agents_installed::save_new_version`). Returns the pane's status line.
pub(super) fn save_agent(
    root: &std::path::Path,
    name: &str,
    scope: AgentScope,
    content: &str,
) -> String {
    let dir = match crate::agents_installed::agents_dir_for(scope, root) {
        Ok(dir) => dir,
        Err(e) => return format!("save failed: {e}"),
    };
    let slug = crate::agents_installed::find_slug(&dir, name)
        .unwrap_or_else(|| crate::agents_installed::slugify(name));
    match crate::agents_installed::save_new_version(&dir, &slug, content) {
        Ok(version) => format!(
            "saved {name} — v{version} is now pinned (previous versions preserved under \
             .versions/{slug}/)"
        ),
        Err(e) => format!("save failed: {e}"),
    }
}

/// The pin-set path: re-point the pin at an existing version — never
/// creates one. Returns the pane's status line.
pub(super) fn pin_agent(
    root: &std::path::Path,
    name: &str,
    scope: AgentScope,
    version: u32,
) -> String {
    let dir = match crate::agents_installed::agents_dir_for(scope, root) {
        Ok(dir) => dir,
        Err(e) => return format!("pin failed: {e}"),
    };
    let Some(slug) = crate::agents_installed::find_slug(&dir, name) else {
        return format!(
            "no installed agent named {name} at the {} scope",
            scope.label()
        );
    };
    match crate::agents_installed::pin_version(&dir, &slug, version) {
        Ok(()) => format!("{name} pinned to v{version} — no new version written"),
        Err(e) => format!("pin failed: {e}"),
    }
}

/// Cap on the free-text `reason` stamped on an agent-use telemetry row.
const AGENT_USE_REASON_MAX: usize = 120;

/// Record the agent-usage telemetry for a `/agent-name task…` invocation:
/// resolution mirrors `CustomExtensions::expand` (commands shadow skills
/// shadow agents — only a real agent invocation records), `version` is the
/// definition's pinned version at this moment, `reason` is the task
/// snippet. The row rides the registry's ledger and is drained into
/// store.db by `agent::record_execution_end` under the execution the
/// expanded prompt runs as.
pub(super) fn record_agent_invocation(
    input: &str,
    custom: &crate::extensions::CustomExtensions,
    registry: &ToolRegistry,
) {
    let trimmed = input.trim();
    let (head, args) = match trimmed.split_once(char::is_whitespace) {
        Some((head, args)) => (head, args),
        None => (trimmed, ""),
    };
    if let Some(crate::extensions::Invocation::Agent(agent)) = custom.lookup(head) {
        let version = crate::agents_installed::active_version_for_source(&agent.source_path);
        let reason: String = args.trim().chars().take(AGENT_USE_REASON_MAX).collect();
        registry.record_agent_use(&agent.name, version, &reason);
    }
}
