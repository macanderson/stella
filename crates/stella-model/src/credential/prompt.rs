//! The interactive step of the credential chain, behind a port.
//!
//! [`ApiKey::resolve`](super::ApiKey::resolve) ends in "ask the human", which
//! is two decisions and one side effect: whether a visible, answerable prompt
//! can exist here at all, and — if it can — reading the secret off the
//! terminal. Both live behind [`CredentialPrompt`] so a caller can supply
//! either one, and [`TerminalPrompt`] is the shipping implementation the
//! zero-argument [`ApiKey::resolve`](super::ApiKey::resolve) uses.
//!
//! The seam exists because the alternative could not be tested. Under `cargo
//! test`, `stdout().is_terminal()` is false, so [`TerminalPrompt::can_prompt`]
//! declines and the arm below it — the one that swallows a failed prompt and
//! degrades to [`CredentialError::NotFound`] — was never reached by the test
//! that claimed to cover it. It was green because of the gate, not because of
//! the swallow (#4576).
//!
//! It is also where #3052 lands. `rpassword` opens `/dev/tty` directly on
//! Unix rather than writing to stdout, so [`TerminalPrompt::decide`]'s
//! `stdout_is_terminal` is a proxy for "the prompt is visible" rather than the
//! exact condition. Probing what `rpassword` actually opens changes one
//! function here — and a real `prompt_password` on a machine with a
//! controlling terminal would then block the suite forever if the tests still
//! had to reach it through the shipping implementation, which is why that fix
//! waits on this one.

use zeroize::Zeroizing;

use super::CredentialError;

/// Asking a human for a provider's API key.
///
/// Two methods rather than one so the gate and the read stay separable: a
/// caller that knows no human is present ([`can_prompt`](Self::can_prompt)
/// returning `false`) never constructs a prompt at all, and a test can admit
/// the gate while making the read fail.
///
/// [`ask`](Self::ask) yields the secret as a plain `String` — the same shape
/// [`ApiKey::new`](super::ApiKey::new) and `CredentialsFile::set` already take
/// — and the value becomes an [`ApiKey`](super::ApiKey), whose plaintext is
/// wiped on drop. An implementation that buffers the secret on the way (as
/// [`TerminalPrompt`] does) is responsible for wiping its own copy.
pub trait CredentialPrompt {
    /// Whether this invocation can host a prompt the user can both see and
    /// answer. `interactive` is the caller's own "would I even attempt this"
    /// decision — `stella-cli` clears it for a machine `--output-format` —
    /// and the implementation adds whatever it knows about the terminal.
    fn can_prompt(&self, interactive: bool) -> bool;

    /// Ask for `provider_id`'s key, naming `env_var` so the user can set it
    /// directly next time. Returns the entered secret with surrounding
    /// whitespace already trimmed; an empty answer is an error, not an empty
    /// key.
    fn ask(&self, provider_id: &str, env_var: &str) -> Result<String, CredentialError>;
}

/// The shipping prompt: `rpassword` on the controlling terminal, gated on
/// [`stella_tty::human_can_answer`].
#[derive(Debug, Clone, Copy, Default)]
pub struct TerminalPrompt;

impl TerminalPrompt {
    /// Whether this invocation can host the interactive credential prompt.
    /// Pure over already-observed booleans (rather than calling `IsTerminal`
    /// itself) so the exact condition is directly unit-testable — the same
    /// shape `stella-cli`'s `agent::engine::approval_capability_for` and
    /// `daemon::approval::console_is_interactive` use.
    ///
    /// The single "is a human present?" derivation (#3036), shared with
    /// `stella-cli`'s approval prompts without this crate depending on that
    /// one (invariant 1). `interactive` here already plays
    /// [`stella_tty::human_can_answer`]'s `interactive_output` role, so this
    /// only adds the check that was missing: a redirected stdout must decline
    /// exactly as a redirected stdin does, not just print a prompt nobody
    /// reads before blocking on an answer nobody can give.
    /// `stdout_is_terminal` is a proxy for whether `rpassword`'s prompt is
    /// actually visible rather than an exact match — it writes to `/dev/tty`
    /// directly on Unix, not stdout — tracked separately as #3052 rather than
    /// folded into that fix.
    pub fn decide(interactive: bool, stdin_is_terminal: bool, stdout_is_terminal: bool) -> bool {
        stella_tty::human_can_answer(interactive, stdin_is_terminal, stdout_is_terminal)
    }
}

impl CredentialPrompt for TerminalPrompt {
    fn can_prompt(&self, interactive: bool) -> bool {
        use std::io::IsTerminal;
        Self::decide(
            interactive,
            std::io::stdin().is_terminal(),
            std::io::stdout().is_terminal(),
        )
    }

    /// Prompt on the terminal, masking input (never echoed) since the
    /// terminal itself isn't a redaction boundary.
    fn ask(&self, provider_id: &str, env_var: &str) -> Result<String, CredentialError> {
        // `Zeroizing` because the raw prompt buffer is a second copy of the
        // secret that the caller never sees: the trimmed `String` returned
        // below is what becomes an `ApiKey`, and without this the untrimmed
        // original would be dropped intact into freed heap.
        let value = Zeroizing::new(
            rpassword::prompt_password(format!(
                "No {env_var} found for `{provider_id}`. Enter it now (saved to \
                 ~/.stella/credentials.toml for next time; input hidden): "
            ))
            .map_err(|e| CredentialError::PromptFailed(e.to_string()))?,
        );
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(CredentialError::PromptFailed(
                "empty input — no credential entered".into(),
            ));
        }
        Ok(trimmed.to_string())
    }
}
