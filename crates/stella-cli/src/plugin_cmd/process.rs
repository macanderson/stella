// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Turning a [`PluginHookRoute`] into a process, with the environment the
//! manifest declared and nothing else.
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
//! `stella-tools` grades as a model credential is refused at spawn, named in
//! the returned report rather than dropped silently — the plugin author has a
//! real fix (ask the host for a role instead), and they can only take it if
//! they are told.
//!
//! Ambient-authority names (`SSH_AUTH_SOCK`, `GIT_SSH_COMMAND`) are
//! deliberately **not** refused: they are disclosed in the consent text, a
//! plugin that drives git over a deploy key genuinely needs one, and unlike a
//! credential they do not let the plugin spend the user's model budget.
//! Refusing them would break a legitimate plugin to prevent nothing the
//! install consent did not already show.

use std::process::Command;

use stella_tools::subprocess_env::is_sensitive_env_name;

use super::roster::PluginHookRoute;

/// A command ready to spawn, plus what was taken away from it.
#[derive(Debug)]
pub(crate) struct PreparedCommand {
    /// The child, with its environment cleared and repopulated from the
    /// manifest's allowlist alone.
    ///
    /// Read by this module's tests and by nothing else in the binary yet: the
    /// site that *spawns* it is the wrapper socket's dispatcher, which lives
    /// in `stella-runtime` and is #3380's half of the work, not this one's.
    /// The lint is right that the field is unread here and the allow says so
    /// rather than denying it — the builder ships ahead of its caller because
    /// the refusal above is a security property that must be in place before
    /// the first dispatch, not bolted on beside it. Tracked in #3380.
    #[allow(dead_code)]
    pub(crate) command: Command,
    /// Allowlisted names refused as model credentials. Empty in the normal
    /// case; a caller must surface a non-empty list rather than swallow it.
    pub(crate) refused: Vec<String>,
}

/// The declared names a host will refuse as model credentials.
///
/// The single implementation of that judgement, so the note `stella plugin
/// install` prints beside the consent text and the names
/// [`prepare_command`] actually withholds cannot disagree — a consent prompt
/// that promised a variable the spawn then refuses is a prompt describing a
/// different program than the one that runs.
pub(crate) fn refused_credentials(names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|name| is_sensitive_env_name(name.as_ref()))
        .cloned()
        .collect()
}

/// Build the child process for `route`, reading the parent environment
/// through `lookup`.
///
/// `lookup` is a parameter rather than a `std::env::var` call so this is
/// testable without mutating the process environment — concurrent
/// `setenv`/`getenv` is undefined behaviour on POSIX and the test runner is
/// multi-threaded (the discipline `crate::paths` exists to enforce).
pub(crate) fn prepare_command<F>(route: &PluginHookRoute, mut lookup: F) -> PreparedCommand
where
    F: FnMut(&str) -> Option<String>,
{
    let refused = refused_credentials(&route.env_allowlist);
    let admitted: Vec<(String, String)> = route
        .env_allowlist
        .iter()
        .filter(|name| !refused.contains(name))
        .filter_map(|name| lookup(name).map(|value| (name.clone(), value)))
        .collect();

    // `argv[0]` is the program; the manifest guarantees a non-empty argv with
    // no blank entries, so an empty slice here is unreachable — but this is
    // runtime data from a third-party file, so it is handled rather than
    // indexed (invariant 5).
    let mut command = Command::new(route.argv.first().map(String::as_str).unwrap_or_default());
    command.args(route.argv.iter().skip(1));
    // The order matters: clear first, then add. Adding first and clearing
    // after would discard exactly the variables the manifest asked for.
    command.env_clear();
    for (name, value) in admitted {
        command.env(name, value);
    }
    PreparedCommand { command, refused }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_core::ports::Principal;
    use stella_plugin::HookEvent;

    fn route(env: &[&str], argv: &[&str]) -> PluginHookRoute {
        PluginHookRoute {
            plugin: "vera".into(),
            principal: Principal::Plugin("vera".into()),
            event: HookEvent::PreToolUse,
            argv: argv.iter().map(|arg| (*arg).to_string()).collect(),
            timeout_secs: 30,
            env_allowlist: env.iter().map(|name| (*name).to_string()).collect(),
        }
    }

    /// The parent this test pretends to be: a developer shell carrying a
    /// provider key, an agent socket, and a couple of ordinary variables.
    fn parent(name: &str) -> Option<String> {
        match name {
            "PLUGIN_MODE" => Some("wrapper".into()),
            "PATH" => Some(std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into())),
            "STELLA_PLUGIN_SECRET" => Some("must-not-leak".into()),
            "ANTHROPIC_API_KEY" => Some("sk-must-not-leak".into()),
            _ => None,
        }
    }

    /// **Witness (b) for A5, spawning half.** An environment variable the
    /// manifest did not name is set in the parent and is absent from the
    /// child — proven by asking the child itself, not by inspecting the
    /// builder.
    ///
    /// Unix-only because it needs a program that prints its environment; the
    /// pure half of the same property is asserted platform-independently by
    /// `stella_plugin::runtime`'s `an_undeclared_variable_does_not_reach_the_child`.
    #[cfg(unix)]
    #[test]
    fn an_undeclared_variable_is_absent_from_the_real_child() {
        let route = route(
            &["PLUGIN_MODE"],
            &[
                "/bin/sh",
                "-c",
                "printf '%s|%s' \"${PLUGIN_MODE-unset}\" \"${STELLA_PLUGIN_SECRET-unset}\"",
            ],
        );
        let prepared = prepare_command(&route, parent);
        let output = {
            let mut command = prepared.command;
            command.output().expect("the child must run")
        };
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "wrapper|unset",
            "the declared variable arrives and the undeclared one does not, \
             even though the parent carries both"
        );
        assert!(prepared.refused.is_empty());
    }

    /// A plugin may ask for the credential that pays for the agent. It does
    /// not get it, and it is told — invariant 3 stays a property rather than
    /// a policy the manifest could opt out of.
    #[test]
    fn a_declared_model_credential_is_refused_and_named() {
        let prepared = prepare_command(
            &route(&["ANTHROPIC_API_KEY", "PLUGIN_MODE"], &["node"]),
            parent,
        );
        assert_eq!(prepared.refused, vec!["ANTHROPIC_API_KEY".to_string()]);
        let names: Vec<String> = prepared
            .command
            .get_envs()
            .filter_map(|(name, value)| value.map(|_| name.to_string_lossy().into_owned()))
            .collect();
        assert_eq!(
            names,
            vec!["PLUGIN_MODE".to_string()],
            "the refusal removes one name and keeps the rest"
        );
    }

    /// The builder clears before it adds. Reversing those two lines is a
    /// silent, total loss of the allowlist, which no assertion about the
    /// refused list would catch.
    #[test]
    fn the_child_environment_is_exactly_the_allowlist() {
        let prepared = prepare_command(
            &route(&["PLUGIN_MODE", "PATH"], &["node", "main.js"]),
            parent,
        );
        // `get_envs` yields the builder's own (sorted) order, not the
        // declaration order, so this is a set comparison on purpose — the
        // claim is about membership, and the order the child sees is the
        // platform's business.
        let mut set: Vec<String> = prepared
            .command
            .get_envs()
            .filter_map(|(name, value)| value.map(|_| name.to_string_lossy().into_owned()))
            .collect();
        set.sort();
        assert_eq!(set, vec!["PATH".to_string(), "PLUGIN_MODE".to_string()]);
        assert_eq!(
            prepared.command.get_program(),
            "node",
            "argv[0] is the program, the rest are arguments"
        );
        let args: Vec<String> = prepared
            .command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["main.js".to_string()]);
    }
}
