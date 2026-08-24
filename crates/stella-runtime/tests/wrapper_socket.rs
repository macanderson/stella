//! The witness: a wrapper plugin participates in a turn through the socket,
//! and its verdict is decided by the **host** from the rule its manifest
//! declared.
//!
//! Failing before #3380 for the plainest possible reason — there was no socket:
//! `stella_runtime::wrapper` did not exist, `stella-runtime` did not depend on
//! `stella-plugin`, and a manifest's `[wrapper]` block was, in its own module's
//! words, "a declared name that load-checks, not a dispatch target".
//!
//! The plugin here uses **no SDK and no library** — it reads one JSON object on
//! stdin and writes one on stdout, and the tooling it needs to do that is
//! `printf`. That is the acceptance criterion `doc:pipeline-as-plugins` §5 sets
//! and the one it is easiest to fail by accident: a Rust-only extension surface
//! is a library with extra steps.
//!
//! Every plugin was a `/bin/sh` script until #3497, which is why this whole
//! file was `#![cfg(unix)]` and the transport was unproven on Windows —
//! including the parts that are not shell-shaped at all: the stdio exchange,
//! the concurrent write/wait, `kill_on_drop`, the group kill, `env_clear()`.
//! They now drive `wrapper-plugin-fixture`, an in-tree plugin binary that links
//! `std` and nothing else (no `serde`, no `stella-plugin`, no JSON parser), so
//! the *mechanism* is exercised on every platform the workspace compiles for
//! and the answer it gives is the same answer a `case`-and-`printf` script gave.
//!
//! One test stays `#[cfg(unix)]` on purpose, and it is the one that is
//! genuinely shell-shaped: [`a_plugin_written_in_sh_answers_the_same_socket`]
//! runs a real `/bin/sh` script through a whole turn, because "a plugin need
//! not be written in Rust" is a claim only a plugin not written in Rust can
//! make. Its comment says so where it sits.
//!
//! What each test pins is named on the test.

use std::collections::BTreeMap;
use std::time::Duration;

use stella_plugin::{
    AfterTurnRequest, BeforeTurnRequest, CandidateGrant, Continuation, EvidenceProvenance,
    EvidenceSet, FlipObservation, HostStage, LoopGrant, Outcome, PROTOCOL_VERSION, Participation,
    PluginManifest, RoundState, SignalValues, StageName, StopReason, TamperFinding, TestBaseline,
    TestPlan, TurnOutcome, UnmetBecause, Verdict, VerdictRule, VolatileContext, WrapperPoint,
};
use stella_protocol::CandidateHandle;
use stella_protocol::completion::MessageRole;
use stella_runtime::wrapper::{
    DEFAULT_WRAPPER_TIMEOUT, HostCallChannel, InProcessWrapper, SubprocessWrapper, TurnWrapper,
    WrapperError, WrapperHandler, admissible, again, judge,
};

/// The reference wrapper's manifest: an arbiter whose definition of done is a
/// budget rather than a flip, so the verdict is decided by numbers the plugin
/// reports against a threshold a human read at install.
const MANIFEST: &str = r#"
name = "reference-wrapper"
description = "keeps the benchmark inside its recorded budget"

[loop]
participation = "arbiter"
hooks = ["Stop"]
# The socket points this wrapper answers. Declared, so an undeclared point is
# never dispatched instead of being discovered by refusal (#3501) — and an
# [oracle] must name after_turn, because that is where its evidence arrives.
points = ["before_turn", "after_turn"]
max_holds = 2

[requirements]
within-budget = "the benchmark p50 stays inside its recorded budget"

[oracle]
command = { argv = ["bench"], timeout_secs = 60 }
flip = "not-applicable"
measurements = ["p50"]

[[oracle.checks]]
requirement = "within-budget"
check = "p50 <= 105"

[roles.triage]
tier = "cheap"

[wrapper]
id = "reference-v1"

[[wrapper.stages]]
name = "triage"

[[wrapper.stages]]
name = "research"
if = "questions > 0"

[[wrapper.stages]]
name = "execute"

[[wrapper.stages]]
name = "verify"
"#;

/// The portable plugin (#3497). Reads one request, writes one response; the
/// only thing it decides is what it *observed*, and the budget it is measured
/// against lives in the manifest above where a reviewer can diff it. Its
/// `reference` mode is the behaviour the `sh` script below still spells out
/// by hand.
const FIXTURE: &str = env!("CARGO_BIN_EXE_wrapper-plugin-fixture");

fn manifest() -> PluginManifest {
    PluginManifest::from_toml_str(MANIFEST).expect("the reference manifest loads")
}

/// The fixture in one of its canned modes, on the default budget.
fn plugin(mode: &[&str]) -> SubprocessWrapper {
    let mut argv = vec![FIXTURE.to_string()];
    argv.extend(mode.iter().map(|part| (*part).to_string()));
    SubprocessWrapper::declare(argv, Vec::new(), DEFAULT_WRAPPER_TIMEOUT)
        .expect("the transport is declared with a program and a budget")
        .wrapper
}

fn before(stage: StageName, goal: &str) -> BeforeTurnRequest {
    BeforeTurnRequest {
        protocol_version: PROTOCOL_VERSION,
        wrapper: "reference-v1".into(),
        stage,
        round: 0,
        goal: goal.into(),
        candidate: None,
        published: Vec::new(),
    }
}

fn after(goal: &str) -> AfterTurnRequest {
    AfterTurnRequest {
        protocol_version: PROTOCOL_VERSION,
        wrapper: "reference-v1".into(),
        // The evidence this reference wrapper reports is the whole round's, so
        // it names no stage: `None` is "this host has no stages", never a
        // stand-in boundary.
        stage: None,
        round: 0,
        goal: goal.into(),
        candidate: None,
        turn: TurnOutcome {
            completed: true,
            answer: "rewrote the hot loop".into(),
            tools: Some(vec!["edit_file".into()]),
            changed_files: Some(vec!["crates/stella-core/src/driver.rs".into()]),
        },
    }
}

/// One whole turn through the socket, as a host would drive it: resolve the
/// declared stage program, run `before_turn` for each stage that runs, run
/// `after_turn` once, then decide.
///
/// Returns what the host concluded, plus the messages the contributions became.
async fn run_turn(
    goal: &str,
) -> (
    Verdict,
    Continuation,
    Vec<stella_protocol::CompletionMessage>,
) {
    run_turn_with(plugin(&["reference"]), goal).await
}

/// [`run_turn`] against a caller-supplied transport, so the same whole turn can
/// be driven by the portable fixture and by a `/bin/sh` script and the two
/// answers compared rather than described.
async fn run_turn_with(
    wrapper: SubprocessWrapper,
    goal: &str,
) -> (
    Verdict,
    Continuation,
    Vec<stella_protocol::CompletionMessage>,
) {
    let manifest = manifest();
    let declared = manifest.wrapper.as_ref().expect("[wrapper] declared");

    let program = declared
        .resolve(&SignalValues {
            test_command: false,
            candidates: 1,
            budget_metered: false,
            conversational: false,
            questions: 2,
            plans: true,
            verifies: true,
            wants_witness: false,
            wants_verifier: false,
            mutating_actions: 1,
            diff_lines: 12,
            witness_authored: false,
            flip_achieved: false,
            tests_red: false,
            tests_green: false,
        })
        .expect("a validated wrapper resolves for every signal set");
    assert_eq!(program.variant(), "reference-v1");
    assert!(
        program.runs(&StageName::Host(HostStage::Research)),
        "questions > 0 this turn"
    );

    let mut messages = Vec::new();
    for stage in program.stages() {
        let response = wrapper
            .before_turn(before(stage.clone(), goal))
            .await
            .expect("the plugin answers before_turn");
        // The check is what produces the value that gets applied, so there is
        // no way through here that skips it (#3494).
        let admitted =
            admissible(&manifest, response).expect("the contribution is within what was declared");
        messages.extend(admitted.into_messages());
    }

    // The plugin reports what it observed; the host merges its own tamper
    // finding before judging (#3499). This host took no snapshot — this
    // manifest's oracle has no flip to protect — and says so itself rather
    // than making the plugin admit to a check it could never perform.
    let observed = wrapper
        .after_turn(after(goal))
        .await
        .expect("the plugin answers after_turn")
        .evidence;
    let evidence = EvidenceSet::from_observed(observed, TamperFinding::NotChecked);

    let verdict = judge(&VerdictRule::from_manifest(&manifest), &evidence);
    let continuation = again(
        &verdict,
        &RoundState {
            holds_spent: 0,
            host_max_holds: 4,
        },
        &manifest.loop_grant,
    );
    (verdict, continuation, messages)
}

/// **The witness.** A non-Rust plugin runs at both points of a real turn, and
/// the verdict is the host's: the same plugin, the same declared rule, two
/// opposite answers decided by what it *measured* rather than by what it
/// claimed.
#[tokio::test]
async fn a_wrapper_plugin_participates_in_a_turn_and_the_host_decides_its_verdict() {
    let (verdict, continuation, _) = run_turn("make the parser faster").await;
    assert_eq!(
        verdict,
        Verdict::Met {
            evidence: EvidenceProvenance::PluginReported
        },
        "p50 103 is inside the 105 budget"
    );
    assert_eq!(
        continuation,
        Continuation::Stop {
            outcome: Outcome::Met {
                evidence: EvidenceProvenance::PluginReported
            }
        }
    );

    let (verdict, continuation, _) = run_turn("make the parser slower on purpose").await;
    let Verdict::Unmet { unmet } = &verdict else {
        panic!("p50 118 is outside the 105 budget, got {verdict:?}");
    };
    assert_eq!(unmet.len(), 1);
    assert_eq!(unmet[0].requirement, "within-budget");
    assert_eq!(
        unmet[0].statement, "the benchmark p50 stays inside its recorded budget",
        "the hold cites the statement a human consented to, not a paraphrase"
    );
    assert_eq!(
        unmet[0].because,
        UnmetBecause::Budget {
            check: "p50 <= 105".into(),
            reported: 118,
        },
        "the rule that decided it is the one the manifest declares"
    );

    // ...and the arbiter's allowance buys another turn, carrying the unmet
    // clause as the correction.
    let Continuation::Again { correction } = &continuation else {
        panic!("an arbiter with holds left asks again, got {continuation:?}");
    };
    assert_eq!(correction.unmet, *unmet);
    // `into_message` is the type's only exit — the body is private so it
    // cannot reach the byte-stable prefix by accident — so the assertion
    // spends the contribution the way a host would.
    let guidance = correction.guidance.clone().into_message().content;
    assert!(
        guidance.contains("within-budget"),
        "the correction names the clause: {guidance}"
    );
}

/// Invariant 7, end to end: whatever a wrapper contributes can only become a
/// message *after* the byte-stable prefix. A wrapper that could reach the
/// system prompt would make every installed plugin a per-turn cache miss for
/// every user who installed it.
#[tokio::test]
async fn contributed_context_never_reaches_the_stable_prefix() {
    let (_, _, messages) = run_turn("make the parser faster").await;
    assert!(
        !messages.is_empty(),
        "the plugin contributes context, or this test proves nothing"
    );
    for message in &messages {
        assert_eq!(
            message.role,
            MessageRole::User,
            "a contribution must be a volatile message, never the system prefix"
        );
    }
}

/// The declared grants bound what a running plugin may say, not just what its
/// manifest may claim. A role intent nobody consented to is refused at the
/// value, exactly as an undeclared signal is refused at the declaration.
#[tokio::test]
async fn a_response_naming_an_undeclared_role_is_refused() {
    let rogue = r#"{"point":"before_turn","body":{"protocol_version":1,"role":"verifier"}}"#;
    let response = plugin(&["emit", rogue])
        .before_turn(before(StageName::Host(HostStage::Triage), "anything"))
        .await
        .expect("the plugin answers");
    let error = admissible(&manifest(), response).expect_err("\"verifier\" is not declared");
    assert!(
        matches!(error, WrapperError::UndeclaredRole { ref role, .. } if role == "verifier"),
        "got {error:?}"
    );

    let mistyped = r#"{"point":"before_turn","body":{"protocol_version":1,"publish":[{"signal":"questions","value":{"boolean":true}}]}}"#;
    let response = plugin(&["emit", mistyped])
        .before_turn(before(StageName::Host(HostStage::Triage), "anything"))
        .await
        .expect("the plugin answers");
    let error = admissible(&manifest(), response).expect_err("a count is not a boolean");
    assert!(
        matches!(error, WrapperError::MistypedSignal { .. }),
        "got {error:?}"
    );
}

/// The child's environment is exactly what it was granted and nothing else.
/// A plugin is third-party code a user installed; inheriting the operator's
/// environment would hand it every credential the shell was carrying, which
/// would make invariant 3 a policy rather than a property.
#[tokio::test]
async fn the_child_sees_exactly_the_environment_it_was_given() {
    // The premise, asserted rather than assumed: this variable really is in
    // the parent, so a `0` below means it was cleared and not that it never
    // existed.
    assert!(
        std::env::var("CARGO_MANIFEST_DIR").is_ok(),
        "the test process must carry CARGO_MANIFEST_DIR for this to prove anything"
    );

    let wrapper = SubprocessWrapper::declare(
        vec![FIXTURE.into(), "env-probe".into()],
        vec![("GRANTED".into(), "42".into())],
        DEFAULT_WRAPPER_TIMEOUT,
    )
    .unwrap()
    .wrapper;

    let evidence = wrapper
        .after_turn(after("probe the environment"))
        .await
        .expect("the probe answers")
        .evidence;
    assert_eq!(
        evidence.measurements,
        BTreeMap::from([("inherited".into(), 0), ("granted".into(), 42)]),
        "the child inherited nothing and received exactly the granted pair"
    );
}

/// An in-process wrapper's own failure is a wrapper failure, named as such —
/// so a host treats "the Rust plugin threw" exactly as it treats "the Python
/// plugin exited 1", and hands `judge` an unobserved evidence set either way.
#[tokio::test]
async fn an_in_process_handler_reports_its_own_failure_as_a_wrapper_failure() {
    struct Broken;
    #[async_trait::async_trait]
    impl WrapperHandler for Broken {
        async fn before_turn(
            &self,
            _request: BeforeTurnRequest,
            _host: &dyn HostCallChannel,
        ) -> Result<stella_plugin::BeforeTurnResponse, WrapperError> {
            Err(WrapperError::Handler {
                wrapper: "reference-v1".into(),
                point: WrapperPoint::BeforeTurn,
                detail: "the recorded baseline is missing".into(),
            })
        }

        async fn after_turn(
            &self,
            _request: AfterTurnRequest,
            _host: &dyn HostCallChannel,
        ) -> Result<stella_plugin::AfterTurnResponse, WrapperError> {
            Err(WrapperError::Handler {
                wrapper: "reference-v1".into(),
                point: WrapperPoint::AfterTurn,
                detail: "the benchmark did not run".into(),
            })
        }
    }

    let error = InProcessWrapper::new(Broken)
        .after_turn(after("x"))
        .await
        .expect_err("the handler failed");
    assert!(
        matches!(
            error,
            WrapperError::Handler {
                point: WrapperPoint::AfterTurn,
                ..
            }
        ),
        "got {error:?}"
    );

    // ...and the host's answer to a wrapper that could not gather is an
    // abstention, never a claim about the work.
    assert_eq!(
        judge(
            &VerdictRule::from_manifest(&manifest()),
            &EvidenceSet::unobserved()
        ),
        Verdict::Undecided {
            reason: stella_plugin::UndecidedReason::MeasurementMissing {
                requirement: "within-budget".into(),
                measurement: "p50".into(),
            }
        }
    );
}

/// The two transports are the same socket. Whatever a Rust plugin can answer
/// in-process, it answers with the identical owned values a subprocess carries
/// over stdio — which is what keeps the wire path from becoming second-class
/// (`doc:pipeline-as-plugins` §5 commitment 2).
#[tokio::test]
async fn the_in_process_transport_answers_what_the_wire_transport_answers() {
    struct Native;
    #[async_trait::async_trait]
    impl WrapperHandler for Native {
        async fn before_turn(
            &self,
            _request: BeforeTurnRequest,
            _host: &dyn HostCallChannel,
        ) -> Result<stella_plugin::BeforeTurnResponse, WrapperError> {
            Ok(stella_plugin::BeforeTurnResponse {
                role: Some("triage".into()),
                context: vec![VolatileContext::new(
                    "budget",
                    "the recorded p50 budget is 105",
                )],
                publish: vec![stella_plugin::PublishedSignal {
                    signal: stella_plugin::Signal::Questions,
                    value: stella_plugin::SignalValue::Count(2),
                }],
                ..stella_plugin::BeforeTurnResponse::empty()
            })
        }

        async fn after_turn(
            &self,
            request: AfterTurnRequest,
            _host: &dyn HostCallChannel,
        ) -> Result<stella_plugin::AfterTurnResponse, WrapperError> {
            let p50 = if request.goal.contains("slower") {
                118
            } else {
                103
            };
            Ok(stella_plugin::AfterTurnResponse {
                protocol_version: PROTOCOL_VERSION,
                evidence: stella_plugin::ObservedEvidence {
                    flip: FlipObservation::NotAttempted,
                    measurements: BTreeMap::from([("p50".into(), p50)]),
                    detail: None,
                },
            })
        }
    }

    let native = InProcessWrapper::new(Native);
    let wire = plugin(&["reference"]);
    let goal = "make the parser slower on purpose";

    assert_eq!(
        native
            .before_turn(before(StageName::Host(HostStage::Triage), goal))
            .await
            .unwrap(),
        wire.before_turn(before(StageName::Host(HostStage::Triage), goal))
            .await
            .unwrap(),
        "both transports contribute the same thing"
    );
    assert_eq!(
        native.after_turn(after(goal)).await.unwrap(),
        wire.after_turn(after(goal)).await.unwrap(),
        "both transports report the same evidence"
    );

    // ...and the host decides both the same way, because the rule is the
    // manifest's and not the transport's.
    let rule = VerdictRule::from_manifest(&manifest());
    let evidence = EvidenceSet::from_observed(
        native.after_turn(after(goal)).await.unwrap().evidence,
        TamperFinding::NotChecked,
    );
    assert!(matches!(judge(&rule, &evidence), Verdict::Unmet { .. }));
}

/// Every way a plugin can fail is a *named* failure, because a host has to act
/// differently on each: never ran, ran and failed, answered unreadably,
/// answered the wrong question, spoke a contract this host does not have.
#[tokio::test]
async fn each_failure_mode_is_named_rather_than_collapsed() {
    let missing = SubprocessWrapper::declare(
        vec!["/nonexistent/wrapper-plugin".into()],
        Vec::new(),
        DEFAULT_WRAPPER_TIMEOUT,
    )
    .unwrap()
    .wrapper
    .after_turn(after("x"))
    .await
    .expect_err("the program does not exist");
    assert!(
        matches!(missing, WrapperError::Spawn { .. }),
        "got {missing:?}"
    );

    let failed = plugin(&["exit", "3", "no such profile"])
        .after_turn(after("x"))
        .await
        .expect_err("the plugin exited non-zero");
    assert!(
        matches!(&failed, WrapperError::Exit { status, stderr, .. }
            if status == "code 3" && stderr.contains("no such profile")),
        "got {failed:?}"
    );

    let garbled = plugin(&["drain-emit", "almost json"])
        .after_turn(after("x"))
        .await
        .expect_err("the answer is not a response");
    assert!(
        matches!(&garbled, WrapperError::Decode { answer, .. } if answer == "almost json"),
        "got {garbled:?}"
    );

    let misaddressed = plugin(&[
        "drain-emit",
        r#"{"point":"before_turn","body":{"protocol_version":1}}"#,
    ])
    .after_turn(after("x"))
    .await
    .expect_err("the plugin answered the other point");
    assert!(
        matches!(
            misaddressed,
            WrapperError::PointMismatch {
                expected: WrapperPoint::AfterTurn,
                actual: WrapperPoint::BeforeTurn,
                ..
            }
        ),
        "got {misaddressed:?}"
    );

    let newer = plugin(&[
        "drain-emit",
        r#"{"point":"before_turn","body":{"protocol_version":99}}"#,
    ])
    .before_turn(before(StageName::Host(HostStage::Triage), "x"))
    .await
    .expect_err("a newer contract may carry a field whose absence changes the answer");
    assert!(
        matches!(
            newer,
            WrapperError::ProtocolVersion {
                declared: 99,
                supported: PROTOCOL_VERSION,
                ..
            }
        ),
        "got {newer:?}"
    );
}

/// A wedged plugin is a bounded loss. The budget is clamped to the same
/// ceiling `stella-core`'s hook plane enforces, so a manifest cannot buy
/// itself an unbounded call any more than an unbounded loop.
#[tokio::test]
async fn a_plugin_that_does_not_answer_is_killed_at_its_budget() {
    let hung = SubprocessWrapper::declare(
        vec![FIXTURE.into(), "hang".into()],
        Vec::new(),
        Duration::from_millis(150),
    )
    .unwrap()
    .wrapper
    .after_turn(after("x"))
    .await
    .expect_err("the plugin never answers");
    assert!(matches!(hung, WrapperError::Timeout { .. }), "got {hung:?}");

    assert_eq!(
        SubprocessWrapper::declare(
            vec![FIXTURE.into()],
            Vec::new(),
            Duration::from_secs(60 * 60),
        )
        .unwrap()
        .wrapper
        .timeout(),
        stella_runtime::wrapper::MAX_WRAPPER_TIMEOUT,
        "an hour-long ask is clamped to the ceiling"
    );

    assert!(matches!(
        SubprocessWrapper::declare(Vec::new(), Vec::new(), DEFAULT_WRAPPER_TIMEOUT),
        Err(WrapperError::EmptyArgv)
    ));
    assert!(matches!(
        SubprocessWrapper::declare(vec![FIXTURE.into()], Vec::new(), Duration::ZERO),
        Err(WrapperError::ZeroTimeout)
    ));
}

/// A `steering` wrapper may gather evidence and report it; it may not hold the
/// completion open. The grade is the grant.
#[test]
fn only_an_arbiter_can_hold_a_turn_open() {
    let verdict = judge(
        &VerdictRule::from_manifest(&manifest()),
        &EvidenceSet {
            provenance: EvidenceProvenance::PluginReported,
            flip: FlipObservation::NotAttempted,
            tamper: TamperFinding::NotChecked,
            measurements: BTreeMap::from([("p50".into(), 118)]),
        },
    );
    let steering = LoopGrant {
        participation: Participation::Steering,
        ..LoopGrant::default()
    };
    let continuation = again(
        &verdict,
        &RoundState {
            holds_spent: 0,
            host_max_holds: 4,
        },
        &steering,
    );
    let Continuation::Stop {
        outcome: Outcome::Unmet { unmet, stopped },
    } = continuation
    else {
        panic!("a steering wrapper stops, got {continuation:?}");
    };
    assert_eq!(stopped, StopReason::NotAnArbiter);
    assert_eq!(
        unmet.len(),
        1,
        "the unmet clause is still reported, never silently dropped"
    );
}

/// **The #3498 witness, end to end.** A plugin written in `sh` — no JSON
/// library, no SDK, no ambient authority — acts on the candidate workspace
/// using *only* what arrived in its request: the canonical root, the test
/// runner's argv, and the red that invocation reported before the turn.
///
/// Failing before the change because `AfterTurnRequest::candidate` was a bare
/// `CandidateHandle` whose six operations existed only as in-process async
/// Rust over a `&dyn` port. A plugin holding one could do nothing with it, so
/// the three reference plugins of macanderson/stella-examples#1 took their test
/// command and baseline from two `[runtime] env` names — default-deny and
/// visible at install consent, but a bend in the socket's own rule that every
/// capability arrives in the request (`doc:wrapper-socket` §6).
///
/// The three measurements are deliberately facts about the *filesystem and the
/// argv*, not echoes: `root_reached` is 1 only if the directory named in the
/// request really holds the candidate's test file, so a grant carrying a
/// plausible-looking path nothing lives at scores 0.
#[tokio::test]
async fn a_plugin_acts_on_the_candidate_using_only_what_the_request_carried() {
    let tree = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tree.path().join("tests")).expect("mkdir");
    std::fs::write(
        tree.path().join("tests/test_flip.py"),
        b"def test(): pass\n",
    )
    .expect("write");
    let root = tree
        .path()
        .canonicalize()
        .expect("canonical root")
        .to_str()
        .expect("a UTF-8 temp path")
        .to_string();

    // What the host mints. In production this is `stella_plugin`'s
    // `host_tree_grant` (the staged pipeline's
    // `ports::CandidateHandles::grant` until #3865 deleted that crate), which
    // canonicalises the root and refuses to mint one it cannot resolve; this
    // crate must not take that edge (`tests/no_policy_crate_edge.rs`), so the grant is
    // built here in the shape that host produces.
    let mut request = after("make the failing test pass");
    request.candidate = Some(
        CandidateGrant::new(CandidateHandle::new("candidate-1"), root.clone()).with_test(
            TestPlan::new("pytest", vec!["tests/test_flip.py".into()])
                .with_baseline(TestBaseline::Failed),
        ),
    );

    let evidence = plugin(&["candidate-probe"])
        .after_turn(request.clone())
        .await
        .expect("the plugin answers")
        .evidence;
    assert_eq!(
        evidence.measurements,
        BTreeMap::from([
            ("root_reached".into(), 1),
            ("test_named".into(), 1),
            ("baseline_red".into(), 1),
        ]),
        "everything the plugin needed was in the request"
    );

    // The falsifier: the same plugin, a grant whose root names a directory
    // that does not exist. `root_reached` collapses, which is what proves the
    // 1 above was a fact about the filesystem rather than the script agreeing
    // with itself.
    let mut elsewhere = request;
    elsewhere.candidate = Some(
        CandidateGrant::new(
            CandidateHandle::new("candidate-1"),
            format!("{root}/not-a-candidate"),
        )
        .with_test(
            TestPlan::new("pytest", vec!["tests/test_flip.py".into()])
                .with_baseline(TestBaseline::Failed),
        ),
    );
    assert_eq!(
        plugin(&["candidate-probe"])
            .after_turn(elsewhere)
            .await
            .expect("the plugin answers")
            .evidence
            .measurements
            .get("root_reached"),
        Some(&0)
    );
}

/// The one genuinely shell-shaped case, kept `#[cfg(unix)]` for that reason
/// and no other (#3497).
///
/// Everything above proves the socket's *mechanism*, which is why it now runs
/// wherever the workspace compiles. This proves something the mechanism tests
/// structurally cannot: that a plugin **need not be written in Rust**. A `sh`
/// script with no JSON parser, no SDK and no library drives the same whole turn
/// to the same host verdict — the acceptance criterion
/// `doc:pipeline-as-plugins` §5 commitment 2 sets, and one only a plugin that
/// is not Rust can make. There is no `/bin/sh` on Windows to make it with.
#[cfg(unix)]
#[tokio::test]
async fn a_plugin_written_in_sh_answers_the_same_socket() {
    const SCRIPT: &str = r#"
input=$(cat)
case "$input" in
  *'"point":"after_turn"'*)
    case "$input" in
      *slower*) p50=118 ;;
      *) p50=103 ;;
    esac
    printf '{"point":"after_turn","body":{"protocol_version":1,"evidence":{"flip":"not-attempted","measurements":{"p50":%s}}}}\n' "$p50"
    ;;
  *)
    printf '%s\n' '{"point":"before_turn","body":{"protocol_version":1,"context":[{"label":"budget","text":"the recorded p50 budget is 105"}],"role":"triage","publish":[{"signal":"questions","value":{"count":2}}]}}'
    ;;
esac
"#;
    let shell = SubprocessWrapper::declare(
        vec!["/bin/sh".into(), "-c".into(), SCRIPT.into()],
        Vec::new(),
        DEFAULT_WRAPPER_TIMEOUT,
    )
    .expect("the transport is declared with a program and a budget")
    .wrapper;

    let (verdict, _, messages) = run_turn_with(shell, "make the parser faster").await;
    assert_eq!(
        verdict,
        Verdict::Met {
            evidence: EvidenceProvenance::PluginReported
        },
        "p50 103 is inside the 105 budget"
    );
    assert!(
        !messages.is_empty(),
        "the script contributes context, or this test proves nothing"
    );
}
