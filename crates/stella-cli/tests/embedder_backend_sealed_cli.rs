//! No `stella-cli` test may let the binary it spawns reach an embedding
//! backend (#4542).
//!
//! `stella_embed::EmbedderEnv::from_process` resolves a *hosted* backend from
//! a bare `VOYAGE_API_KEY` or `OPENAI_API_KEY` — no base URL needed — and a
//! session's code-graph build warms a semantic index. A child inherits the
//! developer's environment, so on a machine with either key exported
//! `cargo test -p stella-cli` bills that developer for the suite. It happened:
//! the outbound call was observed during #4540.
//!
//! # What is proved here
//!
//! * `an_unsealed_child_reaches_the_configured_backend` spawns a child with
//!   the embedder configured and *not* sealed, and asserts the listener is
//!   connected to. Without it, "zero connections" in the arm below would be
//!   consistent with a listener that never worked, a command that never
//!   embeds, and a fix that does nothing.
//! * `a_sealed_child_reaches_no_backend` spawns the same command through
//!   `without_embedder_backend` and asserts zero connections.
//!
//! The observable backend is a local listener named by `STELLA_EMBED_URL`,
//! because a test cannot watch api.voyageai.com. That the seal covers the
//! vendor-key shortcuts too is `stella-embed`'s own assertion: it clears
//! `stella_embed::ENV_VARS`, which `from_lookup_reads_exactly_the_listed_variables`
//! holds equal to what `from_process` reads.
//!
//! # And the guard that keeps it true
//!
//! `every_spawning_test_seals_the_embedder_backend` reads the sibling test
//! sources, because the fix is only worth what the seventeenth test file
//! remembers to do.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

mod common;
use common::SealsEmbedderBackend;

/// Bound once so the source carries the binary-path macro exactly as often as
/// it carries a sealing call — see the guard's own note on counting.
const STELLA: &str = env!("CARGO_BIN_EXE_stella");

/// How long a `stella search` over a one-file workspace is given. Generous:
/// the property is "connects" versus "never connects", not a latency budget.
const DEADLINE: Duration = Duration::from_secs(120);

/// After the child exits, how long the listener is still watched. A connection
/// in flight at exit is still a connection.
const DRAIN: Duration = Duration::from_millis(250);

/// The three directories one child runs against: a workspace with one source
/// file so the code graph has something to index and the search something to
/// rank, plus the user and data tiers moved off the developer's real ones.
struct Sandbox {
    workspace: tempfile::TempDir,
    home: tempfile::TempDir,
    data: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(
            workspace.path().join("lib.rs"),
            "pub fn hello() -> u8 { 7 }\n",
        )
        .expect("source file");
        Self {
            workspace,
            home: tempfile::tempdir().expect("stella home"),
            data: tempfile::tempdir().expect("data dir"),
        }
    }
}

/// `stella search`, pointed at `backend` as its embedding endpoint and given a
/// vendor key besides. `sealed` selects the fix under test.
fn spawn_search(sandbox: &Sandbox, backend: SocketAddr, sealed: bool) -> Child {
    let mut command = Command::new(STELLA);
    command
        .args(["search", "what does hello return"])
        .current_dir(sandbox.workspace.path())
        .env("STELLA_HOME", sandbox.home.path())
        .env("STELLA_DATA_DIR", sandbox.data.path())
        .env("STELLA_NO_ENV_FILE", "1")
        // The developer's exported key, and an endpoint a test can watch.
        .env("VOYAGE_API_KEY", "sk-not-a-real-key")
        .env("STELLA_EMBED_URL", format!("http://{backend}/v1"))
        .env("STELLA_EMBED_MODEL", "voyage-code-3")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if sealed {
        command.without_embedder_backend();
    }
    command.spawn().expect("spawn stella search")
}

/// Answer one connection the way a backend refusing a bad key does, so the
/// child stops rather than climbing the retry ladder. `stella-embed` does not
/// retry a 401 (`an_unauthorized_request_is_not_retried`).
fn refuse(mut stream: TcpStream) {
    let mut scratch = [0u8; 1024];
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .expect("read timeout");
    let _ = stream.read(&mut scratch);
    let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n");
    let _ = stream.flush();
}

/// Run one child to completion and report how many connections the listener
/// accepted while it lived.
fn connections_during(listener: &TcpListener, mut child: Child) -> usize {
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let started = Instant::now();
    let mut connections = 0usize;
    let mut exited_at = None;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                connections += 1;
                stream.set_nonblocking(false).expect("blocking stream");
                refuse(stream);
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
        match exited_at {
            Some(at) if Instant::now().duration_since(at) >= DRAIN => break,
            Some(_) => {}
            None => {
                if child.try_wait().expect("wait on stella").is_some() {
                    exited_at = Some(Instant::now());
                }
            }
        }
        assert!(
            started.elapsed() < DEADLINE,
            "stella search did not finish within {DEADLINE:?}"
        );
    }
    connections
}

#[test]
fn an_unsealed_child_reaches_the_configured_backend() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let addr = listener.local_addr().expect("listener address");
    let sandbox = Sandbox::new();
    let connections = connections_during(&listener, spawn_search(&sandbox, addr, false));
    assert!(
        connections > 0,
        "the embedding backend was never contacted, so this file's other arm \
         would pass against a listener that does not work"
    );
}

#[test]
fn a_sealed_child_reaches_no_backend() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let addr = listener.local_addr().expect("listener address");
    let sandbox = Sandbox::new();
    let connections = connections_during(&listener, spawn_search(&sandbox, addr, true));
    assert_eq!(
        connections, 0,
        "a sealed child still reached the embedding backend"
    );
}

/// Every test in this directory that spawns the binary must seal the embedder
/// backend, because the developer whose key is exported is not the one who
/// wrote the seventeenth test file.
///
/// # How it counts
///
/// Once per occurrence of the binary-path macro, not once per file: a helper
/// that seals one of a file's three spawn sites is the failure this is for.
/// `env_clear` counts as a seal — a child with no inherited environment has no
/// inherited key.
///
/// The two needles are spelled as `concat!` halves so this file's own source
/// does not contain them. A guard that matched itself would count its own
/// needles as evidence about the code it is judging.
#[test]
fn every_spawning_test_seals_the_embedder_backend() {
    let spawns = concat!("CARGO_BIN_EXE", "_stella");
    let seals = [concat!(".without_embedder", "_backend()"), ".env_clear()"];

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut checked = 0usize;
    for entry in std::fs::read_dir(&dir).expect("read tests dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read test source");
        let spawn_sites = source.matches(spawns).count();
        if spawn_sites == 0 {
            continue;
        }
        let sealed: usize = seals.iter().map(|seal| source.matches(seal).count()).sum();
        assert!(
            sealed >= spawn_sites,
            "{}: {spawn_sites} spawn site(s), {sealed} sealed — a spawned child \
             inherits VOYAGE_API_KEY and bills whoever runs the suite (#4542). \
             Add `mod common; use common::SealsEmbedderBackend;` and \
             `.without_embedder_backend()` to the command.",
            path.display()
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "no spawning test was found in {} — the guard read the wrong directory",
        dir.display()
    );
}
