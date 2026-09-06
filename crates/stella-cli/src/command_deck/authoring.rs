//! Accounted LLM authoring operations surfaced by the command deck.

use stella_core::BudgetGuard;
use stella_model::provider::Provider;
use stella_protocol::{CompletionMessage, CompletionRequest};
use stella_tools::ToolRegistry;
use stella_tui::{AgentScope, Inbound};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::Config;
use crate::memory::{ReflectionReport, SessionMemory, should_reflect_on, turn_warrants_reflection};

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

/// The deck's reflection gate, pulled out of [`record_and_reflect_turn`] so
/// it has its own witness test.
///
/// [`should_reflect_on`] admits a failed turn. It excludes only a user's
/// soft stop. [`crate::agent::should_reflect_on_turn`] adds the tool-use
/// gate, the memory check, and the operator's opt-out. `agent/reflect.rs`'s
/// `reflect_on_interactive_turn` uses this same two-part rule for the plain
/// prompt handler and `/goal`. Before this fix the deck used neither part:
/// it read `outcome.is_err()` alone and returned before a failed turn ever
/// reached reflection.
///
/// `opted_out` is a parameter, not an environment read, because the switch
/// is process-global and this test module runs in parallel. A test that
/// read the environment directly would change other tests' answers.
fn deck_should_reflect(
    outcome: &Result<(), crate::failure::CliFailure>,
    turn: &[CompletionMessage],
    has_memory: bool,
    opted_out: bool,
) -> bool {
    should_reflect_on(outcome)
        && crate::agent::should_reflect_on_turn(
            turn_warrants_reflection(turn),
            has_memory,
            opted_out,
        )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the driver loop's own end-of-turn state at its single call site, including two \
              simultaneous `&mut` borrows (`memory`, `budget`) of separate locals — a params \
              struct would hold those two borrows, be constructed once, and move the same eleven \
              fields into `command_deck.rs`, which is a god file"
)]
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
    if !deck_should_reflect(
        outcome,
        turn,
        memory.is_some(),
        crate::agent::reflection_explicitly_disabled(),
    ) {
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
        crate::memory::TurnEvidence::with_friction(turn, friction, outcome.is_ok()),
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

/// An assumed agent's session-facing contract: the persona block for the
/// system prompt, and the two scopes the deck now enforces rather than
/// narrates — the `tools:` grant and the declared `model:`.
pub(super) struct AssumedAgent {
    /// The persona block — the same words a `/name task` invocation opens
    /// with, minus the task.
    pub(super) persona: String,
    /// The `tools:` grant; `None` restricts nothing.
    pub(super) tools: Option<Vec<String>>,
    /// The declared `model:` spec; `None` rides the session's model.
    pub(super) model: Option<String>,
}

/// Load the installed definition `name` at `scope` as an [`AssumedAgent`].
pub(super) fn assumed_agent(
    root: &std::path::Path,
    name: &str,
    scope: AgentScope,
) -> Result<AssumedAgent, String> {
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
    let mut persona = format!(
        "You are acting as the following agent for this whole session.\n\n# Agent: {}\n{}\n\n{body}",
        entry.name, entry.description
    );
    if let Some(tools) = &entry.tools {
        persona.push_str(&format!(
            "\n\nThis agent's toolbelt is restricted to: {}.",
            tools.join(", ")
        ));
    }
    // The declared model comes off the pinned content's frontmatter — the
    // same parse the loader runs, so the picker and a `/name task`
    // invocation can never read the key differently.
    let model = crate::extensions::plan::agent_from_file(&entry.source_path, &entry.content)
        .ok()
        .and_then(|def| def.model);
    Ok(AssumedAgent {
        persona,
        tools: entry.tools.clone(),
        model,
    })
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

#[cfg(test)]
mod tests {
    use stella_protocol::{CompletionMessage, MessageRole, ToolCall};

    use super::deck_should_reflect;
    use crate::failure::CliFailure;

    /// One assistant message with a tool call — enough for
    /// `turn_warrants_reflection` to admit the turn.
    fn tool_using_turn() -> Vec<CompletionMessage> {
        vec![CompletionMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                call_id: "call-1".to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({"path": "src/lib.rs"}),
            }],
            tool_results: Vec::new(),
            attachments: Vec::new(),
        }]
    }

    /// **Witness.** A failed deck turn earns a reflection call.
    ///
    /// Before this fix, `record_and_reflect_turn` always bailed on
    /// `outcome.is_err()`. A failed lead turn never reached `reflect_routed`.
    /// This test checks the gate agrees with `should_reflect_on`: a failure
    /// that is not a soft stop passes, the same as it does for the plain
    /// prompt handler (`agent::reflect::reflect_on_interactive_turn`).
    #[test]
    fn a_failed_turn_that_is_not_a_soft_stop_reflects() {
        let outcome: Result<(), CliFailure> = Err(CliFailure::error("the tool crashed"));
        let turn = tool_using_turn();

        assert!(
            deck_should_reflect(&outcome, &turn, true, false),
            "a failed, non-soft-stop turn with tool use and memory present must reflect"
        );
    }

    /// A user's soft stop is not a failure to learn from — the one exclusion
    /// `should_reflect_on` carries, and the deck must honor it exactly as
    /// every other reflecting door does.
    #[test]
    fn a_user_soft_stop_does_not_reflect() {
        let outcome: Result<(), CliFailure> =
            Err(CliFailure::deliberate_stop(stella_core::SOFT_STOP_REASON));
        let turn = tool_using_turn();

        assert!(
            !deck_should_reflect(&outcome, &turn, true, false),
            "a soft stop must not be recorded as a failure"
        );
    }

    /// A successful turn that used a tool still reflects — the fix must not
    /// have narrowed the success path while widening the failure one.
    #[test]
    fn a_successful_tool_using_turn_still_reflects() {
        let outcome: Result<(), CliFailure> = Ok(());
        let turn = tool_using_turn();

        assert!(deck_should_reflect(&outcome, &turn, true, false));
    }

    /// A turn with no tool calls earns no reflection call regardless of
    /// outcome — the tool-use gate `should_reflect_on_turn` folds in is
    /// unchanged by this fix.
    #[test]
    fn a_turn_with_no_tool_calls_does_not_reflect() {
        let failed: Result<(), CliFailure> = Err(CliFailure::error("boom"));
        let turn = vec![CompletionMessage::assistant("done, no tools needed")];

        assert!(!deck_should_reflect(&failed, &turn, true, false));
    }

    /// The operator's opt-out withholds the call even from a failed,
    /// tool-using turn — the same `opted_out` plumbing every reflecting door
    /// honors.
    #[test]
    fn the_opt_out_withholds_reflection_on_a_failed_turn() {
        let outcome: Result<(), CliFailure> = Err(CliFailure::error("the tool crashed"));
        let turn = tool_using_turn();

        assert!(!deck_should_reflect(&outcome, &turn, true, true));
    }
}
