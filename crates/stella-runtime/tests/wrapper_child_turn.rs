//! **The witness for `child_turn`** (#3564, #3541, `doc:turn-loop-wrappers`
//! §9.3): a plugin asks the host to spend a model call at a declared role
//! intent, and the host — not the plugin — makes it.
//!
//! Every test here fails before the change for one reason: **no host performed
//! the capability.** `child_turn` crossed the wire, was gated exactly like
//! `recall`, and then met an `Unsupported` refusal in the only shipped
//! `HostCapabilities` implementation. So a plugin got no engine, no provider and
//! no credential, and nothing on the wire let it ask the host for a model call —
//! which is why `plugins/stella-research` shipped the checkable half (scanning
//! files) and could not do the half the built-in does (read-only sub-agents
//! answering triage's questions).
//!
//! The plugins below are `sh` scripts with no JSON library, for
//! `wrapper_host_call.rs`'s reason: a capability only Rust can reach is a Rust
//! API with extra steps (`doc:pipeline-as-plugins` §5 commitment 2).
//!
//! **What the plugin never touches, and the tests assert it:** the dispatcher
//! records every [`SubAgentSpec`] it was handed, so a test can read what the
//! host was actually asked to run — the attribution seat, the read-only tool
//! surface, the instruction — and confirm that the only two things the plugin
//! contributed are the two fields `ChildTurnArgs` has.
//!
//! `cfg(unix)` is the same declared gap the rest of this suite carries, tracked
//! in #3497.

#![cfg(unix)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use stella_core::{
    SubAgentDispatcher, SubAgentOutcome, SubAgentReport, SubAgentSpec, SubAgentSpendLedger,
    push_sub_agent_spend,
};
use stella_plugin::{
    BeforeTurnRequest, HostCallRefusal, PROTOCOL_VERSION, PluginManifest, StageName,
};
use stella_protocol::event::ModelCallRole;
use stella_runtime::wrapper::{
    ChildTurns, DEFAULT_HOST_MAX_CALLS, HostCallGate, HostPlanes, SubprocessWrapper, TurnWrapper,
    admissible,
};

/// What a human consents to at install: this plugin answers `before_turn`, may
/// ask for `child_turn`, and declares two role intents — one that resolves to a
/// research seat, one that points straight at the worker.
///
/// `grader` is declared deliberately. The independence rule has to be tested
/// against a role the *manifest permits*, or it proves only that the manifest
/// check works.
const GRADING_MANIFEST: &str = r#"
name = "grading-wrapper"
description = "asks the host to spend a model call it cannot make itself"

[loop]
participation = "steering"
points = ["before_turn"]
calls = ["child_turn"]

[subloop]
stages = ["research"]

[roles.reviewer]
tier = "research"

[roles.grader]
tier = "worker"

[wrapper]
id = "grading-v1"

[[wrapper.stages]]
name = "research"
"#;

/// The host's sub-agent dispatcher — the same port `task_assign` runs on, and
/// the one `ChildTurns` spends through.
///
/// It records every spec, which is how these tests answer the question that
/// matters: **who made the model call?** A plugin that had reached for a
/// provider itself would leave nothing here.
#[derive(Default, Clone)]
struct Dispatcher {
    specs: Arc<Mutex<Vec<SubAgentSpec>>>,
    ledger: SubAgentSpendLedger,
}

impl Dispatcher {
    fn specs(&self) -> Vec<SubAgentSpec> {
        self.specs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl SubAgentDispatcher for Dispatcher {
    async fn dispatch(&self, spec: SubAgentSpec) -> SubAgentOutcome {
        let summary = format!(
            "the diff drops the retry on 429; asked: {}",
            spec.instruction
        );
        self.specs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(spec);
        push_sub_agent_spend(&self.ledger, 0.02);
        SubAgentOutcome::Completed(SubAgentReport {
            summary,
            truncated: false,
            cost_usd: 0.02,
            steps: 3,
            absorbed_messages: 7,
        })
    }
}

fn manifest() -> PluginManifest {
    PluginManifest::from_toml_str(GRADING_MANIFEST).expect("the manifest loads")
}

/// The host a driver would assemble: the plugin's declared role intents bound
/// to this host's dispatcher, behind the gate the manifest's grant declares.
fn host(
    manifest: &PluginManifest,
    max_turns: u32,
) -> (Arc<HostCallGate>, Arc<ChildTurns<Dispatcher>>, Dispatcher) {
    let dispatcher = Dispatcher::default();
    let plane =
        Arc::new(ChildTurns::declare(manifest, dispatcher.clone()).with_max_turns(max_turns));
    let gate = Arc::new(HostCallGate::declare(
        manifest.loop_grant.clone(),
        DEFAULT_HOST_MAX_CALLS,
        Box::new(HostPlanes::none().with_child_turns(Arc::clone(&plane))),
    ));
    (gate, plane, dispatcher)
}

fn plugin(script: &str, gate: Arc<HostCallGate>) -> SubprocessWrapper {
    SubprocessWrapper::declare(
        vec!["/bin/sh".into(), "-c".into(), script.into()],
        Vec::new(),
        Duration::from_secs(10),
    )
    .expect("the transport is declared with a program and a budget")
    .wrapper
    .serving(gate)
}

fn before() -> BeforeTurnRequest {
    BeforeTurnRequest {
        protocol_version: PROTOCOL_VERSION,
        wrapper: "grading-v1".into(),
        stage: StageName::Research,
        round: 0,
        goal: "the retry is dropped on a 429".into(),
        candidate: None,
        published: Vec::new(),
    }
}

/// **The witness.** A plugin names a declared role intent, the host runs the
/// turn, and the plugin contributes the child's answer as context.
///
/// Three claims, and the third is the one the whole design exists for:
///
/// 1. the plugin got a real turn's result back through the wire;
/// 2. the **host** made the call — the dispatcher holds the spec, attributed to
///    the seat the intent resolved to and running read-only;
/// 3. the spend is visible against the declared role, which is
///    `doc:turn-loop-wrappers` §9.2's stated reason for preferring a declared
///    role to a `judge`.
#[tokio::test]
async fn a_plugin_asks_for_a_model_call_at_a_declared_role_and_the_host_makes_it() {
    let manifest = manifest();
    let (gate, plane, dispatcher) = host(&manifest, 2);

    let script = r#"
read -r request
printf '%s\n' '{"call":"child_turn","id":11,"args":{"role":"reviewer","instruction":"does the diff drop the retry?"}}'
read -r answer
case "$answer" in
  *'"result":11'*) ;;
  *) printf 'the answer did not carry the id this plugin chose\n' >&2 ; exit 1 ;;
esac
case "$answer" in
  *'"seat":"research"'*) seat="research" ;;
  *) seat="unknown" ;;
esac
case "$answer" in
  *'drops the retry on 429'*) finding="the reviewer ($seat) says the diff drops the retry on 429" ;;
  *) finding="no assessment was available" ;;
esac
printf '{"point":"before_turn","body":{"protocol_version":1,"context":[{"label":"reviewer","text":"%s"}]}}\n' "$finding"
"#;

    let response = plugin(script, Arc::clone(&gate))
        .before_turn(before())
        .await
        .expect("the plugin asked, was answered, and answered the point");

    let messages = admissible(&manifest, response)
        .expect("the contribution is within what the manifest declared")
        .into_messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].content, "the reviewer (research) says the diff drops the retry on 429",
        "a real turn's result reached the plugin and became the turn's context"
    );

    let specs = dispatcher.specs();
    assert_eq!(
        specs.len(),
        1,
        "exactly one model call, and the host made it"
    );
    assert_eq!(
        specs[0].role,
        ModelCallRole::Research,
        "the role intent resolved to a seat, and the seat is the receipt's attribution"
    );
    assert_eq!(specs[0].instruction, "does the diff drop the retry?");
    assert!(
        !specs[0].write_access,
        "a plugin's child turn is read-only, enforced at execution rather than by prompt"
    );

    let spends = plane.spends();
    assert_eq!(spends.len(), 1, "the spend is visible, not hidden");
    assert_eq!(spends[0].role, "reviewer");
    assert_eq!(spends[0].seat, ModelCallRole::Research);
    assert!((spends[0].cost_usd - 0.02).abs() < f64::EPSILON);

    assert!(
        gate.refusals().is_empty(),
        "a declared call inside the allowance is performed: {:?}",
        gate.refusals()
    );
}

/// A role intent the manifest never declared is refused — `admissible`'s
/// existing rule for `BeforeTurnResponse::role`, enforced at the value that
/// arrives mid-point — and the refusal reaches both sides.
#[tokio::test]
async fn an_undeclared_role_intent_is_refused_to_the_plugin_and_reported_to_the_host() {
    let manifest = manifest();
    let (gate, plane, dispatcher) = host(&manifest, 2);

    let script = r#"
read -r request
printf '%s\n' '{"call":"child_turn","id":1,"args":{"role":"auditor","instruction":"grade it"}}'
read -r answer
case "$answer" in
  *'"refusal":"undeclared"'*) note="the host refused an undeclared role intent; degrading" ;;
  *) note="unexpected answer" ;;
esac
printf '{"point":"before_turn","body":{"protocol_version":1,"context":[{"label":"note","text":"%s"}]}}\n' "$note"
"#;

    let response = plugin(script, Arc::clone(&gate))
        .before_turn(before())
        .await
        .expect("a refused call is a value the plugin reads, never a death");
    let messages = admissible(&manifest, response)
        .expect("the contribution is admissible")
        .into_messages();
    assert_eq!(
        messages[0].content,
        "the host refused an undeclared role intent; degrading"
    );

    let refusals = gate.refusals();
    assert_eq!(refusals.len(), 1, "the refusal is reported, never silent");
    assert_eq!(refusals[0].refusal, HostCallRefusal::Undeclared);
    assert!(
        refusals[0].detail.contains("reviewer"),
        "the report names what the plugin *did* declare: {}",
        refusals[0].detail
    );
    assert!(dispatcher.specs().is_empty(), "nothing was run");
    assert!(plane.spends().is_empty(), "nothing was spent");
}

/// **Verifier independence, through the host-call channel.** The role is
/// declared, it resolves, and the host still refuses it: a plugin may not spend
/// the model whose work it is judging.
///
/// The built-in staged pipeline's roster *reported* this loss for an operator
/// who chose it (`Roster::independence_losses`, deleted with that crate in
/// #3865); a plugin cannot choose it at all, and gets the
/// [`HostCallRefusal::Forbidden`] code that says so rather than one that reads
/// like a misconfiguration.
#[tokio::test]
async fn a_plugin_cannot_name_a_role_intent_that_resolves_to_the_worker() {
    let manifest = manifest();
    assert!(
        manifest
            .roles
            .as_ref()
            .is_some_and(|roles| roles.contains_key("grader")),
        "the manifest declares `grader`, so this refusal is not the manifest check in disguise"
    );
    let (gate, plane, dispatcher) = host(&manifest, 2);

    let script = r#"
read -r request
printf '%s\n' '{"call":"child_turn","id":2,"args":{"role":"grader","instruction":"grade your own work"}}'
read -r answer
case "$answer" in
  *'"refusal":"forbidden"'*) note="the worker's seat is not for sale" ;;
  *) note="unexpected answer" ;;
esac
printf '{"point":"before_turn","body":{"protocol_version":1,"context":[{"label":"note","text":"%s"}]}}\n' "$note"
"#;

    let response = plugin(script, Arc::clone(&gate))
        .before_turn(before())
        .await
        .expect("the point still completes");
    let messages = admissible(&manifest, response)
        .expect("the contribution is admissible")
        .into_messages();
    assert_eq!(messages[0].content, "the worker's seat is not for sale");

    let refusals = gate.refusals();
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].refusal, HostCallRefusal::Forbidden);
    assert!(
        dispatcher.specs().is_empty(),
        "the worker's seat was never dispatched"
    );
    assert!(plane.spends().is_empty());
}

/// **The budget is the host's.** A plugin asking for a third child turn against
/// a ceiling of one is answered `allowance-spent` and buys nothing — the clamp
/// is on what the host performs, not on what the plugin says.
#[tokio::test]
async fn a_plugin_asking_for_more_child_turns_than_the_ceiling_is_clamped_not_obeyed() {
    let manifest = manifest();
    let (gate, plane, dispatcher) = host(&manifest, 1);
    assert_eq!(plane.max_turns(), 1);

    let script = r#"
read -r request
granted=0
refused=0
for id in 1 2 3; do
  printf '{"call":"child_turn","id":%s,"args":{"role":"reviewer","instruction":"ask %s"}}\n' "$id" "$id"
  read -r answer
  case "$answer" in
    *'"refusal":"allowance-spent"'*) refused=$((refused + 1)) ;;
    *'"seat":"research"'*) granted=$((granted + 1)) ;;
    *) printf 'unexpected answer: %s\n' "$answer" >&2 ; exit 1 ;;
  esac
done
printf '{"point":"before_turn","body":{"protocol_version":1,"context":[{"label":"asks","text":"granted %s refused %s"}]}}\n' "$granted" "$refused"
"#;

    let response = plugin(script, Arc::clone(&gate))
        .before_turn(before())
        .await
        .expect("a spent allowance is answered, never fatal");
    let messages = admissible(&manifest, response)
        .expect("the contribution is admissible")
        .into_messages();
    assert_eq!(
        messages[0].content, "granted 1 refused 2",
        "the plugin asked three times and the host ran one"
    );

    assert_eq!(
        dispatcher.specs().len(),
        1,
        "the ceiling bounds model calls, not just answers"
    );
    assert_eq!(plane.spends().len(), 1);
    let refusals = gate.refusals();
    assert_eq!(refusals.len(), 2);
    assert!(
        refusals
            .iter()
            .all(|refused| refused.refusal == HostCallRefusal::AllowanceSpent),
        "{refusals:?}"
    );
}

/// A host that assembled no child-turn plane says so, with the code that means
/// "implemented, nothing behind it here" — never a silence a plugin has to
/// interpret as an answer.
#[tokio::test]
async fn a_host_with_no_child_turn_plane_says_so_rather_than_pretending() {
    let manifest = manifest();
    let gate = Arc::new(HostCallGate::declare(
        manifest.loop_grant.clone(),
        DEFAULT_HOST_MAX_CALLS,
        Box::new(HostPlanes::none()),
    ));

    let script = r#"
read -r request
printf '%s\n' '{"call":"child_turn","id":1,"args":{"role":"reviewer","instruction":"grade it"}}'
read -r answer
case "$answer" in
  *'"refusal":"unavailable"'*) note="this host runs no child turns" ;;
  *) note="unexpected answer" ;;
esac
printf '{"point":"before_turn","body":{"protocol_version":1,"context":[{"label":"note","text":"%s"}]}}\n' "$note"
"#;

    let response = plugin(script, Arc::clone(&gate))
        .before_turn(before())
        .await
        .expect("the point completes");
    let messages = admissible(&manifest, response)
        .expect("the contribution is admissible")
        .into_messages();
    assert_eq!(messages[0].content, "this host runs no child turns");
    assert_eq!(gate.refusals()[0].refusal, HostCallRefusal::Unavailable);
}
