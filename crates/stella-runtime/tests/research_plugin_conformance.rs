//! The witness for Track B's first extraction: `plugins/stella-research`
//! answers a real `before_turn` request over the wire, and what comes back is
//! well-formed against the real `stella_plugin::wire` types.
//!
//! Failing before this landed for the plainest possible reason: the plugin did
//! not exist. `plugins/` did not exist either — `doc:pipeline-as-plugins` §7
//! puts stella-research first ("before_turn only, read-only, no worktree. The
//! safest possible first real plugin") and nothing had been extracted yet.
//!
//! # Graded the way Track C grades, not by hand-rolled assertions
//!
//! `macanderson/stella-examples`'s `plugins/ci/conformance.py` feeds the same
//! vectors to three programs in three languages and compares each answer
//! against the same golden. This is that harness with one thing added and one
//! thing changed:
//!
//! - **Added: the goldens are decoded by the host's own types.** A vector is
//!   parsed into a [`WrapperRequest`] before it is sent, and both the plugin's
//!   answer and the golden are compared as [`WrapperResponse`] values, so a
//!   response carrying a field the host does not know fails here rather than
//!   in a consumer's parser. `conformance.py` compares JSON to JSON, which
//!   cannot see that.
//! - **Changed: the transport is the host's.** Response vectors go through
//!   [`SubprocessWrapper`] — the same code `stella-cli` will dispatch with —
//!   built from the plugin's own `plugin.toml`, with `${plugin_dir}`
//!   interpolated exactly as `stella_cli::plugin_cmd::roster` does it. The
//!   refusal vectors cannot: a typed host *cannot* send an unknown field or a
//!   malformed body, which is the point of grading them. Those are spawned
//!   directly, which models the host this plugin will meet later — one at a
//!   protocol version it does not speak, or one with a field it does not know.
//!
//! # Why the fixture workspace is built here rather than committed
//!
//! Every request carries an absolute `candidate.root`, which no committed file
//! can hold, and the tree has to contain a `target/` directory to prove the
//! scan skips it — a path this repository's own `.gitignore` would drop. So
//! the harness materializes the tree in a `TempDir` and substitutes its path
//! into the vector's `${workspace_root}` before decoding. The goldens stay
//! machine-independent because every path the plugin reports is
//! workspace-relative.
//!
//! # What this file deliberately does not do
//!
//! It never constructs a [`WrapperDispatch`](stella_runtime::wrapper) and never
//! runs a turn. Grading the plugin against the *host sequence* that dispatches
//! it is `research_plugin_dispatch.rs`, kept separate so the wire contract has
//! a witness that stands on its own — this file compiles against the socket
//! alone, and that is the half a protocol change must never break silently.
//!
//! `cfg(unix)` for the reason `wrapper_socket.rs` states and tracked in the
//! same place (#3497): the child is spawned with a POSIX `PATH` and named
//! `python3`, so on Windows this file compiles to nothing.

#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use stella_plugin::{
    HostStage, Participation, PluginManifest, StageName, WrapperPoint, WrapperRequest,
    WrapperResponse,
};
use stella_protocol::completion::MessageRole;
use stella_runtime::wrapper::{SubprocessWrapper, TurnWrapper};
use tempfile::TempDir;

/// The token a vector writes where the fixture workspace's absolute root goes.
const WORKSPACE_TOKEN: &str = "${workspace_root}";
/// The same, for the second fixture — the one large enough to bind the scan's
/// file cap and the orientation listing's entry cap.
const BULK_TOKEN: &str = "${bulk_root}";
/// Files in the bulk fixture: one past `MAX_FILES_SCANNED` in `main.py`, which
/// is what makes the "the scan was bounded" contribution appear at all.
const BULK_FILES: usize = 2001;

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
/// interpolated the way the loader does it
/// (`stella_cli::plugin_cmd::roster`) — the plugin's own declaration, never a
/// path this test knows separately.
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
/// a plugin that quietly read an inherited variable would pass on a developer's
/// machine and fail on a host that withheld it.
fn child_env(manifest: &PluginManifest) -> Vec<(String, String)> {
    manifest
        .runtime
        .as_ref()
        .expect("[runtime]")
        .child_env(|name| std::env::var(name).ok())
}

fn transport(manifest: &PluginManifest) -> SubprocessWrapper {
    let timeout = Duration::from_secs(manifest.runtime.as_ref().expect("[runtime]").timeout_secs);
    SubprocessWrapper::declare(argv(manifest), child_env(manifest), timeout)
        .expect("the manifest declares a program and a budget")
        .wrapper
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("a fixture path has a parent"))
        .expect("fixture directory");
    fs::write(path, contents).expect("fixture file");
}

/// The workspace the vectors research.
///
/// Every file in it is load-bearing for some vector, and the two that must
/// **not** be reported are the point of three of them: a generated tree and a
/// hidden directory both mention `retry_budget`, and neither may appear in a
/// finding.
fn fixture() -> TempDir {
    let dir = TempDir::new().expect("a fixture workspace");
    let root = dir.path();
    write(
        root,
        "AGENTS.md",
        "The fixture workspace's orientation file.\n",
    );
    write(root, "README.md", "A fixture, not a project.\n");
    write(
        root,
        "src/retry.rs",
        "pub struct RetryBudget;\n// retry_budget is honoured here\n",
    );
    write(root, "src/scheduler.rs", "let b = retry_budget();\n");
    // Nine matches for one term: five are reported and the remaining four are
    // named, never dropped in silence.
    let many: String = (1..=9)
        .map(|n| format!("common_term line {n}\n"))
        .collect::<Vec<_>>()
        .concat();
    write(root, "src/many.rs", &many);
    // One matched line far past the per-line clamp.
    write(
        root,
        "src/wide.rs",
        &format!("wide_match {}\n", "x".repeat(900)),
    );
    // Both of these mention the term and neither may be reported.
    write(root, "target/generated.rs", "retry_budget, generated\n");
    write(root, ".hidden/notes.md", "retry_budget, hidden\n");
    dir
}

/// A tree one file past the scan's cap, whose top level is far past the
/// orientation listing's. Both bounds are then visible in one golden — which
/// is the property that matters: a bounded scan that reads as the whole story
/// turns "we did not look" into "it is not there".
fn bulk_fixture() -> TempDir {
    let dir = TempDir::new().expect("a bulk fixture workspace");
    for n in 0..BULK_FILES {
        write(dir.path(), &format!("f{n:04}.txt"), "nothing of interest\n");
    }
    dir
}

fn vectors() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(plugin_dir().join("testdata"))
        .expect("the plugin ships its vectors beside it")
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

fn request_bytes(request: &Path, workspace: &Path, bulk: &Path) -> String {
    fs::read_to_string(request)
        .expect("a readable vector")
        .replace(WORKSPACE_TOKEN, &workspace.display().to_string())
        .replace(BULK_TOKEN, &bulk.display().to_string())
}

/// **The witness.** Every response vector goes through the host's own
/// transport and comes back equal to its golden, decoded by the host's own
/// types.
#[tokio::test]
async fn every_response_vector_answers_with_its_golden_contribution() {
    let manifest = manifest();
    let wrapper = transport(&manifest);
    let workspace = fixture();
    let bulk = bulk_fixture();

    let mut graded = 0;
    for vector in vectors() {
        let golden_path = sibling(&vector, ".expected.json");
        if !golden_path.exists() {
            continue;
        }
        let name = vector
            .file_name()
            .expect("a name")
            .to_string_lossy()
            .to_string();
        let text = request_bytes(&vector, workspace.path(), bulk.path());

        // Decoded before it is sent: a vector that is not a well-formed
        // request is a bug in the fixture, and finding that out here rather
        // than from the plugin's refusal is the difference between grading the
        // plugin and grading the vector.
        let request: WrapperRequest =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name} is not a request: {e}"));
        let WrapperRequest::BeforeTurn(request) = request else {
            panic!("{name} addresses a point this plugin does not declare");
        };

        let response = wrapper
            .before_turn(request)
            .await
            .unwrap_or_else(|e| panic!("{name}: the plugin did not answer: {e}"));

        let golden: WrapperResponse =
            serde_json::from_str(&fs::read_to_string(&golden_path).expect("a readable golden"))
                .unwrap_or_else(|e| panic!("{name}'s golden is not a response: {e}"));
        assert_eq!(
            WrapperResponse::BeforeTurn(response.clone()),
            golden,
            "{name} did not answer with its golden contribution"
        );

        assert!(
            response.role.is_none() && response.publish.is_empty(),
            "{name}: research names no role intent and publishes no signal — \
             `StageName::Host(HostStage::Research).publishes()` is empty in the host, so there \
             is none it could honestly publish"
        );
        // Invariant 7, checked on the value rather than trusted. The one exit
        // from a contribution is `VolatileContext::into_message`, and what it
        // returns is a *user* message — one that rides after the byte-stable
        // system prefix, never inside it, so an installed plugin can never
        // cost a prompt-cache hit.
        for context in response.context {
            assert_eq!(context.into_message().role, MessageRole::User, "{name}");
        }
        graded += 1;
    }
    assert!(graded >= 8, "only {graded} response vectors ran");
}

/// The other half of the contract, and not an afterthought:
/// `BeforeTurnResponse` has no error variant, so a plugin that cannot answer
/// **fails** — non-zero exit, one line on stderr, nothing on stdout — and the
/// host runs the turn without the contribution.
///
/// Spawned directly rather than through [`SubprocessWrapper`], because the
/// host's typed transport cannot express any of these requests. That is the
/// case being graded: a host at a version this plugin does not speak, or one
/// sending a key it does not know, must be refused rather than half-answered.
#[test]
fn every_refusal_vector_refuses_with_the_reason_it_names() {
    let manifest = manifest();
    let argv = argv(&manifest);
    let workspace = fixture();
    let bulk = bulk_fixture();

    let mut graded = 0;
    for vector in vectors() {
        let refusal_path = sibling(&vector, ".refusal.txt");
        if !refusal_path.exists() {
            continue;
        }
        let name = vector
            .file_name()
            .expect("a name")
            .to_string_lossy()
            .to_string();
        let (program, args) = argv.split_first().expect("a program");
        let mut child = Command::new(program)
            .args(args)
            .env_clear()
            .envs(child_env(&manifest))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("{name}: the plugin did not start: {e}"));
        child
            .stdin
            .take()
            .expect("a stdin pipe")
            .write_all(request_bytes(&vector, workspace.path(), bulk.path()).as_bytes())
            .expect("the request is written");
        let output = child.wait_with_output().expect("the plugin exits");

        assert!(
            !output.status.success(),
            "{name}: a refusal must exit non-zero, got {:?} with stdout {:?}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            output.stdout.is_empty(),
            "{name}: a refusing plugin must print nothing on stdout, got {:?}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).trim(),
            fs::read_to_string(&refusal_path)
                .expect("a readable refusal")
                .trim(),
            "{name}: the refusal reason changed"
        );
        graded += 1;
    }
    assert!(graded >= 6, "only {graded} refusal vectors ran");
}

/// A vector carries a golden **or** a refusal, never both and never neither —
/// the hygiene rule that stops a vector from silently grading nothing when a
/// rename drops its sibling.
#[test]
fn every_vector_is_graded_by_exactly_one_sibling() {
    for vector in vectors() {
        let golden = sibling(&vector, ".expected.json").exists();
        let refusal = sibling(&vector, ".refusal.txt").exists();
        assert!(
            golden ^ refusal,
            "{} must carry exactly one of .expected.json / .refusal.txt",
            vector.display()
        );
    }
}

/// What the shipped manifest declares, asserted against the extraction's own
/// bar: `before_turn` **only**, no arbiter powers, and no `[wrapper]` stage
/// order — this plugin participates in a turn, it does not author a variant.
#[test]
fn the_shipped_manifest_declares_before_turn_and_nothing_stronger() {
    let manifest = manifest();
    assert_eq!(manifest.name, "stella-research");
    assert_eq!(manifest.loop_grant.participation, Participation::Steering);
    assert_eq!(manifest.loop_grant.points, vec![WrapperPoint::BeforeTurn]);
    assert!(
        manifest.loop_grant.hooks.is_empty(),
        "research decides nothing, so it binds no gate"
    );
    assert!(manifest.loop_grant.max_holds.is_none());
    assert!(manifest.requirements.is_none() && manifest.oracle.is_none());
    let wrapper = manifest.wrapper.as_ref().expect(
        "a dispatchable wrapper declares its stage order — `WrapperDispatch::bind` \
                 refuses a manifest without one",
    );
    assert_eq!(
        wrapper.id, "research-v1",
        "the variant id is the join key of the A/B comparison §7 requires"
    );
    let research = wrapper
        .stages
        .iter()
        .find(|stage| stage.name == StageName::Host(HostStage::Research))
        .expect("the stage this plugin exists for");
    assert_eq!(
        research.condition.as_deref(),
        Some("questions > 0"),
        "the built-in stage skips byte-for-byte when triage named no questions, \
         and this is that branch exactly"
    );
    assert!(
        manifest.subloop.is_none() && manifest.roles.is_none(),
        "no model call: the socket hands a plugin no engine, provider or credential"
    );
    assert_eq!(
        manifest.runtime.as_ref().expect("[runtime]").env,
        vec!["PATH".to_string()],
        "every path this plugin reads arrives in the request, so PATH is the whole allowlist"
    );
    assert!(
        manifest
            .capabilities
            .iter()
            .all(|capability| capability.risk == stella_plugin::RiskLevel::Low),
        "a read-only stage that writes nothing anywhere asks for nothing above `low`"
    );
}
