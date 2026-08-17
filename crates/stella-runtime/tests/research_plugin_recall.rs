//! The witness for the `recall` half of Track B's first extraction:
//! `plugins/stella-research` answers `StageName::Recall` with a contribution
//! built from frames it **asked the host for**, and with an honest empty one
//! whenever the host would not serve the ask.
//!
//! Failing before this landed by construction, not by accident: the plugin's
//! `contribute()` returned `[]` for every stage but `research`, because the
//! socket was one host-initiated request and one plugin response and nothing in
//! it could reach the context plane (#3540). `doc:wrapper-socket` §6b is the
//! correction — *a plugin may ask the host for a capability; it may never reach
//! for one* — and this file grades the plugin's end of that conversation.
//!
//! # Why this is a third harness rather than more vectors in the first
//!
//! `research_plugin_conformance.rs` sends its vectors through
//! [`SubprocessWrapper`](stella_runtime::wrapper::SubprocessWrapper), which is
//! exactly right for the exchange it grades: one request, one response. A host
//! call is not that shape — the plugin writes a question and reads its answer
//! *before* it answers the point — so the conversation is driven here, one line
//! at a time, and the two harnesses stay honest about which shape each proves.
//!
//! Every vector is three files, not two:
//!
//! | File | What it is |
//! | --- | --- |
//! | `NN-….request.json` | the `before_turn` request |
//! | `NN-….calls.json` | the conversation the host will hold: what it expects to be asked, and what it answers (`null` for "and then dies") |
//! | `NN-….expected.json` / `.refusal.txt` | the grading sibling, exactly as in `testdata/` |
//! | `NN-….stderr.txt` | *optional*: what a degraded call must have reported |
//!
//! # Both sides of the conversation are the host's own types
//!
//! This is the property the file exists for. The plugin's call is decoded as a
//! [`HostCallRequest`] and the vector's answer is re-encoded from a
//! [`HostCallResponse`], so the bytes the plugin reads are the bytes
//! `stella_plugin::host_call`'s own encoder produces, and a plugin that asked
//! with an argument the host cannot decode fails **here** rather than in the
//! field as a refusal nobody traced. The golden is decoded as a
//! [`WrapperResponse`] for the same reason its sibling harness does it.
//!
//! `cfg(unix)` for `wrapper_socket.rs`'s reason, tracked in the same place
//! (#3497): the child is spawned with a POSIX `PATH` and named `python3`.

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use stella_plugin::{
    HostCallRequest, HostCallResponse, PluginManifest, PluginMessage, WrapperResponse,
};
use stella_protocol::completion::MessageRole;

fn plugin_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/stella-research")
        .canonicalize()
        .expect("the first-party plugin ships in this repository at plugins/stella-research")
}

fn manifest() -> PluginManifest {
    let text = fs::read_to_string(plugin_dir().join("plugin.toml"))
        .expect("the manifest file is `plugin.toml`, exactly");
    PluginManifest::from_toml_str(&text).expect("the shipped manifest loads")
}

/// The program and arguments the host would spawn, with `${plugin_dir}`
/// interpolated the way the loader does it (`stella_cli::plugin_cmd::roster`).
fn argv(manifest: &PluginManifest) -> Vec<String> {
    let dir = plugin_dir().display().to_string();
    manifest
        .runtime
        .as_ref()
        .expect("a plugin the host spawns declares [runtime]")
        .argv
        .iter()
        .map(|arg| arg.replace("${plugin_dir}", &dir))
        .collect()
}

/// Exactly the environment the manifest's allowlist admits — default-deny, so
/// a plugin quietly reading an inherited variable fails here rather than on a
/// host that withheld it.
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
    /// the host's own encoder produces. Every ordinary vector is this one.
    Typed(Box<HostCallResponse>),
    /// Written verbatim, because the host's type **cannot express it**. That is
    /// not a loophole, it is the case being graded: `RecallFrame` denies
    /// unknown fields, so no shipped host can send a frame carrying one, and
    /// the only way to ask "what does this plugin do when it meets a host it
    /// has not met" is to be that host. A vector using this says so in its
    /// name.
    Raw(serde_json::Value),
    /// The pipe closes: a host that died mid-conversation, which must degrade
    /// the plugin rather than hang it.
    None,
}

/// What one whole conversation produced.
struct Conversation {
    /// The final `{"point": …}` document, when the plugin answered the point.
    response: Option<WrapperResponse>,
    stderr: String,
    succeeded: bool,
}

/// Read a vector's scripted conversation, decoding each half with the type the
/// host uses for it.
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

/// Play the host's side of one §6b conversation.
///
/// The loop is the whole protocol: write the request, then read the plugin's
/// messages one line at a time as [`PluginMessage`] — a call is answered from
/// the script, the point response ends it. A call the script did not expect
/// fails the test rather than being answered, because "the plugin asked for
/// something this vector did not grant" is the thing the grant exists to make
/// visible.
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

    // The request, one line, and stdin stays open: a host that can answer a
    // call is a host the plugin can still read from.
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
            // Closing the pipe is the answer: end of input, no result.
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

    // Read whole: the degradation report is the evidence that a refused call
    // was reported at all, not a detail. Safe to read after the conversation
    // because this plugin's stderr is one line, far inside a pipe buffer.
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

/// The request as one line, which is the framing a conversational host writes:
/// the vector is pretty-printed for a human and compacted for the wire.
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
/// ends at its golden contribution.
///
/// Both halves of the contract are in this one loop, which is the point: the
/// vector whose host serves `recall` comes back with the frames rendered into a
/// single volatile contribution, and the vectors whose host has no channel,
/// never declared the call, has no allowance left, tried and failed, recalled
/// nothing, or died mid-conversation all come back with the **same empty
/// response a host that never installed this plugin would have used**. A
/// fabricated recall would fail every one of the second group.
#[test]
fn every_recall_vector_ends_at_its_golden_contribution() {
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
        let golden: WrapperResponse =
            serde_json::from_str(&fs::read_to_string(&golden_path).expect("a readable golden"))
                .unwrap_or_else(|e| panic!("{name}'s golden is not a response: {e}"));
        assert_eq!(response, golden, "{name} did not answer with its golden");

        // Invariant 7, checked on the value rather than trusted. A recall
        // plugin contributes on every turn it runs, which is exactly the shape
        // that would wreck prompt-cache hits if it reached the byte-stable
        // prefix — and the one exit from a contribution is
        // `VolatileContext::into_message`, which builds a *user* message.
        let WrapperResponse::BeforeTurn(before) = response else {
            panic!("{name} answers a point this plugin does not declare");
        };
        for context in before.context {
            assert_eq!(context.into_message().role, MessageRole::User, "{name}");
        }

        // What a degraded call had to say for itself, when the vector grades
        // it: a refused or unanswered call is *reported*, never silent (§6b).
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
    assert!(graded >= 8, "only {graded} recall vectors ran");
}

/// The other line §6b draws, and the one that keeps this plugin honest about
/// what it reads: the **outcome** of a host call degrades (above), while the
/// **shape** of its answer refuses. A frame carrying a field no `RecallFrame`
/// declares is a host this plugin has not met, and rendering whatever it
/// recognised out of one would be the "quietly does nothing" failure the whole
/// wire is written against.
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
/// and always the script that drives it — the hygiene rule its sibling harness
/// holds, plus the third file this shape needs.
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

/// The manifest is where "may it ask?" is answered, and the plugin's process is
/// never consulted about it: `[loop] calls` is what a human consented to at
/// install and `LoopGrant::permits_call` is the filter the host applies.
///
/// Asserted here rather than left implicit because the vectors above cannot see
/// it — a scripted host answers whatever the vector says, so nothing in them
/// would notice a manifest that asked for `child_turn` too.
#[test]
fn the_shipped_manifest_asks_for_recall_and_nothing_else() {
    let manifest = manifest();
    assert_eq!(
        manifest.loop_grant.calls,
        vec![stella_plugin::HostCall::Recall],
        "a read-only grounding stage asks for the context plane and nothing else"
    );
    assert!(
        manifest
            .loop_grant
            .permits_call(stella_plugin::HostCall::Recall),
        "the declared grade must be high enough to make the call it declares"
    );
    assert_eq!(
        manifest.loop_grant.max_calls,
        Some(1),
        "one call per point is the ask this plugin actually makes; the number a \
         human consented to must be the number it spends"
    );
}
