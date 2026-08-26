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

/// The binary-path macro, whose occurrences in code are the spawn sites.
///
/// Spelled as `concat!` halves so this file's own source does not carry the
/// needle beyond the one real spawn site above. A guard that matched itself
/// would count its own needles as evidence about the code it is judging.
const SPAWNS: &str = concat!("CARGO_BIN_EXE", "_stella");

/// What seals a spawned child. `env_clear` counts — a child with no inherited
/// environment has no inherited key.
const SEALS: [&str; 2] = [concat!(".without_embedder", "_backend()"), ".env_clear()"];

/// `source` with its comments removed, so prose naming the binary-path macro
/// is not counted as a spawn site (#4986).
///
/// String literals are copied through rather than dropped, because the needle
/// lives inside one — `env!("CARGO_BIN_EXE_…")`. They are also *tracked*, so a
/// `//` inside one does not open a comment and swallow the rest of the line:
/// this directory writes `"http://{addr}/v1"`, and truncating there would drop
/// a sealing call after it and fail a correctly sealed file.
fn code_only(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut code = String::with_capacity(source.len());
    let mut at = 0usize;
    while at < bytes.len() {
        if let Some(end) = raw_string_end(bytes, at) {
            code.push_str(&source[at..end]);
            at = end;
            continue;
        }
        match bytes[at] {
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                while at < bytes.len() && bytes[at] != b'\n' {
                    at += 1;
                }
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                // Rust nests block comments, so this counts depth rather than
                // stopping at the first `*/`.
                let mut depth = 1usize;
                at += 2;
                while at < bytes.len() && depth > 0 {
                    if bytes[at] == b'/' && bytes.get(at + 1) == Some(&b'*') {
                        depth += 1;
                        at += 2;
                    } else if bytes[at] == b'*' && bytes.get(at + 1) == Some(&b'/') {
                        depth -= 1;
                        at += 2;
                    } else {
                        at += 1;
                    }
                }
                // A space, so dropping a comment cannot join the tokens either
                // side of it into a needle.
                code.push(' ');
            }
            b'"' => {
                let end = quoted_end(bytes, at);
                code.push_str(&source[at..end]);
                at = end;
            }
            b'\'' => {
                let end = char_literal_end(bytes, at).unwrap_or(at + 1);
                code.push_str(&source[at..end]);
                at = end;
            }
            _ => {
                let mut end = at + 1;
                while end < bytes.len() && !source.is_char_boundary(end) {
                    end += 1;
                }
                code.push_str(&source[at..end]);
                at = end;
            }
        }
    }
    code
}

/// The byte after the raw string starting at `at`, or `None` when one does not
/// start there. Covers `r"…"`, `r#"…"#` and the `b`-prefixed forms.
fn raw_string_end(bytes: &[u8], at: usize) -> Option<usize> {
    if at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_') {
        return None;
    }
    let mut cursor = at;
    if bytes.get(cursor) == Some(&b'b') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'r') {
        return None;
    }
    cursor += 1;
    let opened = cursor;
    while bytes.get(cursor) == Some(&b'#') {
        cursor += 1;
    }
    let hashes = cursor - opened;
    if bytes.get(cursor) != Some(&b'"') {
        return None;
    }
    cursor += 1;
    while cursor < bytes.len() {
        if bytes[cursor] == b'"' {
            let after = cursor + 1;
            if bytes.len() - after >= hashes
                && bytes[after..].iter().take(hashes).all(|byte| *byte == b'#')
            {
                return Some(after + hashes);
            }
        }
        cursor += 1;
    }
    Some(bytes.len())
}

/// The byte after the ordinary string literal starting at `at`.
fn quoted_end(bytes: &[u8], at: usize) -> usize {
    let mut cursor = at + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor += 2,
            b'"' => return cursor + 1,
            _ => cursor += 1,
        }
    }
    bytes.len()
}

/// The byte after the character literal starting at `at`, or `None` when the
/// quote opens a lifetime or a loop label instead.
fn char_literal_end(bytes: &[u8], at: usize) -> Option<usize> {
    let mut cursor = at + 1;
    if bytes.get(cursor) == Some(&b'\\') {
        cursor += 2;
        // `'\u{1F600}'` — run on to the closing quote of a braced escape.
        while cursor < bytes.len() && bytes[cursor] != b'\'' {
            cursor += 1;
        }
    } else {
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor] & 0b1100_0000 == 0b1000_0000 {
            cursor += 1;
        }
    }
    (bytes.get(cursor) == Some(&b'\'')).then_some(cursor + 1)
}

/// How many spawn sites `source` has, and how many seals — counted over its
/// code, with comments excluded.
fn spawn_and_seal_counts(source: &str) -> (usize, usize) {
    let code = code_only(source);
    let spawn_sites = code.matches(SPAWNS).count();
    let sealed = SEALS.iter().map(|seal| code.matches(seal).count()).sum();
    (spawn_sites, sealed)
}

/// Prose naming the binary-path macro is not a spawn site, and an unsealed
/// spawn beside that prose is still caught (#4986).
///
/// Both fixtures are built from the needles rather than spelled, for the
/// reason [`SPAWNS`] gives. Each carries three hazards at once: a line
/// comment naming the macro, a nested block comment naming it, and a `//`
/// inside a string literal on the same line as the sealing call — so a
/// line-truncating strip fails here too, not only a strip that does nothing.
#[test]
fn prose_naming_the_macro_is_not_a_spawn_site() {
    let one_sealed_spawn = format!(
        "//! Spawned through {SPAWNS}, sealed.\n\
         /* {SPAWNS} again, and /* nested */ */\n\
         fn go() {{\n    \
         Command::new(env!(\"{SPAWNS}\")).env(\"URL\", \"http://a/v1\"){}; \n\
         }}\n",
        SEALS[0]
    );
    assert_eq!(
        spawn_and_seal_counts(&one_sealed_spawn),
        (1, 1),
        "a comment naming the macro was counted as a spawn site, or a `//` \
         inside a string literal swallowed the sealing call after it"
    );

    let leaking = format!("{one_sealed_spawn}fn slip() {{ Command::new(env!(\"{SPAWNS}\")); }}\n");
    let (spawn_sites, sealed) = spawn_and_seal_counts(&leaking);
    assert!(
        sealed < spawn_sites,
        "an unsealed spawn beside the prose must still fail: \
         {spawn_sites} spawn site(s), {sealed} sealed"
    );
}

/// Every test in this directory that spawns the binary must seal the embedder
/// backend, because the developer whose key is exported is not the one who
/// wrote the seventeenth test file.
///
/// # How it counts
///
/// Once per occurrence of the binary-path macro in code, not once per file: a
/// helper that seals one of a file's three spawn sites is the failure this is
/// for. Comments do not count — see [`code_only`].
#[test]
fn every_spawning_test_seals_the_embedder_backend() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut checked = 0usize;
    for entry in std::fs::read_dir(&dir).expect("read tests dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read test source");
        let (spawn_sites, sealed) = spawn_and_seal_counts(&source);
        if spawn_sites == 0 {
            continue;
        }
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
