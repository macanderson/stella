//! How a session resolves its retrieval knobs.
//!
//! Its own module because the failure posture is the interesting part, and it
//! differs from every other settings read in this crate (#712 deliverable 8).

use crate::settings::RetrievalSettings;

/// The retrieval settings in force for a session, read once at open.
///
/// An unreadable or malformed settings file degrades to the shipped defaults
/// rather than failing the session. These are ranking knobs: refusing to open
/// the context plane over a typo in one would cost the user their entire
/// memory to avoid a slightly different frame order. Contrast the suppression
/// read, which fails *closed* — there, the degraded outcome is surfacing
/// something a person asked to forget, which is not a smaller harm than
/// surfacing nothing.
pub(super) fn session_retrieval_settings(workspace_root: &std::path::Path) -> RetrievalSettings {
    crate::settings::Settings::load(workspace_root)
        .ok()
        .and_then(|s| s.context)
        .map(|c| c.retrieval)
        .unwrap_or_default()
}

/// Whether `context.lifecycle.enabled` is on for this session (#713, #714).
///
/// Degrades to `false` on an unreadable settings file. That is the setting's
/// own default, and it is the opposite posture to the retrieval read above,
/// deliberately: this flag moves both the recall plane and the learning loop
/// off the paths users run today. Failing *open* would turn a typo elsewhere in
/// the file into silently enabling a lifecycle nobody asked for, and the safe
/// answer to "I could not tell" is "keep doing what already works".
pub fn session_lifecycle_enabled(workspace_root: &std::path::Path) -> bool {
    crate::settings::Settings::load(workspace_root)
        .ok()
        .and_then(|s| s.context)
        .is_some_and(|c| c.lifecycle.enabled)
}

/// Phase 3 (#714): the promotion thresholds gating when observations may become
/// a durable record.
///
/// Degrades to the documented defaults (three observations across three
/// distinct tasks) on an unreadable settings file, matching the retrieval read
/// above: these are thresholds, and refusing to learn because a settings file
/// has a typo is a worse answer than learning at the documented bar.
pub(super) fn inferred_directive_promotion(
    workspace_root: &std::path::Path,
) -> crate::settings::InferredDirectivePromotion {
    crate::settings::Settings::load(workspace_root)
        .ok()
        .and_then(|s| s.context)
        .map(|c| c.promotion.inferred_directive)
        .unwrap_or_default()
}
