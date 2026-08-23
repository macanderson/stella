use super::*;
// Straight from the table's own module rather than a `config` re-export: it
// is read only here, and a `pub(crate) use` for a test-only name is an
// unused import in every non-test build.
use super::providers::COMMON_KEY_ENV_VARS;

mod credential;
mod docs_sync;
mod parity;
// `pub(in crate::config)` rather than private: `reload::completeness`'s ledger
// drives a real reload against this module's `reload_fixture`, and it is a
// sibling of `tests`, not a descendant.
pub(in crate::config) mod resolution;
mod trusted_engine;

/// Helper: a Settings value parsed from JSON, as the scope-merge would
/// produce it — the seam for exercising resolution without touching
/// `$HOME`, `/etc`, or a real workspace.
fn settings_from(json: &str) -> crate::settings::Settings {
    serde_json::from_str(json).expect("test settings JSON must parse")
}
