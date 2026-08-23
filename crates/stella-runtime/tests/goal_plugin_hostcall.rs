//! The witness for the `child_turn` half of `plugins/stella-goal`: it answers
//! `after_turn` with evidence built from a real verifier-role turn it
//! **asked the host for**, and with an honest empty one whenever the host
//! would not serve the ask.
//!
//! Mirrors `plan_plugin_hostcall.rs` exactly in shape — see that file's
//! module doc for why a host call is driven line-by-line here rather than
//! through [`SubprocessWrapper`](stella_runtime::wrapper::SubprocessWrapper):
//! the plugin writes a question and reads its answer *before* it answers the
//! point, which is not the one-request-one-response shape that transport
//! grades.
//!
//! Every vector is three files, not two:
//!
//! | File | What it is |
//! | --- | --- |
//! | `NN-….request.json` | the `after_turn` request |
//! | `NN-….calls.json` | the conversation the host will hold: what it expects to be asked, and what it answers (`null` for "and then dies") |
//! | `NN-….expected.json` / `.refusal.txt` | the grading sibling |
//! | `NN-….stderr.txt` | *optional*: what a degraded call must have reported |
//!
//! Both sides of the conversation are the host's own types: the plugin's call
//! is decoded as a [`HostCallRequest`] and the vector's answer is re-encoded
//! from a [`HostCallResponse`], so a plugin that asked with an argument the
//! host cannot decode fails **here** rather than in the field.
//!
//! Goldens are regenerated with `BLESS=1 cargo test -p stella-runtime --test
//! goal_plugin_hostcall`, through the same [`common::bless_or_assert`] the
//! three conformance harnesses use (#3548, #4437). This harness drives its
//! vectors through [`converse`] rather than `SubprocessWrapper`, so what it
//! shares is the comparison and not the loop — and, as everywhere else,
//! **read the diff**: a golden blessed without looking is a changelog, not a
//! test.
//!
//! `cfg(unix)` for `wrapper_socket.rs`'s reason, tracked in the same place
//! (#3497): the child is spawned with a POSIX `PATH` and named `python3`.

#![cfg(unix)]

mod common;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use stella_plugin::{
    HostCallRequest, HostCallResponse, PluginManifest, PluginMessage, WrapperResponse,
};

fn plugin_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/stella-goal")
        .canonicalize()
        .expect("the first-party plugin ships in this repository at plugins/stella-goal")
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
        .expect("a plugin the host spawns declares [runtime]")
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

/// One turn of the scripted conversation: what the host expects to be asked,
/// and what it says back.
struct Exchange {
    expect: HostCallRequest,
    answer: Answer,
}

/// What a scripted host says to one call.
enum Answer {
    /// Encoded from [`HostCallResponse`], so the plugin reads the exact bytes
    /// the host's own encoder produces.
    Typed(Box<HostCallResponse>),
    /// Written verbatim, for a shape the host's own type cannot express.
    Raw(serde_json::Value),
    /// The pipe closes: a host that died mid-conversation.
    None,
}

/// What one whole conversation produced.
struct Conversation {
    response: Option<WrapperResponse>,
    stderr: String,
    succeeded: bool,
}

fn script(path: &Path) -> Vec<Exchange> {
    let text = fs::read_to_string(path).expect("a readable call script");
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(&text).expect("a call script is an array of exchanges");
    entries
        .into_iter()
        .map(|mut entry| {
            let object = entry.as_object_mut().expect("an exchange is an object");
            let expect = object
                .remove("expect")
                .expect("an exchange names the call it expects");
            let typed = object.remove("answer");
            let raw = object.remove("answer_raw");
            assert!(
                object.is_empty(),
                "an exchange carries `expect` and one answer and nothing else"
            );
            let answer = match (typed, raw) {
                (Some(serde_json::Value::Null), None) => Answer::None,
                (Some(typed), None) => Answer::Typed(Box::new(
                    serde_json::from_value(typed).expect("a scripted answer is a host-call answer"),
                )),
                (None, Some(raw)) => Answer::Raw(raw),
                _ => panic!(
                    "an exchange answers with exactly one of `answer` (typed, `null` for a host \
                     that dies) and `answer_raw` (verbatim, for a shape no host can encode)"
                ),
            };
            Exchange {
                expect: serde_json::from_value(expect).expect("a scripted call is a host call"),
                answer,
            }
        })
        .collect()
}

/// Play the host's side of one §6b conversation. Identical loop to
/// `plan_plugin_hostcall.rs::converse`.
fn converse(
    argv: &[String],
    env: &[(String, String)],
    request: &str,
    script: &[Exchange],
) -> Conversation {
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
    let lines = BufReader::new(stdout).lines();

    stdin
        .as_mut()
        .expect("stdin")
        .write_all(format!("{request}\n").as_bytes())
        .expect("the request is written");

    let mut response = None;
    let mut asked = 0usize;
    for line in lines {
        let line = line.expect("a readable line");
        if line.trim().is_empty() {
            continue;
        }
        let message: PluginMessage = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("{line:?} is not a plugin message: {e}"));
        let call = match message {
            PluginMessage::Response(answered) => {
                response = Some(answered);
                break;
            }
            PluginMessage::Call(call) => call,
        };
        let exchange = script
            .get(asked)
            .unwrap_or_else(|| panic!("the plugin made an unscripted host call: {line}"));
        assert_eq!(
            call,
            exchange.expect,
            "the plugin's call {} is not the one this vector scripts",
            asked + 1
        );
        asked += 1;
        let encoded = match &exchange.answer {
            Answer::Typed(answer) => serde_json::to_string(answer).expect("the answer encodes"),
            Answer::Raw(answer) => answer.to_string(),
            Answer::None => {
                drop(stdin.take());
                continue;
            }
        };
        let pipe = stdin.as_mut().expect("stdin");
        writeln!(pipe, "{encoded}").expect("the answer is written");
        pipe.flush().expect("the answer reaches the plugin");
    }
    drop(stdin.take());
    assert_eq!(
        asked,
        script.len(),
        "the plugin made {asked} of the {} host calls this vector scripts",
        script.len()
    );

    let mut stderr = String::new();
    BufReader::new(child.stderr.take().expect("a stderr pipe"))
        .read_to_string(&mut stderr)
        .expect("readable stderr");
    let status = child.wait().expect("the plugin exits");
    Conversation {
        response,
        stderr,
        succeeded: status.success(),
    }
}

fn vectors() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(plugin_dir().join("testdata/hostcall"))
        .expect("the host-call vectors ship beside the plugin")
        .map(|entry| entry.expect("a readable vector").path())
        .filter(|path| path.to_string_lossy().ends_with(".request.json"))
        .collect();
    found.sort();
    assert!(!found.is_empty(), "the vector directory must not be empty");
    found
}

fn sibling(request: &Path, suffix: &str) -> PathBuf {
    let name = request
        .file_name()
        .expect("a vector has a file name")
        .to_string_lossy()
        .replace(".request.json", suffix);
    request.with_file_name(name)
}

fn request_line(path: &Path) -> String {
    let text = fs::read_to_string(path).expect("a readable vector");
    let request: stella_plugin::WrapperRequest = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{} is not a request: {e}", path.display()));
    serde_json::to_string(&request).expect("a request encodes")
}

fn name_of(vector: &Path) -> String {
    vector
        .file_name()
        .expect("a name")
        .to_string_lossy()
        .to_string()
}

/// **The witness.** Every response vector holds its scripted conversation and
/// ends at its golden evidence — the vector whose host serves `child_turn`
/// and completes comes back with the parsed verdict rendered as a `met`
/// measurement, and every vector whose host has no channel, resolved the role
/// to an unserved tier, has no allowance left, tried and failed, produced an
/// unparseable report, returned an incomplete turn, or died mid-conversation
/// all come back with the **same empty evidence a host that never installed
/// this plugin would have used** — no fabricated `met` value on any of them.
#[test]
fn every_hostcall_vector_ends_at_its_golden_evidence() {
    let manifest = manifest();
    let argv = argv(&manifest);
    let env = child_env(&manifest);

    let mut graded = 0;
    for vector in vectors() {
        let golden_path = sibling(&vector, ".expected.json");
        if !golden_path.exists() {
            continue;
        }
        let name = name_of(&vector);
        let script = script(&sibling(&vector, ".calls.json"));
        let held = converse(&argv, &env, &request_line(&vector), &script);

        assert!(
            held.succeeded,
            "{name}: the plugin answered but exited non-zero"
        );
        let response = held
            .response
            .unwrap_or_else(|| panic!("{name}: the plugin never answered the point"));
        // Nothing in a `child_turn` contribution is wall clock: the host's
        // scripted answer supplies every number, so `actual` is already
        // deterministic and there is nothing to pin.
        common::bless_or_assert(&name, &golden_path, &response, |_| {});

        let WrapperResponse::AfterTurn(_after) = response else {
            panic!("{name} answers a point this vector set does not exercise");
        };

        let reported = sibling(&vector, ".stderr.txt");
        if reported.exists() {
            assert_eq!(
                held.stderr.trim(),
                fs::read_to_string(&reported)
                    .expect("a readable report")
                    .trim(),
                "{name}: the degradation report changed"
            );
        } else {
            assert!(
                held.stderr.trim().is_empty(),
                "{name}: an ungraded vector reported {:?}",
                held.stderr
            );
        }
        graded += 1;
    }
    assert!(graded >= 6, "only {graded} host-call vectors ran");
}

/// The other line §6b draws: the **outcome** of a host call degrades (above),
/// while the **shape** of its answer refuses. An `ok` payload missing a
/// required `ChildTurnResult` field is a host this plugin has not met.
#[test]
fn a_misshapen_answer_refuses_with_the_reason_it_names() {
    let manifest = manifest();
    let argv = argv(&manifest);
    let env = child_env(&manifest);

    let mut graded = 0;
    for vector in vectors() {
        let refusal_path = sibling(&vector, ".refusal.txt");
        if !refusal_path.exists() {
            continue;
        }
        let name = name_of(&vector);
        let script = script(&sibling(&vector, ".calls.json"));
        let held = converse(&argv, &env, &request_line(&vector), &script);

        assert!(!held.succeeded, "{name}: a refusal must exit non-zero");
        assert!(
            held.response.is_none(),
            "{name}: a refusing plugin answers no point"
        );
        assert_eq!(
            held.stderr.trim(),
            fs::read_to_string(&refusal_path)
                .expect("a readable refusal")
                .trim(),
            "{name}: the refusal reason changed"
        );
        graded += 1;
    }
    assert!(graded >= 1, "no refusal vector ran");
}

/// A vector carries a golden **or** a refusal, never both and never neither,
/// and always the script that drives it.
#[test]
fn every_vector_is_graded_by_exactly_one_sibling_and_scripts_its_host() {
    for vector in vectors() {
        let golden = sibling(&vector, ".expected.json").exists();
        let refusal = sibling(&vector, ".refusal.txt").exists();
        assert!(
            golden ^ refusal,
            "{} must carry exactly one of .expected.json / .refusal.txt",
            vector.display()
        );
        assert!(
            sibling(&vector, ".calls.json").exists(),
            "{} must script the conversation its host holds, `[]` for none",
            vector.display()
        );
    }
}

/// The manifest is where "may it ask?" is answered, and the plugin's process
/// is never consulted about it: `[loop] calls` is what a human consented to
/// at install and `LoopGrant::permits_call` is the filter the host applies.
#[test]
fn the_shipped_manifest_asks_for_child_turn_and_nothing_else() {
    let manifest = manifest();
    assert_eq!(
        manifest.loop_grant.calls,
        vec![stella_plugin::HostCall::ChildTurn],
        "a goal-supervision plugin asks for one bounded model call and nothing else"
    );
    assert!(
        manifest
            .loop_grant
            .permits_call(stella_plugin::HostCall::ChildTurn),
        "the declared grade must be high enough to make the call it declares"
    );
    assert_eq!(
        manifest.loop_grant.max_calls,
        Some(1),
        "main.py asks once per point, and the per-point gate is what grades that"
    );
    assert_eq!(
        manifest.loop_grant.max_child_turns,
        Some(8),
        "the whole-run ChildTurns ceiling is its own key now (#3839)"
    );
}
