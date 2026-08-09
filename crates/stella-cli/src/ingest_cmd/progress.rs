//! The elapsed-time line `stella ingest` shows while a model call is in flight.
//!
//! Extraction is one provider call per document, and on a real instruction file
//! it runs for minutes — this repository's own `AGENTS.md` took 5m18s across two
//! calls. The command printed the filename and then nothing at all for that
//! whole window, which is indistinguishable from a wedged process. The failure
//! that costs something is the recovery: the first thing a person does to a
//! frozen terminal is Ctrl-C, and here that abandons a call already paid for
//! that was about to succeed.
//!
//! So the wait is narrated instead. On a terminal the line rewrites itself in
//! place once a second; anywhere else — a pipe, CI, `NO_COLOR` — it prints once
//! and stays put, because a carriage return into a log file is noise rather
//! than progress.

use std::io::Write;
use std::time::{Duration, Instant};

use colored::Colorize;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

/// How often the elapsed line redraws on a terminal. One second is the slowest
/// tick that still reads as *running* rather than stopped.
const TICK: Duration = Duration::from_secs(1);

/// A running progress line, owned for the length of exactly one model call.
///
/// Construct with [`Progress::start`] and always end with [`Progress::finish`].
/// The call site keeps that pairing structural — `finish` runs on the statement
/// after the `await`, before the result is inspected — so no error path can
/// leave a ticker drawing over later output.
pub(super) struct Progress {
    /// `None` when this process is not drawing (non-TTY), which makes
    /// [`Progress::finish`] a no-op rather than a special case.
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl Progress {
    /// Begin narrating `label`.
    ///
    /// Must be called from inside a Tokio runtime: the ticker is a spawned task
    /// so that it is polled while the caller awaits the provider. Extraction's
    /// current-thread runtime drives it on the same thread, which is why the
    /// line keeps moving through a multi-minute call without a second thread.
    pub(super) fn start(label: String) -> Self {
        if !drawing() {
            println!("  {}", format!("{label} …").dimmed());
            return Self {
                stop: None,
                task: None,
            };
        }
        let (stop, mut rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let started = Instant::now();
            let mut ticker = tokio::time::interval(TICK);
            // The first tick fires immediately, so the line appears at 0s
            // rather than a second into a wait that is already too quiet.
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    _ = ticker.tick() => draw(&line(&label, started.elapsed())),
                }
            }
            erase();
        });
        Self {
            stop: Some(stop),
            task: Some(task),
        }
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

/// Should this process redraw a line in place?
///
/// Delegates to the cinematic's predicate so `NO_COLOR`, `STELLA_NO_ANIM` and a
/// non-TTY stdout are decided in one place. `stella ingest` carries no
/// `--no-anim` flag of its own, and `false` says exactly that: the flag was not
/// given.
fn drawing() -> bool {
    crate::init_fx::animation_enabled(false)
}

/// The line's text, without the cursor control that positions it.
///
/// Split out from [`draw`] because it is the whole of what a reader sees and
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
fn draw(text: &str) {
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
}
