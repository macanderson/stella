//! Whether the credential chain's last step — the masked interactive prompt —
//! may fire in this process.
//!
//! `stella_model::credential::ApiKey::resolve` states the contract plainly:
//! "headless/non-interactive callers (CI, `--output-format stream-json`)
//! should pass `false` and get a clean `NotFound` instead of hanging on a read
//! from a stdin that isn't there." Nothing honoured it. Every `--model
//! provider/slug` path passed `true` unconditionally, so
//! `stella --model anthropic/… --output-format json run '…'` launched from an
//! attached terminal with no key stopped dead on a password prompt while the
//! caller waited on a JSON object that could never arrive — the machine
//! interface deadlocked on a human one.
//!
//! A process-wide latch rather than a threaded parameter because the two
//! non-`main` entry points into `Config::load` (`agent::run_init`,
//! `ingest_cmd::extract_all`) have no view of the requested output format and
//! have no business growing one; this mirrors [`JSON_SUMMARY_EMITTED`] and
//! `signals::INTERRUPTED`, the other two facts about the invocation that
//! `main` establishes once.
//!
//! [`JSON_SUMMARY_EMITTED`]: crate::note_json_summary_emitted

static INTERACTIVE_CREDENTIALS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

/// Forbid the interactive credential prompt for the rest of this process.
/// Called by `main` once the requested output format is known.
pub(crate) fn forbid_interactive_credentials() {
    INTERACTIVE_CREDENTIALS.store(false, std::sync::atomic::Ordering::SeqCst);
}

/// Whether `interactive` may still be honoured. `ApiKey::resolve` applies its
/// own `stdin().is_terminal()` guard on top of this; this is the *policy*
/// half, which a tty check cannot answer.
pub(super) fn interactive_allowed() -> bool {
    INTERACTIVE_CREDENTIALS.load(std::sync::atomic::Ordering::SeqCst)
}
