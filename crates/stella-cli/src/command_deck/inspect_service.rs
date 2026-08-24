//! The INSPECT overlay's driver half (`⌃g`, `/inspect`): answer the deck's
//! recorded-call index, and the reconstruction of the one call it selects.
//!
//! Split out of `command_deck.rs`, which is a grandfathered god file closed to
//! growth (AGENTS.md § "God files — plan around them, never into them"), when
//! the reconstruction gained the provenance breakdown below.
//!
//! # Why the split of the system prompt happens here
//!
//! `stella-tui` links no store crate and knows no prompt headings — every type
//! on its envelope is a shape this driver maps into. The provenance markers
//! are the prompt assembler's own section-opener constants
//! ([`crate::agent::prompt::provenance`]), and the assembler lives in this
//! binary, so this is the only place that can both read a reconstruction and
//! name the setting behind each span of it. The overlay renders labels it is
//! handed and derives nothing.
//!
//! This is the same bargain the message bodies already take: they are rendered
//! by `crate::inspect`'s own functions so the overlay and `stella inspect`
//! cannot disagree about what a call looked like. The sections are that rule
//! applied to the prompt's provenance — `stella inspect --system-prompt` and
//! this overlay split the same bytes with the same table.

use std::sync::Arc;

use tokio::sync::mpsc;

use stella_store::Store;
use stella_tui::{Inbound, WorkspaceInput};

/// The INSPECT overlay's driver half: answer the recorded-call index and the
/// reconstruction of one call. Returns `false` for anything else so the caller
/// can keep trying the other service handlers.
///
/// Both arms are blocking SQLite reads — `reconstruct_call` replays the block
/// registry and the event journal — so they run on `spawn_blocking` and answer
/// out of band, the same shape as [`WorkspaceInput::FocusGraphFile`]. Stalling
/// the event pump to rebuild a prompt would stutter a live turn.
pub(super) fn service_inspect_action(
    input: &WorkspaceInput,
    store: &Option<Arc<Store>>,
    execution_id: Option<i64>,
    in_tx: &mpsc::UnboundedSender<Inbound>,
) -> bool {
    if !matches!(
        input,
        WorkspaceInput::InspectRefresh | WorkspaceInput::InspectCall { .. }
    ) {
        return false;
    }
    // No store (claim mode, or it failed to open) or no turn yet: answer with
    // an empty index rather than silence, so the overlay renders its "nothing
    // recorded yet" line instead of looking hung.
    let (Some(store), Some(execution_id)) = (store.clone(), execution_id) else {
        let _ = in_tx.send(Inbound::RecordedCalls(Vec::new()));
        return true;
    };
    let in_tx = in_tx.clone();
    match input {
        WorkspaceInput::InspectRefresh => {
            tokio::task::spawn_blocking(move || {
                let calls = store.recorded_calls(execution_id).unwrap_or_default();
                let _ = in_tx.send(Inbound::RecordedCalls(
                    calls.iter().map(recorded_call_info).collect(),
                ));
            });
        }
        WorkspaceInput::InspectCall {
            turn_instance,
            step,
            call_seq,
        } => {
            let (turn_instance, step, call_seq) = (*turn_instance, *step, *call_seq);
            tokio::task::spawn_blocking(move || {
                let Ok(recon) = store.reconstruct_call(execution_id, turn_instance, step, call_seq)
                else {
                    let _ = in_tx.send(Inbound::RecordedCalls(Vec::new()));
                    return;
                };
                // Re-read the header so the detail can name the model/role that
                // served this call; the reconstruction itself carries only the
                // messages.
                let call = store
                    .recorded_calls(execution_id)
                    .unwrap_or_default()
                    .iter()
                    .find(|c| {
                        (c.turn_instance, c.step, c.call_seq) == (turn_instance, step, call_seq)
                    })
                    .map(recorded_call_info)
                    .unwrap_or_else(|| stella_tui::RecordedCallInfo {
                        turn_instance,
                        step,
                        call_seq,
                        call_role: "unknown".into(),
                        provider: "unknown".into(),
                        model: "unknown".into(),
                        estimated_input_tokens: 0,
                    });
                let _ = in_tx.send(Inbound::InspectedCall(Box::new(stella_tui::InspectView {
                    call,
                    messages: recon.messages.iter().map(inspect_message).collect(),
                    verified: recon.is_verified(),
                    unresolved: recon.unresolved.len(),
                    digest_mismatches: recon.digest_mismatches.len(),
                    journal_era: crate::inspect::deck_journal_era(recon.journal_era),
                })));
            });
        }
        _ => unreachable!("guarded by the matches! above"),
    }
    true
}

/// One reconstructed message, flattened for the overlay.
///
/// The body comes from `crate::inspect`'s renderer verbatim: the overlay shows
/// wire shape, and the two surfaces must not disagree about what a message
/// looked like. The system message additionally carries its provenance
/// breakdown — see the module docs for why the split belongs here.
fn inspect_message(message: &stella_protocol::CompletionMessage) -> stella_tui::InspectMessage {
    stella_tui::InspectMessage {
        role: crate::inspect::role_tag(message.role).to_string(),
        content: crate::inspect::message_body(message),
        sections: system_sections(message),
    }
}

/// The provenance breakdown of a system message, or an empty list for every
/// other role.
///
/// Empty is also the answer for a system message that resolved to no bytes:
/// the overlay's fallback then renders `content`, which is what the receipt
/// actually holds. A single section wrapping an empty body would claim
/// the model was sent an empty base persona, which the receipt does not say.
fn system_sections(
    message: &stella_protocol::CompletionMessage,
) -> Vec<stella_tui::InspectSection> {
    if message.role != stella_protocol::MessageRole::System || message.content.is_empty() {
        return Vec::new();
    }
    crate::agent::prompt::provenance::sections(&message.content)
        .into_iter()
        .map(|section| stella_tui::InspectSection {
            label: section.label.to_string(),
            source: section.source.to_string(),
            body: section.body.to_string(),
        })
        .collect()
}

fn recorded_call_info(call: &stella_store::RecordedCall) -> stella_tui::RecordedCallInfo {
    stella_tui::RecordedCallInfo {
        turn_instance: call.turn_instance,
        step: call.step,
        call_seq: call.call_seq,
        call_role: call.call_role.clone(),
        provider: call.provider.clone(),
        model: call.model.clone(),
        estimated_input_tokens: call.estimated_input_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::prompt::{MEMORIES_HEADER, SESSION_ENVIRONMENT_HEADER};
    use stella_protocol::CompletionMessage;

    /// The overlay must be handed the same split `stella inspect
    /// --system-prompt` prints, and only for the system message — a user
    /// message that happens to quote a heading is not a prompt with
    /// provenance.
    #[test]
    fn only_the_system_message_carries_a_provenance_breakdown() {
        let system = inspect_message(&CompletionMessage::system(format!(
            "You are Stella.{SESSION_ENVIRONMENT_HEADER}Workspace root: /w"
        )));
        assert_eq!(
            system
                .sections
                .iter()
                .map(|s| s.label.as_str())
                .collect::<Vec<_>>(),
            ["Base instructions", "Session environment"]
        );
        let user = inspect_message(&CompletionMessage::user(format!(
            "quoting {SESSION_ENVIRONMENT_HEADER} in a goal"
        )));
        assert!(
            user.sections.is_empty(),
            "provenance is a property of the assembled prompt, not of any text quoting it"
        );
    }

    /// The property the CLI renderer promises, asserted on the shape the deck
    /// actually receives: nothing is hidden by rendering sections instead of
    /// the flat body.
    #[test]
    fn the_sections_handed_to_the_deck_concatenate_to_the_message() {
        let content =
            format!("base{SESSION_ENVIRONMENT_HEADER}env{MEMORIES_HEADER}\n### m\nlesson");
        let mapped = inspect_message(&CompletionMessage::system(content.clone()));
        let rebuilt: String = mapped.sections.iter().map(|s| s.body.as_str()).collect();
        assert_eq!(rebuilt, content);
    }

    /// A system message the receipt could not resolve gets no breakdown, so
    /// the overlay falls back to the body rather than drawing one empty
    /// section labelled as a base persona.
    #[test]
    fn an_empty_system_message_gets_no_sections() {
        assert!(
            inspect_message(&CompletionMessage::system(""))
                .sections
                .is_empty()
        );
    }
}
