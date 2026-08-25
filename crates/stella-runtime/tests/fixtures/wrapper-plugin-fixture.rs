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
//! It sits under `tests/` rather than `src/bin/` because `no_ambient_reads.rs`
//! scans everything under `src/` and refuses a `std::env::var` — a process
//! global is the same for all N sessions a host assembles in one process. This
//! is a separate process whose `env-probe` mode exists to report the
//! environment it received, so it has to read one. Keeping it out of the
//! library is the honest way to say that; teaching the invariant an exemption
//! would not be.
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
//! - `tally <path>` — append a byte per start, so a caller counting bytes
//!   counts process starts.
//! - `dispatch-reference` — `wrapper_dispatch.rs`'s reference wrapper: a
//!   `scope`-declaring `before_turn`, and an `after_turn` p50 that keys on
//!   whether the turn mentioned the correction.
//! - `dispatch-contributed` — the same, with a third branch for a contributed
//!   stage name, which is what proves it reaches the plugin as itself.
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
        // Appends one byte per start, so a caller counting bytes counts
        // process starts — how `wrapper_dispatch.rs` proves a composition
        // reuses one process rather than spawning per point.
        "tally" => {
            let _ = read_request();
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(arg(&args, 1))
            {
                let _ = f.write_all(b"x\n");
            }
            emit("{\"point\":\"before_turn\",\"body\":{\"protocol_version\":1}}");
        }
        "dispatch-reference" => dispatch_reference(&read_request()),
        "dispatch-contributed" => dispatch_contributed(&read_request()),
        // Claims a flip only when a candidate grant naming `sh` arrived, and
        // says `unobservable` otherwise — so a host that withholds the grant
        // gets an honest refusal rather than an assertion the plugin could
        // not have made. The two-substring match is the `case` the shell
        // script spelled out.
        //
        // Both arms are load-bearing, from different files.
        // `wrapper_claimed_evidence.rs` takes only the granted side (its
        // `report()` always passes `candidate: Some(..)`), while
        // `wrapper_decided_flip.rs` calls `run(None, ..)` — so neutering
        // this branch fails `without_the_grant_or_the_snapshot_the_verdict_is_undecided`
        // there. Checked by ablation, not assumed.
        "flip-if-granted" => {
            let request = read_request();
            let flip = if request.contains("\"root\"") && request.contains("\"program\":\"sh\"") {
                "achieved"
            } else {
                "unobservable"
            };
            emit(&format!(
                "{{\"point\":\"after_turn\",\"body\":{{\"protocol_version\":1,\
                 \"evidence\":{{\"flip\":\"{flip}\"}}}}}}"
            ));
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
/// `wrapper_dispatch.rs`'s own reference plugin, which its `/bin/sh` script
/// used to spell out (#4697). Distinct from [`fn@reference`] above in two ways
/// the tests assert on: the `after_turn` p50 keys on "unrolled the loop"
/// rather than "slower", and the `before_turn` declares a `scope`.
///
/// The branch is the whole point. A mode that answered one canned body
/// whatever it was asked would pass nothing here — the dispatch tests read
/// both p50 values back and compare them, so a fixture that stopped
/// branching fails them rather than passing vacuously.
fn dispatch_reference(request: &str) {
    if request.contains("\"point\":\"after_turn\"") {
        let p50 = if request.contains("unrolled the loop") {
            101
        } else {
            118
        };
        emit(&measurements(&[("p50", &p50.to_string())]));
    } else {
        emit(
            "{\"point\":\"before_turn\",\"body\":{\"protocol_version\":1,\"context\":\
             [{\"label\":\"budget\",\"text\":\"the recorded p50 budget is 105\"}],\
             \"role\":\"triage\",\"scope\":[\"crates/stella-core\"],\
             \"publish\":[{\"signal\":\"questions\",\"value\":{\"count\":2}}]}}",
        );
    }
}

/// The contributed-stage plugin, whose three-way branch is what proves a
/// contributed stage name reaches the plugin as itself rather than as the
/// host stage it runs under.
fn dispatch_contributed(request: &str) {
    if request.contains("\"point\":\"after_turn\"") {
        emit(&measurements(&[("answers", "1")]));
    } else if request.contains("\"stage\":\"triage-lite\"") {
        emit(
            "{\"point\":\"before_turn\",\"body\":{\"protocol_version\":1,\"context\":\
             [{\"label\":\"triage-lite\",\"text\":\"this task is small; skip the plan\"}]}}",
        );
    } else {
        emit(
            "{\"point\":\"before_turn\",\"body\":{\"protocol_version\":1,\"context\":\
             [{\"label\":\"other\",\"text\":\"a host stage\"}]}}",
        );
    }
}

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
