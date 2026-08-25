//! Session-level slash command dispatch (`run_deck_command`) and the
//! installed-agents manager's synchronous ops (`handle_agents_input`).
//!
//! Split out of `command_deck.rs` (closed to growth) the way `skills.rs` and
//! `authoring.rs` were — this is the deck's `/command` vocabulary itself,
//! genuinely self-contained behind the driver loop's two call sites.

use stella_model::provider::Provider;
use stella_protocol::{AgentEvent, CompletionMessage};
use stella_tools::ToolRegistry;
use stella_tui::{Inbound, WorkspaceInput};
use tokio::sync::mpsc::UnboundedSender;

use super::authoring::agents_list_inbound;
use super::panel_snapshots::engine_config_inbound;
use super::{
    LEAD, add_dir, authoring, command_side, init_cmd, model_cmd, profile_cmd, settings_io, skills,
};
use crate::config::Config;
use crate::interactive::AskUserIo;

/// The disposition of a would-be slash command.
pub(super) enum DeckCommand {
    /// Not a command — run the model turn as usual.
    Prompt,
    /// A custom command/skill invocation — run the model turn with this
    /// expanded prompt instead of the raw `/name args` input.
    Expanded(String),
    /// Handled as a command; skip the model turn.
    Handled,
    /// `/init` finished successfully; skip the turn AND refresh the session's
    /// derived state (memory domains, Graph tab, custom extensions) which the
    /// new taxonomy/index changed.
    InitCompleted,
    /// `/model <provider/slug>` typed in full: skip the turn and apply the
    /// session-only model switch. Carried back to the driver loop rather
    /// than applied in `run_deck_command`, because the switch moves state
    /// only the loop owns (the provider handle, the prompt plane, the lead's
    /// registered meta) — see `session_override`.
    SessionModel(String),
}

// The deck's productized vocabulary (`DECK_BUILTINS`) and the
// reserved-name guard (`deck_reserved`) live in `skills`, beside the
// slash-menu builder that consumes them (the god-file rule).

/// An argument-carrying form of `/info` (né `/models` — the old head still
/// parses) — handled model-free: when the configured model itself is
/// broken, `/info refresh` is how the user digs out, and routing it into a
/// model turn fails on the very error being fixed. Parsed conservatively —
/// a single recognized token (plus `refresh --force`); anything
/// sentence-like stays a prompt, matching the "`/init do the thing` is a
/// model prompt" rule.
pub(super) enum ModelsCommand {
    /// `/info refresh [--force]` — re-sync the catalog, no model call.
    Refresh { force: bool },
    /// `/info list` — the same listing the bare `/info` prints.
    List,
    /// `/info <typo>` — one unrecognized token: a mistyped subcommand,
    /// answered with usage instead of a wasted model call.
    Usage(String),
}

/// Parse `trimmed` as a [`ModelsCommand`]; `None` leaves it on the normal
/// path (custom expansion, then prompt).
pub(super) fn parse_models_command(trimmed: &str) -> Option<ModelsCommand> {
    let (head, rest) = trimmed.split_once(char::is_whitespace)?;
    let rest = rest.trim();
    if !matches!(head, "/info" | "/models") || rest.is_empty() {
        return None;
    }
    let mut words = rest.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some("refresh"), None, None) => Some(ModelsCommand::Refresh { force: false }),
        (Some("refresh"), Some("--force"), None) => Some(ModelsCommand::Refresh { force: true }),
        (Some("list"), None, None) => Some(ModelsCommand::List),
        (Some(word), None, None) => Some(ModelsCommand::Usage(word.to_string())),
        // A sentence after `/models` stays a prompt.
        _ => None,
    }
}

// ── Installed-agents manager (the AGENTS tab's INSTALLED AGENTS pane) ───────

/// Handle one synchronous installed-agents op (refresh / save / pin) —
/// pure filesystem work, answered with a fresh [`Inbound::AgentsList`].
/// Called from BOTH the idle and the in-turn recv sites, so the manager
/// works whether or not a turn is running. Returns `true` when the input
/// was one of the manager's; anything else is left to the caller's arms.
pub(super) fn handle_agents_input(
    input: &WorkspaceInput,
    cfg: &Config,
    in_tx: &UnboundedSender<Inbound>,
) -> bool {
    let root = &cfg.workspace_root;
    match input {
        WorkspaceInput::AgentsRefresh => {
            let _ = in_tx.send(agents_list_inbound(root, None));
            true
        }
        WorkspaceInput::AgentSave {
            name,
            scope,
            content,
        } => {
            let status = authoring::save_agent(root, name, *scope, content);
            let _ = in_tx.send(agents_list_inbound(root, Some(status)));
            true
        }
        WorkspaceInput::AgentPin {
            name,
            scope,
            version,
        } => {
            let status = authoring::pin_agent(root, name, *scope, *version);
            let _ = in_tx.send(agents_list_inbound(root, Some(status)));
            true
        }
        WorkspaceInput::AgentDelete { name, scope } => {
            let status = authoring::delete_agent(root, name, *scope);
            let _ = in_tx.send(agents_list_inbound(root, Some(status)));
            true
        }
        _ => false,
    }
}

/// Handle a session-level slash command. Output goes into the lead agent's
/// transcript as `Text` events — the deck renders exclusively from events, so
/// printing to stdout (which the alternate screen owns) is never an option.
///
/// Vocabulary: `/help`, `/clear`, `/info`, `/model`, `/init`, `/agents`.
/// `/files`, `/diff`, `/graph` are deck-local (tab switches) and
/// consumed in interactive mode; an unknown bare `/command` gets a hint rather than a
/// wasted model call. Every productized command is no-argument, so the
/// *whole* trimmed input is matched — `/init do the thing` is a model prompt,
/// not a silent reindex that discards the rest. Custom commands/skills (⚡)
/// DO take arguments: `/fix-bug issue-42` expands the `fix-bug` template
/// with `issue-42`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_deck_command(
    prompt: &str,
    in_tx: &UnboundedSender<Inbound>,
    messages: &mut Vec<CompletionMessage>,
    system_prompt: &str,
    provider: &dyn Provider,
    registry: &ToolRegistry,
    cfg: &mut Config,
    custom: &crate::extensions::CustomExtensions,
    budget_limit: Option<f64>,
    // This deck's session registry id — what scopes `/export` to the session
    // the user is actually in (#2558).
    session_id: &str,
    // The deck's question channel, so `/init`'s first-session conversion
    // offer raises a card instead of a TTY prompt through the render.
    ask_io: &dyn AskUserIo,
) -> DeckCommand {
    let trimmed = prompt.trim();
    if !trimmed.starts_with('/') {
        return DeckCommand::Prompt;
    }
    let say = |text: String| {
        let _ = in_tx.send(Inbound::Event {
            agent: LEAD.to_string(),
            event: AgentEvent::Text { text },
        });
    };
    // The queue-free commands live in `command_side`, shared with the two
    // `WorkspaceInput::Command` arms so a mid-turn `/export` and a queued
    // one from an old journal run the same code. Asked first: whatever it
    // recognizes never reaches the arms below.
    if command_side::run(trimmed, cfg, in_tx, session_id) {
        return DeckCommand::Handled;
    }
    match trimmed {
        "/clear" => {
            // Reset the driver's own LLM history…
            messages.clear();
            messages.push(CompletionMessage::system(system_prompt.to_string()));
            // …and the deck's session view: blank the transcript (including the
            // `/clear` echo the paired PromptStarted just pushed), zero the cost
            // stat, and return the progress bar to idle. No `say()` — that would
            // re-populate the transcript we are clearing.
            let _ = in_tx.send(Inbound::SessionReset {
                agent: LEAD.to_string(),
            });
        }
        // Bare `/model` is normally consumed deck-side (it opens the session
        // model picker); a queued or replayed one lands here and gets the
        // textual summary instead of silence. NOT queue-free
        // (`command_side`): `/model <spec>` below switches the running
        // session, so the whole command stays on the turn-coupled path
        // rather than having its bare form answer from a different place.
        "/model" => {
            say(model_cmd::current_summary(cfg));
        }
        "/init" => {
            // The splash replay, the narrator, and the question channel all
            // live in `init_cmd` — this file is closed to growth.
            match init_cmd::run(
                provider,
                &cfg.workspace_root,
                &cfg.model_id,
                budget_limit,
                ask_io,
                in_tx,
                LEAD,
            )
            .await
            {
                Ok(()) => return DeckCommand::InitCompleted,
                Err(e) => say(format!("init failed: {e}")),
            }
        }
        "/reload" => say(settings_io::reload_command(cfg, in_tx)),
        // Deck-local commands (tab switches, `/agents` opening the Agents
        // tab, the transcript-page overlays) are normally consumed in
        // interactive mode, but a queued one reaches here — accept it as handled (a no-op)
        // rather than calling it "unknown".
        "/files" | "/diff" | "/graph" | "/agents" | "/agent" | "/skills" | "/mcp"
        | "/mcp-search" | "/settings" | "/sessions" | "/subagents" | "/context" | "/inspect"
        | "/inbox" => {}
        _ => {
            if let Some(reply) = add_dir::handle(trimmed, cfg, registry) {
                say(reply);
                return DeckCommand::Handled;
            }
            // `/model <provider/slug>` — switch THIS session's model (the
            // typed twin of the picker); `/model default <provider/slug>` —
            // persist the default for future sessions. Validation + the
            // settings write live in `model_cmd` (parity with the SETTINGS
            // tab); handled before the whitespace check below, which would
            // otherwise mistake `/model x` for a prompt.
            if let Some(command) = model_cmd::parse_model_command(trimmed) {
                match command {
                    model_cmd::ModelCommand::Usage => say(
                        "usage: `/model <provider/slug>` switches this session's model \
                         (e.g. `/model zai/glm-5.2`); `/model default <provider/slug>` \
                         persists the default for new sessions; `/model` alone opens \
                         the picker."
                            .to_string(),
                    ),
                    model_cmd::ModelCommand::Override(id) => {
                        return DeckCommand::SessionModel(id);
                    }
                    model_cmd::ModelCommand::Default(id) => {
                        match model_cmd::set_default_model(cfg, &id) {
                            Ok(msg) => {
                                say(msg);
                                // Refresh an open SETTINGS tab with the merged view.
                                let _ = in_tx.send(engine_config_inbound(cfg, None));
                            }
                            Err(msg) => say(msg),
                        }
                    }
                }
                return DeckCommand::Handled;
            }
            // `/profile [name]` — retune every role at once. Claimed here,
            // above the whitespace check below, which would otherwise bill
            // `/profile ultra` as a model prompt.
            if let Some(reply) = profile_cmd::handle(cfg, trimmed) {
                say(reply.message);
                if reply.settings_changed {
                    // Refresh an open SETTINGS tab with the merged view.
                    let _ = in_tx.send(engine_config_inbound(cfg, None));
                }
                return DeckCommand::Handled;
            }
            // A custom command/skill/agent (⚡): expand its template —
            // arguments and all — into the prompt the model turn runs.
            // Reserved names never reach a custom definition (`/init do the
            // thing` stays a model prompt even if a custom `init` exists).
            // An AGENT invocation additionally records a usage-telemetry
            // row (agent, pinned version, task) on the registry's ledger.
            if let Some(expanded) = custom.expand(trimmed, &skills::deck_reserved()) {
                authoring::record_agent_invocation(trimmed, custom, registry);
                return DeckCommand::Expanded(expanded);
            }
            // A bare unknown /word is a typo'd command, not a prompt — say so
            // instead of spending a model call. Anything with arguments (e.g.
            // `/src/main.rs explain`) falls through and stays a prompt.
            if trimmed.contains(char::is_whitespace) {
                return DeckCommand::Prompt;
            }
            say(format!(
                "unknown command `{trimmed}` — try /help, /clear, /info, /model, /agent, /theme, /init, /agents, /export, /donate, /files, /diff, /graph"
            ));
        }
    }
    DeckCommand::Handled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deck_arg_commands_parse_models_forms_and_leave_sentences_as_prompts() {
        assert!(matches!(
            parse_models_command("/models refresh"),
            Some(ModelsCommand::Refresh { force: false })
        ));
        assert!(matches!(
            parse_models_command("/models refresh --force"),
            Some(ModelsCommand::Refresh { force: true })
        ));
        assert!(matches!(
            parse_models_command("/models list"),
            Some(ModelsCommand::List)
        ));
        // One unrecognized token is a typo'd subcommand → usage, never a
        // model call; a sentence stays a prompt.
        assert!(matches!(
            parse_models_command("/models refrsh"),
            Some(ModelsCommand::Usage(_))
        ));
        assert!(parse_models_command("/models what can I use").is_none());
        // Bare forms and non-command paths are not arg commands — and the
        // removed `/model-<role>` heads no longer parse (model config lives
        // on the SETTINGS tab).
        assert!(parse_models_command("/models").is_none());
        assert!(parse_models_command("/model-default zai/glm-5.2").is_none());
        assert!(parse_models_command("/src/main.rs explain").is_none());
    }
}
