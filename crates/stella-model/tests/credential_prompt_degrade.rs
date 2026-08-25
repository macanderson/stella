//! Witness for the credential-resolution resilience contract.
//!
//! `ApiKey::resolve` documents that an interactive caller that cannot actually
//! read stdin should receive a clean [`CredentialError::NotFound`] — "instead of
//! hanging on a read from a stdin that isn't there" (see `resolve`'s doc and the
//! module's "interactive prompt on first use, which never silently fails with an
//! opaque provider error" contract). A prompt that fails must therefore degrade
//! to `NotFound`; it must never surface the raw
//! [`CredentialError::PromptFailed`] that the underlying password reader emits.
//!
//! Until #4576 this file could not reach that arm. It drove the shipping
//! `ApiKey::resolve`, whose prompt is `TerminalPrompt`, and under `cargo test`
//! `stdout().is_terminal()` is false — so the gate declined and the swallow
//! underneath it never ran. The test was green because of the gate. Every
//! assertion here now goes through `ApiKey::resolve_with_prompt` and a prompt
//! that reports itself available, and each one counts the calls it received, so
//! a regression to "the gate declined" fails on the count rather than passing on
//! the error.

use std::cell::Cell;

use stella_model::credential::{ApiKey, CredentialError, CredentialPrompt, CredentialSource};

/// An env var name unique enough that no other test (or the host environment)
/// will have set it, so `resolve` falls all the way through flag → env → file
/// and reaches the interactive step.
const UNSET_ENV_VAR: &str = "STELLA_WITNESS_PROMPT_DEGRADE_KEY_V1";

/// A prompt that is always available and answers with whatever the test set,
/// counting how many times it was asked.
///
/// `Cell` rather than an atomic: `resolve_with_prompt` takes `&dyn
/// CredentialPrompt` and calls it on the caller's own thread, so there is no
/// sharing to synchronise.
struct ScriptedPrompt {
    answer: Result<String, CredentialError>,
    asked: Cell<usize>,
}

impl ScriptedPrompt {
    fn answering(answer: Result<String, CredentialError>) -> Self {
        Self {
            answer,
            asked: Cell::new(0),
        }
    }
}

impl CredentialPrompt for ScriptedPrompt {
    fn can_prompt(&self, interactive: bool) -> bool {
        interactive
    }

    fn ask(&self, _provider_id: &str, _env_var: &str) -> Result<String, CredentialError> {
        self.asked.set(self.asked.get() + 1);
        self.answer.clone()
    }
}

fn clear_env() {
    // SAFETY: a uniquely-named env var; the tests in this file own it.
    unsafe {
        std::env::remove_var(UNSET_ENV_VAR);
    }
}

/// The regression guard: the prompt runs, it fails, and the failure does not
/// escape as `PromptFailed`.
#[test]
fn a_failing_prompt_degrades_to_not_found() {
    clear_env();
    let prompt = ScriptedPrompt::answering(Err(CredentialError::PromptFailed(
        "Device not configured (os error 6)".into(),
    )));

    let err = ApiKey::resolve_with_prompt("witness", UNSET_ENV_VAR, None, None, true, &prompt)
        .unwrap_err();

    assert_eq!(
        prompt.asked.get(),
        1,
        "the prompt must actually be reached — a declined gate would make the \
         assertion below pass without exercising the swallow at all"
    );
    assert!(
        !matches!(err, CredentialError::PromptFailed(_)),
        "a failed prompt must degrade to NotFound, not surface the raw prompt \
         failure; got {err:?}"
    );
    assert_eq!(
        err,
        CredentialError::NotFound {
            env_var: UNSET_ENV_VAR.to_string()
        },
        "interactive resolution with no usable prompt must yield NotFound"
    );
}

/// An empty answer is the other failure `prompt_for_key` reports, and it takes
/// the same path — named separately because it is the one a user can cause by
/// pressing return, not a broken terminal.
#[test]
fn an_empty_answer_degrades_to_not_found_too() {
    clear_env();
    let prompt = ScriptedPrompt::answering(Err(CredentialError::PromptFailed(
        "empty input — no credential entered".into(),
    )));

    let err = ApiKey::resolve_with_prompt("witness", UNSET_ENV_VAR, None, None, true, &prompt)
        .unwrap_err();

    assert_eq!(prompt.asked.get(), 1);
    assert_eq!(
        err,
        CredentialError::NotFound {
            env_var: UNSET_ENV_VAR.to_string()
        }
    );
}

/// The other side of the arm, so the degradation above reads as a swallowed
/// failure rather than an unreachable branch: a prompt that answers produces a
/// key attributed to [`CredentialSource::Interactive`], which is the
/// discriminant `stella-cli`'s write-back keys off.
#[test]
fn a_successful_prompt_resolves_as_interactive() {
    clear_env();
    let prompt = ScriptedPrompt::answering(Ok("sk-typed-at-the-prompt".to_string()));

    let (key, source) =
        ApiKey::resolve_with_prompt("witness", UNSET_ENV_VAR, None, None, true, &prompt).unwrap();

    assert_eq!(prompt.asked.get(), 1);
    assert_eq!(key.reveal(), "sk-typed-at-the-prompt");
    assert_eq!(source, CredentialSource::Interactive);
}

/// A non-interactive caller never reaches the prompt at all — the contract the
/// gate exists for, asserted on the call count instead of inferred from the
/// error.
#[test]
fn a_non_interactive_caller_never_asks() {
    clear_env();
    let prompt = ScriptedPrompt::answering(Ok("sk-never-read".to_string()));

    let err = ApiKey::resolve_with_prompt("witness", UNSET_ENV_VAR, None, None, false, &prompt)
        .unwrap_err();

    assert_eq!(
        prompt.asked.get(),
        0,
        "a caller in non-interactive mode must not be prompted"
    );
    assert_eq!(
        err,
        CredentialError::NotFound {
            env_var: UNSET_ENV_VAR.to_string()
        }
    );
}
