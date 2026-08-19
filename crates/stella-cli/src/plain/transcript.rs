//! The plain surface's transcript frames — the character-grid renderer, on
//! scrollback.
//!
//! # The tension this module resolves
//!
//! [`stella_transcript::grid::render`] draws a *whole* turn: the header rail
//! states the turn's step count, wall time and cost, and the frame closes with
//! a matching bottom rail. None of those numbers exist until the turn ends. The
//! plain surface, by contrast, is append-only — it writes into the user's own
//! scrollback and can never revisit a line it has printed (see the parent
//! module's header). A streamed frame is therefore not merely awkward, it is
//! unrepresentable: the top rail would have to be printed before its own
//! contents are known.
//!
//! So the frame is emitted **once, when the turn completes**, and what happens
//! before that depends on who is watching:
//!
//! - **On a terminal**, a terse progress line per tool call is printed while
//!   the turn runs, then erased immediately before the frame replaces it. Each
//!   is truncated to the terminal width so it occupies exactly one physical
//!   row — the cursor-up erase is only correct if the count of rows printed
//!   equals the count of lines printed, which wrapping would break.
//! - **Anywhere else** — a log file, a supervised child's console file, the
//!   bytes `stella daemon logs` replays — nothing is printed until the frame.
//!   A reader of those bytes arrives after the fact and wants the transcript,
//!   not a progress trace that the frame below then repeats.
//!
//! `STELLA_PLAIN_STREAM=1` restores the pre-frame line-by-line rendering for a
//! caller that genuinely wants tokens as they arrive.

use std::io::{IsTerminal, Write};

use stella_protocol::AgentEvent;
use stella_transcript::{FoldState, Run, Status, Zoom, grid};
use stella_tui::transcript_build::RunBuilder;

/// The width a frame is drawn to when the terminal will not say.
const FALLBACK_WIDTH: usize = 100;

/// Widest frame we will draw. A very wide terminal spreads a digest row's
/// chips so far from its verb that the row stops reading as one thing.
const MAX_WIDTH: usize = 120;

/// Accumulates a turn and prints its frame when it closes.
pub struct TranscriptPrinter {
    builder: RunBuilder,
    /// How many progress rows are on screen awaiting erasure. Always `0` when
    /// not writing to a terminal.
    progress_rows: usize,
    interactive: bool,
    /// How many turns have been printed, so a frame is never re-printed.
    printed_turns: usize,
}

impl TranscriptPrinter {
    /// A printer for a run, deciding once whether a human is watching.
    #[must_use]
    pub fn new(name: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            builder: RunBuilder::new(name, model),
            progress_rows: 0,
            interactive: std::io::stdout().is_terminal(),
            printed_turns: 0,
        }
    }

    /// Whether the caller asked for the legacy line-by-line rendering.
    #[must_use]
    pub fn streaming_requested() -> bool {
        std::env::var("STELLA_PLAIN_STREAM").is_ok_and(|v| v == "1")
    }

    /// Open a turn around the user's prompt.
    pub fn start_turn(&mut self, prompt: &str) {
        self.builder.start_turn(prompt);
    }

    /// Fold one event in, and show progress if a human is watching.
    pub fn observe(&mut self, event: &AgentEvent) {
        self.builder.push(event);
        if let AgentEvent::ToolStart { call } = event {
            self.progress(&call.name);
        }
        if matches!(event, AgentEvent::TurnComplete { .. }) {
            self.flush();
        }
    }

    /// Close the open turn and print every frame not yet printed.
    pub fn flush(&mut self) {
        self.builder.finish_turn(Status::Ok);
        self.erase_progress();
        let run = self.builder.snapshot();
        for index in self.printed_turns..run.turns.len() {
            print!("{}", frame(&run, index, width()));
        }
        self.printed_turns = run.turns.len();
        let _ = std::io::stdout().flush();
    }

    fn progress(&mut self, tool: &str) {
        if !self.interactive {
            return;
        }
        // Truncated so the row cannot wrap; see the module doc on why the
        // erase depends on that.
        let mut line = format!("  · {tool}");
        line.truncate(width());
        println!("{line}");
        self.progress_rows += 1;
    }

    fn erase_progress(&mut self) {
        if self.progress_rows == 0 {
            return;
        }
        // Up N rows, then clear from the cursor to the end of the screen.
        print!("\x1b[{}A\x1b[J", self.progress_rows);
        self.progress_rows = 0;
    }
}

/// One turn's frame, as ANSI text ending in a newline.
///
/// Rendered from a run holding only that turn, because `grid::render` draws
/// every turn it is given and this surface has already printed the earlier
/// ones. The trailing status line `grid::render` appends is dropped for the
/// same reason: it is a whole-run footer, and printing "1 turns" under every
/// frame would state something false about the session.
fn frame(run: &Run, index: usize, width: usize) -> String {
    let Some(turn) = run.turns.get(index) else {
        return String::new();
    };
    let single = Run {
        name: run.name.clone(),
        model: run.model.clone(),
        started_at: run.started_at.clone(),
        turns: vec![turn.clone()],
    };
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Steps);
    let mut lines = grid::render(&single, &state, width);
    lines.pop();
    let body = grid::to_ansi256(&lines);
    if body.is_empty() {
        body
    } else {
        format!("{body}\n")
    }
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
