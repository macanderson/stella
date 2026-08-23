//! **The witness for the host-call channel** (#3540, `doc:wrapper-socket` §6b):
//! a plugin asks the host for a capability mid-point, is handed real frames,
//! and contributes context built from them.
//!
//! Every test here fails before the change for the plainest possible reason:
//! **there was no channel.** The socket was strictly one-directional — one host
//! request, one plugin response — so a plugin could not ask for anything, the
//! manifest could not declare a capability to ask for, and the transport closed
//! stdin the moment the request was written. `stella-research` shipped its
//! research half and declined recall because there was nothing on the wire it
//! could use.
//!
//! The plugins below are `sh` scripts. That is the point rather than a
//! convenience: the channel has to be usable by a program with a JSON parser and
//! nothing else, or the wire contract is a Rust API with extra steps
//! (`doc:pipeline-as-plugins` §5). None of them uses a JSON library at all.
//!
//! `cfg(unix)` is the same declared gap the rest of this suite carries, tracked
//! in #3497.

#![cfg(unix)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use stella_plugin::{
    BeforeTurnRequest, HostCall, HostCallRefusal, HostStage, LoopGrant, PROTOCOL_VERSION,
    Participation, PluginManifest, RecallFrame, StageName, WrapperPoint,
};
use stella_runtime::wrapper::{
    DEFAULT_HOST_MAX_CALLS, HostCallGate, HostPlanes, RecallHost, SubprocessWrapper, TurnWrapper,
    WrapperError, admissible,
};

/// A plugin that asks for recall and contributes what it was given.
///
/// The manifest is what a human consents to at install: it answers
/// `before_turn`, it may ask for `recall`, and it may ask **once**.
const RECALLING_MANIFEST: &str = r#"
name = "recalling-wrapper"
description = "asks the host what it already knows before the turn runs"

[loop]
participation = "steering"
points = ["before_turn"]
calls = ["recall"]
max_calls = 1

[wrapper]
id = "recalling-v1"

[[wrapper.stages]]
name = "recall"
"#;

/// The context plane behind the host, with something worth recalling.
struct Remembers;

#[async_trait]
impl RecallHost for Remembers {
    async fn recall(&self, goal: &str) -> Vec<RecallFrame> {
        vec![
            RecallFrame {
                label: "lesson: retries".to_string(),
                kind: "memory".to_string(),
                source: "context.db".to_string(),
                uri: None,
                content: format!("the retry budget is 3, and {goal} hit it last run"),
            },
            RecallFrame {
                label: "symbol: retry_budget".to_string(),
                kind: "symbol".to_string(),
                source: "codegraph.db".to_string(),
                uri: Some("src/retry.rs".to_string()),
                content: "pub const RETRY_BUDGET: u32 = 3;".to_string(),
            },
        ]
    }
}

fn manifest(text: &str) -> PluginManifest {
    PluginManifest::from_toml_str(text).expect("the manifest loads")
}

fn gate(grant: LoopGrant, ceiling: u32) -> Arc<HostCallGate> {
    Arc::new(HostCallGate::declare(
        grant,
        ceiling,
        Box::new(HostPlanes::recalling(Remembers)),
    ))
}

fn plugin(script: &str, gate: Arc<HostCallGate>, timeout: Duration) -> SubprocessWrapper {
    SubprocessWrapper::declare(
        vec!["/bin/sh".into(), "-c".into(), script.into()],
        Vec::new(),
        timeout,
    )
    .expect("the transport is declared with a program and a budget")
    .wrapper
    .serving(gate)
}

fn before() -> BeforeTurnRequest {
    BeforeTurnRequest {
        protocol_version: PROTOCOL_VERSION,
        wrapper: "recalling-v1".into(),
        stage: StageName::Host(HostStage::Recall),
        round: 0,
        goal: "retry_budget is not honoured".into(),
        candidate: None,
        published: Vec::new(),
    }
}

/// **The witness.** A plugin asks for recall while `before_turn` is open, reads
/// the frames the host retrieved, and contributes context built out of them.
///
/// The script is the whole proof that this is a *wire* capability: it reads one
/// line, writes a call, reads the answer, and answers the point — with `read`,
/// `case` and `printf`, and no JSON parser anywhere.
#[tokio::test]
async fn a_plugin_asks_for_recall_mid_point_and_contributes_what_it_was_given() {
    let manifest = manifest(RECALLING_MANIFEST);
    let gate = gate(manifest.loop_grant.clone(), DEFAULT_HOST_MAX_CALLS);
    // The goal is read back out of the request, so the frames the host returns
    // are genuinely a function of what the plugin asked about.
    let script = r#"
read -r request
printf '%s\n' '{"call":"recall","id":7,"args":{"goal":"retry_budget is not honoured","limit":2}}'
read -r answer
case "$answer" in
  *'"result":7'*) ;;
  *) printf 'the answer did not carry the id this plugin chose\n' >&2 ; exit 1 ;;
esac
case "$answer" in
  *'the retry budget is 3'*) known="the retry budget is 3 (recalled)" ;;
  *) known="nothing was recalled" ;;
esac
case "$answer" in
  *'RETRY_BUDGET'*) known="$known, and src/retry.rs defines it" ;;
esac
printf '{"point":"before_turn","body":{"protocol_version":1,"context":[{"label":"recall","text":"%s"}]}}\n' "$known"
"#;

    let response = plugin(script, Arc::clone(&gate), Duration::from_secs(10))
        .before_turn(before())
        .await
        .expect("the plugin asked, was answered, and answered the point");

    let messages = admissible(&manifest, response)
        .expect("the contribution is within what the manifest declared")
        .into_messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].content, "the retry budget is 3 (recalled), and src/retry.rs defines it",
        "the frames the host retrieved reached the plugin and became the turn's context"
    );
    assert!(
        gate.refusals().is_empty(),
        "a declared call inside the allowance is performed, not refused: {:?}",
        gate.refusals()
    );
}

/// **A plugin that asks and then walks away is named as such** (#3794): it was
/// served, it exited cleanly, and it never answered the point.
///
/// The distinction is the whole point of the variant. Silence from a plugin
/// that asked for nothing is `NoResponse` — a plugin with nothing to say —
/// and silence from one that was mid-errand names the errand, so its author is
/// pointed at the call it abandoned rather than at the point in general. The
/// case was unreachable before this: the loop had already drained every
/// complete message the framer held by the time stdout ended, so the trailing
/// parse that raised `UnansweredCall` could only ever see `None`, and this
/// script was answered `NoResponse`.
#[tokio::test]
async fn a_plugin_that_asks_and_then_ends_without_answering_names_what_it_abandoned() {
    let manifest = manifest(RECALLING_MANIFEST);
    let gate = gate(manifest.loop_grant.clone(), DEFAULT_HOST_MAX_CALLS);
    // Asks, and exits without ever reading the answer. Its stdout closes with
    // the call as the last thing it said.
    let script = r#"
read -r request
printf '%s\n' '{"call":"recall","id":7,"args":{"goal":"retry_budget is not honoured","limit":2}}'
"#;

    let failed = plugin(script, Arc::clone(&gate), Duration::from_secs(10))
        .before_turn(before())
        .await
        .expect_err("an abandoned point is not a contribution");

    match &failed {
        WrapperError::UnansweredCall { call, .. } => {
            assert_eq!(
                *call,
                HostCall::Recall,
                "the errand the plugin walked away from is the one it is named by"
            );
        }
        other => panic!("the failure is named, not collapsed into another one: {other}"),
    }
}

/// An undeclared call is refused the way an undeclared hook is never invoked —
/// and the refusal reaches **both** sides: the plugin, which degrades honestly
/// rather than dying, and the host's report, because a capability quietly not
/// performed is a bug its author cannot diagnose.
#[tokio::test]
async fn an_undeclared_call_is_refused_to_the_plugin_and_reported_to_the_host() {
    let manifest = manifest(RECALLING_MANIFEST);
    let gate = gate(manifest.loop_grant.clone(), DEFAULT_HOST_MAX_CALLS);
    assert!(
        !manifest.loop_grant.permits_call(HostCall::ChildTurn),
        "this manifest declares recall and nothing else"
    );

    let script = r#"
read -r request
printf '%s\n' '{"call":"child_turn","id":1,"args":{"role":"verifier","instruction":"grade it"}}'
read -r answer
case "$answer" in
  *'"refusal":"undeclared"'*) note="the host refused child_turn; degrading" ;;
  *) note="unexpected answer" ;;
esac
printf '{"point":"before_turn","body":{"protocol_version":1,"context":[{"label":"note","text":"%s"}]}}\n' "$note"
"#;

    let response = plugin(script, Arc::clone(&gate), Duration::from_secs(10))
        .before_turn(before())
        .await
        .expect("a refused call is a value the plugin reads, never a death");

    let messages = admissible(&manifest, response)
        .expect("the contribution is admissible")
        .into_messages();
    assert_eq!(
        messages[0].content,
        "the host refused child_turn; degrading"
    );

    let refusals = gate.refusals();
    assert_eq!(refusals.len(), 1, "the refusal is reported, never silent");
    assert_eq!(refusals[0].call, HostCall::ChildTurn);
    assert_eq!(refusals[0].refusal, HostCallRefusal::Undeclared);
}

/// The plugin's `max_calls` is an ask. This host's ceiling is one, whatever the
/// manifest wanted, and the call past it is answered `allowance-spent` — so a
/// plugin learns to stop asking rather than being killed for it.
#[tokio::test]
async fn the_call_allowance_is_the_hosts_and_a_spent_one_is_answered() {
    let manifest = manifest(RECALLING_MANIFEST);
    let generous = LoopGrant {
        max_calls: Some(64),
        ..manifest.loop_grant.clone()
    };
    let gate = gate(generous, 1);
    assert_eq!(gate.max_calls(), 1, "the ask is clamped to the ceiling");

    let script = r#"
read -r request
printf '%s\n' '{"call":"recall","id":1,"args":{"goal":"first"}}'
read -r first
printf '%s\n' '{"call":"recall","id":2,"args":{"goal":"second"}}'
read -r second
case "$second" in
  *'"refusal":"allowance-spent"'*) note="the second ask was refused" ;;
  *) note="the allowance was not enforced" ;;
esac
printf '{"point":"before_turn","body":{"protocol_version":1,"context":[{"label":"note","text":"%s"}]}}\n' "$note"
"#;

    let response = plugin(script, Arc::clone(&gate), Duration::from_secs(10))
        .before_turn(before())
        .await
        .expect("a spent allowance is answered, not fatal");
    let messages = admissible(&manifest, response)
        .expect("admissible")
        .into_messages();
    assert_eq!(messages[0].content, "the second ask was refused");
    assert_eq!(
        gate.refusals()[0].refusal,
        HostCallRefusal::AllowanceSpent,
        "and the host was told which ask it turned down"
    );
}

/// **A plugin cannot buy time by talking.** The point timeout covers the whole
/// conversation, not each turn of it: this plugin asks forever, is answered
/// every time inside its allowance and refused after it, and is still killed at
/// the same deadline a silent plugin would be.
#[tokio::test]
async fn the_conversation_cannot_outlive_the_point_timeout() {
    let manifest = manifest(RECALLING_MANIFEST);
    let gate = gate(manifest.loop_grant.clone(), DEFAULT_HOST_MAX_CALLS);
    let budget = Duration::from_millis(700);

    // Never answers the point; just keeps asking.
    let script = r#"
read -r request
n=0
while : ; do
  n=$((n+1))
  printf '%s\n' "{\"call\":\"recall\",\"id\":$n,\"args\":{\"goal\":\"again\"}}"
  read -r answer || exit 0
done
"#;

    let started = Instant::now();
    let error = plugin(script, Arc::clone(&gate), budget)
        .before_turn(before())
        .await
        .expect_err("a conversation that never ends is killed at the deadline");
    let elapsed = started.elapsed();

    assert!(
        matches!(error, WrapperError::Timeout { .. }),
        "the deadline is the deadline, whatever was said inside it: {error}"
    );
    assert!(
        elapsed < budget * 4,
        "killed after {elapsed:?} of a {budget:?} budget — talking bought time"
    );
    assert!(
        gate.refusals()
            .iter()
            .all(|refused| refused.refusal == HostCallRefusal::AllowanceSpent),
        "everything past the allowance was refused rather than performed: {:?}",
        gate.refusals()
    );
}

/// A grade below `steering` may not ask, however its `calls` list reads — the
/// filter is the same authoritative one `permits_hook` and `permits_point` are,
/// and it grades participation rather than trusting the list.
#[tokio::test]
async fn a_grant_below_steering_leaks_no_capability() {
    let smuggled = LoopGrant {
        participation: Participation::Observer,
        hooks: Vec::new(),
        points: vec![WrapperPoint::BeforeTurn],
        calls: vec![HostCall::Recall],
        max_calls: None,
        max_fanout_width: None,
        max_holds: None,
    };
    let gate = gate(smuggled, DEFAULT_HOST_MAX_CALLS);
    assert!(
        !gate.offers_calls(),
        "an observer's declared call is not a call this host will answer"
    );
}

/// **A dead writer is not the same defect as a missing gate, and the real
/// fault must not be discarded.**
///
/// The plugin here closes its own stdin right after reading the request —
/// legitimate for a script that pipelines calls ahead of reading their
/// answers, exactly as the framing doc at the top of `host_call.rs` blesses —
/// and then keeps asking. The manifest declares `recall` and a gate is
/// attached and answers the first ask or two normally; only once the host's
/// write back to the child's closed stdin has actually failed and the
/// internal writer task has exited does a further ask find no way to be
/// answered.
///
/// Before the fix this returned `WrapperError::UnannouncedCall` — the
/// manifest/no-gate error — even though the manifest plainly declares
/// `[loop] calls = ["recall"]` and `gate` is plainly attached, which is an
/// operator being told to fix a config that was never broken while the real
/// transport fault (a broken pipe) is silently dropped by `writer.abort()`
/// never being joined. This asserts the distinct, correctly-named error and
/// that it carries the real I/O error rather than nothing.
#[tokio::test]
async fn a_dead_writer_mid_conversation_is_not_reported_as_a_missing_gate() {
    let manifest = manifest(RECALLING_MANIFEST);
    let generous = LoopGrant {
        max_calls: Some(8),
        ..manifest.loop_grant.clone()
    };
    let gate = gate(generous, DEFAULT_HOST_MAX_CALLS);

    // Closing stdin immediately after the request is read guarantees the
    // host's very first answer write lands on an already-closed pipe. What it
    // does *not* guarantee is when the writer task notices: the host has to
    // answer one ask, have that write fail, and have the failed task drop its
    // receiver before a later ask's guard is checked. A fixed pause between a
    // fixed number of asks bets on that happening inside the window — a bet
    // this test lost on a loaded machine, reporting the child's exit instead
    // of the fault. So the plugin keeps asking, with no read in between
    // (pipelining ahead rather than a round trip), until the host stops the
    // conversation.
    //
    // Measured, on an idle machine: two asks fail every time and three pass —
    // the fault is reported on the ask that finds the writer already gone, so
    // there has to *be* one after that point. Any fixed count is therefore a
    // bet that the writer's death propagates within it, and any `sleep` is the
    // same bet in a larger denomination: a loaded machine can still run the
    // list to its end first, and the child's exit is then reported instead of
    // the fault (#3632). Unbounded, the supply outlasts the writer's
    // scheduling by construction, and the child cannot exit early:
    // once the host stops reading, the next `printf` blocks on a full pipe and
    // the child is reaped by `kill_on_drop` and the group-kill guard on the
    // error path. The point timeout below is the backstop if the host ever
    // stops ending the conversation at all.
    let script = r#"
read -r request
exec 0<&-
while :; do
  printf '%s\n' '{"call":"recall","id":1,"args":{"goal":"ask"}}'
done
"#;

    let error = plugin(script, Arc::clone(&gate), Duration::from_secs(30))
        .before_turn(before())
        .await
        .expect_err("a channel that died mid-conversation cannot answer a further ask");

    let WrapperError::AnswerChannelFailed { call, source, .. } = &error else {
        panic!(
            "a dead writer must be reported as its own fault, not the manifest/gate error: \
             {error}"
        );
    };
    assert_eq!(*call, HostCall::Recall);
    // The real cause reached the caller instead of being thrown away by an
    // unjoined `writer.abort()`.
    assert_eq!(
        source.kind(),
        std::io::ErrorKind::BrokenPipe,
        "the recovered error names the actual transport fault: {source}"
    );
}
