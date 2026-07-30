//! `stella inspect` — the read surface over the context receipts: show the
//! exact `Vec<CompletionMessage>` a past model call was sent, rebuilt from the
//! append-only fold rather than from any live engine state.
//!
//! The storage layer has been able to do this since the receipts plane landed
//! ([`Store::reconstruct_call`]); until now nothing exposed it, so the honest
//! answer to "what context did the model actually get?" was "it's in SQLite,
//! write your own query." This is that query.
//!
//! Reads only — like `stella stats`, it refuses to create `.stella/` as a side
//! effect of being asked a question, and needs no API key or provider.
//!
//! Three levels, narrowing as you supply more:
//!
//! - `stella inspect` — executions that have receipts, most recent first.
//! - `stella inspect <id>` — every recorded model call of one execution.
//! - `stella inspect <id> --step N` — the reconstructed context of one call.
//!
//! ## What "verified" means in the output
//!
//! Every block whose preimage came from the event journal is re-hashed and
//! checked against the digest the receipt recorded at emission, so a torn
//! journal or a fabricated block surfaces as a mismatch instead of a
//! plausible-looking lie. The two gap kinds (the system prefix and the
//! assembled user/recall message) are stored as local bytes, so their check is
//! tautological and is deliberately not counted as evidence — see
//! [`stella_store::Reconstruction`]. The banner reports both facts rather than
//! flattening them into one word.

use serde::Serialize;
use stella_protocol::{CompletionMessage, MessageRole, ToolOutput};
use stella_store::{Reconstruction, RecordedCall, Store};

/// How much of a message body the text format prints before eliding. The whole
/// point of this command is seeing the real bytes, so the cap is generous and
/// `--full` removes it entirely.
const DEFAULT_BODY_LIMIT: usize = 4_000;

/// Prompt preview width in the execution index.
const PROMPT_PREVIEW: usize = 68;

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum InspectFormat {
    /// Human-readable, role-delimited transcript.
    Text,
    /// The reconstructed messages plus the verification verdict, as JSON.
    Json,
}

/// `stella inspect [id] [--turn T] [--step N] [--call-seq S]`.
pub(crate) fn run_inspect(
    execution_id: Option<i64>,
    turn: u32,
    step: Option<u64>,
    call_seq: u64,
    format: InspectFormat,
    full: bool,
) -> Result<(), String> {
    let store = open_readonly_store()?;
    match (execution_id, step) {
        (None, _) => list_executions(&store, format),
        (Some(id), None) => list_calls(&store, id, format),
        (Some(id), Some(step)) => show_call(&store, id, turn, step, call_seq, format, full),
    }
}

/// Open the workspace store without creating it. Mirrors `stella stats`: a
/// question about recorded history must never write.
fn open_readonly_store() -> Result<Store, String> {
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let db_path = stella_store::existing_workspace_private_sqlite_path(&workspace_root, "store.db")
        .map_err(|e| format!("cannot resolve local store: {e}"))?;
    if db_path.is_none() {
        return Err(
            "no local store in this workspace yet — run something first, then inspect it".into(),
        );
    }
    Store::open(&workspace_root).map_err(|e| format!("cannot open local store: {e}"))
}

fn list_executions(store: &Store, format: InspectFormat) -> Result<(), String> {
    let rows = store
        .inspectable_executions(40)
        .map_err(|e| format!("cannot read receipts: {e}"))?;
    if matches!(format, InspectFormat::Json) {
        return print_json(&rows.iter().map(execution_json).collect::<Vec<_>>());
    }
    if rows.is_empty() {
        println!(
            "No reconstructable executions recorded yet.\n\
             Receipts are written by builds carrying the context-receipts plane; \
             executions from before it landed have none."
        );
        return Ok(());
    }
    println!("{:>8}  {:>5}  {:<20}  PROMPT", "EXEC", "CALLS", "STARTED");
    for row in &rows {
        println!(
            "{:>8}  {:>5}  {:<20}  {}",
            row.execution_id,
            row.calls,
            truncate(&row.started_at, 20),
            truncate(row.prompt.lines().next().unwrap_or(""), PROMPT_PREVIEW),
        );
    }
    println!("\nstella inspect <EXEC> to list that execution's model calls.");
    Ok(())
}

fn list_calls(store: &Store, execution_id: i64, format: InspectFormat) -> Result<(), String> {
    let calls = store
        .recorded_calls(execution_id)
        .map_err(|e| format!("cannot read receipts: {e}"))?;
    if matches!(format, InspectFormat::Json) {
        return print_json(&calls.iter().map(call_json).collect::<Vec<_>>());
    }
    if calls.is_empty() {
        println!("Execution {execution_id} has no recorded receipts.");
        return Ok(());
    }
    // The FRAME column is empty for every call recorded while
    // `context.lifecycle.enabled` was off, which is the default — so it reads
    // as "not computed", not as "computed and empty". Two calls showing the
    // same id saw byte-identical context; that comparison is the column's job,
    // so the id is shown rather than the full hash it prefixes.
    let any_frame = calls.iter().any(|c| c.compiled_frame_id.is_some());
    println!(
        "{:>4}  {:>4}  {:>3}  {:<14}  {:<10}  {:>9}{}",
        "TURN",
        "STEP",
        "SEQ",
        "ROLE",
        "PROVIDER",
        "EST TOK",
        if any_frame { "  FRAME" } else { "" }
    );
    for call in &calls {
        println!(
            "{:>4}  {:>4}  {:>3}  {:<14}  {:<10}  {:>9}{}",
            call.turn_instance,
            call.step,
            call.call_seq,
            truncate(&call.call_role, 14),
            truncate(&call.provider, 10),
            call.estimated_input_tokens,
            match (&call.compiled_frame_id, any_frame) {
                (Some(id), _) => format!("  {id}"),
                (None, true) => "  —".to_string(),
                (None, false) => String::new(),
            }
        );
    }
    println!(
        "\nstella inspect {execution_id} --step <STEP> [--turn <TURN>] [--call-seq <SEQ>] \
         to see the context one call was sent."
    );
    Ok(())
}

fn show_call(
    store: &Store,
    execution_id: i64,
    turn: u32,
    step: u64,
    call_seq: u64,
    format: InspectFormat,
    full: bool,
) -> Result<(), String> {
    let recon = store
        .reconstruct_call(execution_id, turn, step, call_seq)
        .map_err(|e| format!("cannot reconstruct: {e}"))?;
    if recon.messages.is_empty() && recon.unresolved.is_empty() {
        return Err(format!(
            "no receipt for execution {execution_id} turn {turn} step {step} call-seq {call_seq} \
             — `stella inspect {execution_id}` lists what was recorded"
        ));
    }
    match format {
        InspectFormat::Json => print_json(&reconstruction_json(&recon)),
        InspectFormat::Text => {
            print_reconstruction(&recon, execution_id, turn, step, call_seq, full);
            Ok(())
        }
    }
}

fn print_reconstruction(
    recon: &Reconstruction,
    execution_id: i64,
    turn: u32,
    step: u64,
    call_seq: u64,
    full: bool,
) {
    println!("execution {execution_id} · turn {turn} · step {step} · call-seq {call_seq}");
    println!(
        "{} message(s){}",
        recon.messages.len(),
        if full { ", full bodies" } else { "" }
    );
    // Report the two failure modes separately: an unresolved block is a
    // documented coverage gap, a digest mismatch is a tampering/torn-journal
    // signal. Collapsing them into "unverified" would hide which one happened.
    if !recon.unresolved.is_empty() {
        println!(
            "!  {} block(s) could not be resolved (synthetic results, discarded \
             speculation, or attachments): {}",
            recon.unresolved.len(),
            recon.unresolved.join(", ")
        );
    }
    if !recon.digest_mismatches.is_empty() {
        println!(
            "!! {} block(s) did NOT re-hash to their recorded digest — the journal is torn \
             or was altered: {}",
            recon.digest_mismatches.len(),
            recon.digest_mismatches.join(", ")
        );
    }
    if recon.is_verified() {
        println!("verified: every journal-resolved block re-hashed to its recorded digest");
    }
    for (index, message) in recon.messages.iter().enumerate() {
        println!("\n─── [{index}] {} ───", role_tag(message.role));
        let body = message_body(message);
        if full || body.len() <= DEFAULT_BODY_LIMIT {
            println!("{body}");
        } else {
            let cut = floor_char_boundary(&body, DEFAULT_BODY_LIMIT);
            println!(
                "{}\n… {} more bytes (--full to print, --format json for the exact bytes)",
                &body[..cut],
                body.len() - cut
            );
        }
    }
}

/// The message's printable body: text content, plus a compact rendering of any
/// tool calls/results it carried (they are separate blocks in the receipt but
/// belong to one message on the wire).
///
/// Shared with the deck's INSPECT overlay (`command_deck::service_inspect_action`):
/// the CLI and the deck must never disagree about what a message looked like,
/// so there is one renderer, not a copy per surface.
pub(crate) fn message_body(message: &CompletionMessage) -> String {
    let mut out = String::new();
    if !message.content.is_empty() {
        out.push_str(&message.content);
    }
    for call in &message.tool_calls {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!(
            "→ tool_call {} {} {}",
            call.call_id, call.name, call.input
        ));
    }
    for result in &message.tool_results {
        if !out.is_empty() {
            out.push('\n');
        }
        // Render the payload directly rather than serializing the whole
        // `ToolOutput` enum. Serialization escapes every newline and quote
        // inside a result body (file contents, command stdout), turning a
        // readable transcript into one long line of `\n` and `\"`.
        out.push_str("← tool_result ");
        out.push_str(&result.call_id);
        out.push('\n');
        match &result.output {
            ToolOutput::Ok { content } => out.push_str(content),
            ToolOutput::Error { message } => out.push_str(&format!("error: {message}")),
        }
    }
    out
}

/// The stable wire tag for a message role — the same token the deck's INSPECT
/// overlay shows, for the same reason [`message_body`] is shared.
pub(crate) fn role_tag(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

#[derive(Serialize)]
struct ExecutionJson {
    execution_id: i64,
    kind: String,
    prompt: String,
    started_at: String,
    calls: u64,
}

#[derive(Serialize)]
struct CallJson {
    turn_instance: u32,
    step: u64,
    call_seq: u64,
    call_role: String,
    provider: String,
    model: String,
    estimated_input_tokens: u64,
    /// Phase 2 (#713): the compiled frame's identity, when the lifecycle was
    /// on for this call. Omitted rather than null when it was off — a receipt
    /// with no frame is not a receipt with an empty frame.
    #[serde(skip_serializing_if = "Option::is_none")]
    compiled_frame_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frame_hash: Option<String>,
}

#[derive(Serialize)]
struct MessageJson {
    role: &'static str,
    content: String,
}

#[derive(Serialize)]
struct ReconstructionJson {
    verified: bool,
    unresolved: Vec<String>,
    digest_mismatches: Vec<String>,
    messages: Vec<MessageJson>,
}

fn execution_json(row: &stella_store::InspectableExecution) -> ExecutionJson {
    ExecutionJson {
        execution_id: row.execution_id,
        kind: row.kind.clone(),
        prompt: row.prompt.clone(),
        started_at: row.started_at.clone(),
        calls: row.calls,
    }
}

fn call_json(call: &RecordedCall) -> CallJson {
    CallJson {
        turn_instance: call.turn_instance,
        step: call.step,
        call_seq: call.call_seq,
        call_role: call.call_role.clone(),
        provider: call.provider.clone(),
        model: call.model.clone(),
        estimated_input_tokens: call.estimated_input_tokens,
        compiled_frame_id: call.compiled_frame_id.clone(),
        frame_hash: call.frame_hash.clone(),
    }
}

fn reconstruction_json(recon: &Reconstruction) -> ReconstructionJson {
    ReconstructionJson {
        verified: recon.is_verified(),
        unresolved: recon.unresolved.clone(),
        digest_mismatches: recon.digest_mismatches.clone(),
        messages: recon
            .messages
            .iter()
            .map(|m| MessageJson {
                role: role_tag(m.role),
                content: message_body(m),
            })
            .collect(),
    }
}

fn print_json<T: Serialize>(value: &T) -> Result<(), String> {
    let rendered =
        serde_json::to_string_pretty(value).map_err(|e| format!("cannot render json: {e}"))?;
    println!("{rendered}");
    Ok(())
}

fn truncate(s: &str, limit: usize) -> String {
    if s.chars().count() <= limit {
        return s.to_string();
    }
    let cut: String = s.chars().take(limit.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// Largest index `<= limit` that is a char boundary — slicing a UTF-8 body at a
/// raw byte offset would panic on any multibyte character.
fn floor_char_boundary(s: &str, limit: usize) -> usize {
    let mut cut = limit.min(s.len());
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_counts_characters_not_bytes() {
        assert_eq!(truncate("abc", 8), "abc");
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
        // Multibyte input must not panic or split a character.
        assert_eq!(truncate("ααααααα", 3), "αα…");
    }

    #[test]
    fn body_cut_lands_on_a_char_boundary() {
        let s = "α".repeat(100);
        let cut = floor_char_boundary(&s, 51);
        assert!(s.is_char_boundary(cut), "cut must be sliceable");
        assert_eq!(cut, 50, "rounds down to the boundary below the limit");
    }

    #[test]
    fn message_body_renders_tool_round_trips_not_just_text() {
        use stella_protocol::{ToolCall, ToolOutput, ToolResult};
        let message = CompletionMessage {
            role: MessageRole::Assistant,
            content: "reading the file".into(),
            tool_calls: vec![ToolCall {
                call_id: "c1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "a.rs"}),
            }],
            tool_results: vec![],
            attachments: vec![],
        };
        let body = message_body(&message);
        assert!(body.contains("reading the file"));
        assert!(body.contains("→ tool_call c1 read_file"));

        let tool = CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: vec![],
            tool_results: vec![ToolResult {
                call_id: "c1".into(),
                output: ToolOutput::Ok {
                    content: "fn main() {}".into(),
                },
            }],
            attachments: vec![],
        };
        assert!(message_body(&tool).contains("← tool_result c1"));
    }

    #[test]
    fn message_body_keeps_newlines_and_quotes_unescaped() {
        // Tool results routinely carry file contents and command stdout with
        // embedded newlines and quotes. The old renderer serialized the whole
        // `ToolOutput` enum, which escaped every `\n` and `"` into one long
        // unreadable line. The body must print the raw payload instead.
        use stella_protocol::ToolResult;
        let tool = CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: vec![],
            tool_results: vec![ToolResult {
                call_id: "c1".into(),
                output: ToolOutput::Ok {
                    content: "fn main() {\n    println!(\"hi\");\n}".into(),
                },
            }],
            attachments: vec![],
        };
        let body = message_body(&tool);
        assert!(body.contains("fn main() {\n    println!(\"hi\");\n}"));
        assert!(!body.contains("\\n"));
        assert!(!body.contains("\\\""));
    }

    #[test]
    fn message_body_labels_error_results() {
        use stella_protocol::ToolResult;
        let tool = CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: vec![],
            tool_results: vec![ToolResult {
                call_id: "e1".into(),
                output: ToolOutput::Error {
                    message: "file not found".into(),
                },
            }],
            attachments: vec![],
        };
        let body = message_body(&tool);
        assert!(body.contains("← tool_result e1\nerror: file not found"));
    }
}
