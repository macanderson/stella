//! The plain surface's transcript — the character-grid renderer, streamed onto
//! scrollback as the turn happens.
//!
//! # One rendering, not two
//!
//! This surface is append-only: it writes into the user's own scrollback and
//! can never revisit a line it has printed. That used to be read as "a frame
//! cannot be streamed", because [`stella_transcript::grid`]'s turn frame put
//! the turn's status and accounting on its *top* rail and neither exists when
//! the turn opens. So the frame was emitted once, at `TurnComplete`, and a
//! terse `· {tool}` progress row was printed per call in the meantime and then
//! erased.
//!
//! That produced two renderings of one turn, and the user saw the worse one
//! for the entire run — minutes of `· bash` on a terminal, while the very same
//! code printed the real transcript to a daemon's console file, where nobody
//! was watching it happen. It also could not erase correctly: the cursor-up
//! that removed the progress rows was unbounded, so a turn with more tool calls
//! than the terminal had rows walked off the top of the screen and cleared
//! whatever was there instead.
//!
//! The frame is streamable now — its status and chips moved to the closing
//! rail, which is the only place they are knowable
//! ([`grid::render_turn_lines`]'s contract, pinned by
//! `render_turn_is_append_only_as_a_turn_grows`). So there is one rendering:
//! the transcript itself, written a line at a time as the turn produces it. A
//! terminal and a log file receive the same bytes, which is the property the
//! two used to differ on most visibly.
//!
//! # What "as the turn produces it" means
//!
//! Only lines that can never change are written, which is
//! [`RunBuilder::settled`]'s job: a tool call reaches scrollback when its
//! result does, not when it dispatches, because the row carries its duration,
//! output fold and cost. The closing rail is withheld until the turn ends for
//! the same reason.

use std::io::Write;

use stella_protocol::AgentEvent;
use stella_transcript::{FoldState, Run, Status, Zoom, grid};
use stella_tui::transcript_build::RunBuilder;

/// The width a frame is drawn to when the terminal will not say.
const FALLBACK_WIDTH: usize = 100;

/// Widest frame we will draw. A very wide terminal spreads a digest row's
/// chips so far from its verb that the row stops reading as one thing.
const MAX_WIDTH: usize = 120;

/// Accumulates a turn and writes its frame to scrollback as it settles.
pub struct TranscriptPrinter {
    builder: RunBuilder,
    /// Lines of the turn currently open that are already on scrollback.
    emitted: usize,
    /// Turns written in full, so a frame is never written twice.
    printed_turns: usize,
    /// The column count every frame is drawn to, resolved once.
    ///
    /// Read at construction rather than per line, because a streamed frame is
    /// the one thing that cannot survive a mid-turn resize: the rails and the
    /// chip column of the lines already written are fixed, and re-measuring
    /// would close a frame at a width its own top rail was not drawn to. A
    /// pinned width means a resize takes effect on the next run instead of
    /// tearing the one in progress.
    width: usize,
}

impl TranscriptPrinter {
    /// A printer for a run, measuring the terminal once.
    #[must_use]
    pub fn new(name: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            builder: RunBuilder::new(name, model),
            emitted: 0,
            printed_turns: 0,
            width: width(),
        }
    }

    /// Whether the caller asked for the legacy line-by-line card rendering.
    #[must_use]
    pub fn streaming_requested() -> bool {
        std::env::var("STELLA_PLAIN_STREAM").is_ok_and(|v| v == "1")
    }

    /// Open a turn around the user's prompt.
    pub fn start_turn(&mut self, prompt: &str) {
        self.builder.start_turn(prompt);
        self.emitted = 0;
    }

    /// Fold one event in and write whatever it settled.
    pub fn observe(&mut self, event: &AgentEvent) {
        let text = self.observed(event);
        write(&text);
    }

    /// Close the open turn and write every line not yet written.
    pub fn flush(&mut self) {
        let text = self.flushed();
        write(&text);
    }

    // The two above are the whole I/O surface of this module: everything that
    // decides *what* to print is below and returns it, so a test can read the
    // scrollback this surface would produce without capturing a file
    // descriptor (invariant #2's discipline, one crate out).

    /// What [`Self::observe`] would write.
    fn observed(&mut self, event: &AgentEvent) -> String {
        self.builder.push(event);
        if matches!(event, AgentEvent::TurnComplete { .. }) {
            return self.flushed();
        }
        // A reasoning or text delta can settle nothing — `settled` withholds
        // the block each one appends to — so re-rendering the turn for one is
        // pure work. They are also the two events that arrive at token rate,
        // which is what makes the exclusion worth stating rather than leaving
        // to the suffix arithmetic to discover.
        if matches!(
            event,
            AgentEvent::TextDelta { .. } | AgentEvent::Reasoning { .. }
        ) {
            return String::new();
        }
        self.advance()
    }

    /// What [`Self::flush`] would write.
    fn flushed(&mut self) -> String {
        self.builder.finish_turn(Status::Ok);
        let run = self.builder.snapshot();
        let mut out = String::new();
        for index in self.printed_turns..run.turns.len() {
            // The first unwritten turn is the one that was open, so its already
            // streamed prefix is skipped; any later turn is written whole.
            let from = if index == self.printed_turns {
                self.emitted
            } else {
                0
            };
            append(&mut out, &frame(&run, index, self.width), from);
        }
        self.printed_turns = run.turns.len();
        self.emitted = 0;
        out
    }

    /// The lines the open turn has settled since the last call.
    fn advance(&mut self) -> String {
        if !self.builder.is_turn_open() {
            return String::new();
        }
        let run = self.builder.settled();
        let Some(index) = run.turns.len().checked_sub(1) else {
            return String::new();
        };
        let lines = frame(&run, index, self.width);
        // The closing rail is withheld while the turn is open: it carries the
        // status and the cost, and a turn that is still running has neither.
        let body = &lines[..lines.len().saturating_sub(1)];
        let mut out = String::new();
        append(&mut out, body, self.emitted);
        self.emitted = body.len();
        out
    }
}

/// One turn's frame, as ANSI lines.
///
/// Rendered one turn at a time rather than through `grid::render`, which draws
/// the whole run and appends a keybinding footer. Both are wrong here: the
/// earlier turns are already on scrollback and cannot be redrawn, and the
/// footer describes a fold cursor this surface does not have.
fn frame(run: &Run, index: usize, width: usize) -> Vec<String> {
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Steps);
    grid::render_turn_lines(run, &state, index, width)
        .iter()
        .map(|line| grid::to_ansi256(std::slice::from_ref(line)))
        .collect()
}

/// Push `lines[from..]` onto `out`, one per row.
fn append(out: &mut String, lines: &[String], from: usize) {
    for line in lines.iter().skip(from) {
        out.push_str(line);
        out.push('\n');
    }
}

/// One write per batch. A turn can settle several lines at once, and a partial
/// line reaching a reader that is tailing the file would tear the frame.
fn write(text: &str) {
    if text.is_empty() {
        return;
    }
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

/// The width to draw at: the terminal's, clamped, or a fixed fallback when
/// nothing can say (a pipe, a log file, a console file).
fn width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .filter(|c| *c > 0)
        .unwrap_or(FALLBACK_WIDTH)
        .clamp(40, MAX_WIDTH)
}

#[cfg(test)]
mod tests;
