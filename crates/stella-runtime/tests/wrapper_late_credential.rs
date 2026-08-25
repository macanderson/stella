// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The witness for #3529: a name registered as a model credential **after** a
//! transport was declared still never reaches a child.
//!
//! # Why this needs a test binary of its own
//!
//! `stella_tools::subprocess_env`'s credential registry is process-global and
//! monotonic — `register_sensitive_env_names` only ever adds, and nothing
//! removes. So the moment this file registers a name, every sibling test in
//! the same binary asserting that some name is *admitted* is running against a
//! different registry than it was written for. `wrapper_env_refusal.rs` makes
//! exactly that assertion, which is why the registration lives here and why
//! #3512 did not fold this in.
//!
//! For the same reason there is **one test function**, not three: cargo runs a
//! binary's tests on parallel threads, and the ordering this witness depends
//! on — admitted, then registered, then refused — is only an ordering inside
//! one function.
//!
//! # What was broken
//!
//! `SubprocessWrapper::declare` filtered the resolved pairs through
//! `refuses_env_name` exactly once, at construction. That judgement is not a
//! pure function of its argument: it also consults the registry above. So one
//! manifest got two different answers depending on the order in which the host
//! did two unrelated things —
//!
//! - settings register `CORP_AUTH`, then the wrapper is declared: the pair is
//!   refused, as intended;
//! - the wrapper is declared, then settings register `CORP_AUTH`: the
//!   transport holds the resolved pair and hands it to every child it spawns,
//!   for the life of the process.
//!
//! The second ordering does not occur on today's call path, and that is an
//! argument from call order rather than a property — which is the exact thing
//! #3512 existed to stop relying on.
//!
//! The child that answers is `wrapper-plugin-fixture` rather than a
//! `/bin/sh` script, so this file runs on Windows too (#4697). Its
//! `two-env-probe` mode reports which named variables reached the child,
//! which is the question these tests ask.

use stella_plugin::{AfterTurnRequest, DriveRequest, PROTOCOL_VERSION, TurnOutcome};
use stella_runtime::wrapper::{
    DEFAULT_WRAPPER_TIMEOUT, DriverError, SubprocessDriver, SubprocessWrapper, TurnWrapper,
    WrapperError, refuses_env_name,
};

/// A name no static rule can infer — the shape `register_sensitive_env_names`
/// exists for, per its own doc ("a custom provider may use a name such as
/// `CORP_AUTH`"). It must not end in any credential suffix, or this file would
/// prove nothing about the registry.
const LATE: &str = "STELLA_TEST_CORP_AUTH";

fn declared_env() -> Vec<(String, String)> {
    vec![
        (LATE.to_string(), "must-not-leak".to_string()),
        ("PLUGIN_MODE".to_string(), "wrapper".to_string()),
    ]
}

/// A plugin that reports, as a measurement, whether it can see the variable.
///
/// The child is the oracle for `wrapper_env_refusal.rs`'s reason: inspecting
/// the transport's own field would only prove the constructor agrees with
/// itself.
const FIXTURE: &str = env!("CARGO_BIN_EXE_wrapper-plugin-fixture");

fn after() -> AfterTurnRequest {
    AfterTurnRequest {
        protocol_version: PROTOCOL_VERSION,
        wrapper: "late-credential-probe".into(),
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

fn wrapper() -> SubprocessWrapper {
    SubprocessWrapper::declare(
        vec![
            FIXTURE.to_string(),
            "two-env-probe".into(),
            "STELLA_TEST_CORP_AUTH".into(),
            "PLUGIN_MODE".into(),
        ],
        declared_env(),
        DEFAULT_WRAPPER_TIMEOUT,
    )
    .expect("the transport is declared with a program and a budget")
    .wrapper
}

/// **The witness.** Declare first, register second, and the child still never
/// receives the pair.
///
/// The first half is the falsifier: before the registration the same name is
/// genuinely admitted and the same child genuinely sees it, so the second half
/// is about the registration rather than about the fixture. On `main` the
/// second half spawns and answers `late: 10` — the credential in the child's
/// environment, from a transport declared one call earlier.
#[tokio::test]
async fn a_credential_registered_after_declare_never_reaches_the_child() {
    // Admitted, and observably so — `${VAR:+1}0` renders 10 when the variable
    // is set and 0 when it is not.
    assert!(
        !refuses_env_name(LATE),
        "this name must not be inferable from any static rule, or the registry is untested"
    );
    let before = wrapper()
        .after_turn(after())
        .await
        .expect("the probe answers")
        .evidence
        .measurements;
    assert_eq!(
        before.get("late").copied(),
        Some(10),
        "the falsifier: with nothing registered, this name IS handed to the child"
    );

    // The transports declared while the name was still admitted. They hold the
    // resolved pair, and on the old code they hand it over forever. Both are
    // built *before* the registration, because a transport declared after it
    // is filtered at declare time and would prove nothing about this.
    let held = wrapper();
    let held_driver = SubprocessDriver::declare(
        vec![FIXTURE.to_string(), "drain-emit".into(), String::new()],
        declared_env(),
        DEFAULT_WRAPPER_TIMEOUT,
    )
    .expect("the transport is declared with a program and a budget")
    .driver;

    stella_tools::subprocess_env::register_sensitive_env_names([LATE]);
    assert!(
        refuses_env_name(LATE),
        "the registry is what the judgement consults"
    );

    let error = held
        .after_turn(after())
        .await
        .expect_err("a name the registry now claims must not reach a child");
    match &error {
        WrapperError::CredentialRegisteredLate { names, .. } => {
            assert_eq!(names, &vec![LATE.to_string()]);
        }
        other => panic!("the refusal must name what it withheld, got {other}"),
    }
    // Loud, not quiet: the report a caller already read is spent, so a second
    // silent narrowing here would reach nobody.
    assert!(error.to_string().contains(LATE), "{error}");
    assert!(
        !error.to_string().contains("must-not-leak"),
        "the refusal names the variable and never its value: {error}"
    );

    // The driver channel spawns a child from a resolved environment for the
    // same reasons and had the same hole.
    let error = held_driver
        .drive(DriveRequest::new("session-1"))
        .await
        .expect_err("the driver refuses the same pair");
    assert!(
        matches!(error, DriverError::CredentialRegisteredLate { .. }),
        "{error}"
    );

    // A wrapper declared *after* the registration is refused at declare time,
    // as it always was, and its report says so — the two mechanisms answer the
    // same question at different moments and must not disagree.
    let admitted = SubprocessWrapper::declare(
        vec![
            FIXTURE.to_string(),
            "two-env-probe".into(),
            "STELLA_TEST_CORP_AUTH".into(),
            "PLUGIN_MODE".into(),
        ],
        declared_env(),
        DEFAULT_WRAPPER_TIMEOUT,
    )
    .expect("the transport is declared with a program and a budget");
    assert_eq!(admitted.refused, vec![LATE.to_string()]);
    let after_registration = admitted
        .wrapper
        .after_turn(after())
        .await
        .expect("a wrapper carrying no refused pair still runs")
        .evidence
        .measurements;
    assert_eq!(
        after_registration.get("late").copied(),
        Some(0),
        "the child does not see the credential"
    );
    assert_eq!(
        after_registration.get("mode").copied(),
        Some(10),
        "and everything else it declared is still there"
    );
}
