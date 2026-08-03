// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella --simple` — the single-pane accessible chat surface (#936).
//!
//! ## Why this exists
//!
//! `stella-tui` has always carried a single-session REPL — `shell::run`, the
//! whole of `ui`, and `render`'s top-level frame composer — and until this
//! module nothing outside the crate's own tests ever called it. That is the
//! one state with the costs of both options: a second implementation of
//! composer layout and gate handling that every deck change has to be mirrored
//! into or knowingly not, plus a few hundred green assertions describing
//! behaviour no user could reach. This driver is the caller that resolves it.
//!
//! ## What it is for
//!
//! Not "the deck with fewer features". The deck is an alternate-screen,
//! full-repaint UI, which is the single most screen-reader-hostile shape a
//! terminal program can take: a reader is handed a rectangle of cells that is
//! rewritten wholesale several times a second, with no notion of "a new line
//! appeared". This surface inverts that. It draws **inline** on the user's own
//! screen and pushes each completed message into normal scrollback exactly
//! once, so the conversation is ordinary terminal output — announced as it
//! arrives, reachable with a review cursor, and readable by every
//! accessibility bridge that already knows how to read a terminal. The panels
//! stack in one column so reading order is document order, and the hardware
//! cursor sits on the real insertion point, which is also what a CJK IME needs
//! to place its candidate window (#935). All of that lives in
//! [`stella_tui::RunOptions::screen_reader`]; this module is the engine half.
//!
//! ## Shape
//!
//! One conversation, one turn at a time — the same engine stack
//! `agent::run_interactive` assembles, with the TUI in place of stdin and
//! `println!`. `AgentEvent`s flow out to the shell over one channel and
//! [`UserInput`]s flow back over another. Two seams need naming:
//!
//! - **ask_user** ([`SimpleAskUserIo`]): raw mode owns stdin, so the plain
//!   REPL's `read_line` prompt cannot work here. The io emits an `AskUser`
//!   event, parks on a channel, and echoes the answer back as that card's own
//!   `ToolResult` — the event-pure clear the fold in `stella_tui::model`
//!   expects. Same contract as the deck's io, one lane instead of many.
//! - **Cancel**: the engine has no abort input, so `Esc`/`Ctrl-C` drops the
//!   in-flight turn future at its next await point and truncates the partial
//!   turn out of the conversation, exactly as the deck does.
//!
//! What it deliberately does not have: tabs, a prompt queue, sub-agents, mid
//! turn steering, durable resume. Those are deck features, and a user who
//! needs them is asked to run the deck. Announcing a surface as accessible and
//! then quietly under-delivering the *conversation* would be the worse
//! failure, so what is here is whole.

use std::sync::Arc;

use async_trait::async_trait;
use stella_core::ports::ToolExecutor;
use stella_core::{BudgetGuard, CalibrationMap, Engine, TurnOutcome};
use stella_model::provider::Provider;
use stella_protocol::{AgentEvent, CompletionMessage, ToolOutput};
use stella_store::Store;
use stella_tools::ToolRegistry;
use stella_tools::custom::{CustomTool, CustomToolSet};
use stella_tools::hook_runner::ShellHookRunner;
use stella_tui::{RunOptions, SlashCommand, UserInput};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::agent;
use crate::config::Config;
use crate::interactive::{AskUserIo, FREE_TEXT_LABEL, InteractiveToolSet, SkillRegistry};
use crate::memory::{SessionMemory, inject_recall_block};
use crate::runtime::TokioSleeper;

/// Ids for the cards [`SimpleAskUserIo`] mints. Its own namespace, like the
/// deck's: a card must be cleared by its own echoed `ToolResult` and never by
/// an unrelated result that happened to reuse the id.
static NEXT_SIMPLE_ASK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The slash vocabulary the `/` popup offers. Deliberately only what this
/// surface actually handles: a menu that lists a command the driver ignores
/// teaches the user that the menu lies, and this is the surface whose users
/// can least afford to go hunting for why nothing happened.
const SIMPLE_BUILTINS: &[(&str, &str)] = &[
    ("/help", "what this surface does and which keys drive it"),
    ("/clear", "forget the conversation, keep the session"),
    ("/files", "list the files this session has touched"),
    ("/exit", "end the session"),
];

/// Names the driver resolves itself, so a custom command can never shadow one
/// (mirrors `agent::REPL_RESERVED`'s role in the plain REPL).
const SIMPLE_RESERVED: &[&str] = &["/help", "/clear", "/files", "/exit", "/quit"];

fn simple_slash_commands(custom: &crate::extensions::CustomExtensions) -> Vec<SlashCommand> {
    let mut commands: Vec<SlashCommand> = SIMPLE_BUILTINS
        .iter()
        .map(|(name, description)| SlashCommand::new(*name, *description))
        .collect();
    let customs = custom.slash_entries(&commands);
    commands.extend(customs);
    commands
}

/// Run one `--simple` session to completion.
///
/// Returns when the user quits, the shell's input stream ends, or the shell
/// itself fails — always after the terminal has been restored (the shell's
/// guard owns that, including on a panic unwind).
pub(crate) async fn run_simple_session(
    cfg: &Config,
    budget_limit: Option<f64>,
) -> Result<(), String> {
    // Same authority posture as the plain REPL: one interactive conversation
    // driven by a human at a terminal. The surface differs, the authority
    // question does not.
    crate::enterprise_telemetry::authorize_execution_surface(
        crate::enterprise_telemetry::ExecutionSurface::Interactive,
    )?;

    let provider = agent::build_provider(cfg)?;
    let registry: Arc<ToolRegistry> = Arc::new(
        agent::new_tool_registry(cfg.workspace_root.clone(), agent::registry_options(cfg)).await,
    );
    // `warn: false` on every diagnostic below this point: the shell owns the
    // screen from the moment it starts, so anything printed after it would be
    // painted over. Startup problems still reach stderr *before* the shell
    // starts, which is where the notice below is printed too.
    let mcp = crate::agent::connect_mcp(
        cfg,
        registry.clone(),
        Some(registry.mcp_usage_ledger()),
        true,
    )
    .await?;
    agent::populate_schema_index(&registry, &cfg.workspace_root)?;
    crate::subagent::install_for_session(cfg, &registry)?;
    let active_rules =
        crate::rules::enforce_workspace_rules(&registry, &cfg.workspace_root, &cfg.authority);
    let (_session_graph, _graph_build) = agent::spawn_session_graph(
        &cfg.workspace_root,
        registry.clone(),
        Box::new(|line| eprintln!("  {line}")),
        Box::new(|| {}),
    );
    let base_tools: &dyn ToolExecutor = match &mcp {
        Some(set) => set.as_ref(),
        None => &*registry,
    };
    let custom_tools = agent::discover_custom_tools(cfg, true).await;
    let mut budget = agent::build_budget_guard(budget_limit);
    let store = agent::open_store(&cfg.workspace_root);
    let calibration = agent::seed_calibration(&store, cfg);
    let custom = crate::extensions::CustomExtensions::load_with_authority(
        &cfg.workspace_root,
        &cfg.authority,
    );

    // Built once and reused verbatim on `/clear` — the byte-stable prefix is
    // the prompt-cache contract (see `build_system_prompt`).
    let system_prompt = agent::with_session_hook_context(
        agent::build_system_prompt(cfg, &cfg.workspace_root, &active_rules),
        cfg,
    )
    .await;
    let mut messages = vec![CompletionMessage::system(system_prompt.clone())];
    // `warn: false`: past here diagnostics would be painted over by the shell.
    let mut memory =
        SessionMemory::open_for_session(&cfg.workspace_root, false, &cfg.authority, &active_rules);
    if let Some(m) = &mut memory {
        m.register_external_providers(|_| {}).await;
    }
    let mut presence = agent::SessionPresence::announce(cfg, "simple session", &registry);
    let activation = crate::discovery::new_activation();

    // ── channels ────────────────────────────────────────────────────────────
    // events: driver → shell. submissions: shell → driver. answers: the
    // driver's router → whichever `ask_user` call is currently parked.
    let (events_tx, events_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let (sub_tx, mut sub_rx) = mpsc::unbounded_channel::<UserInput>();
    let (ask_tx, ask_rx) = mpsc::unbounded_channel::<String>();
    let ask_io = SimpleAskUserIo {
        events: events_tx.clone(),
        answers: Arc::new(tokio::sync::Mutex::new(ask_rx)),
    };

    // Said on the normal screen, before the shell starts, because this surface
    // is a deliberate subset and a user who reaches for a deck feature should
    // learn it is absent from a sentence rather than from nothing happening.
    eprintln!(
        "▸ simple surface — one conversation, drawn inline; completed messages go to \
         your scrollback."
    );
    eprintln!("  No tabs, prompt queue, or resume: run `stella chat` for those.");

    let opts = RunOptions {
        slash_commands: simple_slash_commands(&custom),
        debug_log_path: crate::command_deck::debug_log_path(),
        screen_reader: true,
        ..RunOptions::default()
    };
    let shell = tokio::spawn(stella_tui::run(opts, events_rx, sub_tx));

    let mut exit_ok = true;
    // ── the driver loop ─────────────────────────────────────────────────────
    while let Some(input) = sub_rx.recv().await {
        let prompt = match input {
            UserInput::Prompt { text, .. } => text,
            // Between turns there is no question parked, so a stray answer or
            // scope decision belongs to a card that no longer exists. The deck
            // drains these for the same reason.
            UserInput::AskUserAnswer { .. } | UserInput::ScopeDecision(_) => continue,
            UserInput::Cancel => break,
        };
        let trimmed = prompt.trim();
        if trimmed.is_empty() {
            continue;
        }
        match handle_local_command(trimmed, &registry, &system_prompt, &mut messages) {
            LocalCommand::Quit => break,
            LocalCommand::Handled(text) => {
                emit_notice(&events_tx, &text);
                continue;
            }
            LocalCommand::NotLocal => {}
        }

        // A custom command/skill (⚡) expands into the prompt the turn runs.
        // Reserved names never reach a custom definition, so the vocabulary
        // above cannot be shadowed.
        let expanded = if trimmed.starts_with('/') {
            custom.expand(trimmed, SIMPLE_RESERVED)
        } else {
            None
        };
        let prompt = expanded.as_deref().unwrap_or(trimmed).to_string();

        messages.push(crate::attachments::user_message(&prompt));

        let mut recall_event = None;
        if let Some(m) = &mut memory {
            let recalled = m.recall_block_reported(&prompt).await;
            recall_event = recalled.telemetry_event();
            inject_recall_block(&mut messages, recalled.text);
        }

        let turn_start = messages.len();
        let files_before = registry.files_touched().len();
        let started_unix = crate::memory::unix_now_secs();
        presence.update_prompt(&prompt);

        let execution = agent::begin_execution(&store, "simple", &prompt, cfg, Some(presence.id()));
        // The turn future borrows `messages`/`budget`, so it lives in its own
        // scope: the borrow must end before the loop touches them again, and
        // a cancel works precisely by dropping it early.
        let result = {
            let turn = run_simple_turn(
                &*provider,
                base_tools,
                &custom_tools,
                &registry,
                &mut messages,
                &mut budget,
                &calibration,
                cfg,
                execution,
                files_before,
                &events_tx,
                &ask_io,
                &activation,
                recall_event,
            );
            tokio::pin!(turn);
            loop {
                tokio::select! {
                    outcome = &mut turn => break TurnEnd::Finished(outcome),
                    maybe = sub_rx.recv() => match maybe {
                        // The turn is parked on a question; route the answer
                        // to whichever `ask_user` call is waiting.
                        Some(UserInput::AskUserAnswer { answer, .. }) => {
                            let _ = ask_tx.send(answer);
                        }
                        // Cancel drops the turn future at its next await
                        // point — the only abort the engine offers.
                        Some(UserInput::Cancel) => break TurnEnd::Cancelled,
                        // No pipeline on this surface, so no scope gate can
                        // be pending; a decision here has no card behind it.
                        Some(UserInput::ScopeDecision(_)) => {}
                        // One turn at a time: this surface has no prompt
                        // queue, and saying so beats silently swallowing it.
                        Some(UserInput::Prompt { .. }) => emit_notice(
                            &events_tx,
                            "a turn is already running — press Esc to interrupt it first",
                        ),
                        // The shell went away.
                        None => break TurnEnd::ShellClosed,
                    },
                }
            }
        };

        presence.needs_input();
        let result = match result {
            TurnEnd::Finished(outcome) => outcome,
            TurnEnd::Cancelled | TurnEnd::ShellClosed => {
                // The dropped future took its channel senders with it. Truncate
                // the partial turn so the next prompt starts from the last
                // committed state rather than from half a tool loop.
                messages.truncate(turn_start);
                emit_notice(&events_tx, "turn cancelled");
                if matches!(result, TurnEnd::ShellClosed) {
                    break;
                }
                continue;
            }
        };

        agent::record_turn_episode(
            &memory,
            &prompt,
            &result,
            &registry,
            files_before,
            started_unix,
            &messages[turn_start..],
        )
        .await;
        if let Err(error) = &result {
            exit_ok = false;
            let _ = events_tx.send(AgentEvent::Error {
                message: error.clone(),
                retryable: false,
            });
        }
    }

    // Closing the event stream is what tells the shell the session is over;
    // it restores the terminal and returns.
    drop(events_tx);
    let _ = shell.await;
    if let Some(set) = &mcp {
        set.close_all().await;
    }
    presence.finish(exit_ok, None);
    Ok(())
}

/// How the wait for one turn ended.
enum TurnEnd {
    /// The engine returned — success or failure, both are the turn's answer.
    Finished(Result<(), String>),
    /// The user interrupted it.
    Cancelled,
    /// The shell exited underneath us.
    ShellClosed,
}

/// A driver-resolved command, or a prompt for the model.
enum LocalCommand {
    /// End the session.
    Quit,
    /// Handled here; the string is what to say about it.
    Handled(String),
    /// Not one of ours — run it as a turn.
    NotLocal,
}

/// Resolve the handful of commands that must never reach the model: ending the
/// session, clearing history, and reporting local state. Kept pure of I/O (it
/// returns what to say rather than printing it) so it is testable without a
/// terminal, an engine, or a channel.
fn handle_local_command(
    input: &str,
    registry: &ToolRegistry,
    system_prompt: &str,
    messages: &mut Vec<CompletionMessage>,
) -> LocalCommand {
    match input {
        "/exit" | "/quit" => LocalCommand::Quit,
        "/clear" => {
            // Rebuilt from the same string, not re-derived: the byte-stable
            // system prefix is the prompt-cache contract, and re-assembling it
            // here would invalidate the cache on every `/clear`.
            *messages = vec![CompletionMessage::system(system_prompt.to_string())];
            LocalCommand::Handled("conversation cleared".to_string())
        }
        "/files" => {
            let touched = registry.files_touched();
            if touched.is_empty() {
                return LocalCommand::Handled("no files touched yet".to_string());
            }
            // `[CRUD] path`, spelled out rather than colour-coded: this is the
            // surface where colour is the least likely thing to reach the
            // user, so the CRUD letters carry the meaning on their own (the
            // same letters `tui::files_touched_panel` colours elsewhere).
            let mut out = format!("{} file(s) touched:", touched.len());
            for (path, ops) in touched {
                out.push_str(&format!("\n  [{ops}] {path}"));
            }
            LocalCommand::Handled(out)
        }
        "/help" => LocalCommand::Handled(
            "simple surface — one conversation, drawn inline on your own screen.\n\
             Completed messages move into scrollback as ordinary output, so a screen \
             reader announces each one once and your review cursor can go back through \
             them.\n\
             \n\
             Enter sends · Esc interrupts a running turn · Ctrl-C quits\n\
             Tab moves focus between the transcript and the files list\n\
             Ctrl-R expands reasoning · / opens the command menu\n\
             \n\
             /clear forget the conversation · /files what has been touched · /exit end \
             the session\n\
             \n\
             No tabs, prompt queue, sub-agents, or resume here — run `stella chat` for \
             those."
                .to_string(),
        ),
        _ => LocalCommand::NotLocal,
    }
}

/// The marker every driver notice opens with.
///
/// The same `▸` the CLI already uses on the normal screen for "the program is
/// speaking, not the model" (see the surface notice in
/// [`run_simple_session`], and `main`'s plain-REPL banner).
const NOTICE_MARKER: &str = "▸ ";

/// Say something to the user through the transcript itself.
///
/// Driver notices ride the event stream rather than `println!` because the
/// shell owns the screen: a direct write would land on top of a frame and be
/// repainted away. As a `Text` event it becomes a transcript entry, which
/// means it also reaches scrollback — so the notice is still there after it
/// scrolls, which is the whole point of this surface.
///
/// It is marked because of what that costs. `AgentEvent` has no system-notice
/// variant, and a `Text` entry renders on the **agent** rail — so an unmarked
/// "conversation cleared" is the transcript stating that the model said it.
/// On a surface being read aloud, where the rail glyph is the only thing
/// distinguishing speakers and it may not be announced at all, that is the
/// kind of small lie worth one character to avoid.
fn emit_notice(events: &UnboundedSender<AgentEvent>, text: &str) {
    let _ = events.send(AgentEvent::Text {
        delta: format!("{NOTICE_MARKER}{text}\n"),
    });
}

/// Persist each event and forward it to the shell.
///
/// The deck's [`crate::command_deck::spawn_forwarder`] wraps each event in an
/// `Inbound::Event` for its multi-lane fold; this surface has one lane and the
/// shell consumes `AgentEvent` natively, so the wrapper is exactly the part
/// that is not needed. The returned bit is false after any persistence
/// failure and is carried into execution closeout — telemetry fails closed
/// rather than recording a partial run as complete.
fn spawn_simple_forwarder(
    mut rx: UnboundedReceiver<AgentEvent>,
    execution: Option<(Arc<Store>, i64)>,
    provider_id: String,
    events: UnboundedSender<AgentEvent>,
) -> tokio::task::JoinHandle<bool> {
    tokio::spawn(async move {
        let mut seq = 0u64;
        let mut store_warned = false;
        let mut persistence_complete = true;
        let mut bridge = crate::diag_bridge::DomainBridge::new(
            crate::diag_boot::dx(),
            Some(crate::diag_boot::workspace_root()),
        );
        while let Some(event) = rx.recv().await {
            bridge.observe(&event);
            if let Some((store, id)) = &execution {
                let outcome = agent::persist_event_detailed(store, *id, seq, &event, &provider_id);
                if !outcome.is_complete() {
                    persistence_complete = false;
                    // One warning per turn, naming the condition that actually
                    // occurred — a failed INSERT and unreported provider usage
                    // are different faults and must not be conflated.
                    if !store_warned && let Some(message) = outcome.message("this session") {
                        store_warned = true;
                        let _ = events.send(AgentEvent::Error {
                            message,
                            retryable: true,
                        });
                    }
                }
                seq += 1;
            }
            let _ = events.send(event);
        }
        bridge.finish();
        persistence_complete
    })
}

/// One model turn, with its events forwarded to the shell.
///
/// The same tool stack, persistence, and closeout as the deck's lead turn,
/// minus the parts that only mean something with more than one lane: no claim
/// tap (nothing else is writing to this tree), no supervisor channel (no
/// sub-agents), no steering tap (no mid-turn steering on this surface).
#[allow(clippy::too_many_arguments)]
async fn run_simple_turn(
    provider: &dyn Provider,
    base_tools: &dyn ToolExecutor,
    custom_tools: &[CustomTool],
    registry: &ToolRegistry,
    messages: &mut Vec<CompletionMessage>,
    budget: &mut BudgetGuard,
    calibration: &CalibrationMap,
    cfg: &Config,
    execution: Option<(Arc<Store>, i64)>,
    files_before: usize,
    events: &UnboundedSender<AgentEvent>,
    ask_io: &SimpleAskUserIo,
    activated: &crate::discovery::ActivatedTools,
    // This turn's `ContextRecall`, carried from the caller because recall runs
    // before this channel exists — it has to, since its frames go into the
    // messages the turn is built from.
    recall_event: Option<AgentEvent>,
) -> Result<(), String> {
    budget.begin_turn();

    let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
    let forwarder = spawn_simple_forwarder(
        rx,
        execution.clone(),
        cfg.provider.id.to_string(),
        events.clone(),
    );
    if let Some(event) = recall_event {
        let _ = tx.send(event);
    }
    // File changes ride this turn's channel, from the recorder that already
    // computes the session's file-touch ledger — so the transcript, the files
    // panel, and the audit log cannot disagree about what changed.
    registry.attach_events(stella_core::EventSender::new(tx.clone()));

    // Every tx clone lives in this scope, so dropping `tx` below closes the
    // channel and lets the forwarder finish (same drop-order rule as
    // `agent::run_turn` and the deck's lead turn).
    let outcome = {
        let customs = CustomToolSet::new(
            base_tools,
            custom_tools.to_vec(),
            cfg.workspace_root.clone(),
        );
        // A stub event channel: this surface's io raises its own `AskUser`
        // card, so the tool set emitting one too would draw it twice.
        let (stub_tx, _stub_rx) = mpsc::unbounded_channel();
        let interactive = InteractiveToolSet::new(&customs, stub_tx, Box::new(ask_io.clone()))
            .with_skill_registry(SkillRegistry::from_env(cfg.workspace_root.clone()));
        let permitted = agent::PolicyToolSet::new(&interactive, agent::session_tool_policy(cfg));
        // Discovery sits above the policy filter (it must see the full
        // *permitted* catalog) and below nothing else here.
        let tools = crate::discovery::DiscoveryToolSet::new(&permitted, cfg.workspace_root.clone())
            .with_project_prompts_allowed(cfg.authority.project_prompts_allowed)
            .with_activation(activated.clone());
        let hook_runner = ShellHookRunner;
        let mut engine = Engine::with_sleeper(
            provider,
            &tools,
            agent::engine_config_for(cfg),
            &TokioSleeper,
        )
        .with_calibration(calibration);
        if let Some(hooks) = &cfg.hooks {
            engine = engine.with_hooks(hooks, &hook_runner);
        }
        engine.run_turn(messages, budget, &tx).await
    };

    drop(tx);
    let persistence_complete = forwarder.await.unwrap_or(false);

    if let Some((store, id)) = &execution {
        let (outcome_label, cost) = match &outcome {
            TurnOutcome::Completed { cost_usd, .. } => ("completed", *cost_usd),
            TurnOutcome::Aborted { cost_usd, .. } => ("aborted", *cost_usd),
        };
        if !agent::record_execution_end(
            store,
            *id,
            registry,
            files_before,
            outcome_label,
            cost,
            persistence_complete,
        ) && persistence_complete
        {
            let _ = events.send(AgentEvent::Error {
                message: "store write failed — the audit record (files touched / memory \
                          citations / outcome) for this execution is incomplete"
                    .to_string(),
                retryable: true,
            });
        }
    }

    match outcome {
        TurnOutcome::Completed { .. } => Ok(()),
        TurnOutcome::Aborted { reason, .. } => Err(reason),
    }
}

// ── ask_user through the simple surface ─────────────────────────────────────

/// [`AskUserIo`] over this surface's channels: emit an `AskUser` event, await
/// the shell's `AskUserAnswer`, echo the answer back as the card's own
/// `ToolResult` (the event-pure clear), and return it in the shape
/// `interactive::execute_ask_user`'s parser expects — an exact option match
/// becomes its 1-based index, anything else passes through as free text.
#[derive(Clone)]
struct SimpleAskUserIo {
    events: UnboundedSender<AgentEvent>,
    answers: Arc<tokio::sync::Mutex<UnboundedReceiver<String>>>,
}

#[async_trait]
impl AskUserIo for SimpleAskUserIo {
    async fn prompt(&self, question: &str, options: &[String]) -> Result<String, String> {
        // `execute_ask_user` appends the free-text affordance before calling
        // us; the card renders its own ("or type your own answer"), so
        // presenting the label as a pickable option would double it — and
        // picking it would return the label itself as the answer.
        let mut presented: Vec<String> = options.to_vec();
        if presented
            .last()
            .is_some_and(|o| o.starts_with(FREE_TEXT_LABEL))
        {
            presented.pop();
        }

        let id = format!(
            "simple-ask-{}",
            NEXT_SIMPLE_ASK.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let mut answers = self.answers.lock().await;
        // Drop answers stranded by a cancelled turn — they belong to a card
        // that no longer exists.
        while answers.try_recv().is_ok() {}

        let _ = self.events.send(AgentEvent::AskUser {
            id: id.clone(),
            question: question.to_string(),
            options: presented.clone(),
        });

        let answer = answers
            .recv()
            .await
            .ok_or_else(|| "the session closed before the question was answered".to_string())?;

        // The echoed ToolResult is what clears the pending card in the fold
        // (matched by this exact id) — without it the gate keeps eating keys
        // for the rest of the turn.
        let _ = self.events.send(AgentEvent::ToolResult {
            call_id: id,
            output: ToolOutput::Ok {
                content: answer.clone(),
            },
            duration_ms: 0,
            speculated: false,
        });

        match presented.iter().position(|option| *option == answer) {
            Some(i) => Ok((i + 1).to_string()),
            None => Ok(answer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> ToolRegistry {
        ToolRegistry::new(
            std::env::temp_dir(),
            stella_tools::RegistryOptions::default(),
        )
    }

    #[test]
    fn exit_and_quit_end_the_session() {
        let mut messages = vec![CompletionMessage::system("sys")];
        for word in ["/exit", "/quit"] {
            assert!(matches!(
                handle_local_command(word, &registry(), "sys", &mut messages),
                LocalCommand::Quit
            ));
        }
    }

    /// `/clear` must rebuild the history from the SAME system string it was
    /// built with. Re-deriving the prompt here would change its bytes and
    /// invalidate the provider-side prompt cache on every clear — the prefix
    /// being byte-stable is the whole contract.
    #[test]
    fn clear_restores_exactly_the_original_system_prefix() {
        let system = "the original system prompt";
        let mut messages = vec![
            CompletionMessage::system(system.to_string()),
            crate::attachments::user_message("hello"),
        ];
        let outcome = handle_local_command("/clear", &registry(), system, &mut messages);
        assert!(matches!(outcome, LocalCommand::Handled(_)));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, system);
    }

    /// Anything that is not one of the driver's own commands has to reach the
    /// model — including a slash command it does not know, which may be a
    /// custom one the expander resolves.
    #[test]
    fn an_unknown_command_is_not_swallowed() {
        let mut messages = vec![CompletionMessage::system("sys")];
        for input in ["hello", "/agents", "/goal ship it", "/pipeline"] {
            assert!(
                matches!(
                    handle_local_command(input, &registry(), "sys", &mut messages),
                    LocalCommand::NotLocal
                ),
                "{input:?} must fall through to the turn"
            );
        }
    }

    /// The `/` menu is a promise about what the driver will do. Every entry it
    /// advertises must therefore be one the driver actually resolves or
    /// expands — an advertised command that silently becomes a model prompt is
    /// exactly the failure this surface's users can least afford to debug.
    #[test]
    fn every_advertised_builtin_is_one_the_driver_handles() {
        let mut messages = vec![CompletionMessage::system("sys")];
        for (name, description) in SIMPLE_BUILTINS {
            assert!(
                !description.is_empty(),
                "{name} is advertised with no description"
            );
            assert!(
                !matches!(
                    handle_local_command(name, &registry(), "sys", &mut messages),
                    LocalCommand::NotLocal
                ),
                "{name} is in the menu but falls through to the model"
            );
            assert!(
                SIMPLE_RESERVED.contains(name),
                "{name} is advertised but not reserved, so a custom command could shadow it"
            );
        }
    }

    /// `AgentEvent` has no system-notice variant, so a driver message goes out
    /// as `Text` — which renders on the AGENT rail. Unmarked, the transcript
    /// would be asserting that the model said "conversation cleared". The rail
    /// glyph is a visual distinction, and this is the surface where visual
    /// distinctions are the ones least likely to land.
    #[test]
    fn a_driver_notice_is_marked_as_the_program_speaking() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        emit_notice(&tx, "conversation cleared");
        let AgentEvent::Text { delta } = rx.try_recv().expect("a notice was sent") else {
            panic!("a notice rides the transcript as Text");
        };
        assert!(
            delta.starts_with(NOTICE_MARKER),
            "an unmarked notice reads as model speech: {delta:?}"
        );
        assert!(delta.contains("conversation cleared"));
        assert!(delta.ends_with('\n'), "notices are whole lines: {delta:?}");
    }

    /// `/help` is the only place this surface explains that it is a subset.
    /// A user who reaches for a deck feature and finds nothing must be able to
    /// learn why from here.
    #[test]
    fn help_names_the_keys_and_the_missing_features() {
        let mut messages = vec![CompletionMessage::system("sys")];
        let LocalCommand::Handled(text) =
            handle_local_command("/help", &registry(), "sys", &mut messages)
        else {
            panic!("/help must be handled locally");
        };
        for expected in ["Enter", "Esc", "scrollback", "stella chat"] {
            assert!(
                text.contains(expected),
                "/help never mentions {expected:?}:\n{text}"
            );
        }
    }
}
