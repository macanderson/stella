// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What `stella plugin install` and `stella plugin doctor` tell a user about
//! the environment their plugin will *not* get.
//!
//! # This module reports; it does not enforce
//!
//! It used to do both, and the "both" was the defect (#3512). The refusal
//! lived here, in a `std::process::Command` builder nothing spawned, while the
//! socket's only transport builds a `tokio::process::Command` in
//! `stella_runtime::wrapper` — two types that cannot meet. So the narrowing
//! held for `stella-cli` and for no other driver: `stella-serve` and an
//! embedded `stella-engine` host, the two acceptance criteria beside the CLI
//! in `doc:wrapper-socket` §6, each reached the socket's own constructor and
//! handed the plugin the key that pays for the agent. Invariant 3 was this
//! binary's policy rather than a property of the socket.
//!
//! The enforcement is now at the boundary every host crosses
//! ([`stella_runtime::wrapper::SubprocessWrapper::declare`], which reports
//! what it withheld). What is left here is the half only a CLI can do: say so
//! at **consent time**, before the user agrees to an install, rather than
//! leaving them to discover a missing variable at the first dispatch.
//!
//! # Default-deny, and why it is `env_clear` rather than a scrub
//!
//! `stella_tools::subprocess_env::scrub_spawn_env` is a **denylist**: it
//! removes the credential and ambient-authority families from an otherwise
//! inherited environment, which is the right shape for a tool the user's own
//! agent runs on the user's own behalf. A plugin is a different party. It is
//! third-party code the user installed once, and the set of variables it needs
//! is knowable only by its author — so it declares them, the consent prompt
//! shows them, and the child is started from empty
//! ([`stella_plugin::Runtime::child_env`]). Anything the author did not name,
//! and anything the operator's shell picked up after the install, is simply
//! not there.
//!
//! # The one narrowing the manifest cannot make
//!
//! A manifest may *ask* for `ANTHROPIC_API_KEY`. Honouring that would end
//! invariant 3 as a property: "every model call is made by the host" survives
//! only while a plugin cannot make one itself. So a declared name that
//! `stella-tools` grades as a model credential is refused at spawn and named,
//! never dropped silently — the plugin author has a real fix (ask the host for
//! a role instead), and they can only take it if they are told.
//!
//! Ambient-authority names (`SSH_AUTH_SOCK`, `GIT_SSH_COMMAND`) are
//! deliberately **not** refused: they are disclosed in the consent text, a
//! plugin that drives git over a deploy key genuinely needs one, and unlike a
//! credential they do not let the plugin spend the user's model budget.
//! Refusing them would break a legitimate plugin to prevent nothing the
//! install consent did not already show.

use stella_runtime::wrapper::refuses_env_name;

/// The declared names a host will refuse as model credentials.
///
/// A thin projection of [`stella_runtime::wrapper::refuses_env_name`] over a
/// manifest's declared list, and deliberately not a second judgement: the note
/// `stella plugin install` prints beside the consent text and the names the
/// socket actually withholds are then the same decision, so a prompt cannot
/// promise a variable the spawn refuses.
pub(crate) fn refused_credentials(names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|name| refuses_env_name(name))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The consent text names credentials and only credentials.
    ///
    /// The pairing this report must not drift from — that the socket withholds
    /// exactly these — is asserted across the crate boundary by
    /// `what_install_corrects_is_exactly_what_the_socket_withholds` in
    /// `plugin_cmd::tests`, because the socket is the thing that now decides.
    #[test]
    fn a_declared_model_credential_is_named_and_an_ordinary_variable_is_not() {
        let declared: Vec<String> = ["PLUGIN_MODE", "ANTHROPIC_API_KEY", "PATH", "SSH_AUTH_SOCK"]
            .into_iter()
            .map(str::to_string)
            .collect();
        assert_eq!(
            refused_credentials(&declared),
            vec!["ANTHROPIC_API_KEY".to_string()],
            "credentials are refused; ambient authority is disclosed and admitted"
        );
        assert!(refused_credentials(&[]).is_empty());
    }
}
