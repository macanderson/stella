//! A portable wrapper plugin for the socket's own tests — the in-tree stand-in
//! for the `/bin/sh` scripts that made every one of them `#[cfg(unix)]`
//! (#3497).
//!
//! It is **not** a Rust SDK and must never become one. It links `std` and
//! nothing else: no `serde`, no `stella-plugin`, no JSON parser. Every request
//! it reads is matched as a substring and every response it writes is a
//! `println!` of a literal, which is exactly the tooling a `printf`-and-`case`
//! shell script had. That is the acceptance criterion `doc:pipeline-as-plugins`
//! §5 commitment 2 sets — a Rust-only extension surface is a library with extra
//! steps — and the way to fail it by accident is to reach for the crate's own
//! wire types because they are one `use` away.
//!
//! What it replaces is a real gap rather than a stylistic one. On Windows the
//! shell fixtures compiled to nothing, so the stdio exchange, the concurrent
//! write/wait, `kill_on_drop`, the Job Object group kill (#3550) and
//! `env_clear()` were all unproven there — none of which is shell-shaped.
//!
//! `tests/` locate it with `env!("CARGO_BIN_EXE_wrapper-plugin-fixture")`;
//! cargo builds it automatically for the test targets that name it.
//!
//! # Modes
//!
//! `argv[1]` selects one canned behaviour. Some modes read stdin and some do
//! not, and the transport treats the two differently: a fixed-answer plugin
//! that closes stdin early is legitimate, and the broken pipe it causes must
//! not be read as a lost request, so both shapes stay covered.
//!
//! - `reference` — the reference wrapper: contributes context and a signal at
//!   `before_turn`, reports a measured `p50` at `after_turn`.
//! - `emit <text>` — write `<text>` and exit **without** reading stdin.
//! - `drain-emit <text>` — read stdin to EOF, then write `<text>`.
//! - `exit <code> <stderr>` — read stdin, complain on stderr, exit `<code>`.
//! - `hang` — never read, never answer (the timeout case).
//! - `env-probe` — report whether the parent's environment leaked in, and what
//!   was granted.
//! - `candidate-probe` — report facts about the candidate grant that arrived in
//!   the request: whether its root really holds the test file, whether the test
//!   program is the one named, whether the baseline was red.
//! - `flood <bytes> <heartbeat>` — an unterminated answer larger than the
//!   capture ceiling, then an endless trickle.
//! - `trailing <bytes>` — answer correctly, then keep writing to stdout.
//! - `background <heartbeat>` — start a grandchild and hang, so a timeout has a
//!   tree to fail to kill.
//! - `heartbeat <path>` — the grandchild: append a byte to `<path>` forever.

use std::io::{Read, Write};
use std::time::Duration;

/// How often a heartbeat appends. Short enough that a second of observation
/// separates a live process from a killed one by tens of bytes.
const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(25);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("");
    match mode {
        "reference" => reference(&read_request()),
        "emit" => emit(arg(&args, 1)),
        "drain-emit" => {
            let _ = read_request();
            emit(arg(&args, 1));
        }
        "exit" => {
            let _ = read_request();
            let code: i32 = arg(&args, 1).parse().unwrap_or(1);
            eprintln!("{}", arg(&args, 2));
            std::process::exit(code);
        }
        "hang" => std::thread::sleep(Duration::from_secs(300)),
        "env-probe" => {
            let _ = read_request();
            // `CARGO_MANIFEST_DIR` is in the test process and in nothing the
            // socket grants, so a `0` here means it was cleared rather than
            // that it never existed — the test asserts that premise itself.
            let inherited = u8::from(std::env::var_os("CARGO_MANIFEST_DIR").is_some());
            let granted = std::env::var("GRANTED").unwrap_or_else(|_| "0".into());
            emit(&measurements(&[
                ("inherited", &inherited.to_string()),
                ("granted", &granted),
            ]));
        }
        "candidate-probe" => candidate_probe(&read_request()),
        "flood" => flood(arg(&args, 1).parse().unwrap_or(0), arg(&args, 2)),
        "trailing" => trailing(arg(&args, 1).parse().unwrap_or(0)),
        "background" => background(arg(&args, 1)),
        "heartbeat" => heartbeat(arg(&args, 1)),
        other => {
            eprintln!("wrapper-plugin-fixture: no such mode `{other}`");
            std::process::exit(64);
        }
    }
}

/// `args[index]`, or the empty string. A missing argument is a broken test
/// rather than a plugin fault, and the mode that reads it fails loudly on its
/// own terms.
fn arg(args: &[String], index: usize) -> &str {
    args.get(index).map_or("", String::as_str)
}

/// Read the whole request. Lossy on purpose: this is a substring matcher, and
/// refusing to answer over an encoding question would be a fault the host
/// cannot tell from a real one.
fn read_request() -> String {
    let mut buf = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Write one message and flush it. Nothing here buffers past the process, so a
/// dropped flush would look to the host exactly like a plugin that said
/// nothing.
fn emit(message: &str) {
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "{message}");
    let _ = stdout.flush();
}

/// An `after_turn` response carrying exactly these measurements.
fn measurements(pairs: &[(&str, &str)]) -> String {
    let body = pairs
        .iter()
        .map(|(name, value)| format!("\"{name}\":{value}"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"point\":\"after_turn\",\"body\":{{\"protocol_version\":1,\"evidence\":\
         {{\"flip\":\"not-attempted\",\"measurements\":{{{body}}}}}}}}}"
    )
}

/// The reference wrapper: a budget rather than a flip, so its verdict is
/// decided by a number it reports against a threshold a human read at install.
fn reference(request: &str) {
    if request.contains("\"point\":\"after_turn\"") {
        let p50 = if request.contains("slower") { 118 } else { 103 };
        emit(&measurements(&[("p50", &p50.to_string())]));
    } else {
        emit(
            "{\"point\":\"before_turn\",\"body\":{\"protocol_version\":1,\"context\":\
             [{\"label\":\"budget\",\"text\":\"the recorded p50 budget is 105\"}],\
             \"role\":\"triage\",\"publish\":[{\"signal\":\"questions\",\
             \"value\":{\"count\":2}}]}}",
        );
    }
}

/// Report what the candidate grant in the request is worth on this filesystem.
///
/// `root_reached` is a fact about the disk, not an echo: it is 1 only when the
/// directory the grant named really holds the candidate's test file, so a grant
/// carrying a plausible-looking path nothing lives at scores 0.
fn candidate_probe(request: &str) {
    let root = string_field(request, "root").unwrap_or_default();
    let program = string_field(request, "program").unwrap_or_default();
    let baseline = string_field(request, "baseline").unwrap_or_default();
    let reached = u8::from(
        std::path::Path::new(&root)
            .join("tests")
            .join("test_flip.py")
            .is_file(),
    );
    emit(&measurements(&[
        ("root_reached", &reached.to_string()),
        ("test_named", &u8::from(program == "pytest").to_string()),
        ("baseline_red", &u8::from(baseline == "failed").to_string()),
    ]));
}

/// The value of the first `"<name>":"..."` in `request`, with `\\` and `\"`
/// unescaped.
///
/// The escape handling is not pedantry: a Windows root is
/// `C:\Users\…`, which any JSON encoder writes as `C:\\Users\\…`, so a matcher
/// that took the raw bytes would name a directory that does not exist and the
/// probe above would report a 0 it had manufactured itself. A shell plugin
/// faces exactly this and answers it the same way.
fn string_field(request: &str, name: &str) -> Option<String> {
    let key = format!("\"{name}\":\"");
    let rest = &request[request.find(&key)? + key.len()..];
    let mut value = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(value),
            '\\' => value.push(chars.next()?),
            _ => value.push(c),
        }
    }
    None
}

/// Append a byte to `path` forever — a liveness signal a test can read without
/// asking the OS about a pid.
///
/// `ps` was what the unix version used, and it cannot be ported: a killed
/// orphan lingers as a zombie until someone reaps it, so the answer needed a
/// state column Windows has no equivalent of. A file that stops growing is the
/// same observation made portably, and it is the stronger one — it reports what
/// the process was still *doing*, not merely that a table row survived it.
fn heartbeat(path: &str) -> ! {
    loop {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = file.write_all(b".");
            let _ = file.flush();
        }
        std::thread::sleep(HEARTBEAT_INTERVAL);
    }
}

/// Write an answer that never closes, past the host's capture ceiling, and then
/// trickle forever.
///
/// The heartbeat runs on its own thread rather than in the write loop, and that
/// is the whole design: once the host stops reading, the trickle blocks in
/// `write` and a *living* process would stop looking alive to any observer
/// watching its output. The thread touches no pipe, so the file keeps growing
/// exactly as long as the process is running.
fn flood(bytes: usize, heartbeat_path: &str) -> ! {
    let path = heartbeat_path.to_string();
    std::thread::spawn(move || heartbeat(&path));
    let mut stdout = std::io::stdout();
    let _ = write!(
        stdout,
        "{{\"point\":\"before_turn\",\"body\":{{\"context\":[{{\"label\":\"x\",\"text\":\""
    );
    let chunk = vec![b'a'; 64 * 1024];
    let mut written = 0;
    while written < bytes {
        let take = chunk.len().min(bytes - written);
        if stdout.write_all(&chunk[..take]).is_err() {
            break;
        }
        written += take;
    }
    let _ = stdout.flush();
    loop {
        let _ = stdout.write_all(b"tick");
        let _ = stdout.flush();
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Answer the point correctly, then keep writing to stdout past the pipe
/// buffer the host has stopped draining.
fn trailing(bytes: usize) {
    emit("{\"point\":\"before_turn\",\"body\":{\"protocol_version\":1,\"context\":[]}}");
    let mut stdout = std::io::stdout();
    let chunk = vec![b'a'; 8 * 1024];
    let mut written = 0;
    while written < bytes {
        let take = chunk.len().min(bytes - written);
        if stdout.write_all(&chunk[..take]).is_err() {
            return;
        }
        written += take;
    }
    let _ = stdout.flush();
}

/// Start a grandchild that outlives this process, then hang without answering.
///
/// The grandchild is what `kill_on_drop` cannot reach: it is not the host's
/// child, so only the process group (unix) or the Job Object (Windows) the
/// transport put this process into can take it down with the turn.
fn background(heartbeat_path: &str) -> ! {
    let started = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe)
            .args(["heartbeat", heartbeat_path])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    });
    if let Err(error) = started {
        // The test's whole subject is a grandchild that outlives the turn, so
        // one that never started must not be mistaken for one that was killed.
        eprintln!("wrapper-plugin-fixture: no grandchild started: {error}");
        std::process::exit(70);
    }
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}
