// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Wiring the diagnostic plane into the binary — slice 2 of
//! `docs/spec/diagnostics.md`, "the moment the project can say *attach the
//! log*".
//!
//! Before this, three failures were each indistinguishable from success
//! (§1.2), and the middle one decides whether this project is pleasant to
//! contribute to: *a user hits a bug we cannot reproduce, and there is no
//! artifact to ask them for.* Every mature OSS CLI can say "run it again with
//! `-vv` and attach the log". For a coding agent it is worse than usual —
//! runs are nondeterministic and expensive, so "reproduce it with verbose on"
//! is frequently not a request the user can fulfil. Hence [`install`], and
//! hence the crash ring reaching disk without anyone having asked for it.
//!
//! ## The handle may be ambient; the context may not
//!
//! [`install`] hands the [`Dx`] back and `main` holds it. It *also* parks a
//! clone in a `OnceLock`, and the distinction between what is allowed to be
//! ambient here is worth being precise about, because §7.1 is emphatic on the
//! subject.
//!
//! What §7.1 forbids is an ambient **`Cx`** — a thread-local or task-local
//! carrying which session/turn/step a record belongs to. That is wrong under
//! `!Send` turn futures and wrong under `tokio` task migration, and stella does
//! not need it: invariant 2 already forces every decision function to receive
//! its inputs explicitly, so correlation rides as a parameter. It still does —
//! [`DomainBridge`](crate::diag_bridge::DomainBridge) accrues its own `Cx` from
//! the event stream and stamps each record itself.
//!
//! A **sink handle** is a different animal: process-wide configuration fixed at
//! boot, exactly like the panic hook, which is genuinely global because it
//! takes no parameter. The event drains ([`spawn_renderer`] and
//! `spawn_forwarder`) are spawned from thirteen call sites across the deck,
//! fleet and sub-session paths, most of which have no `Dx` in scope and no
//! reason to grow a parameter for one. Threading it there would buy nothing
//! §7.1 is protecting and cost a large, risky diff through code that has
//! nothing to do with logging.
//!
//! So: the handle is reachable, the context is not.
//!
//! [`spawn_renderer`]: crate::agent::spawn_renderer

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use colored::Colorize;
use stella_diag::{
    Bound, Cx, Dx, Filter, JsonlSink, Level, LevelFilter, Parsed, Record, Sink, TerminalGated,
    TextSink, diag,
};

use crate::cli::GlobalArgs;

/// The default a `--log-file` gets when the operator did not otherwise raise
/// the floor. A file nobody reads until something breaks may as well contain
/// enough to explain it; stderr stays at `warn` so a normal run is quiet.
const FILE_FLOOR: LevelFilter = LevelFilter::Info;

/// The process's diagnostic handle and the workspace it is reporting on.
///
/// Set once by [`install`]. See the module docs for why this one thing is
/// allowed to be reachable when a `Cx` is not.
static BOOTED: OnceLock<(Arc<Dx>, PathBuf)> = OnceLock::new();

/// The process's diagnostic handle, or a disabled one if [`install`] has not
/// run.
///
/// Never `None`: a caller reaching for diagnostics before boot — or in a unit
/// test that never booted — gets a handle whose ring still fills and whose
/// sinks are empty, rather than an `Option` every call site has to answer for.
/// Reserved for the event drains and anything else genuinely spawned beyond
/// reach of `main`'s handle; ordinary code should take a `&Dx`.
pub(crate) fn dx() -> Arc<Dx> {
    booted().0.clone()
}

/// The workspace root [`install`] was given, for path classification.
pub(crate) fn workspace_root() -> PathBuf {
    booted().1.clone()
}

fn booted() -> &'static (Arc<Dx>, PathBuf) {
    BOOTED.get_or_init(|| (Arc::new(Dx::disabled()), PathBuf::from(".")))
}

/// Where crash dumps land: the 0700 directory `trace.rs` already establishes.
pub(crate) fn crash_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".stella").join("private")
}

/// Resolve the filter from the surfaces §6 lists, most specific first.
///
/// 1. `--log-level <spec>` — the full grammar, typed on this command line
/// 2. `-v` / `-vv` / `-vvv` — `info` / `debug` / `trace`
/// 3. `STELLA_LOG`, then `STELLA_SERVE_LOG` — the same grammar
/// 4. `warn`
///
/// A command-line flag beats an environment variable, which is why
/// `--log-level` does not carry clap's `env = …`: with it, `STELLA_LOG=warn
/// stella -vv run …` would silently ignore the `-vv` the user just typed,
/// because clap cannot tell an env-sourced value from a typed one.
pub(crate) fn resolve_filter(globals: &GlobalArgs) -> Parsed {
    if let Some(spec) = globals.log_level.as_deref() {
        return Filter::parse(spec);
    }
    if let Some(level) = LevelFilter::from_verbosity(globals.verbose) {
        return Parsed {
            filter: Filter::level(level),
            bad: Vec::new(),
        };
    }
    stella_diag::filter_from_env()
}

/// Build the process's diagnostic handle, install the panic hook, and report
/// anything wrong with the filter itself.
///
/// Called once, from `main`, which then owns the returned handle.
pub(crate) fn install(globals: &GlobalArgs, workspace_root: &Path) -> Arc<Dx> {
    let Parsed { filter, bad } = resolve_filter(globals);
    let mut sinks = Vec::new();

    if !filter.is_off() {
        let sink = stderr_sink(std::io::stderr().is_terminal(), std::io::stderr());
        sinks.push(Bound::new(filter.clone(), sink));
    }

    // A failed `--log-file` is reported and then dropped, never fatal: the
    // user asked for a run, not for a log file, and refusing to start because
    // a log path is unwritable would be the diagnostic plane taking a turn
    // hostage (§3.4).
    let mut log_file_error = None;
    if let Some(path) = globals.log_file.as_deref() {
        match JsonlSink::file(path) {
            Ok(sink) => sinks.push(Bound::new(
                filter.clone().at_least(FILE_FLOOR),
                Arc::new(sink) as Arc<dyn Sink>,
            )),
            Err(error) => log_file_error = Some(error),
        }
    }

    let dx = Arc::new(Dx::new(sinks));
    // Losing this race means another thread booted first; take that one, so
    // every drain and `main` agree on a single ring.
    let dx = BOOTED
        .get_or_init(|| (dx, workspace_root.to_path_buf()))
        .0
        .clone();
    let dir = crash_dir(workspace_root);
    stella_diag::install_panic_hook(dx.clone(), dir, |path| {
        // Presentation, deliberately: naming a file for a human to read is the
        // third plane's job, and §2.1's routing rule keeps it out of the
        // record. This is the sentence stella could not say before.
        eprintln!(
            "{} diagnostics written to {} — attaching it to an issue is safe; \
             it contains no prompts, paths, or model output",
            "stella:".yellow().bold(),
            path.display()
        );
    });

    // Now that there is somewhere to say it: every clause the filter parser
    // refused. This ordering is the whole reason `Filter::parse` reports
    // rather than complains — the complaint about a bad filter has to outlive
    // the filter that would have suppressed it.
    for clause in &bad {
        dx.emit(Record::new(
            Level::Warn,
            "diag.filter.bad_clause",
            module_path!(),
            Cx::EMPTY,
            Parsed::report(clause),
        ));
    }
    if let Some(error) = log_file_error {
        diag!(
            &dx,
            warn,
            "diag.log_file.unavailable",
            error = sink_error_code(error),
        );
        eprintln!(
            "{} could not open --log-file ({error}); continuing without it",
            "stella:".yellow().bold()
        );
    }

    diag!(
        &dx,
        info,
        "cli.boot",
        version = crate::build_info::version_static(),
        verbosity = globals.verbose,
    );
    dx
}

/// The stderr sink §6 describes, and the one place the TTY question is asked.
///
/// Human-readable on a terminal, JSONL when redirected: a person watching a
/// screen and a program parsing a pipe want different things, and which one is
/// present is knowable without asking.
///
/// The terminal branch is additionally **gated**, and that is the half §6 did
/// not anticipate. On a TTY this sink and the deck are aimed at the same
/// screen, and `ratatui` paints through a buffer it assumes owns it — so a
/// `warn` written straight to stderr does not scroll past, it lands at the
/// hardware cursor and wedges itself into the frame. `TerminalGated` declines
/// to write while a renderer holds the screen; the crash ring and any
/// `--log-file` still take those records, so nothing an operator would attach
/// is lost (see [`stella_diag::TerminalHold`]).
///
/// The redirected branch is deliberately **not** gated. A pipe is not the
/// terminal, no frame can be corrupted through it, and whatever is reading the
/// other end is entitled to every record even mid-deck.
///
/// `writer` is a parameter rather than `TextSink::stderr()` inline so a test
/// can ask both branches what they do without owning the process's stderr.
fn stderr_sink(is_terminal: bool, writer: impl std::io::Write + Send + 'static) -> Arc<dyn Sink> {
    if is_terminal {
        Arc::new(TerminalGated::new(TextSink::to_writer(writer)))
    } else {
        Arc::new(JsonlSink::to_writer(writer))
    }
}

/// A sink error as a closed vocabulary rather than a message.
///
/// `SinkError` is already content-free, but it is another crate's enum and
/// §5.2 admits only what this workspace declared loggable — so it is mapped
/// onto a spelling this crate owns rather than given a `Display` pass.
fn sink_error_code(error: stella_diag::SinkError) -> &'static str {
    match error {
        stella_diag::SinkError::Io(kind) => match kind {
            std::io::ErrorKind::PermissionDenied => "permission_denied",
            std::io::ErrorKind::NotFound => "not_found",
            std::io::ErrorKind::IsADirectory => "is_a_directory",
            _ => "io",
        },
        stella_diag::SinkError::Encode => "encode",
        stella_diag::SinkError::Poisoned => "poisoned",
    }
}

/// Record that the run failed, then dump the ring; returns the file written.
///
/// §7.4 puts the ring on disk on a panic **or** a non-zero exit from `main`,
/// and the second half is the one that matters more often: most failures are a
/// returned `Err`, not a panic, and those are exactly the runs a user is about
/// to file an issue about.
///
/// The error *message* is deliberately not a field. It is the one string in the
/// process most likely to have a path or a provider response interpolated into
/// it, and §5.2 has no exception for the message that happens to be attached to
/// a failure. What the record carries is the shape of the exit — which is what
/// a reader of the dump needs, since the message itself already went to stderr
/// where the human is.
///
/// `debug`, not `error`, and the level is the whole point of §6's split. This
/// record is a **marker for a later reader of the dump**, not news: the human
/// already has the failure on stderr and the shell already has the exit code,
/// so emitting at `error` would print a second, terser copy of a message the
/// user just read. The ring takes every record at every level regardless of
/// filter, so the dump gets its marker either way — which is exactly the case
/// "the filter governs sinks, never emission" was built for.
pub(crate) fn dump_on_failure(
    dx: &Dx,
    workspace_root: &Path,
    interrupted: bool,
) -> Option<PathBuf> {
    diag!(dx, debug, "cli.run.failed", interrupted = interrupted);
    stella_diag::dump(dx, &crash_dir(workspace_root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    use crate::cli::Cli;

    fn globals(args: &[&str]) -> GlobalArgs {
        let mut argv = vec!["stella"];
        argv.extend_from_slice(args);
        argv.push("models");
        Cli::try_parse_from(argv).expect("parse").globals
    }

    /// The surface §6 specifies, end to end.
    #[test]
    fn verbosity_flags_select_the_levels_the_design_names() {
        for (args, expected) in [
            (vec![], LevelFilter::Warn),
            (vec!["-v"], LevelFilter::Info),
            (vec!["-vv"], LevelFilter::Debug),
            (vec!["-vvv"], LevelFilter::Trace),
            (vec!["-vvvv"], LevelFilter::Trace),
        ] {
            let filter = resolve_filter(&globals(&args)).filter;
            assert_eq!(filter.default_level(), expected, "{args:?}");
        }
    }

    #[test]
    fn log_level_takes_the_full_filter_grammar() {
        let parsed = resolve_filter(&globals(&["--log-level", "warn,stella_store=debug"]));
        assert!(parsed.bad.is_empty());
        assert!(parsed.filter.admits("stella_store::migrate", Level::Debug));
        assert!(!parsed.filter.admits("stella_cli::agent", Level::Debug));
    }

    /// A flag the user typed beats an environment variable they exported once.
    /// This is the case clap's `env = …` would have got wrong, silently.
    #[test]
    fn a_typed_flag_outranks_the_environment() {
        // `resolve_filter` reads the environment only as its last resort, so
        // this holds without mutating the process env — which a parallel test
        // suite could not do safely anyway.
        let from_flag = resolve_filter(&globals(&["-vv"])).filter;
        assert_eq!(from_flag.default_level(), LevelFilter::Debug);

        let from_spec = resolve_filter(&globals(&["--log-level", "error", "-vv"])).filter;
        assert_eq!(
            from_spec.default_level(),
            LevelFilter::Error,
            "--log-level is more specific than -v and must win"
        );
    }

    /// §6: a typo in a log knob must not take a process down. It is reported,
    /// and the clauses around it still apply.
    #[test]
    fn a_bad_filter_clause_is_reported_and_not_fatal() {
        let parsed = resolve_filter(&globals(&["--log-level", "warn,stella_store=shout"]));
        assert_eq!(parsed.bad.len(), 1);
        assert_eq!(parsed.filter.default_level(), LevelFilter::Warn);
    }

    #[test]
    fn a_log_file_floor_never_lowers_an_operators_choice() {
        let quiet = Filter::parse("warn").filter.at_least(FILE_FLOOR);
        assert_eq!(quiet.default_level(), LevelFilter::Info);
        let loud = Filter::parse("trace").filter.at_least(FILE_FLOOR);
        assert_eq!(loud.default_level(), LevelFilter::Trace);
    }

    /// The binary's `module_path!()` root is the **bin target name**, `stella`
    /// — not `stella_cli`, which is only the package name. A user who types
    /// `stella_cli=debug` would otherwise get silence and no explanation, so
    /// the flag's help says `stella` and this pins it down.
    #[test]
    fn the_binarys_own_records_live_under_the_target_named_stella() {
        assert_eq!(
            module_path!().split("::").next(),
            Some("stella"),
            "if the bin target is renamed, --log-level's help text is now wrong"
        );

        let filter = resolve_filter(&globals(&["--log-level", "off,stella=trace"])).filter;
        assert!(filter.admits(module_path!(), Level::Trace));
        // And it must not accidentally select the library crates that merely
        // start with the same six letters.
        assert!(!filter.admits("stella_store::migrate", Level::Error));
        assert!(!filter.admits("stella_model::anthropic", Level::Error));
    }

    #[test]
    fn the_crash_directory_is_the_one_traces_already_use() {
        assert_eq!(
            crash_dir(Path::new("/w")),
            Path::new("/w/.stella/private"),
            "a crash dump must land in the 0700 directory trace.rs establishes"
        );
    }

    /// A writer that hands its bytes back, so a sink's real output is
    /// assertable without owning the process's stderr.
    #[derive(Clone, Default)]
    struct Shared(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Shared {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().expect("lock").clone()).expect("utf8")
        }
    }

    impl std::io::Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn a_warning() -> Record {
        Record::new(
            Level::Warn,
            "agent.tool.result",
            "stella::diag_bridge",
            Cx::EMPTY,
            stella_diag::Fields::new().with("ok", false),
        )
    }

    /// The regression this gate exists for: a `warn` record reaching a live
    /// terminal while the deck is drawing on it lands at the cursor and wedges
    /// itself through the frame. `agent.tool.result ok=false` is a `warn` by
    /// design — a benchmark needs the failure signal — so the fix has to be
    /// that the terminal sink stays quiet, not that the record stops existing.
    ///
    /// Both branches are asserted in one test on purpose: the hold is a
    /// process-wide static, so a second test function holding it concurrently
    /// would silence this one's sink at random.
    #[test]
    fn only_the_terminal_branch_goes_quiet_while_the_deck_owns_the_screen() {
        let on_tty = Shared::default();
        let redirected = Shared::default();
        let tty_sink = stderr_sink(true, on_tty.clone());
        let pipe_sink = stderr_sink(false, redirected.clone());

        let hold = stella_diag::TerminalHold::acquire();
        tty_sink.write(&a_warning()).expect("write");
        pipe_sink.write(&a_warning()).expect("write");

        assert_eq!(
            on_tty.text(),
            "",
            "this is the byte that painted `WARN agent.tool.result` over the composer"
        );
        assert!(
            redirected.text().contains("agent.tool.result"),
            "a pipe is not a frame and must still get every record: {:?}",
            redirected.text()
        );

        drop(hold);
        tty_sink.write(&a_warning()).expect("write");
        assert!(
            on_tty.text().contains("agent.tool.result"),
            "a terminal with no renderer on it is an ordinary stderr: {:?}",
            on_tty.text()
        );
    }

    /// Every `SinkError` maps to a fixed spelling — no `Display`, so no
    /// filename can ride along in the one field that reports a file problem.
    #[test]
    fn a_sink_error_records_as_a_closed_vocabulary() {
        let denied = stella_diag::SinkError::Io(std::io::ErrorKind::PermissionDenied);
        assert_eq!(sink_error_code(denied), "permission_denied");
        assert_eq!(
            sink_error_code(stella_diag::SinkError::Io(std::io::ErrorKind::Other)),
            "io"
        );
    }
}
