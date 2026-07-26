//! Lossless CGP-to-pipeline recall projection.

use std::collections::HashSet;

use stella_pipeline::RecalledFrame;

/// Preserve host-owned provider identity and the frame's complete provenance.
/// Source is the origin-most actor; method is the latest derivation step.
pub(super) fn project_recalled_frame(
    attributed: crate::contextgraph::AttributedContextFrame,
) -> Option<RecalledFrame> {
    let frame = attributed.frame;
    let citation_label = frame.citation_label.clone()?;
    let source = frame
        .provenance
        .iter()
        .find_map(|entry| entry.by.clone())
        .unwrap_or_else(|| attributed.provider.clone());
    let method = frame
        .provenance
        .iter()
        .rev()
        .find_map(|entry| entry.method.clone());
    let uri = frame
        .uri
        .clone()
        .or_else(|| frame.provenance.iter().find_map(|entry| entry.uri.clone()));
    let kind = serde_json::to_value(frame.kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    Some(RecalledFrame {
        citation_label,
        provider: attributed.provider,
        source,
        kind,
        uri,
        method,
        content: frame.content.as_deref().unwrap_or("").trim().to_string(),
        token_cost: frame.token_cost,
        id: Some(frame.id),
        // Phase 2 (#713): the store already minted this over exactly the bytes
        // that became `content`; dropping it here is what made every recall
        // event's `content_digest` null. Carried, never recomputed — a locally
        // derived hash would agree with the provider's only by luck (this
        // projection trims the content), and a digest that does not match the
        // record it claims to identify is worse than none.
        content_digest: frame.content_digest,
    })
}

/// Whether a recalled frame has been suppressed — quarantined by repeated
/// untruthful citations, or explicitly forgotten by the user.
///
/// Covers both recallable kinds. `episode` used to be excluded, which made an
/// entire class of frame unsuppressable: an episode is a verbatim copy of a
/// past user prompt, it is recalled and injected exactly like a memory, and
/// `stella memory forget` silently had no effect on one. A prior instruction
/// could therefore keep surfacing in unrelated runs with no way to stop it —
/// the shape that let "can you remove the witness tests please" reach the
/// witness author on a task about a TUI keybinding.
pub(super) fn is_suppressed_local_frame(
    frame: &RecalledFrame,
    suppressed: &HashSet<String>,
) -> bool {
    frame.provider == "workspace-memory"
        && matches!(frame.kind.as_str(), "memory" | "episode")
        && frame.id.as_ref().is_some_and(|id| suppressed.contains(id))
}
