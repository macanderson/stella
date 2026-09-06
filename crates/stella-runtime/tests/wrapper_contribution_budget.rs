//! A plugin's text reaches the steering plane, costs what it costs, and is
//! cut when the turn cannot afford it.
//!
//! `SteeringSource::Plugin` had a rank and no producer. So a plugin that was
//! switched on could put any number of tokens in front of the model. Nothing
//! priced it, ranked it, or said what became of it. These tests drive
//! `stella_runtime::WrapperDispatch`, which is the shipped sequence. An
//! in-process plugin sits on one side and a `TurnDriver` that keeps what it is
//! handed sits on the other.
//!
//! They fail before this change for a plain reason. `TurnPrelude` held no
//! steering record to read. `WrapperDispatch` held no allowance to fit text
//! into. So the first two tests name methods that did not exist, and the third
//! had nothing that could cut a message.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use stella_core::steering::SteeringSource;
use stella_core::steering::ledger::SteeringLedger;
use stella_core::steering::plugins::ContextAllowance;
use stella_plugin::{
    AfterTurnRequest, AfterTurnResponse, BeforeTurnRequest, BeforeTurnResponse, FlipObservation,
    ObservedEvidence, PROTOCOL_VERSION, PluginManifest, SignalValues, TamperFinding, TurnOutcome,
    VolatileContext,
};
use stella_runtime::wrapper::{
    DrivenTurn, HostCallChannel, InProcessWrapper, RoundInput, TurnDriver, TurnPrelude,
    WrapperDispatch, WrapperError, WrapperHandler,
};

/// A steering-grade plugin that speaks at `research` and nowhere else.
///
/// `before_turn_stages` narrows the `before_turn` point to the stages a
/// manifest names. That is what makes the "an undeclared stage adds no row"
/// test a claim about the shipped filter. It is not a claim about how this
/// fixture behaves.
const RESEARCH_ONLY: &str = r#"
name = "stella-research"
description = "grounds the turn before it starts"

[loop]
participation = "steering"
points = ["before_turn"]
before_turn_stages = ["research"]

[wrapper]
id = "research-v1"

[[wrapper.stages]]
name = "research"

[[wrapper.stages]]
name = "execute"
"#;

/// The same plugin with the stage list taken off. So it is asked at both
/// stages, and it speaks at both.
const EVERY_STAGE: &str = r#"
name = "stella-research"
description = "grounds the turn before it starts"

[loop]
participation = "steering"
points = ["before_turn"]

[wrapper]
id = "research-v1"

[[wrapper.stages]]
name = "research"

[[wrapper.stages]]
name = "execute"
"#;

fn manifest(text: &str) -> PluginManifest {
    PluginManifest::from_toml_str(text).expect("the fixture manifest loads")
}

/// Says one message at every stage it is asked at, sized by `words`.
struct Contributor {
    words: usize,
}

#[async_trait]
impl WrapperHandler for Contributor {
    async fn before_turn(
        &self,
        request: BeforeTurnRequest,
        _host: &dyn HostCallChannel,
    ) -> Result<BeforeTurnResponse, WrapperError> {
        Ok(BeforeTurnResponse {
            context: vec![VolatileContext::new(
                request.stage.as_str(),
                format!(
                    "{} {}",
                    request.stage.as_str(),
                    "ground ".repeat(self.words)
                ),
            )],
            ..BeforeTurnResponse::empty()
        })
    }

    async fn after_turn(
        &self,
        _request: AfterTurnRequest,
        _host: &dyn HostCallChannel,
    ) -> Result<AfterTurnResponse, WrapperError> {
        Ok(AfterTurnResponse {
            protocol_version: PROTOCOL_VERSION,
            evidence: ObservedEvidence {
                flip: FlipObservation::NotAttempted,
                measurements: BTreeMap::new(),
                detail: None,
            },
        })
    }
}

/// A host that keeps the one prelude it was handed.
#[derive(Default)]
struct Recorder {
    seen: Option<TurnPrelude>,
}

#[async_trait(?Send)]
impl TurnDriver for Recorder {
    async fn run_turn(&mut self, prelude: TurnPrelude) -> DrivenTurn {
        self.seen = Some(prelude);
        DrivenTurn {
            outcome: TurnOutcome {
                completed: true,
                answer: "done".into(),
                tools: None,
                changed_files: None,
            },
            tamper: TamperFinding::NotChecked,
        }
    }
}

/// The host's pre-turn snapshot. Every field is answered here because
/// `SignalValues` derives no `Default`. A host must say what it saw.
fn signals() -> SignalValues {
    SignalValues {
        test_command: false,
        candidates: 0,
        budget_metered: false,
        conversational: false,
        questions: 0,
        plans: false,
        verifies: false,
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

fn input() -> RoundInput {
    RoundInput {
        goal: "fix the flaky test".into(),
        signals: signals(),
        candidate: None,
    }
}

fn bind(text: &str, words: usize) -> WrapperDispatch {
    WrapperDispatch::bind(
        manifest(text),
        Arc::new(InProcessWrapper::new(Contributor { words })),
    )
    .expect("[wrapper] declared")
}

async fn drive(dispatch: WrapperDispatch) -> TurnPrelude {
    let mut recorder = Recorder::default();
    dispatch
        .run(input(), &mut recorder)
        .await
        .expect("the dispatch ran");
    recorder.seen.expect("the host was asked to run a turn")
}

/// **The witness.** Text from a granted stage reaches the plane as a
/// `SteeringSource::Plugin` row. The row is named for the plugin and the
/// stage. It is priced over the text that reached the prompt.
#[tokio::test]
async fn a_granted_contribution_reaches_the_plane() {
    let prelude = drive(bind(RESEARCH_ONLY, 4)).await;

    let steering = prelude.steering().clone();
    assert_eq!(steering.selected.len(), 1, "{:?}", steering.selected);
    assert_eq!(steering.selected[0].source, SteeringSource::Plugin);
    assert_eq!(steering.selected[0].handle, "stella-research/research");
    assert!(steering.dropped.is_empty(), "{:?}", steering.dropped);

    let injected: u64 = prelude
        .into_messages()
        .iter()
        .map(|message| stella_protocol::estimate_tokens(&message.content))
        .sum();
    assert_eq!(
        steering.est_tokens(),
        injected,
        "the plane charged exactly what the prompt carries"
    );
}

/// A stage the manifest never named adds no row. The filter that stops the
/// dispatch also stops the cost. So a row is never a claim about a plugin
/// nobody asked.
#[tokio::test]
async fn an_undeclared_stage_produces_no_candidate() {
    let narrowed = drive(bind(RESEARCH_ONLY, 4)).await;
    let every = drive(bind(EVERY_STAGE, 4)).await;

    assert_eq!(
        narrowed.steering().selected.len(),
        1,
        "one declared stage, one candidate"
    );
    assert_eq!(
        every.steering().selected.len(),
        2,
        "and both stages when the manifest declares both: {:?}",
        every.steering().selected
    );
    assert!(
        !narrowed
            .steering()
            .selected
            .iter()
            .any(|candidate| candidate.handle.ends_with("/execute")),
        "the undeclared stage contributed nothing to cost"
    );
}

/// **The witness for the budget.** Text the turn cannot afford is kept out of
/// the prompt and named in the drop report. The allowance is the one cell the
/// block has already spent from.
#[tokio::test]
async fn a_contribution_over_the_allowance_is_withheld_and_named() {
    let ledger = Arc::new(SteeringLedger::new());
    ledger.open_turn();
    // The turn's records, skills and frames got there first and left ten
    // tokens, which no contribution below fits in.
    ledger.spend(990);

    let prelude = drive(
        bind(EVERY_STAGE, 60)
            .with_context_allowance(ContextAllowance::new(1_000, Arc::clone(&ledger))),
    )
    .await;

    let steering = prelude.steering().clone();
    assert_eq!(steering.selected.len(), 0, "{:?}", steering.selected);
    assert_eq!(steering.dropped.len(), 2, "{:?}", steering.dropped);
    assert!(
        steering
            .dropped
            .iter()
            .all(|drop| drop.source == SteeringSource::Plugin),
        "the drops arrive on the plane as plugin drops: {:?}",
        steering.dropped
    );
    assert!(
        prelude.into_messages().is_empty(),
        "and nothing the allowance refused reached the prompt"
    );
    assert_eq!(ledger.spent(), 990, "nothing was charged for a refusal");
}

/// A plugin that never asked for `before_turn` is not asked, and costs
/// nothing. The grant is the gate, one rung above the stage list.
#[tokio::test]
async fn a_plugin_that_declared_no_point_contributes_nothing() {
    const NO_POINTS: &str = r#"
name = "stella-quiet"
description = "declares a stage order and answers nothing"

[loop]
participation = "steering"

[wrapper]
id = "quiet-v1"

[[wrapper.stages]]
name = "execute"
"#;

    let prelude = drive(bind(NO_POINTS, 40)).await;

    assert!(prelude.steering().selected.is_empty());
    assert!(prelude.steering().dropped.is_empty());
    assert!(prelude.into_messages().is_empty());
}
