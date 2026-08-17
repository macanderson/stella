//! The host sequence: a wrapper plugin **participates** in a turn, rather than
//! being available to.
//!
//! `tests/wrapper_socket.rs` proves the socket answers. It plays the host
//! itself, calling the four points in an order the test file wrote down — so
//! every property it establishes is a property of that file, and nothing in the
//! workspace had to hold the same order. That is exactly what #3494 found:
//! `grep -rn "before_turn\|after_turn" crates/stella-cli/src
//! crates/stella-pipeline/src` returned one port handle and no dispatch.
//!
//! These tests drive `stella_runtime::WrapperDispatch` — the production
//! sequence — with a plugin written in `sh` on one side and a recording
//! [`TurnDriver`] on the other. They fail before the change for the plainest
//! possible reason: `WrapperDispatch` did not exist, so no code path anywhere
//! carried a contribution from a plugin into a turn.
//!
//! `cfg(unix)` for the reason `wrapper_socket.rs` states: the plugins here are
//! `/bin/sh` scripts, so on Windows this file compiles to nothing (#3497).

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use stella_plugin::{
    Outcome, PluginManifest, SignalValues, StageName, StopReason, TamperFinding, TurnOutcome,
    UnmetBecause, Verdict,
};
use stella_protocol::completion::{CompletionMessage, MessageRole};
use stella_runtime::wrapper::{
    DEFAULT_WRAPPER_TIMEOUT, DrivenTurn, RoundInput, SubprocessWrapper, TurnDriver, TurnPrelude,
    WrapperDispatch, WrapperError,
};

/// An arbiter whose definition of done is a budget rather than a flip, so the
/// verdict turns on a number the plugin reports against a threshold a human
/// read at install.
const MANIFEST: &str = r#"
name = "reference-wrapper"
description = "keeps the benchmark inside its recorded budget"

[loop]
participation = "arbiter"
hooks = ["Stop"]
points = ["before_turn", "after_turn"]
max_holds = 3

[requirements]
within-budget = "the benchmark p50 stays inside its recorded budget"

[oracle]
command = { argv = ["bench"], timeout_secs = 60 }
flip = "not-applicable"
measurements = ["p50"]

[[oracle.checks]]
requirement = "within-budget"
check = "p50 <= 105"

[subloop]
stages = ["triage"]

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
"#;

/// The plugin. It contributes a budget note at every stage, and reports a p50
/// that is **outside** the budget until the turn it is told about mentions the
/// correction — so a second round is the only thing that can make it green.
const PLUGIN: &str = r#"
input=$(cat)
case "$input" in
  *'"point":"after_turn"'*)
    case "$input" in
      *"unrolled the loop"*) p50=101 ;;
      *) p50=118 ;;
    esac
    printf '{"point":"after_turn","body":{"protocol_version":1,"evidence":{"flip":"not-attempted","measurements":{"p50":%s}}}}\n' "$p50"
    ;;
  *)
    printf '%s\n' '{"point":"before_turn","body":{"protocol_version":1,"context":[{"label":"budget","text":"the recorded p50 budget is 105"}],"role":"triage","scope":["crates/stella-core"],"publish":[{"signal":"questions","value":{"count":2}}]}}'
    ;;
esac
"#;

fn manifest() -> PluginManifest {
    PluginManifest::from_toml_str(MANIFEST).expect("the reference manifest loads")
}

fn plugin(script: &str) -> Arc<SubprocessWrapper> {
    Arc::new(
        SubprocessWrapper::declare(
            vec!["/bin/sh".into(), "-c".into(), script.into()],
            Vec::new(),
            DEFAULT_WRAPPER_TIMEOUT,
        )
        .expect("the transport is declared with a program and a budget")
        .wrapper,
    )
}

/// The host's pre-turn snapshot. `questions: 2` is what makes the conditional
/// `research` stage run, so the program below is three stages rather than two.
fn signals() -> SignalValues {
    SignalValues {
        test_command: false,
        candidates: 1,
        budget_metered: false,
        conversational: false,
        questions: 2,
        plans: true,
        verifies: true,
        wants_witness: false,
        wants_verifier: false,
        mutating_actions: 0,
        diff_lines: 0,
        witness_authored: false,
        flip_achieved: false,
        tests_red: false,
        tests_green: false,
    }
}

fn input(goal: &str) -> RoundInput {
    RoundInput {
        goal: goal.into(),
        signals: signals(),
        candidate: None,
    }
}

/// What one turn was asked to do — the whole point of the seam, recorded.
struct Recorded {
    round: u32,
    stages: Vec<StageName>,
    role: Option<String>,
    scope: Vec<String>,
    messages: Vec<CompletionMessage>,
}

/// A host that records what it was asked for and answers with a fixed turn.
///
/// Its `answer` is what a real engine would have produced: the second round's
/// answer names the correction, which is what the plugin's `after_turn`
/// measures against. So "another turn ran" is not asserted from a counter — it
/// is the only thing that can change the verdict.
struct Recorder {
    turns: Vec<Recorded>,
    answers: Vec<&'static str>,
}

impl Recorder {
    fn new(answers: Vec<&'static str>) -> Self {
        Self {
            turns: Vec::new(),
            answers,
        }
    }
}

#[async_trait(?Send)]
impl TurnDriver for Recorder {
    async fn run_turn(&mut self, prelude: TurnPrelude) -> DrivenTurn {
        let answer = self
            .answers
            .get(self.turns.len())
            .copied()
            .unwrap_or("no further work");
        self.turns.push(Recorded {
            round: prelude.round(),
            stages: prelude.stages().to_vec(),
            role: prelude.role().map(str::to_string),
            scope: prelude.scope().to_vec(),
            messages: prelude.into_messages(),
        });
        DrivenTurn {
            outcome: TurnOutcome {
                completed: true,
                answer: answer.into(),
                tools: vec!["edit_file".into()],
                changed_files: vec!["crates/stella-core/src/driver.rs".into()],
            },
            // This host holds no candidate worktree, so it took no
            // authoring-time snapshot and says so itself (#3499). The oracle
            // here declares `flip = "not-applicable"`, so nothing turns on it.
            tamper: TamperFinding::NotChecked,
        }
    }
}

/// **The witness.** A wrapper plugin participates in a real turn: its
/// `before_turn` contribution reaches the prompt *after* the stable prefix, its
/// `after_turn` evidence reaches `judge`, and `judge`'s verdict is what decides
/// whether another turn runs.
///
/// Fails before #3494 because `WrapperDispatch` did not exist — nothing
/// dispatched, so no contribution could reach a turn and no verdict could ask
/// for another one.
#[tokio::test]
async fn a_wrapper_plugins_verdict_decides_whether_another_turn_runs() {
    let dispatch = WrapperDispatch::bind(manifest(), plugin(PLUGIN)).expect("[wrapper] declared");
    assert_eq!(dispatch.variant(), "reference-v1");

    // The first turn does not mention the correction, so the plugin measures
    // p50 118 — outside the declared budget. The second one does.
    let mut host = Recorder::new(vec!["rewrote the parser", "unrolled the loop"]);
    let report = dispatch
        .run(input("make the parser faster"), &mut host)
        .await
        .expect("a validated wrapper resolves");

    assert!(
        report.faults.is_empty(),
        "the plugin answered every point: {:?}",
        report.faults
    );
    assert_eq!(
        host.turns.len(),
        2,
        "the first round's verdict was Unmet and the arbiter had holds left, so the host was \
         asked for a second turn"
    );
    assert_eq!(report.rounds, 2);
    assert_eq!(report.verdict, Verdict::Met, "p50 101 is inside 105");
    assert_eq!(report.outcome, Outcome::Met);
    assert!(report.met());

    // The contribution reached the turn — as volatile messages, never the
    // stable prefix (invariant 7). One per stage the program ran.
    let first = &host.turns[0];
    assert_eq!(first.round, 0);
    assert_eq!(
        first.stages,
        [StageName::Triage, StageName::Research, StageName::Execute],
        "`questions > 0` is published by triage, so the conditional stage runs"
    );
    assert_eq!(
        first.messages.len(),
        3,
        "one contribution per stage that ran"
    );
    for message in &first.messages {
        assert_eq!(
            message.role,
            MessageRole::User,
            "a contribution rides after the byte-stable prefix, never inside it"
        );
        assert_eq!(message.content, "the recorded p50 budget is 105");
    }
    assert_eq!(
        first.role.as_deref(),
        Some("triage"),
        "the declared role intent reached the turn, checked against [roles]"
    );
    assert_eq!(first.scope, ["crates/stella-core"]);

    // The second turn carries the correction the *verdict* produced, last,
    // after that round's own contributions — and it names the clause a human
    // consented to.
    let second = &host.turns[1];
    assert_eq!(second.round, 1);
    assert_eq!(
        second.messages.len(),
        4,
        "three contributions + the correction"
    );
    let correction = &second.messages[3];
    assert_eq!(correction.role, MessageRole::User);
    assert!(
        correction.content.contains("within-budget")
            && correction.content.contains("p50 <= 105")
            && correction.content.contains("118"),
        "the correction cites the declared requirement and what the evidence said: {}",
        correction.content
    );
}

/// The falsifier for the test above: with the plugin never able to report a
/// number inside the budget, the loop stops at the host's ceiling rather than
/// running forever — and the unmet clause is reported, never dropped.
#[tokio::test]
async fn a_spent_allowance_stops_the_loop_and_still_reports_what_is_unmet() {
    let dispatch = WrapperDispatch::bind(manifest(), plugin(PLUGIN))
        .expect("[wrapper] declared")
        .with_host_max_holds(1);
    let mut host = Recorder::new(vec!["rewrote the parser"]);
    let report = dispatch
        .run(input("make the parser faster"), &mut host)
        .await
        .expect("a validated wrapper resolves");

    assert_eq!(
        host.turns.len(),
        2,
        "one hold is one extra turn, and the manifest's ask of 3 is clamped to the host's 1"
    );
    let Outcome::Unmet { unmet, stopped } = &report.outcome else {
        panic!(
            "p50 118 twice is outside the budget, got {:?}",
            report.outcome
        );
    };
    assert_eq!(
        *stopped,
        StopReason::AllowanceSpent {
            spent: 1,
            allowed: 1
        }
    );
    assert_eq!(unmet.len(), 1);
    assert_eq!(unmet[0].requirement, "within-budget");
    assert_eq!(
        unmet[0].because,
        UnmetBecause::Budget {
            check: "p50 <= 105".into(),
            reported: 118,
        }
    );
}

/// A plugin that cannot answer is an **abstention**, not a claim. The turn
/// still runs — a wrapper with nothing to say is not the user's fault — and the
/// verdict abstains rather than blaming the worker for evidence nobody
/// collected. Both failures are reported.
#[tokio::test]
async fn a_wrapper_that_fails_abstains_and_the_turn_still_runs() {
    let dispatch = WrapperDispatch::bind(manifest(), plugin("cat >/dev/null; exit 7"))
        .expect("[wrapper] declared");
    let mut host = Recorder::new(vec!["did the work anyway"]);
    let report = dispatch
        .run(input("make the parser faster"), &mut host)
        .await
        .expect("a validated wrapper resolves");

    assert_eq!(host.turns.len(), 1, "the turn ran without a contribution");
    assert!(
        host.turns[0].messages.is_empty(),
        "a wrapper that could not speak contributed nothing"
    );
    assert_eq!(
        report.faults.len(),
        4,
        "three before_turn stages and one after_turn, each reported: {:?}",
        report.faults
    );
    assert!(
        report
            .faults
            .iter()
            .all(|fault| matches!(fault, WrapperError::Exit { .. })),
        "{:?}",
        report.faults
    );
    assert!(
        matches!(report.verdict, Verdict::Undecided { .. }),
        "unobserved evidence abstains rather than passing or failing: {:?}",
        report.verdict
    );
    assert!(!report.met());
}

/// An undeclared point is never dispatched — the authoritative filter asked
/// before the process is, so a plugin that would happily answer one it never
/// declared is simply never asked (#3501).
#[tokio::test]
async fn an_undeclared_point_is_never_dispatched() {
    let declared_after_only = MANIFEST.replace(
        "points = [\"before_turn\", \"after_turn\"]",
        "points = [\"after_turn\"]",
    );
    let manifest = PluginManifest::from_toml_str(&declared_after_only)
        .expect("declaring one point is a legal manifest");
    // The plugin below fails at whatever it is asked, so a dispatched point is
    // a fault and an undispatched one is silence. Three stages resolve, and if
    // `before_turn` were asked anyway there would be four faults rather than
    // the one `after_turn` earns.
    let dispatch = WrapperDispatch::bind(manifest, plugin("cat >/dev/null; exit 7"))
        .expect("[wrapper] declared");
    let mut host = Recorder::new(vec!["work"]);
    let report = dispatch
        .run(input("make the parser faster"), &mut host)
        .await
        .expect("a validated wrapper resolves");
    assert_eq!(
        host.turns[0].stages.len(),
        3,
        "the stage program still resolved — the point filter is what withheld the dispatch"
    );
    assert!(
        host.turns[0].messages.is_empty(),
        "before_turn was never dispatched, so nothing was contributed"
    );
    assert_eq!(
        report.faults.len(),
        1,
        "only the declared point was asked, and only it could fail: {:?}",
        report.faults
    );
}

/// A manifest with no `[wrapper]` has no stage order and no variant id, so it
/// is refused by name rather than driven with an invented default.
#[test]
fn a_manifest_without_a_wrapper_block_is_refused_by_name() {
    let hooks_only = PluginManifest::from_toml_str(
        "name = \"lint-gate\"\n[loop]\nparticipation = \"steering\"\nhooks = [\"PreToolUse\"]",
    )
    .expect("a hooks-only manifest is legal");
    let error = WrapperDispatch::bind(
        hooks_only,
        Arc::new(
            SubprocessWrapper::declare(
                vec!["/bin/true".into()],
                Vec::new(),
                Duration::from_secs(1),
            )
            .expect("declared")
            .wrapper,
        ),
    )
    .expect_err("there is no stage order to run");
    assert!(
        matches!(error, WrapperError::NotAWrapper { ref plugin } if plugin == "lint-gate"),
        "got {error:?}"
    );
}
