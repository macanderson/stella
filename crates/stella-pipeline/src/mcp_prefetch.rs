//! The orchestrator's Best-of-N MCP pre-fetch hook (issue #248):
//! [`fold`] is called once at the top of a fan-out, folding shared context
//! into every candidate's starting messages instead of each candidate
//! independently paying to look it up. Split out of `pipeline.rs` to keep
//! the orchestrator small.

use stella_protocol::CompletionMessage;

use crate::ports::McpPrefetchPort;

/// Ceiling on the pre-fetched context, in chars (~5k tokens) — the same
/// budget the recall clamp settled on, and for the same reason: this block
/// rides ahead of every candidate's every turn, so an unbounded server
/// answer multiplied itself by N candidates × every revision. Head-kept
/// with an in-band marker, so a clamped fetch can never be mistaken for a
/// whole one.
const PREFETCH_BUDGET_CHARS: usize = 20_000;

/// `goal`/`n` describe the fan-out about to start; `port` is `None` when the
/// caller wired no pre-fetch hook. `Some` only when there is genuinely new
/// context to share — `None` means the caller's original `base_messages`
/// must be used unchanged (never allocate a redundant clone).
pub(crate) async fn fold(
    port: Option<&dyn McpPrefetchPort>,
    goal: &str,
    n: u32,
    base_messages: &[CompletionMessage],
) -> Option<Vec<CompletionMessage>> {
    let context = port?.prefetch(goal).await?;
    let context = {
        let mut kept: String = context.chars().take(PREFETCH_BUDGET_CHARS).collect();
        if kept.chars().count() < context.chars().count() {
            kept.push_str("\n[… prefetched context truncated at the fan-out budget …]");
        }
        kept
    };
    let mut messages = base_messages.to_vec();
    messages.push(CompletionMessage::user(format!(
        "Shared MCP context gathered once before the {n}-candidate fan-out:\n\n{context}"
    )));
    Some(messages)
}
