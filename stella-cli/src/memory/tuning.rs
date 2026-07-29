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
/// Degrades to `false` on an unreadable settings file — which is NOT the
/// shipped default (the lifecycle ships on; see `LifecycleSettings::default`)
/// but the deliberate fail-safe posture, opposite to the retrieval read above:
/// an unparseable file may be the very file in which someone turned the
/// lifecycle off, and silently overriding an opt-out we failed to read is
/// worse than leaving a default unapplied.
pub fn session_lifecycle_enabled(workspace_root: &std::path::Path) -> bool {
    let Ok(settings) = crate::settings::Settings::load(workspace_root) else {
        // Unreadable or malformed. This is the one case that does NOT take the
        // default: a file we could not parse may be the file in which someone
        // turned the lifecycle off, and silently overriding an opt-out we
        // failed to read is worse than leaving a default unapplied.
        return false;
    };
    // An absent `context` block means "the defaults", not "off" — a workspace
    // with no settings.json at all is the common case, and it must land on the
    // shipped default like every other one.
    settings.context.unwrap_or_default().lifecycle.enabled
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

/// Phase 4 (#715): the efficacy thresholds that decide when a record's
/// selection health counts as failing.
///
/// Degrades to the documented defaults for the same reason
/// [`inferred_directive_promotion`] does. The conversion is here rather than a
/// `From` impl in `stella-core` so the fold stays free of any settings type —
/// it is a pure function over evidence and must remain testable without a
/// workspace.
pub(crate) fn selection_health_policy(
    workspace_root: &std::path::Path,
) -> stella_core::context_record::SelectionHealthPolicy {
    let efficacy = crate::settings::Settings::load(workspace_root)
        .ok()
        .and_then(|s| s.context)
        .map(|c| c.efficacy)
        .unwrap_or_default();
    stella_core::context_record::SelectionHealthPolicy {
        min_attributable_uses: efficacy.min_attributable_uses,
        not_helpful_ratio_threshold: efficacy.not_helpful_ratio_threshold,
        min_attribution_confidence: efficacy.min_attribution_confidence,
    }
}

#[cfg(test)]
mod tests {
    use super::session_lifecycle_enabled;

    /// The case that decides whether the flip actually shipped: a workspace
    /// with no `settings.json` at all. The reader used to ask
    /// `is_some_and(..)` on an `Option<ContextSettings>`, so an absent block
    /// answered `false` no matter what the struct's default was — flipping
    /// `LifecycleSettings::default()` alone would have left every real user off.
    #[test]
    fn a_workspace_with_no_settings_file_gets_the_lifecycle() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            session_lifecycle_enabled(dir.path()),
            "a fresh workspace must land on the shipped default"
        );
    }

    /// An empty `context` block means "the defaults", not "off".
    #[test]
    fn an_empty_context_block_gets_the_lifecycle() {
        let dir = tempfile::tempdir().expect("tempdir");
        let stella = dir.path().join(".stella");
        std::fs::create_dir_all(&stella).expect("stella dir");
        std::fs::write(stella.join("settings.json"), r#"{"context":{}}"#).expect("write");
        assert!(session_lifecycle_enabled(dir.path()));
    }

    /// An explicit opt-out is honored — the whole point of keeping the switch.
    #[test]
    fn an_explicit_false_turns_the_lifecycle_off() {
        let dir = tempfile::tempdir().expect("tempdir");
        let stella = dir.path().join(".stella");
        std::fs::create_dir_all(&stella).expect("stella dir");
        std::fs::write(
            stella.join("settings.json"),
            r#"{"context":{"lifecycle":{"enabled":false}}}"#,
        )
        .expect("write");
        assert!(!session_lifecycle_enabled(dir.path()));
    }

    /// A file we cannot parse is the one case that does not take the default:
    /// it may be the file in which someone turned the lifecycle off, and
    /// overriding an opt-out we failed to read is worse than leaving a default
    /// unapplied.
    #[test]
    fn an_unparseable_settings_file_stays_off() {
        let dir = tempfile::tempdir().expect("tempdir");
        let stella = dir.path().join(".stella");
        std::fs::create_dir_all(&stella).expect("stella dir");
        std::fs::write(stella.join("settings.json"), "{ not json").expect("write");
        assert!(!session_lifecycle_enabled(dir.path()));
    }
}
