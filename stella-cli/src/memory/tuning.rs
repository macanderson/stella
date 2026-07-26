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

/// Phase 2 (#713): whether `context.lifecycle.enabled` is on for this session.
///
/// Same failure posture as [`session_retrieval_settings`] and for a stronger
/// reason: an unreadable settings file degrades to `false`, which is the
/// setting's own default and the state that preserves every pre-adaptive
/// behavior. Failing *open* here would turn a typo elsewhere in the file into
/// silently enabling a lifecycle the user never asked for.
pub fn session_lifecycle_enabled(workspace_root: &std::path::Path) -> bool {
    crate::settings::Settings::load(workspace_root)
        .ok()
        .and_then(|s| s.context)
        .is_some_and(|c| c.lifecycle.enabled)
}
