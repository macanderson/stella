//! The queue-free command runner — the driver half of
//! [`WorkspaceInput::Command`].
//!
//! The mid-turn arm cannot run `run_deck_command`: the running turn holds the
//! conversation and the provider, which is exactly why every slash command
//! used to sit in the prompt queue behind the lead (`/export` rendered in the
//! queue popup as a "pending prompt" until the turn ended). The queue-free
//! subset ([`skills::SIDEBAND`]) needs none of that state, so this module
//! runs it from **either** arm with what is always free: the config, the
//! inbound channel, and the session id. Anything slow (`/export`'s archive,
//! `/models refresh`) spawns and reports on completion — the driver's event
//! pump is never awaited against.
//!
//! Replies ride [`Inbound::ShellEvent`]: transcript-only, no status flip —
//! mid-turn, the turn's own events keep telling the truth about the turn,
//! and between turns there is no `Running` flicker to undo.

use super::*;
// `/info`'s own vocabulary, which lives with the rest of the slash-command
// parsing rather than in the parent (#4775 moved it there).
use super::slash_commands::{ModelsCommand, parse_models_command};

/// Try `trimmed` as a queue-free command. `true` = recognized and handled
/// (possibly by spawning a task that reports later); `false` = not one —
/// the caller keeps its old route for the text (an argument these commands
/// do not take, or a head that is not on [`skills::SIDEBAND`]).
pub(super) fn run(
    trimmed: &str,
    cfg: &Config,
    in_tx: &UnboundedSender<Inbound>,
    session_id: &str,
) -> bool {
    let say = |text: String| {
        let _ = in_tx.send(Inbound::ShellEvent {
            agent: LEAD.to_string(),
            event: AgentEvent::Text { text },
        });
    };
    let head = trimmed.split_whitespace().next().unwrap_or(trimmed);
    if !skills::is_sideband(head) {
        return false;
    }
    match trimmed {
        "/help" => {
            let _ = in_tx.send(Inbound::ShowHelp);
        }
        "/info" | "/models" => say(Config::available_models_plain(None)),
        "/theme" => say(theme_cmd::current_summary(cfg)),
        // Export THIS session's telemetry to a timestamped ZIP of raw JSON
        // dumps + a self-contained HTML dashboard, in the background: the
        // in-flight turn is not paused, not queued behind, not interrupted.
        // The session id is what scopes the archive (#2558).
        "/export" => {
            let root = cfg.workspace_root.clone();
            let session_id = session_id.to_string();
            let in_tx = in_tx.clone();
            tokio::spawn(async move {
                let text = crate::export::export_command(&root, &session_id).await;
                let _ = in_tx.send(Inbound::ShellEvent {
                    agent: LEAD.to_string(),
                    event: AgentEvent::Text { text },
                });
            });
        }
        "/donate" => say(DONATE.to_string()),
        _ => {
            // `/theme <slug>` — switch + persist. The live switch is a
            // buffer remap in `stella_tui`, so it lands on the next frame.
            if let Some(command) = theme_cmd::parse_theme_command(trimmed) {
                match command {
                    theme_cmd::ThemeCommand::Set(name) => match theme_cmd::set_theme(name) {
                        Ok(msg) | Err(msg) => say(msg),
                    },
                    theme_cmd::ThemeCommand::Usage(arg) => say(theme_cmd::usage(&arg)),
                }
                return true;
            }
            // The `/info` argument forms, model-free by design — a catalog
            // refresh is part of digging out of a broken model setting, so it
            // can never be allowed to depend on a working model.
            if let Some(command) = parse_models_command(trimmed) {
                match command {
                    ModelsCommand::Refresh { force } => {
                        say("Model catalog refresh…".to_string());
                        let in_tx = in_tx.clone();
                        let cfg = cfg.clone();
                        tokio::spawn(async move {
                            let emit = |line: String| {
                                let _ = in_tx.send(Inbound::ShellEvent {
                                    agent: LEAD.to_string(),
                                    event: AgentEvent::Text { text: line },
                                });
                            };
                            let mut emit_line = emit;
                            if let Err(e) =
                                crate::model_catalog::run_refresh_emit(force, &mut emit_line).await
                            {
                                emit_line(format!("refresh failed: {e}"));
                            }
                            // A refreshed catalog is what the SETTINGS tab's
                            // model vocabulary — and the `/model` argument
                            // menu that reads it — are derived from.
                            let _ = in_tx.send(engine_config_inbound(&cfg, None));
                        });
                    }
                    ModelsCommand::List => say(Config::available_models_plain(None)),
                    ModelsCommand::Usage(word) => say(format!(
                        "`/info {word}` — unknown subcommand; try `/info` or `/info list` \
                         (the listing) or `/info refresh [--force]` (re-sync the catalog)"
                    )),
                }
                return true;
            }
            // A queue-free head with arguments it does not take (`/export
            // now`): not handled here — the caller keeps its old route.
            return false;
        }
    }
    true
}

/// The `/donate` text, verbatim from the dispatcher it moved out of.
const DONATE: &str = "❤️  Support Stella\n\
     \n\
     Stella is free, open-source, and local-first — no server, no \
     account, no telemetry sent home. If it's saving you time or \
     money, consider becoming a GitHub Sponsor:\n\
     \n\
       → https://github.com/sponsors/macanderson\n\
     \n\
     Recurring sponsorships keep development sustainable. You'll \
     see the available tiers and perks (one-time and monthly) on \
     that page. Every pledge helps fund the next feature, the next \
     provider, and the next release.\n\
     \n\
     Thank you! 🙏";
