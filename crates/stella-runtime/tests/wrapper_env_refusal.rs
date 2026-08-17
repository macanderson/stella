//! The witness for #3512: the **socket** refuses a model credential, so the
//! refusal holds for every host and not just for `stella-cli`.
//!
//! Failing before the change for a structural reason rather than an
//! off-by-one. The narrowing lived in
//! `stella-cli`'s `plugin_cmd::process::prepare_command`, which builds a
//! **`std::process::Command`** (sync); the socket's only transport builds its
//! own **`tokio::process::Command`** from `Vec<(String, String)>`. The two
//! types cannot meet, so `PreparedCommand::command` was `#[allow(dead_code)]`
//! waiting for a spawner that could never consume it, and every other driver —
//! `stella-serve`, an embedded `stella-engine` host, the two acceptance
//! criteria beside the CLI in `doc:wrapper-socket` §6 — reached the socket's
//! own constructor and handed the plugin the key that pays for the agent.
//! Invariant 3 was CLI policy, not a property of the socket.
//!
//! **There is deliberately no `stella-cli` in this file.** That is the whole
//! claim: a host that never links the CLI still cannot leak the credential.
//! `stella-runtime`'s manifest cannot depend on `stella-cli` (the edge would
//! be a cycle), so the absence is enforced by the build and not by discipline.
//!
//! `cfg(unix)` for `tests/wrapper_socket.rs`'s reason and tracked in the same
//! issue: the child probe is a `/bin/sh` script, so on Windows the spawning
//! half of this file compiles to nothing (#3497). The two pure assertions —
//! the report and the `Debug` redaction — are platform-independent and are
//! written so they stay that way.

use std::time::Duration;

#[cfg(unix)]
use stella_plugin::{AfterTurnRequest, PROTOCOL_VERSION, TurnOutcome};
#[cfg(unix)]
use stella_runtime::wrapper::TurnWrapper;
use stella_runtime::wrapper::{
    AdmittedWrapper, DEFAULT_WRAPPER_TIMEOUT, SubprocessWrapper, refuses_env_name,
};

/// The environment a careless host would hand over: a manifest that asked for
/// the provider key, an ambient-authority name the socket deliberately
/// *admits*, and one ordinary variable.
fn declared_env() -> Vec<(String, String)> {
    vec![
        ("ANTHROPIC_API_KEY".into(), "sk-must-not-leak".into()),
        ("GITHUB_TOKEN".into(), "ghp-must-not-leak".into()),
        ("SSH_AUTH_SOCK".into(), "/tmp/agent-must-not-print".into()),
        ("PLUGIN_MODE".into(), "wrapper".into()),
    ]
}

#[cfg(unix)]
fn after() -> AfterTurnRequest {
    AfterTurnRequest {
        protocol_version: PROTOCOL_VERSION,
        wrapper: "env-probe".into(),
        // This probe drives the transport directly, with no stage program
        // resolved — `None` is "this host has no stages", which is the honest
        // answer here rather than a stand-in boundary.
        stage: None,
        round: 0,
        goal: "probe the environment the socket granted".into(),
        candidate: None,
        turn: TurnOutcome {
            completed: true,
            answer: "nothing".into(),
            tools: Some(Vec::new()),
            changed_files: Some(Vec::new()),
        },
    }
}

/// **Witness 1.** A manifest naming the credential that pays for the agent
/// does not get it — asserted through the socket, by asking the child itself.
///
/// The child is the oracle on purpose. Inspecting the constructor's own field
/// would prove the constructor agrees with itself; only the process can say
/// what the process received.
#[cfg(unix)]
#[tokio::test]
async fn the_socket_withholds_a_model_credential_from_the_child() {
    let probe = r#"
cat >/dev/null
printf '{"point":"after_turn","body":{"protocol_version":1,"evidence":{"flip":"not-attempted","measurements":{"key":%s,"token":%s,"sock":%s,"mode":%s}}}}\n' \
  "${ANTHROPIC_API_KEY:+1}0" "${GITHUB_TOKEN:+1}0" "${SSH_AUTH_SOCK:+1}0" "${PLUGIN_MODE:+1}0"
"#;
    let admitted = SubprocessWrapper::declare(
        vec!["/bin/sh".into(), "-c".into(), probe.into()],
        declared_env(),
        DEFAULT_WRAPPER_TIMEOUT,
    )
    .expect("the transport is declared with a program and a budget");

    let measurements = admitted
        .wrapper
        .after_turn(after())
        .await
        .expect("the probe answers")
        .evidence
        .measurements;

    // `${VAR:+1}0` renders 10 when the variable is set and non-empty, 0 when
    // it is unset — one shell expansion that distinguishes both, rather than
    // an `if` per name.
    assert_eq!(
        measurements.get("key").copied(),
        Some(0),
        "the child received ANTHROPIC_API_KEY: the socket is not the refusal point"
    );
    assert_eq!(
        measurements.get("token").copied(),
        Some(0),
        "the child received GITHUB_TOKEN"
    );
    assert_eq!(
        measurements.get("mode").copied(),
        Some(10),
        "the refusal removed the credentials and kept the rest"
    );
    assert_eq!(
        measurements.get("sock").copied(),
        Some(10),
        "SSH_AUTH_SOCK is ambient authority, not a credential: it is disclosed in the consent \
         text and a plugin driving git over a deploy key needs it, so refusing it would break a \
         legitimate plugin to prevent nothing"
    );
}

/// **Witness 2.** The refusal is an answer the caller receives, not a silence.
///
/// A plugin that quietly does not get what its manifest asked for is a bug its
/// author cannot diagnose; the reason the names come back is so a host can say
/// them out loud.
#[test]
fn the_refusal_is_reported_rather_than_dropped() {
    let AdmittedWrapper { wrapper, refused } = SubprocessWrapper::declare(
        vec!["node".into(), "main.js".into()],
        declared_env(),
        DEFAULT_WRAPPER_TIMEOUT,
    )
    .expect("the transport is declared with a program and a budget");

    assert_eq!(
        refused,
        vec!["ANTHROPIC_API_KEY".to_string(), "GITHUB_TOKEN".to_string()],
        "every withheld name is named, in the order the manifest declared them"
    );
    assert_eq!(wrapper.program(), "node");

    // The consent-time report a host prints is the same judgement, exported so
    // the prompt cannot promise a variable the spawn then refuses.
    assert!(refuses_env_name("ANTHROPIC_API_KEY"));
    assert!(!refuses_env_name("SSH_AUTH_SOCK"));

    // A plugin that asked for nothing forbidden is refused nothing — the
    // guard has to be silent in the normal case or a host learns to ignore it.
    let clean = SubprocessWrapper::declare(
        vec!["node".into()],
        vec![("PLUGIN_MODE".into(), "wrapper".into())],
        DEFAULT_WRAPPER_TIMEOUT,
    )
    .expect("the transport is declared with a program and a budget");
    assert!(clean.refused.is_empty());
}

/// **Witness 3.** `{:?}` on the transport prints environment names and never
/// values.
///
/// Deriving `Debug` over the resolved pairs puts a live secret into every
/// `tracing::debug!(?wrapper)`, `dbg!`, and assertion-failure message. The
/// values under test are the ones the socket *admits* on purpose —
/// `SSH_AUTH_SOCK` and a deploy-key `GIT_SSH_COMMAND` — because the refusal
/// above cannot be what keeps them out of the log.
#[test]
fn debug_prints_environment_names_and_never_values() {
    let admitted = SubprocessWrapper::declare(
        vec!["node".into()],
        vec![
            ("SSH_AUTH_SOCK".into(), "/tmp/agent-must-not-print".into()),
            (
                "GIT_SSH_COMMAND".into(),
                "ssh -i /home/dev/.ssh/deploy_key".into(),
            ),
        ],
        Duration::from_secs(30),
    )
    .expect("the transport is declared with a program and a budget");

    let rendered = format!("{:?}", admitted.wrapper);
    assert!(
        rendered.contains("SSH_AUTH_SOCK") && rendered.contains("GIT_SSH_COMMAND"),
        "the names are what a debug line is for: {rendered}"
    );
    assert!(
        !rendered.contains("/tmp/agent-must-not-print"),
        "an admitted value reached a debug line: {rendered}"
    );
    assert!(
        !rendered.contains("deploy_key"),
        "an admitted value reached a debug line: {rendered}"
    );

    // The report wraps the transport, so its own `Debug` must not reopen the
    // hole the transport's closed.
    let rendered = format!("{admitted:?}");
    assert!(
        !rendered.contains("/tmp/agent-must-not-print"),
        "the report's debug leaked what the transport's does not: {rendered}"
    );
}

/// A refused credential's **value** is dropped, not carried along beside its
/// name — so the report cannot become the leak the transport stopped being.
#[test]
fn a_refused_credential_leaves_no_value_behind() {
    let admitted =
        SubprocessWrapper::declare(vec!["node".into()], declared_env(), DEFAULT_WRAPPER_TIMEOUT)
            .expect("the transport is declared with a program and a budget");

    let rendered = format!("{admitted:?}");
    assert!(
        !rendered.contains("sk-must-not-leak") && !rendered.contains("ghp-must-not-leak"),
        "a refused credential's value survived into the report: {rendered}"
    );
}
