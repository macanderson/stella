//! **The witness for the driver channel's transport** (#3634, #3599 B0):
//! a driver process is handed a session, asks the host for a capability, reads
//! the answer, and ends the session by saying what should happen next.
//!
//! Every test here fails before the change for the plainest possible reason:
//! **nothing carried a byte.** `stella_plugin::driver` declared what a driver
//! may write and `stella_runtime::wrapper::DriverCallGate` decided what it may
//! be told, and no code path connected the two — `DriveRequest` was never
//! written to a process and `DriverMessage` was never read from one.
//!
//! The drivers below are `sh` scripts, for the reason
//! `tests/wrapper_host_call.rs` runs its plugins as `sh`: a channel that needs
//! an SDK is a Rust API with extra steps (`doc:pipeline-as-plugins` §5). None of
//! them uses a JSON library. The self-driving loop this channel exists for is
//! eight markdown files and a shell script today (#3599), so "a shell program
//! can drive" is the requirement rather than a demonstration.
//!
//! `cfg(unix)` is the same declared gap the rest of this suite carries, tracked
//! in #3497: the fix is a portable in-tree fixture binary rather than a skipped
//! test.

#![cfg(unix)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use stella_plugin::{
    DriveNext, DriveRequest, DriverCall, DriverGrant, DriverOk, HostCallFailure, HostCallRefusal,
    PluginManifest,
};
use stella_runtime::wrapper::{
    DEFAULT_DRIVER_MAX_CALLS, DriverCallGate, DriverCapabilities, DriverError,
    NoDriverCapabilities, SubprocessDriver,
};

/// What a human consents to at install: this plugin drives, and while driving it
/// may ask for the backlog and may file into it — and nothing else. `[loop]
/// participation` stays `none`, which is the whole point of #3599 B0: a driver
/// is not on the in-turn ladder and does not need to be.
const DRIVING_MANIFEST: &str = r#"
name = "driving-plugin"
description = "drives Stella from outside a turn"

[loop]
participation = "none"

[driver]
calls = ["backlog_next", "backlog_file"]
max_calls = 4
"#;

/// A host that performs whatever the gate admits, so a test can isolate the
/// *transport's* behaviour from the capability's.
struct Serves;

#[async_trait]
impl DriverCapabilities for Serves {
    async fn perform(&self, _call: DriverCall) -> Result<DriverOk, HostCallFailure> {
        Ok(DriverOk {})
    }
}

fn manifest(text: &str) -> PluginManifest {
    PluginManifest::from_toml_str(text).expect("the manifest loads")
}

fn grant_of(text: &str) -> DriverGrant {
    manifest(text)
        .driver
        .expect("this manifest declares a [driver] block")
}

fn gate(
    grant: DriverGrant,
    ceiling: u32,
    capabilities: Box<dyn DriverCapabilities>,
) -> Arc<DriverCallGate> {
    Arc::new(DriverCallGate::declare(grant, ceiling, capabilities))
}

fn driver(script: &str, gate: Arc<DriverCallGate>, timeout: Duration) -> SubprocessDriver {
    SubprocessDriver::declare(
        vec!["/bin/sh".into(), "-c".into(), script.into()],
        Vec::new(),
        timeout,
    )
    .expect("the transport is declared with a program and a budget")
    .driver
    .serving(gate)
}

/// A driver with no gate at all — the host that attached none. Stdin closes
/// after the request, exactly as it does for a manifest declaring no calls.
fn ungated(script: &str, timeout: Duration) -> SubprocessDriver {
    SubprocessDriver::declare(
        vec!["/bin/sh".into(), "-c".into(), script.into()],
        Vec::new(),
        timeout,
    )
    .expect("the transport is declared with a program and a budget")
    .driver
}

/// **The witness.** A driver is opened, asks the host for a capability it
/// declared, reads the host's answer, and ends the session asking to be woken.
///
/// The script is the whole proof that this is a *wire* capability: it reads one
/// line, writes an ask, reads the answer, and writes a `next` — with `read`,
/// `case` and `printf`, and no JSON parser anywhere.
#[tokio::test]
async fn a_driver_holds_a_session_asks_for_a_capability_and_says_what_comes_next() {
    let gate = gate(
        grant_of(DRIVING_MANIFEST),
        DEFAULT_DRIVER_MAX_CALLS,
        Box::new(Serves),
    );

    // The session identity is read back out of the request, so the answer is
    // genuinely a function of what the host opened rather than a fixed string.
    let script = r#"
read -r request
case "$request" in
  *'"session":"cycle-17"'*) ;;
  *) printf 'the driver was not told which session it is in\n' >&2 ; exit 1 ;;
esac
printf '%s\n' '{"call":"backlog_next","id":1}'
read -r answer
case "$answer" in
  *'"result":1'*'"ok"'*) secs=900 ;;
  *) printf 'the host did not perform the declared capability: %s\n' "$answer" >&2 ; exit 1 ;;
esac
printf '{"point":"drive","body":{"next":{"sleep":{"secs":%s}}}}\n' "$secs"
"#;

    let response = driver(script, Arc::clone(&gate), Duration::from_secs(10))
        .drive(DriveRequest::new("cycle-17"))
        .await
        .expect("the driver was opened, served, and ended its own session");

    assert_eq!(
        response.next,
        DriveNext::Sleep { secs: 900 },
        "the driver's terminal answer reached the host verbatim"
    );
    assert!(
        gate.refusals().is_empty(),
        "a declared capability inside the allowance is performed, not refused: {:?}",
        gate.refusals()
    );
}

/// **The other half of the same gate, and the other terminal answer.** An
/// undeclared capability is refused with a code the driver branches on, *and the
/// session keeps running* — it goes on to halt, with a reason built out of the
/// refusal it read.
///
/// Either direction alone is half a gate: a channel that refuses everything is
/// also "correct" by the first test, and a channel that never refuses is
/// "correct" by nothing at all.
#[tokio::test]
async fn an_undeclared_capability_is_refused_to_the_driver_and_the_session_continues() {
    let grant = grant_of(DRIVING_MANIFEST);
    assert!(
        !grant.permits_call(DriverCall::DeliverMerge),
        "this manifest declares the backlog verbs and nothing else"
    );
    let gate = gate(grant, DEFAULT_DRIVER_MAX_CALLS, Box::new(Serves));

    let script = r#"
read -r request
printf '%s\n' '{"call":"deliver_merge","id":1}'
read -r refused
case "$refused" in
  *'"refusal":"undeclared"'*) ;;
  *) printf 'a capability this driver never declared was performed: %s\n' "$refused" >&2 ; exit 1 ;;
esac
printf '%s\n' '{"call":"backlog_file","id":2}'
read -r served
case "$served" in
  *'"result":2'*'"ok"'*) reason="merge was not granted; filed the finding and stopped" ;;
  *) reason="the session died on a refusal" ;;
esac
printf '{"point":"drive","body":{"next":{"halt":{"reason":"%s"}}}}\n' "$reason"
"#;

    let response = driver(script, Arc::clone(&gate), Duration::from_secs(10))
        .drive(DriveRequest::new("cycle-18"))
        .await
        .expect("a refused capability is a value the driver reads, never a death");

    assert_eq!(
        response.next,
        DriveNext::Halt {
            reason: "merge was not granted; filed the finding and stopped".to_string()
        },
        "the driver degraded to what it was permitted and said so"
    );

    let refusals = gate.refusals();
    assert_eq!(refusals.len(), 1, "the refusal is reported, never silent");
    assert_eq!(refusals[0].call, DriverCall::DeliverMerge);
    assert_eq!(refusals[0].refusal, HostCallRefusal::Undeclared);
}

/// The host that ships today performs no verb at all, and says so to the driver
/// rather than to a log nobody reads. A driver bound to it keeps running.
///
/// This is the answer a real `plugins/stella-selfdriving` gets until B1-B6 land,
/// so it is worth a test rather than a comment: the degradation path is the
/// shipping path.
#[tokio::test]
async fn the_shipping_host_performs_no_capability_yet_and_the_driver_is_told() {
    let gate = gate(
        grant_of(DRIVING_MANIFEST),
        DEFAULT_DRIVER_MAX_CALLS,
        Box::new(NoDriverCapabilities),
    );

    let script = r#"
read -r request
printf '%s\n' '{"call":"backlog_next","id":1}'
read -r answer
case "$answer" in
  *'"refusal":"unsupported"'*) reason="the host has no backlog yet" ;;
  *) reason="unexpected: $answer" ;;
esac
printf '{"point":"drive","body":{"next":{"halt":{"reason":"%s"}}}}\n' "$reason"
"#;

    let response = driver(script, Arc::clone(&gate), Duration::from_secs(10))
        .drive(DriveRequest::new("cycle-19"))
        .await
        .expect("an unsupported capability is answered, not fatal");
    assert_eq!(
        response.next,
        DriveNext::Halt {
            reason: "the host has no backlog yet".to_string()
        }
    );
    assert_eq!(gate.refusals()[0].refusal, HostCallRefusal::Unsupported);
}

/// The manifest's `max_calls` is an ask. This host's ceiling is one, whatever
/// the manifest wanted, and the ask past it is answered `allowance-spent` — so a
/// driver learns to stop asking rather than being killed for it.
#[tokio::test]
async fn the_session_allowance_is_the_hosts_and_a_spent_one_is_answered() {
    let gate = gate(grant_of(DRIVING_MANIFEST), 1, Box::new(Serves));
    assert_eq!(gate.max_calls(), 1, "the manifest's ask of 4 is clamped");

    let script = r#"
read -r request
printf '%s\n' '{"call":"backlog_next","id":1}'
read -r first
printf '%s\n' '{"call":"backlog_next","id":2}'
read -r second
case "$second" in
  *'"refusal":"allowance-spent"'*) reason="one ask per session here" ;;
  *) reason="the ceiling did not hold" ;;
esac
printf '{"point":"drive","body":{"next":{"halt":{"reason":"%s"}}}}\n' "$reason"
"#;

    let response = driver(script, Arc::clone(&gate), Duration::from_secs(10))
        .drive(DriveRequest::new("cycle-20"))
        .await
        .expect("a spent allowance is answered, not fatal");
    assert_eq!(
        response.next,
        DriveNext::Halt {
            reason: "one ask per session here".to_string()
        }
    );
    assert_eq!(
        gate.refusals()[0].refusal,
        HostCallRefusal::AllowanceSpent,
        "and the host was told which ask it turned down"
    );
}

/// A driver that asks for nothing is served by a host that holds its stdin open
/// for nothing: the request is written, stdin closes, and the driver reads it
/// with `cat` the way a program with no conversation would.
///
/// The bound this proves is the one that would otherwise hang: a driver reading
/// to EOF against a pipe held open for a conversation it can never have waits
/// until the deadline. The elapsed-time assertion is what makes that
/// falsifiable — without it, "it answered" and "it answered after the budget"
/// look identical.
#[tokio::test]
async fn a_driver_that_declares_no_capability_gets_its_eof_and_is_not_left_hanging() {
    let gate = gate(
        DriverGrant::default(),
        DEFAULT_DRIVER_MAX_CALLS,
        Box::new(Serves),
    );
    assert!(
        !gate.offers_calls(),
        "an empty [driver] calls list can never be served"
    );

    let script = r#"
request=$(cat)
case "$request" in
  *'"point":"drive"'*) reason="read the whole session request to EOF" ;;
  *) reason="unexpected request" ;;
esac
printf '{"point":"drive","body":{"next":{"halt":{"reason":"%s"}}}}\n' "$reason"
"#;

    // Served by the gate, not by `ungated`: the transport's own
    // `offers_calls()` consultation is the thing under test, and a driver with
    // no gate at all would reach the same EOF for a different reason.
    let budget = Duration::from_secs(10);
    let started = Instant::now();
    let response = driver(script, Arc::clone(&gate), budget)
        .drive(DriveRequest::new("cycle-21"))
        .await
        .expect("a driver that asks for nothing still ends its session");
    assert_eq!(
        response.next,
        DriveNext::Halt {
            reason: "read the whole session request to EOF".to_string()
        }
    );
    assert!(
        started.elapsed() < budget / 2,
        "the driver got its EOF and answered, rather than waiting out the budget: {:?}",
        started.elapsed()
    );
}

/// A driver that asks when no channel was ever opened is told so, rather than
/// left to hang until the deadline. Two causes, and the message names both: its
/// manifest declares no `[driver] calls`, or this host attached no gate.
#[tokio::test]
async fn an_ask_with_no_channel_open_is_reported_rather_than_waited_out() {
    let script = r#"
read -r request
printf '%s\n' '{"call":"backlog_next","id":1}'
read -r answer
printf '{"point":"drive","body":{"next":{"halt":{"reason":"unreachable"}}}}\n'
"#;

    let budget = Duration::from_secs(10);
    let started = Instant::now();
    let failed = ungated(script, budget)
        .drive(DriveRequest::new("cycle-22"))
        .await
        .expect_err("an unanswerable ask is a failure, not a silence");

    match &failed {
        DriverError::UnannouncedCall { call, .. } => {
            assert_eq!(*call, DriverCall::BacklogNext);
            assert!(
                failed.to_string().contains("[driver] calls"),
                "the message names the manifest half of the fix: {failed}"
            );
        }
        other => panic!("the failure is named, not collapsed into another one: {other}"),
    }
    assert!(
        started.elapsed() < budget / 2,
        "reported immediately rather than at the deadline: {:?}",
        started.elapsed()
    );
}

/// A driver that exits without a `next` has decided nothing, and the transport
/// refuses to invent one.
///
/// The failure this prevents is the expensive one: a host that read silence as
/// `halt` would retire a loop on a crash, and one that read it as `sleep` would
/// restart a broken driver forever.
#[tokio::test]
async fn a_driver_that_ends_without_a_next_is_a_failure_rather_than_a_halt() {
    let script = r#"
cat >/dev/null
printf 'the driver crashed before deciding\n' >&2
"#;

    let failed = ungated(script, Duration::from_secs(10))
        .drive(DriveRequest::new("cycle-23"))
        .await
        .expect_err("silence is not a decision");

    match &failed {
        DriverError::NoResponse { stderr, .. } => {
            assert!(
                stderr.contains("crashed before deciding"),
                "the driver's own words reach the host: {stderr}"
            );
        }
        other => panic!("the failure is named, not collapsed into another one: {other}"),
    }
}

/// A driver that exits non-zero is an `Exit` carrying its status and its stderr,
/// not a decode failure about the answer it never wrote.
#[tokio::test]
async fn a_driver_that_dies_loudly_is_reported_with_its_status_and_stderr() {
    let script = r#"
cat >/dev/null
printf 'no issue provider configured\n' >&2
exit 3
"#;

    let failed = ungated(script, Duration::from_secs(10))
        .drive(DriveRequest::new("cycle-24"))
        .await
        .expect_err("a non-zero exit is a failure");

    assert!(
        matches!(&failed, DriverError::Exit { status, stderr, .. }
            if status == "code 3" && stderr.contains("no issue provider configured")),
        "got {failed:?}"
    );
}

/// **A driver cannot buy time by talking.** The budget covers the whole session,
/// not each message of it: this driver asks forever, is answered every time
/// inside its allowance and refused after it, and is still killed at the same
/// deadline a silent driver would be.
#[tokio::test]
async fn the_session_cannot_outlive_its_budget_by_asking() {
    let gate = gate(
        grant_of(DRIVING_MANIFEST),
        DEFAULT_DRIVER_MAX_CALLS,
        Box::new(Serves),
    );
    let budget = Duration::from_millis(700);

    // Never writes a `next`. Every iteration is a legitimate, declared ask.
    let script = r#"
read -r request
while :; do
  printf '%s\n' '{"call":"backlog_next","id":1}'
  read -r answer || exit 0
done
"#;

    let started = Instant::now();
    let failed = driver(script, Arc::clone(&gate), budget)
        .drive(DriveRequest::new("cycle-25"))
        .await
        .expect_err("a session that never ends is killed at its budget");

    assert!(
        matches!(&failed, DriverError::Timeout { timeout, .. } if *timeout == budget),
        "got {failed:?}"
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed >= budget,
        "killed no earlier than the budget it was given: {elapsed:?}"
    );
    assert!(
        elapsed < budget * 8,
        "and killed at the budget rather than whenever the driver felt like stopping: {elapsed:?}"
    );
}

/// The transport names what it could not start, rather than reporting the
/// silence that follows. An installation problem the user fixes is a different
/// answer from a driver that ran and failed, which is why the two are separate
/// variants.
#[tokio::test]
async fn a_driver_that_cannot_be_started_names_the_program() {
    let program = "/nonexistent/driver-that-is-not-installed";
    let failed =
        SubprocessDriver::declare(vec![program.into()], Vec::new(), Duration::from_secs(1))
            .expect("declaring it is fine; starting it is not")
            .driver
            .drive(DriveRequest::new("cycle-26"))
            .await
            .expect_err("a missing program is a failure the user must fix");

    match &failed {
        DriverError::Spawn { program: named, .. } => assert_eq!(named, program),
        other => panic!("the failure is named, not collapsed into another one: {other}"),
    }
}

/// **The child is the oracle.** A driver does not get the credential that pays
/// for the agent, and it does not inherit one from the host process either —
/// asked of the spawned process itself, not of the constructor.
///
/// Inspecting `AdmittedDriver::refused` proves only that the constructor agrees
/// with itself, which is the trap `tests/wrapper_env_refusal.rs` names in so
/// many words. It matters more here than the symmetry suggests: `declare`'s
/// filter runs over the pairs the *host* passed, and those never held the host
/// process's own environment. `Command::env_clear` is the entire mechanism that
/// stops a driver inheriting `ANTHROPIC_API_KEY` from the shell that started
/// Stella, and nothing but a child reading its own environment can see it.
///
/// A driver is the plugin with the most reason to want a model key — it exists
/// to get work done — and invariant 3's "every model call is made by the host"
/// is what `work_start` will be for.
#[tokio::test]
async fn a_driver_is_denied_the_model_credential_and_inherits_none_from_the_host() {
    // Set in *this* process, and never passed to `declare`. A child that reports
    // seeing it is a child that inherited it.
    //
    // SAFETY: `set_var` is unsound only against concurrent readers of the
    // environment in another thread. Nothing in this crate reads the process
    // environment at all — `tests/no_ambient_reads.rs` enforces exactly that —
    // and this test's own reader is a separate process reading its own copy,
    // taken at `spawn`.
    unsafe {
        std::env::set_var("STELLA_DRIVER_INHERITANCE_PROBE", "leaked");
    }

    // `${NAME:+1}0` is `10` when the child can see `NAME` and `0` when it
    // cannot — the same probe `tests/wrapper_env_refusal.rs` uses, and it
    // reports the *absence* of a variable without ever putting its value on
    // the wire.
    let probe = r#"
read -r request
printf '{"point":"drive","body":{"next":{"halt":{"reason":"key=%s inherited=%s ordinary=%s"}}}}\n' \
  "${ANTHROPIC_API_KEY:+1}0" "${STELLA_DRIVER_INHERITANCE_PROBE:+1}0" "${STELLA_DRIVER_ROLE:+1}0"
"#;

    let admitted = SubprocessDriver::declare(
        vec!["/bin/sh".into(), "-c".into(), probe.into()],
        vec![
            // Declared by the manifest, and refused: a credential.
            ("ANTHROPIC_API_KEY".into(), "sk-must-not-arrive".into()),
            // Declared by the manifest, and admitted: an ordinary name.
            ("STELLA_DRIVER_ROLE".into(), "selfdriving".into()),
        ],
        Duration::from_secs(10),
    )
    .expect("the transport is declared with a program and a budget");
    assert_eq!(
        admitted.refused,
        vec!["ANTHROPIC_API_KEY".to_string()],
        "the refusal is reported to the host as well as enforced on the child"
    );

    let response = admitted
        .driver
        .drive(DriveRequest::new("cycle-27"))
        .await
        .expect("a driver denied a credential still runs");

    assert_eq!(
        response.next,
        DriveNext::Halt {
            reason: "key=0 inherited=0 ordinary=10".to_string()
        },
        "the child saw the name its manifest declared, saw neither the refused \
         credential nor anything inherited from this process"
    );
}

/// A driver that keeps writing to stdout after it has already ended its session
/// does not wedge the session.
///
/// `converse` stops reading stdout the instant it finds the `next` — there is
/// nothing left in the wire contract to parse — so anything written after that
/// sits in an OS pipe (about 64 KiB on Linux) nobody is draining. Without
/// `settle` draining stdout too, such a child blocks in `write(2)`, never
/// reaches `exit(2)`, and `child.wait()` hangs until the session budget fires —
/// turning a driver that decided correctly into a `Timeout`, which a loop would
/// read as a driver to retire or restart rather than one to wake on schedule.
///
/// The script writes three pipe buffers past its answer, which reproduces the
/// deadlock without going anywhere near the capture ceiling. The elapsed bound
/// is what tells "drained" from "hung until the budget": the two are otherwise
/// indistinguishable from the answer alone.
#[tokio::test]
async fn a_driver_that_talks_after_deciding_does_not_wedge_the_session() {
    let budget = Duration::from_secs(8);
    let trailing_past_one_pipe_buffer = 3 * 64 * 1024;
    let script = format!(
        "read -r request\n\
         printf '%s\\n' '{{\"point\":\"drive\",\"body\":{{\"next\":{{\"sleep\":{{\"secs\":60}}}}}}}}'\n\
         yes stella | tr -d '\\n' | head -c {trailing_past_one_pipe_buffer}\n",
    );

    let started = Instant::now();
    let response = ungated(&script, budget)
        .drive(DriveRequest::new("cycle-28"))
        .await
        .expect(
            "a driver that decided correctly and then kept talking is not lost to a pipe deadlock",
        );
    let elapsed = started.elapsed();

    assert_eq!(
        response.next,
        DriveNext::Sleep { secs: 60 },
        "the decision itself is unaffected by what came after it"
    );
    assert!(
        elapsed < budget / 2,
        "the trailing output was drained rather than waited out: {elapsed:?}"
    );
}
