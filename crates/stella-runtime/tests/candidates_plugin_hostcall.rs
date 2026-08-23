//! `plugins/stella-candidates` — best-of-N, graded through a scripted host
//! conversation (#4029 P3.1, `doc:plugin-completion-plan` §4.2).
//!
//! # What this file is for
//!
//! `candidate_fanout`, `run_test` and `adopt_candidate` landed with #3844 and
//! **nothing consumed them** — three capabilities on the wire with no plugin
//! asking for any. This plugin is the consumer, and it is the only thing in the
//! tree that reaches the `again?` point through a real fan-out.
//!
//! A host call is not a one-request-one-response exchange, so this drives the
//! plugin line by line rather than through `SubprocessWrapper` — the same shape
//! `goal_plugin_hostcall.rs` and `plan_plugin_hostcall.rs` use, with the script
//! inline because these cases are about *sequences of calls* rather than about
//! golden bytes.
//!
//! # What it can and cannot prove today
//!
//! §4.2's witness asks for "two candidates, one of which fails its test
//! command; the plugin adopts the other", then the anti-vacuity half: "both
//! pass, and the smaller diff wins".
//!
//! The first half **cannot pass**: `run_test` is answered
//! `HostCallRefusal::Unsupported` by every host (#3580), so there is no test
//! signal to fail on. What is graded here instead is the half that can be
//! proven — the smaller diff wins, an unfinished candidate is never adopted —
//! plus the thing that matters more while #3580 is open: that the plugin
//! **asks anyway** and degrades honestly, reporting
//! `test-signal-available = 0` rather than ranking silently.
//!
//! When #3580 lands, the missing half becomes writable — a candidate answering
//! `passed: false` losing to one answering `passed: true` — against
//! `main.py::choose`'s existing rule, with no change to the plugin. Until then
//! that test would only be asserting against a host nobody has built, so it is
//! deliberately absent rather than written against a fixture.
//!
//! `cfg(unix)` for `wrapper_socket.rs`'s reason (#3497).

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde_json::{Value, json};
use stella_plugin::{HostCall, Participation, PluginManifest, WrapperPoint};

fn plugin_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/stella-candidates")
        .canonicalize()
        .expect("the first-party plugin ships at plugins/stella-candidates")
}

fn manifest() -> PluginManifest {
    let text = fs::read_to_string(plugin_dir().join("plugin.toml"))
        .expect("the manifest file is `plugin.toml`, exactly");
    PluginManifest::from_toml_str(&text).expect("the shipped manifest loads")
}

fn argv(manifest: &PluginManifest) -> Vec<String> {
    let dir = plugin_dir().display().to_string();
    manifest
        .runtime
        .as_ref()
        .expect("[runtime]")
        .argv
        .iter()
        .map(|arg| stella_plugin::expand_plugin_dir(arg, Path::new(&dir)))
        .collect()
}

fn child_env(manifest: &PluginManifest) -> Vec<(String, String)> {
    manifest
        .runtime
        .as_ref()
        .expect("[runtime]")
        .child_env(|name| std::env::var(name).ok())
}

/// An `after_turn` request with a candidate grant and no test plan.
fn request(goal: &str) -> String {
    json!({
        "point": "after_turn",
        "body": {
            "protocol_version": 1,
            "wrapper": "candidates-v1",
            "round": 0,
            "goal": goal,
            "turn": { "completed": true, "answer": "a first attempt" }
        }
    })
    .to_string()
}

/// One fan-out candidate, as the host's answer spells it.
fn candidate(handle: &str, completed: bool, lines: u32) -> Value {
    json!({
        "candidate": handle,
        "root": format!("/tmp/{handle}"),
        "report": format!("{handle} did the work"),
        "completed": completed,
        "files_changed": 1,
        "lines_changed": lines
    })
}

fn ok(id: u32, payload: Value) -> Value {
    json!({ "result": id, "ok": payload })
}

fn refused(id: u32, code: &str, detail: &str) -> Value {
    json!({ "result": id, "err": { "refusal": code, "detail": detail } })
}

/// What one whole conversation produced.
struct Conversation {
    /// The calls the plugin made, in order, as raw messages.
    calls: Vec<Value>,
    /// The `evidence` object it finished with, when it finished.
    evidence: Option<Value>,
    stderr: String,
    succeeded: bool,
}

impl Conversation {
    fn measurement(&self, name: &str) -> Option<u64> {
        self.evidence
            .as_ref()?
            .get("measurements")?
            .get(name)?
            .as_u64()
    }

    /// The `args` of the one call made for `call`, when exactly one was.
    fn call_args(&self, call: &str) -> Option<&Value> {
        let mut found = self
            .calls
            .iter()
            .filter(|message| message.get("call").and_then(Value::as_str) == Some(call));
        let first = found.next()?;
        first.get("args")
    }

    fn calls_named(&self, call: &str) -> usize {
        self.calls
            .iter()
            .filter(|message| message.get("call").and_then(Value::as_str) == Some(call))
            .count()
    }
}

/// Play the host's side of one conversation.
///
/// `answers` is consulted in order; a call the script has no answer for closes
/// the pipe, which is a host that died mid-conversation and is itself a case
/// worth being able to write.
fn converse(request_text: &str, answers: &[Value]) -> Conversation {
    let manifest = manifest();
    let argv = argv(&manifest);
    let env = child_env(&manifest);
    let (program, args) = argv.split_first().expect("a program");

    let mut child: Child = Command::new(program)
        .args(args)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the plugin starts");

    let mut stdin = Some(child.stdin.take().expect("a stdin pipe"));
    let stdout = child.stdout.take().expect("a stdout pipe");

    stdin
        .as_mut()
        .expect("stdin")
        .write_all(format!("{request_text}\n").as_bytes())
        .expect("the request is written");

    let mut calls = Vec::new();
    let mut evidence = None;
    for line in BufReader::new(stdout).lines() {
        let line = line.expect("a readable line");
        if line.trim().is_empty() {
            continue;
        }
        let message: Value =
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("{line:?} is not JSON: {e}"));

        // The response ends the conversation; anything else is a host call.
        if message.get("point").is_some() {
            evidence = message
                .get("body")
                .and_then(|body| body.get("evidence"))
                .cloned();
            break;
        }

        let index = calls.len();
        calls.push(message);
        match answers.get(index) {
            Some(answer) => {
                let pipe = stdin.as_mut().expect("stdin");
                writeln!(pipe, "{answer}").expect("the answer is written");
                pipe.flush().expect("the answer reaches the plugin");
            }
            None => {
                drop(stdin.take());
            }
        }
    }
    drop(stdin.take());

    let mut stderr = String::new();
    BufReader::new(child.stderr.take().expect("a stderr pipe"))
        .read_to_string(&mut stderr)
        .expect("readable stderr");
    let succeeded = child.wait().expect("the plugin exits").success();

    Conversation {
        calls,
        evidence,
        stderr,
        succeeded,
    }
}

/// **The witness (§4.2's anti-vacuity half).** Three candidates all finish; the
/// one that changed the fewest lines is adopted and the rest are discarded.
///
/// This is the half of §4.2's witness that can be proven while #3580 is open.
/// It fails before this plugin exists for the plainest possible reason: nothing
/// in the tree asked for a `candidate_fanout` at all, so there was no consumer
/// of the capability #3844 built and no code path that reached
/// `adopt_candidate`.
#[test]
fn the_smallest_finished_candidate_wins_and_is_adopted() {
    let conversation = converse(
        &request("make the retry budget honoured"),
        &[
            // 1. the fan-out
            ok(
                1,
                json!({
                    "requested": 4,
                    "candidates": [
                        candidate("candidate-1", true, 120),
                        candidate("candidate-2", true, 18),
                        candidate("candidate-3", true, 64),
                    ]
                }),
            ),
            // 2-4. run_test, refused as every host does today
            refused(2, "unsupported", "this host does not re-run tests (#3580)"),
            refused(3, "unsupported", "this host does not re-run tests (#3580)"),
            refused(4, "unsupported", "this host does not re-run tests (#3580)"),
            // 5. the adoption
            ok(
                5,
                json!({ "adopted": "candidate-2", "discarded": ["candidate-1", "candidate-3"] }),
            ),
        ],
    );

    assert!(
        conversation.succeeded,
        "the plugin exited non-zero: {}",
        conversation.stderr
    );

    let adopted = conversation
        .call_args(CALL_ADOPT)
        .and_then(|args| args.get("candidate"))
        .and_then(Value::as_str);
    assert_eq!(
        adopted,
        Some("candidate-2"),
        "the smallest diff (18 lines) wins over 64 and 120; the plugin asked to adopt {adopted:?}"
    );

    assert_eq!(conversation.measurement("candidates-scored"), Some(3));
    assert_eq!(conversation.measurement("candidate-adopted"), Some(1));
    assert_eq!(
        conversation.measurement("winner-lines-changed"),
        Some(18),
        "the winner's size is reported, so a reader can see what was chosen and not only that \
         something was"
    );
}

/// **The degradation witness (#3580).** The plugin asks `run_test` for every
/// candidate even though every host refuses it, and reports the absence as a
/// number rather than ranking silently.
///
/// This is the half that matters most while #3580 is open, and it is what makes
/// the "ask, degrade, disclose" decision checkable rather than a claim in a
/// README. A plugin that had routed around the refusal — reading the grant's
/// `TestPlan` and running the tests itself — would make no `run_test` call at
/// all, and this test is what would catch that.
#[test]
fn run_test_is_asked_for_every_candidate_and_its_refusal_is_reported_as_a_number() {
    let conversation = converse(
        &request("anything"),
        &[
            ok(
                1,
                json!({
                    "requested": 4,
                    "candidates": [
                        candidate("candidate-1", true, 10),
                        candidate("candidate-2", true, 20),
                    ]
                }),
            ),
            refused(2, "unsupported", "this host does not re-run tests (#3580)"),
            refused(3, "unsupported", "this host does not re-run tests (#3580)"),
            ok(
                4,
                json!({ "adopted": "candidate-1", "discarded": ["candidate-2"] }),
            ),
        ],
    );

    assert_eq!(
        conversation.calls_named(CALL_RUN_TEST),
        2,
        "one ask per candidate — the designed path, taken even though this host refuses it"
    );
    assert_eq!(
        conversation.measurement("test-signal-available"),
        Some(0),
        "no host served run_test, and the plugin says so as a declared measurement rather than \
         leaving a reader to assume the ranking used tests"
    );
    assert!(
        conversation.stderr.contains("#3580"),
        "and names the issue on stderr for a human: {}",
        conversation.stderr
    );
    // It still chose, on the signals it does have.
    assert_eq!(conversation.measurement("candidate-adopted"), Some(1));
}

/// A candidate that did not finish is never adopted, even when it is the
/// smallest.
///
/// `completed = false` is an ordinary outcome — a carve ran out, a step cap hit
/// — and its workspace is still readable. Adopting it would land a half-change
/// on the real tree, which is worse than adopting nothing.
#[test]
fn an_unfinished_candidate_is_never_adopted_however_small_its_diff() {
    let conversation = converse(
        &request("anything"),
        &[
            ok(
                1,
                json!({
                    "requested": 4,
                    "candidates": [
                        candidate("candidate-1", false, 2),
                        candidate("candidate-2", true, 90),
                    ]
                }),
            ),
            refused(2, "unsupported", "no test signal"),
            refused(3, "unsupported", "no test signal"),
            ok(
                4,
                json!({ "adopted": "candidate-2", "discarded": ["candidate-1"] }),
            ),
        ],
    );

    let adopted = conversation
        .call_args(CALL_ADOPT)
        .and_then(|args| args.get("candidate"))
        .and_then(Value::as_str);
    assert_eq!(
        adopted,
        Some("candidate-2"),
        "the 2-line candidate never finished, so the 90-line one that did is the only choice"
    );
    assert_eq!(conversation.measurement("winner-lines-changed"), Some(90));
}

/// When no candidate finished, nothing is adopted — and the plugin says so with
/// numbers that let the declared check refuse.
#[test]
fn no_finished_candidate_means_nothing_is_adopted() {
    let conversation = converse(
        &request("anything"),
        &[
            ok(
                1,
                json!({
                    "requested": 4,
                    "candidates": [
                        candidate("candidate-1", false, 5),
                        candidate("candidate-2", false, 7),
                    ]
                }),
            ),
            refused(2, "unsupported", "no test signal"),
            refused(3, "unsupported", "no test signal"),
        ],
    );

    assert_eq!(
        conversation.calls_named(CALL_ADOPT),
        0,
        "there is nothing to adopt, so nothing is asked for"
    );
    assert_eq!(conversation.measurement("candidates-scored"), Some(2));
    assert_eq!(
        conversation.measurement("candidate-adopted"),
        Some(0),
        "reported as a 0 rather than omitted: the plugin ran, asked, and adopted nothing, which \
         is a claim the declared check can refuse on"
    );
}

/// A host that serves no fan-out plane leaves the plugin with nothing to choose
/// between, and it reports that rather than failing.
#[test]
fn a_host_with_no_fanout_plane_is_a_report_of_nothing_not_a_failure() {
    let conversation = converse(
        &request("anything"),
        &[refused(
            1,
            "unavailable",
            "this host holds no isolation substrate",
        )],
    );

    assert!(
        conversation.succeeded,
        "a refused capability is a degradation, not a plugin bug: {}",
        conversation.stderr
    );
    assert_eq!(conversation.measurement("candidates-scored"), Some(0));
    assert_eq!(conversation.measurement("candidate-adopted"), Some(0));
    assert_eq!(
        conversation.calls_named(CALL_ADOPT),
        0,
        "nothing was built, so nothing is adopted"
    );
}

/// The role intent every candidate runs at must resolve to the **worker's**
/// seat — the inverse of `child_turn`'s rule, and the one thing about this
/// capability a plugin author has to get right.
///
/// A child turn may not resolve to the worker's seat, because a plugin must not
/// grade work with the model that did it. A fan-out candidate is not evidence
/// about the work, it **is** the work, so booking it against `triage` would put
/// spend on the receipt under a responsibility that did no writing.
#[test]
fn the_shipped_manifest_declares_a_worker_tier_for_its_fanout_role() {
    let manifest = manifest();

    assert_eq!(manifest.name, "stella-candidates");
    assert_eq!(manifest.loop_grant.participation, Participation::Arbiter);
    assert_eq!(manifest.loop_grant.points, vec![WrapperPoint::AfterTurn]);

    for call in [
        HostCall::CandidateFanout,
        HostCall::RunTest,
        HostCall::AdoptCandidate,
    ] {
        assert!(
            manifest.loop_grant.calls.contains(&call),
            "the manifest grants {call}; got {:?}",
            manifest.loop_grant.calls
        );
    }

    let roles = manifest.roles.as_ref().expect("[roles]");
    let builder = roles.get("builder").expect("[roles.builder]");
    assert_eq!(
        builder.tier, "worker",
        "a fan-out candidate IS the work, so its role must resolve to the worker's seat"
    );

    let width = manifest
        .loop_grant
        .max_fanout_width
        .expect("[loop] max_fanout_width");
    assert!(width > 0, "a zero width bounds nothing");
}

const CALL_RUN_TEST: &str = "run_test";
const CALL_ADOPT: &str = "adopt_candidate";
