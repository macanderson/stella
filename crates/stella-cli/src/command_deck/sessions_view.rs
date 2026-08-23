//! The SESSIONS overlay's snapshot: session-registry records projected into
//! the TUI's own types, each with its store rollup (turns, spend, model) and
//! — once a model has written one — a one-sentence description.
//!
//! Split out of `command_deck.rs` under the god-file rule when
//! [`session_phase`] grew its `Stopped` arm (#1653); the rollup and the
//! describer landed here for the same reason.

use std::sync::Arc;

use stella_protocol::{CompletionMessage, CompletionRequest, ModelCallRole};
use stella_store::{SessionRegistry, SessionStatus, Store};
use tokio::sync::mpsc::UnboundedSender;

use super::{Inbound, chrome_note};
use crate::config::Config;

/// What a SESSIONS verb needs to know about the process servicing it: the
/// registry and store it reads, the config a describer builds its provider
/// from, and which record is this process's own. One borrow, so the deck's
/// registry verbs carry it as one argument.
pub(super) struct SessionScope<'a> {
    pub(super) registry: &'a SessionRegistry,
    pub(super) store: &'a Option<Arc<Store>>,
    pub(super) cfg: &'a Config,
    pub(super) budget_limit: Option<f64>,
    /// This process's own session record id.
    pub(super) mine: &'a str,
    pub(super) workspace: &'a str,
}

impl SessionScope<'_> {
    /// The overlay snapshot for this scope.
    pub(super) fn snapshot(&self) -> Inbound {
        sessions_inbound(
            self.registry,
            self.store.as_deref(),
            self.mine,
            self.workspace,
        )
    }
}

/// The SESSIONS overlay snapshot: every registry record mapped to the deck's
/// [`stella_tui::SessionInfo`], flagging this process's own record and the
/// rows that can be reopened HERE (no live owner, this workspace, durable
/// state on disk — ⏎ navigates into those). The store, when there is one,
/// supplies each row's turns, spend and model.
pub(super) fn sessions_inbound(
    registry: &SessionRegistry,
    store: Option<&Store>,
    mine: &str,
    workspace: &str,
) -> Inbound {
    let sessions = registry
        .list()
        .into_iter()
        .map(|r| {
            let stats = store
                .and_then(|s| s.session_stats(&r.id).ok())
                .unwrap_or_default();
            stella_tui::SessionInfo {
                mine: r.id == mine,
                resumable: r.id != mine && r.workspace == workspace && registry.resumable(&r.id),
                phase: session_phase(r.status),
                id: r.id,
                title: r.title,
                summary: r.summary,
                description: r.description,
                workspace: r.workspace,
                started_ms: r.started_at_ms,
                updated_ms: r.updated_at_ms,
                turns: stats.turns,
                spend_micros: (stats.cost_usd * 1_000_000.0).round().max(0.0) as u64,
                model: stats.model,
            }
        })
        .collect();
    Inbound::Sessions(sessions)
}

/// Most sessions one refresh describes — the overlay re-asks on every open,
/// so a long backlog is worked down a few rows at a time rather than billed
/// all at once.
const DESCRIBE_PER_REFRESH: usize = 6;

/// Write a one-sentence description for this workspace's sessions that have
/// none yet, then push a fresh snapshot.
///
/// Spawned, never awaited on the event pump: each description is a model
/// call. Only sessions with recorded prompts are described — a session that
/// never ran a turn has nothing to describe and keeps its summary. The
/// provider is built in the task, as the skill author does, so the pump holds
/// no reference across the calls.
pub(super) fn describe_sessions(scope: &SessionScope<'_>, in_tx: UnboundedSender<Inbound>) {
    let Some(store) = scope.store.clone() else {
        return;
    };
    let registry = scope.registry.clone();
    let cfg = scope.cfg.clone();
    let budget_limit = scope.budget_limit;
    let mine = scope.mine.to_string();
    let workspace = scope.workspace.to_string();
    let pending: Vec<_> = registry
        .list()
        .into_iter()
        .filter(|r| r.description.is_none() && r.workspace == workspace)
        .filter(|r| r.status != SessionStatus::Archived)
        .take(DESCRIBE_PER_REFRESH)
        .collect();
    if pending.is_empty() {
        return;
    }
    tokio::spawn(async move {
        let provider = match crate::agent::build_provider(&cfg) {
            Ok(p) => p,
            Err(error) => {
                let _ = in_tx.send(chrome_note(format!(
                    "session descriptions need a model: {error}"
                )));
                return;
            }
        };
        let mut wrote = false;
        for record in pending {
            let Ok(stats) = store.session_stats(&record.id) else {
                continue;
            };
            if stats.prompts.is_empty() {
                continue;
            }
            let request = CompletionRequest {
                messages: describe_messages(&record.title, &stats.prompts),
                max_output_tokens: Some(80),
                temperature: Some(0.2),
                effort: None,
                tools: Vec::new(),
                reasoning: None,
                params: None,
            };
            let Ok(accounted) = crate::accounted_call::complete_standalone(
                &cfg.workspace_root,
                &*provider,
                ModelCallRole::Summarization,
                "session_describe",
                &cfg.model_id,
                budget_limit,
                request,
            )
            .await
            else {
                continue;
            };
            let sentence = one_sentence(&accounted.result.text);
            if sentence.is_empty() {
                continue;
            }
            if registry.set_description(&record.id, &sentence).is_ok() {
                wrote = true;
            }
        }
        if wrote {
            let _ = in_tx.send(sessions_inbound(&registry, Some(&store), &mine, &workspace));
        }
    });
}

/// The describer's prompt: the session's title and its prompts, oldest
/// first, and the one-sentence contract.
fn describe_messages(title: &str, prompts: &[String]) -> Vec<CompletionMessage> {
    let mut body = format!("Session: {title}\n\nPrompts the user gave, in order:\n");
    for (i, prompt) in prompts.iter().enumerate() {
        let flat: String = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
        let capped: String = flat.chars().take(300).collect();
        body.push_str(&format!("{}. {capped}\n", i + 1));
    }
    vec![
        CompletionMessage::system(
            "You describe a coding session in ONE sentence of at most 18 words, past tense, \
             naming the concrete thing worked on. No preamble, no quotes, no trailing period \
             commentary. Output the sentence only.",
        ),
        CompletionMessage::user(body),
    ]
}

/// The first sentence of a model's answer, trimmed and capped — a describer
/// that answers with a paragraph still yields one line.
fn one_sentence(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = flat
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .trim();
    let end = flat
        .char_indices()
        .find(|(i, c)| matches!(c, '.' | '!' | '?') && flat[i + c.len_utf8()..].starts_with(' '))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(flat.len());
    flat[..end]
        .chars()
        .take(160)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Store status → TUI phase (the TUI mirrors the enum so it never links the
/// store crate).
fn session_phase(status: SessionStatus) -> stella_tui::SessionPhase {
    match status {
        SessionStatus::InProgress => stella_tui::SessionPhase::InProgress,
        SessionStatus::NeedsInput => stella_tui::SessionPhase::NeedsInput,
        SessionStatus::Paused => stella_tui::SessionPhase::Paused,
        SessionStatus::Cancelled => stella_tui::SessionPhase::Cancelled,
        SessionStatus::Stopped => stella_tui::SessionPhase::Stopped,
        SessionStatus::Complete => stella_tui::SessionPhase::Complete,
        SessionStatus::Archived => stella_tui::SessionPhase::Archived,
        SessionStatus::Error => stella_tui::SessionPhase::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_describer_keeps_one_sentence_and_strips_the_wrapping() {
        assert_eq!(
            one_sentence("\"Wired a dedup digest into the finding store. It also added tests.\""),
            "Wired a dedup digest into the finding store."
        );
        assert_eq!(one_sentence("  Fixed the parser\n"), "Fixed the parser");
        assert_eq!(one_sentence(""), "");
    }

    #[test]
    fn the_describer_prompt_numbers_the_sessions_prompts() {
        let messages = describe_messages(
            "stella: fix",
            &["fix the parser".into(), "add a test".into()],
        );
        let user = messages[1].content.clone();
        assert!(user.contains("1. fix the parser"), "{user}");
        assert!(user.contains("2. add a test"), "{user}");
    }
}
