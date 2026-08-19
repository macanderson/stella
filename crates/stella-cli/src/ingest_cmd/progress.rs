//! The live line `stella ingest` shows while it is working.
//!
//! Extraction is minutes of work on a real instruction file — this repository's
//! own `AGENTS.md` took 5m18s across two calls, and chunking a long document
//! buys one paid call per slice on top of that. The command used to print the
//! filename and then nothing at all for that whole window, which is
//! indistinguishable from a wedged process. The failure that costs something is
//! the recovery: the first thing a person does to a frozen terminal is Ctrl-C,
//! and here that abandons calls already paid for that were about to succeed.
//!
//! So the wait is narrated, and the narration *moves*: the label is owned by the
//! caller and changes as the work does — which slice is in flight, which record
//! is being processed, how many are left. A ticker that only counts seconds
//! proves the process is alive; one that names what it is doing also proves it
//! is making progress, which is the question a person actually has.
//!
//! Two rhythms, because a terminal and a log file want opposite things:
//!
//! - On a terminal the line rewrites itself in place once a second.
//! - Anywhere else — a pipe, CI, `NO_COLOR` — a fresh line is printed at most
//!   every [`HEARTBEAT`], because a carriage return into a log file is noise but
//!   *silence* is the thing that reads as failure. It used to print once and
//!   stay put, so a piped run went quiet for the entire call.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use colored::Colorize;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

/// How often the elapsed line redraws on a terminal. One second is the slowest
/// tick that still reads as *running* rather than stopped.
const TICK: Duration = Duration::from_secs(1);

/// How often a non-terminal run prints a fresh status line.
///
/// Five seconds is the longest a person will watch an unchanging screen before
/// concluding the process is stuck, and the shortest interval that does not turn
/// a CI log into a flipbook of one command.
const HEARTBEAT: Duration = Duration::from_secs(5);

/// A running progress line, owned for the length of one stretch of work.
///
/// Construct with [`Progress::start`], rename with [`Progress::say`] as the work
/// moves, and always end with [`Progress::finish`]. The call site keeps that
/// pairing structural — `finish` runs on the statement after the `await`, before
/// the result is inspected — so no error path can leave a ticker drawing over
/// later output.
pub(super) struct Progress {
    /// The text the ticker is currently drawing. Shared so the work can rename
    /// the line without stopping and restarting it, which on a terminal would
    /// reset the elapsed clock the reader is using to judge whether to wait.
    label: Arc<Mutex<String>>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
    /// Whether this process redraws in place, decided once at construction so
    /// the answer cannot change mid-line.
    drawing: bool,
}

impl Progress {
    /// Begin narrating `label`.
    ///
    /// Must be called from inside a Tokio runtime: the ticker is a spawned task
    /// so that it is polled while the caller awaits the provider. Extraction's
    /// current-thread runtime drives it on the same thread, which is why the
    /// line keeps moving through a multi-minute call without a second thread —
    /// and why any long stretch of *synchronous* work must yield (see
    /// [`Progress::say`]).
    pub(super) fn start(label: String) -> Self {
        let drawing = drawing();
        let shared = Arc::new(Mutex::new(label));
        let label = Arc::clone(&shared);
        let (stop, mut rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let started = Instant::now();
            let interval = if drawing { TICK } else { HEARTBEAT };
            let mut ticker = tokio::time::interval(interval);
            // The first tick fires immediately, so the line appears at 0s
            // rather than an interval into a wait that is already too quiet.
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    _ = ticker.tick() => {
                        let text = line(&read(&label), started.elapsed());
                        if drawing {
                            redraw(&text);
                        } else {
                            println!("{text}");
                        }
                    }
                }
            }
            if drawing {
                erase();
            }
        });
        Self {
            label: shared,
            stop: Some(stop),
            task: Some(task),
            drawing,
        }
    }

    /// Rename the line: what the process is doing *now*.
    ///
    /// Cheap and non-blocking — it swaps a string the ticker reads on its next
    /// tick, and never draws anything itself. A caller inside a long synchronous
    /// loop must still yield to the runtime between calls, or the ticker task
    /// (single-threaded here) never gets to run and the newest label is the only
    /// one anyone sees.
    pub(super) fn say(&self, label: String) {
        if let Ok(mut held) = self.label.lock() {
            *held = label;
        }
    }

    /// Print something durable without stopping the line.
    ///
    /// A retry, a slice that failed — the things a reader needs in scrollback
    /// after the run, not just while it is happening. Clears the ticker's row
    /// first so the note does not land on top of a half-drawn line; the ticker
    /// reclaims a fresh row on its next tick.
    pub(super) fn note(&self, text: &str) {
        if self.drawing {
            print!("\r\x1b[2K");
        }
        println!("{text}");
        let _ = std::io::stdout().flush();
    }

    /// Stop narrating and leave the cursor on a clean line.
    ///
    /// Awaits the ticker so the erase has landed before the caller prints its
    /// own result — otherwise the two race for the same row.
    pub(super) async fn finish(mut self) {
        if let Some(stop) = self.stop.take() {
            // A closed receiver means the task already exited; there is nothing
            // to act on either way.
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

/// Read the shared label, tolerating a poisoned lock — a progress line is not
/// worth a panic, and the previous text is a fine thing to draw.
fn read(label: &Arc<Mutex<String>>) -> String {
    label
        .lock()
        .map(|held| held.clone())
        .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
}

/// Should this process redraw a line in place?
///
/// Delegates to the shared predicate so `NO_COLOR`, `STELLA_NO_ANIM` and a
/// non-TTY stdout are decided in one place. `stella ingest` carries no
/// `--no-anim` flag of its own, and `false` says exactly that: the flag was not
/// given.
fn drawing() -> bool {
    crate::term_policy::animation_enabled(false)
}

/// The line's text, without the cursor control that positions it.
///
/// Split out from [`redraw`] because it is the whole of what a reader sees and
/// the only part worth pinning in a test.
fn line(label: &str, elapsed: Duration) -> String {
    format!("  {} {}", label.dimmed(), human(elapsed).dimmed())
}

/// Whole seconds under a minute, `<m>m<ss>s` above it.
///
/// Zero-padded seconds so the field stops jittering in width as it counts —
/// `1m09s` then `1m10s`, never `1m9s` then `1m10s`.
fn human(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

/// Rewrite the current row: carriage return, clear to end of line, print.
fn redraw(text: &str) {
    print!("\r\x1b[2K{text}");
    let _ = std::io::stdout().flush();
}

/// Leave the row empty and the cursor at its start.
fn erase() {
    print!("\r\x1b[2K");
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_reads_as_seconds_below_a_minute() {
        assert_eq!(human(Duration::from_secs(0)), "0s");
        assert_eq!(human(Duration::from_secs(59)), "59s");
    }

    #[test]
    fn elapsed_reads_as_minutes_and_padded_seconds_above_one() {
        assert_eq!(human(Duration::from_secs(60)), "1m00s");
        assert_eq!(human(Duration::from_secs(69)), "1m09s");
        assert_eq!(human(Duration::from_secs(318)), "5m18s");
    }

    /// The width of the elapsed field must not change as it counts, or the line
    /// visibly jitters on every tick that crosses a ten.
    #[test]
    fn elapsed_field_width_is_stable_across_a_minute() {
        let widths: std::collections::BTreeSet<usize> = (60..120)
            .map(|s| human(Duration::from_secs(s)).len())
            .collect();
        assert_eq!(widths.len(), 1, "elapsed field changed width: {widths:?}");
    }

    /// The rendered line carries the label and the elapsed time. Colour is
    /// stripped in a non-TTY test process, so this asserts on the text.
    #[test]
    fn line_carries_the_label_and_the_elapsed_time() {
        let rendered = line("extracting AGENTS.md", Duration::from_secs(318));
        assert!(rendered.contains("extracting AGENTS.md"), "{rendered}");
        assert!(rendered.contains("5m18s"), "{rendered}");
    }

    /// Neither rhythm may exceed five seconds of silence: that is the interval
    /// at which an unchanging screen starts reading as a hang.
    #[test]
    fn neither_rhythm_goes_quiet_for_more_than_five_seconds() {
        assert!(TICK <= Duration::from_secs(5));
        assert!(HEARTBEAT <= Duration::from_secs(5));
    }

    /// The witness for the renaming half: the line the ticker draws follows the
    /// caller's latest `say`, so the narration can name the record in hand
    /// rather than only counting seconds.
    #[tokio::test]
    async fn the_line_follows_the_latest_label() {
        let progress = Progress::start("extracting AGENTS.md".to_string());
        progress.say("processing pkg-manager — 48 of 141".to_string());
        let shown = line(&read(&progress.label), Duration::from_secs(7));
        assert!(
            shown.contains("processing pkg-manager — 48 of 141"),
            "{shown}"
        );
        assert!(
            !shown.contains("extracting"),
            "stale label still drawn: {shown}"
        );
        progress.finish().await;
    }
}
