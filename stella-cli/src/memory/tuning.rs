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
