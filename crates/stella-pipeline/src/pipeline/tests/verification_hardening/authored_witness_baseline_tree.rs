//! The authored witness's failing baseline, published as a `Baseline` tree
//! (#2272).
//!
//! The defect this pins: across every recorded ArenaBench trial, not one
//! `proof.oracle` step or `oracle_trace` entry carried `tree: "baseline"`.
//! The only site that ever emitted one sits behind a *configured*
//! `--test-command` (`pipeline.rs`'s pre-execute observation), and a run that
//! authors its own witness is by construction a run with no such command. The
//! authored witness's baseline WAS observed — in a pristine snapshot of the
//! candidate's own pre-execution tree — but the fold that transplanted it into
//! the candidate's oracle (`witness_stage.rs`) mutated oracle state and
//! recorded nothing, so the leading `✗` a reader saw in the rendered trace was
//! the post-execute *candidate* run. A consumer could not tell "failed on the
//! old tree, passed on the new" from "failed, then passed in the same tree" —
//! the same absence-rendering-as-distinction failure class as #2132 and #2108.
//!
//! A child module for the same two reasons `flip_halt_arming` is: it reaches
//! the shared fakes through the parent's own `use super::*`, and the
//! already-oversized `tests.rs` does not grow another module declaration.

use super::*;

/// #2272's witness: a run that authors its own witness publishes the failing
/// baseline it observed as a `ProofTree::Baseline` observation — on the proof
/// rail and in the verdict's ladder snapshot — so the fail→pass flip on the
/// wire is a two-tree claim rather than one tree observed twice.
///
/// On `main` this fails on both halves at once: no `Baseline` proof step is
/// emitted at all when no test command is configured, and the trace's first
/// entry is the post-execute `Candidate` pass.
#[tokio::test]
async fn an_authored_witness_records_its_failing_baseline_as_a_baseline_tree() {
    // triage → "single"; worker → done; THEN the witness author, because
    // authoring is demand-driven and runs after execution.
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("wrote the test.\nTEST_COMMAND: cargo test --test witness witness -- --exact"),
    ]);
    // The two trees of the flip. Candidate (id 0): the authored command
    // PASSES post-execute. Baseline (id 1): the same command FAILS on the
    // pristine pre-execution snapshot — the observation this test is about.
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let candidate = FakeWorkspace::new(0, vec![true], Ok(vec![]), log.clone()).with_repo_status(
        SeqRepoStatus::new(vec![vec![], vec![("tests/witness.rs", "w1")]]),
    );
    let baseline = FakeWorkspace::new(1, vec![false], Ok(vec![]), log.clone()).with_repo_status(
        SeqRepoStatus::new(vec![vec![], vec![("tests/witness.rs", "w1")]]),
    );
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log);
    // No `test_command`: the configured-command baseline site cannot run, so
    // every `Baseline` observation below has to come from the authored path.
    let config = PipelineConfig::default();
    assert!(
        config.test_command.is_none(),
        "the authored path is only reachable with no configured command"
    );
    let (outcome, events, _) =
        run_isolated_with_router(&provider, &port, config, "Fix the retry bug", router()).await;
    let outcome = outcome.expect("run succeeds");

    // (a) The rail carries the baseline observation.
    let baseline_proofs: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Proof {
                step:
                    ProofStep::Oracle {
                        command,
                        passed,
                        tree: ProofTree::Baseline,
                        ..
                    },
            } => Some((command.clone(), *passed)),
            _ => None,
        })
        .collect();
    assert_eq!(
        baseline_proofs,
        vec![(
            "cargo test --test witness witness -- --exact".to_string(),
            false
        )],
        "the authored witness's failing baseline is published once, as a \
         baseline tree, naming the command it was observed with: {events:?}"
    );

    // (b) The verdict's provenance carries the same two-tree trace.
    let verdict = outcome.verdict.expect("verified");
    assert!(verdict.passed && verdict.deterministic, "{verdict:?}");
    let snapshot = verdict
        .ladder
        .as_deref()
        .expect("the verdict carries the snapshot it was decided from");
    let trace = &snapshot.oracle_trace;
    assert_eq!(
        trace.first().map(|o| (o.tree, o.passed)),
        Some((ProofTree::Baseline, false)),
        "the trace OPENS on the failing baseline — the observation that arms \
         the flip — not on the candidate run: {trace:?}"
    );
    assert!(
        trace
            .iter()
            .skip(1)
            .any(|o| o.tree == ProofTree::Candidate && o.passed),
        "and a passing candidate observation follows it, so the recorded \
         flip spans two trees: {trace:?}"
    );
}
