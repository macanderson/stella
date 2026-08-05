// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Where records go: the [`Sink`] port and the four implementations that need
//! no dependency this crate does not already have.
//!
//! The port is the whole point, and it is inherited unchanged from
//! `serve-observability.md`: emission is typed and every site talks to
//! [`Dx`](crate::Dx), so adopting `tracing` later is **one more impl in this
//! file and no emit site touched**. `docs/design/diagnostics/diagnostics.md` §10 is that
//! deferral re-argued at workspace scope, and it lands on `tracing` arriving as
//! an off-by-default `otel` cargo feature — which is exactly one more `impl
//! Sink`.
//!
//! [`Sink::write`] returns a `Result` **that [`Dx`](crate::Dx) discards**, and
//! that asymmetry is deliberate. A sink must be able to say it failed (invariant
//! 5: no silent discards), and a caller must never be able to fail because of
//! it (§3.4: losing a log line must never lose a turn). Property 4 of §12 pins
//! the pair down.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::filter::Filter;
use crate::private;
use crate::record::Record;

/// Why a sink could not record something.
///
/// Typed rather than a `String` (invariant 5), and content-free for the same
/// reason records are: an `io::Error`'s `Display` carries the filename, so only
/// its [`std::io::ErrorKind`] is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkError {
    Io(std::io::ErrorKind),
    /// A record that would not serialize. Unreachable in practice — every
    /// field type in this crate is serde-clean — but not an excuse to panic.
    Encode,
    /// Another thread panicked holding this sink's lock.
    Poisoned,
}

impl std::fmt::Display for SinkError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(kind) => write!(formatter, "diagnostic sink I/O failed: {kind}"),
            Self::Encode => formatter.write_str("diagnostic record would not serialize"),
            Self::Poisoned => formatter.write_str("diagnostic sink lock was poisoned"),
        }
    }
}

impl std::error::Error for SinkError {}

impl From<std::io::Error> for SinkError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.kind())
    }
}

/// Somewhere a record can land.
pub trait Sink: Send + Sync {
    /// Record one. Returning `Err` is how a sink reports its own failure; it
    /// never propagates to the emitting caller.
    fn write(&self, record: &Record) -> Result<(), SinkError>;

    fn flush(&self) -> Result<(), SinkError> {
        Ok(())
    }
}

/// One sink and the filter that governs it.
///
/// Per-sink rather than one filter for the whole process, because the two
/// surfaces §6 lists genuinely want different answers: stderr defaults to
/// `warn` so a normal run is quiet, while `--log-file` defaults to `info`
/// because a file nobody reads until something breaks may as well contain
/// enough to explain it.
pub struct Bound {
    filter: Filter,
    sink: Arc<dyn Sink>,
}

impl Bound {
    #[must_use]
    pub fn new(filter: Filter, sink: Arc<dyn Sink>) -> Self {
        Self { filter, sink }
    }

    #[must_use]
    pub fn filter(&self) -> &Filter {
        &self.filter
    }

    pub(crate) fn offer(&self, record: &Record) -> Option<Result<(), SinkError>> {
        self.filter
            .admits(record.target.as_str(), record.level)
            .then(|| self.sink.write(record))
    }

    pub(crate) fn flush(&self) -> Result<(), SinkError> {
        self.sink.flush()
    }
}

/// One JSON object per line.
///
/// The machine-readable shape, and the one `--log-file` writes. The `Mutex`
/// serializes whole lines: two threads writing concurrently would otherwise
/// interleave mid-record and produce unparseable output.
pub struct JsonlSink {
    out: Mutex<Box<dyn Write + Send>>,
}

impl JsonlSink {
    #[must_use]
    pub fn stderr() -> Self {
        Self::to_writer(std::io::stderr())
    }

    #[must_use]
    pub fn to_writer(writer: impl Write + Send + 'static) -> Self {
        Self {
            out: Mutex::new(Box::new(writer)),
        }
    }

    /// Append to `path`, owner-only, creating the parent directory if needed.
    ///
    /// 0600 because a log file is the easiest accidental egress there is —
    /// §3.2's sharper reading of invariant 3 is that *operators ship logs*.
    /// The contents are content-free by construction, and the permissions are
    /// belt and braces.
    pub fn file(path: &Path) -> Result<Self, SinkError> {
        Ok(Self::to_writer(private::open_append(path)?))
    }
}

impl Sink for JsonlSink {
    fn write(&self, record: &Record) -> Result<(), SinkError> {
        let mut line = serde_json::to_vec(record).map_err(|_| SinkError::Encode)?;
        line.push(b'\n');
        let mut out = self.out.lock().map_err(|_| SinkError::Poisoned)?;
        out.write_all(&line)?;
        Ok(())
    }

    fn flush(&self) -> Result<(), SinkError> {
        self.out.lock().map_err(|_| SinkError::Poisoned)?.flush()?;
        Ok(())
    }
}

/// One human-readable line per record.
///
/// The default when stderr is a TTY (§6). Deliberately not colourised and
/// deliberately not clever: this is the diagnostic plane, not the presentation
/// plane, and §2's whole point is that the two stop borrowing each other's
/// clothes.
pub struct TextSink {
    out: Mutex<Box<dyn Write + Send>>,
}

impl TextSink {
    #[must_use]
    pub fn stderr() -> Self {
        Self::to_writer(std::io::stderr())
    }

    #[must_use]
    pub fn to_writer(writer: impl Write + Send + 'static) -> Self {
        Self {
            out: Mutex::new(Box::new(writer)),
        }
    }

    /// `WARN store.migration.retry stella_store::migrate turn=3 attempt=2`
    fn render(record: &Record) -> String {
        use std::fmt::Write as _;

        let mut line = format!(
            "{:<5} {} {}",
            record.level.as_static().to_ascii_uppercase(),
            record.code,
            record.target
        );
        if let Some(session) = record.cx.session {
            let _ = write!(line, " session={session}");
        }
        if let Some(turn) = record.cx.turn {
            let _ = write!(line, " turn={turn}");
        }
        if let Some(step) = record.cx.step {
            let _ = write!(line, " step={step}");
        }
        for (key, value) in record.fields.iter() {
            // For Reviewed values, render only the reviewed content, not the note.
            // The note carries reviewer justification, not runtime information,
            // and belongs in files/debug sinks, not user-facing output.
            let rendered = if let crate::FieldValue::Reviewed(reviewed) = value {
                reviewed.value().to_owned()
            } else {
                // Through serde rather than a second renderer: a hand-written
                // formatter for `FieldValue` would be a place for the two spellings
                // of a record to disagree, and the JSON one is the authority.
                let s = serde_json::to_string(value).unwrap_or_else(|_| String::from("\"?\""));
                s.trim_matches('"').to_owned()
            };
            let _ = write!(line, " {key}={rendered}");
        }
        line
    }
}

impl Sink for TextSink {
    fn write(&self, record: &Record) -> Result<(), SinkError> {
        let mut line = Self::render(record).into_bytes();
        line.push(b'\n');
        let mut out = self.out.lock().map_err(|_| SinkError::Poisoned)?;
        out.write_all(&line)?;
        Ok(())
    }

    fn flush(&self) -> Result<(), SinkError> {
        self.out.lock().map_err(|_| SinkError::Poisoned)?.flush()?;
        Ok(())
    }
}

/// How many live [`TerminalHold`]s there are.
///
/// A count rather than a flag, because the holds genuinely nest: the deck can
/// hand the terminal to the fleet dashboard and take it back, and a flag would
/// let whichever one exited first un-silence stderr underneath the other.
static TERMINAL_HOLDS: AtomicUsize = AtomicUsize::new(0);

/// Claims the terminal for a full-screen renderer until it is dropped.
///
/// §2.1 routes each plane to its own destination and §6 sends the diagnostic
/// plane's default to stderr — but "stderr" and "the deck" are the same
/// terminal, which is the one collision the routing rule does not describe.
/// `ratatui` paints stdout through a diff-based buffer it believes it owns
/// exclusively; a record written straight to stderr lands at the hardware
/// cursor instead, so the frame acquires bytes the buffer has no cell for and
/// will not repaint until something else dirties that row. The user sees a
/// `WARN …` line wedged through the middle of the composer.
///
/// So a renderer takes a hold for exactly as long as it owns the screen, and
/// [`TerminalGated`] declines to write while one is live.
///
/// This is a routing decision, not a discard: the filter already declines
/// records per-sink without that counting as a loss, and the same records still
/// reach the crash ring — which takes everything at every level regardless of
/// filter (§7.4) — and any `--log-file`, which is not a terminal and is not
/// gated. "Attach the log" keeps working through a deck session; it is only
/// the copy that would have corrupted the frame that is dropped.
#[derive(Debug)]
#[must_use = "the terminal is released the instant this is dropped"]
pub struct TerminalHold(());

impl TerminalHold {
    /// Take a hold. Release is [`Drop`], so a renderer that panics or bails
    /// part-way through setup un-silences stderr on the way out.
    pub fn acquire() -> Self {
        TERMINAL_HOLDS.fetch_add(1, Ordering::AcqRel);
        Self(())
    }

    /// Whether any renderer currently owns the terminal.
    #[must_use]
    pub fn is_active() -> bool {
        TERMINAL_HOLDS.load(Ordering::Acquire) > 0
    }
}

impl Drop for TerminalHold {
    fn drop(&mut self) {
        TERMINAL_HOLDS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A sink whose bytes land on the terminal, silenced while a [`TerminalHold`]
/// is live.
///
/// Wrap the stderr sink in this **only when stderr is a TTY**. When it has been
/// redirected to a file or a pipe it is not the terminal any renderer is
/// drawing on, nothing can be corrupted, and a consumer on the other end of
/// that pipe is entitled to every record.
pub struct TerminalGated {
    inner: Arc<dyn Sink>,
}

impl TerminalGated {
    pub fn new(inner: impl Sink + 'static) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

impl Sink for TerminalGated {
    fn write(&self, record: &Record) -> Result<(), SinkError> {
        if TerminalHold::is_active() {
            return Ok(());
        }
        self.inner.write(record)
    }

    /// Forwarded unconditionally: a flush moves bytes already accepted and
    /// paints nothing new, and skipping it would strand whatever was written
    /// before the renderer took the screen.
    fn flush(&self) -> Result<(), SinkError> {
        self.inner.flush()
    }
}

/// Discards everything.
///
/// The default when a caller does not care, and what keeps every library type
/// in the workspace usable without wiring a sink — the same job
/// `stella-serve::observe::sink::NullSink` does today.
pub struct NullSink;

impl Sink for NullSink {
    fn write(&self, _record: &Record) -> Result<(), SinkError> {
        Ok(())
    }
}

/// Records in memory, for assertions.
///
/// This is what makes acceptance criteria testable: a test matches on a
/// `Record`'s `code` and typed fields rather than grepping stderr for a phrase,
/// so the assertion survives any change to how records are rendered.
#[derive(Default)]
pub struct Capture {
    records: Mutex<Vec<Record>>,
}

impl Capture {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every record written so far, in order.
    #[must_use]
    pub fn records(&self) -> Vec<Record> {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[must_use]
    pub fn codes(&self) -> Vec<String> {
        self.records()
            .iter()
            .map(|record| record.code.as_str().to_owned())
            .collect()
    }

    pub fn count(&self, predicate: impl Fn(&Record) -> bool) -> usize {
        self.records().iter().filter(|r| predicate(r)).count()
    }

    #[must_use]
    pub fn find(&self, code: &str) -> Option<Record> {
        self.records()
            .into_iter()
            .find(|record| record.code.as_str() == code)
    }
}

impl Sink for Capture {
    fn write(&self, record: &Record) -> Result<(), SinkError> {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(record.clone());
        Ok(())
    }
}

/// Fails every write, for property 4 of §12.
///
/// Public rather than test-only so a downstream crate can prove the same
/// property of *its* wiring: "a broken log did not break my turn" is a claim
/// every embedder should be able to make about itself.
pub struct FailingSink;

impl Sink for FailingSink {
    fn write(&self, _record: &Record) -> Result<(), SinkError> {
        Err(SinkError::Io(std::io::ErrorKind::BrokenPipe))
    }

    fn flush(&self) -> Result<(), SinkError> {
        Err(SinkError::Io(std::io::ErrorKind::BrokenPipe))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cx, Fields, Level};

    /// A writer that hands its bytes back, so a sink's actual output is
    /// assertable without a file.
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Shared {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().expect("lock").clone()).expect("utf8")
        }
    }

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn a_record() -> Record {
        Record::at(
            1_754_006_400_123,
            Level::Warn,
            "store.migration.retry",
            "stella_store::migrate",
            Cx::new().turn(3),
            Fields::new()
                .with("attempt", 2_u64)
                .with("reason", "locked"),
        )
    }

    #[test]
    fn jsonl_writes_one_object_per_line() {
        let shared = Shared::default();
        let sink = JsonlSink::to_writer(shared.clone());
        sink.write(&a_record()).expect("write");
        sink.write(&a_record()).expect("write");

        let text = shared.text();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            let value: serde_json::Value = serde_json::from_str(line).expect("parse");
            assert_eq!(value["code"], "store.migration.retry");
            assert_eq!(value["fields"]["attempt"], 2);
        }
    }

    #[test]
    fn text_renders_one_readable_line() {
        let shared = Shared::default();
        let sink = TextSink::to_writer(shared.clone());
        sink.write(&a_record()).expect("write");
        assert_eq!(
            shared.text(),
            "WARN  store.migration.retry stella_store::migrate turn=3 attempt=2 reason=locked\n"
        );
    }

    /// The whole gate in one test, deliberately: `TERMINAL_HOLDS` is a
    /// process-wide static and the test binary is threaded, so splitting these
    /// assertions into separate `#[test]`s would let one function's hold
    /// silence another function's sink and fail at random.
    #[test]
    fn a_gated_sink_goes_quiet_exactly_while_a_renderer_owns_the_terminal() {
        let shared = Shared::default();
        let sink = TerminalGated::new(TextSink::to_writer(shared.clone()));

        assert!(!TerminalHold::is_active(), "nothing should hold by default");
        sink.write(&a_record()).expect("write");
        assert!(
            shared.text().contains("store.migration.retry"),
            "an unheld terminal must still get its records: {:?}",
            shared.text()
        );

        let before = shared.text();
        let outer = TerminalHold::acquire();
        sink.write(&a_record()).expect("write");
        assert_eq!(
            shared.text(),
            before,
            "a record written while the deck owns the screen lands mid-frame"
        );

        // Nested, because the deck can hand the screen to the fleet dashboard
        // and take it back. The inner release must not un-silence the outer.
        let inner = TerminalHold::acquire();
        drop(inner);
        assert!(TerminalHold::is_active(), "the outer hold is still live");
        sink.write(&a_record()).expect("write");
        assert_eq!(shared.text(), before, "the inner drop released too much");

        // A flush is still forwarded — it moves bytes already accepted and
        // paints nothing new.
        sink.flush().expect("flush");

        drop(outer);
        assert!(!TerminalHold::is_active(), "the screen is back");
        sink.write(&a_record()).expect("write");
        assert!(
            shared.text().len() > before.len(),
            "a restored terminal must get records again"
        );
    }

    #[test]
    fn a_bound_sink_drops_what_its_filter_excludes() {
        let capture = Arc::new(Capture::new());
        let bound = Bound::new(
            Filter::parse("error").filter,
            capture.clone() as Arc<dyn Sink>,
        );
        assert!(
            bound.offer(&a_record()).is_none(),
            "warn must not pass error"
        );
        assert!(capture.records().is_empty());

        let bound = Bound::new(
            Filter::parse("off,stella_store=debug").filter,
            capture.clone() as Arc<dyn Sink>,
        );
        assert!(bound.offer(&a_record()).is_some());
        assert_eq!(capture.records().len(), 1);
    }

    #[test]
    fn a_failing_sink_reports_a_typed_error() {
        let error = FailingSink.write(&a_record()).expect_err("must fail");
        assert_eq!(error, SinkError::Io(std::io::ErrorKind::BrokenPipe));
    }

    /// §5.3: an `io::Error`'s `Display` carries the filename, which is exactly
    /// the leak this design exists to close. Converting one keeps the kind and
    /// drops everything else — there is no field on [`SinkError`] a path could
    /// survive in.
    #[test]
    fn converting_an_io_error_drops_the_filename_it_names() {
        let io = std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "/home/ada/.stella/private/store.db: permission denied",
        );
        let error = SinkError::from(io);
        assert_eq!(error, SinkError::Io(std::io::ErrorKind::PermissionDenied));
        assert!(
            !error.to_string().contains("ada"),
            "the path survived: {error}"
        );
    }

    #[test]
    fn a_file_sink_writes_owner_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("stella.jsonl");
        let sink = JsonlSink::file(&path).expect("open");
        sink.write(&a_record()).expect("write");
        sink.flush().expect("flush");

        let text = std::fs::read_to_string(&path).expect("read");
        assert_eq!(text.lines().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "a log file is owner-only");
        }
    }

    /// Terminal rendering must not include provenance notes, only the reviewed
    /// value. The note carries reviewer justification, not runtime information,
    /// and belongs in the file/debug sink, not in user-facing run output.
    #[test]
    fn text_sink_omits_provenance_notes_from_reviewed_fields() {
        use crate::Redacted;

        let shared = Shared::default();
        let sink = TextSink::to_writer(shared.clone());

        let record = Record::at(
            1_754_006_400_123,
            Level::Warn,
            "agent.step.usage_incomplete",
            "stella::diag_bridge",
            Cx::new(),
            Fields::new()
                .with(
                    "provider",
                    Redacted::reviewed(
                        "openrouter".to_owned(),
                        crate::note!("a provider/model identifier chosen by the operator"),
                    ),
                )
                .with("reason", "timeout")
                .with("duration_ms", 10002_u64),
        );

        sink.write(&record).expect("write");
        let output = shared.text();

        // The rendered line should contain the reviewed value and the signal fields
        assert!(output.contains("provider=openrouter"), "{output}");
        assert!(output.contains("reason=timeout"), "{output}");
        assert!(output.contains("duration_ms=10002"), "{output}");

        // But it must NOT contain the provenance note
        assert!(
            !output.contains("chosen by the operator"),
            "the note text leaked into terminal output: {output}"
        );
    }
}
