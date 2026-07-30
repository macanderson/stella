//! Startup system notifications, shown as a transient dialog.
//!
//! **The transcript is the home for agent and user messages only.** A system
//! notification — "a previous session is resumable", the code-graph index
//! totals, an `mcp.toml` that was not trusted — is neither, so it must never
//! become a transcript row. Before this module the driver fabricated an
//! `AgentEvent::Text` for each one (`command_deck::chrome_note`), which made
//! the deck render machine chatter as though the agent had said it, and left
//! it in the scrollback for the rest of the session.
//!
//! They surface here instead: one small centered dialog that stands for
//! [`DWELL`] past its most recent line and then gets out of the way.
//! Dismissal is deliberately cheap and total — any key, any mouse event, or
//! simply waiting.
//!
//! Two behaviours are worth stating because they are choices, not accidents:
//!
//! * **The dwell restarts on every new line.** Startup notices do not all
//!   arrive at once: the code-graph totals land seconds after launch, once the
//!   background index pass finishes, and MCP connect results later still. A
//!   timer anchored to the *first* line would expire before the only line most
//!   users actually want. Anchoring to the *latest* line means the dialog is
//!   readable for a full [`DWELL`] after the last thing it has to say.
//! * **An explicit dismiss is final for the session.** Once the user has
//!   waved it away, a late-arriving line does not pop it back up — a dialog
//!   that reappears after you dismissed it is worse than one that never
//!   showed. Late lines are dropped rather than queued.
//!
//! Like [`crate::splash`], the visible state is a pure function of elapsed
//! time, so two renders at the same instant produce byte-identical buffers.

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

use crate::render::row::wrap_one_indent;
use crate::theme;

/// How long the dialog stands after its most recent line. Long enough to read
/// a two-line summary, short enough that nobody reaches for the keyboard to
/// clear it.
pub const DWELL: Duration = Duration::from_secs(3);

/// The dialog never grows past this, however wide the terminal is — long
/// measures are hard to read, and this is glanceable text.
const MAX_WIDTH: u16 = 72;

/// Column the headline of each notice starts at.
const HEAD_INDENT: usize = 2;

/// Column the detail clauses (and every wrap continuation) start at. The step
/// in from [`HEAD_INDENT`] is what makes a multi-clause notice scannable
/// instead of a run-on sentence.
const DETAIL_INDENT: usize = 6;

/// Ephemeral dialog state. Not part of the model — pure presentation, exactly
/// like [`crate::splash::SplashState`].
#[derive(Debug, Clone, Default)]
pub struct NoticeState {
    /// Notices in arrival order, each still the driver's raw one-line string.
    /// Splitting and wrapping happen at render time, where the frame width is
    /// known.
    entries: Vec<String>,
    /// When the most recent line landed — the dwell is measured from here, not
    /// from the first line (see the module docs).
    last_at: Option<Instant>,
    /// Set by [`Self::dismiss`]. Final for the session.
    dismissed: bool,
}

impl NoticeState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one system notification. A no-op once dismissed, and blank text
    /// is dropped rather than rendered as an empty row.
    pub fn push(&mut self, text: impl Into<String>) {
        if self.dismissed {
            return;
        }
        let text = text.into();
        if text.trim().is_empty() {
            return;
        }
        self.entries.push(text);
        self.last_at = Some(Instant::now());
    }

    /// Dismiss now — any key, any mouse event. Idempotent, and permanent for
    /// the session: later notices will not reopen the dialog.
    pub fn dismiss(&mut self) {
        self.dismissed = true;
    }

    /// Whether the dialog should be on screen this frame.
    pub fn is_visible(&self) -> bool {
        if self.dismissed || self.entries.is_empty() {
            return false;
        }
        self.last_at.is_some_and(|at| at.elapsed() < DWELL)
    }

    /// The notices collected so far, newest last. Exposed for tests and for
    /// any surface that wants to re-show them (nothing does yet).
    pub fn entries(&self) -> &[String] {
        &self.entries
    }
}

/// Break one run-on notice into a headline plus indented detail clauses.
///
/// Every notice the driver emits shares a shape: a short claim, then
/// em-dash-separated elaboration, sometimes with a trailing parenthetical of
/// counts. `"✓ code graph: 14157 symbols, 5991 imports across 640 files (150
/// re-parsed, 490 unchanged this pass) — skipped 2 generated files"` becomes
/// the claim, then the counts, then the skip note — three scannable rows
/// instead of one sentence that runs off the edge.
///
/// Splitting happens **here, at the presentation edge**, rather than by
/// reshaping the driver's format strings: the same strings are the CLI's
/// single-line stdout for `stella init`, where one line is correct.
pub(crate) fn split_clauses(text: &str) -> (String, Vec<String>) {
    let mut clauses = text
        .split(" — ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let Some(head) = clauses.next() else {
        return (String::new(), Vec::new());
    };
    let mut details: Vec<String> = clauses.collect();
    // A trailing "(…)" on the headline is a parenthetical of counts, not part
    // of the claim — lift it to its own row so the claim reads clean. Only
    // when something precedes it: a notice that is *entirely* parenthesised
    // has no claim to separate it from.
    if let Some(rest) = head.strip_suffix(')')
        && let Some(open) = rest.rfind('(')
        && !rest[..open].trim().is_empty()
    {
        let inner = rest[open + 1..].trim().to_string();
        let claim = rest[..open].trim().to_string();
        if !inner.is_empty() {
            details.insert(0, inner);
            return (claim, details);
        }
    }
    (head, details)
}

/// Lay one notice out as a headline row plus one row per detail clause, each
/// wrapped with a hanging indent so continuations line up under their clause
/// rather than under the left border.
fn notice_lines(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    // The driver's multi-line notices (the MCP outcome report) are already
    // structured; treat each of their lines as its own notice.
    for source in text.lines().map(str::trim).filter(|s| !s.is_empty()) {
        let (head, details) = split_clauses(source);
        if !head.is_empty() {
            let line = Line::from(vec![
                Span::raw(" ".repeat(HEAD_INDENT)),
                Span::styled(head, theme::heading()),
            ]);
            wrap_one_indent(line, width, DETAIL_INDENT, &mut out);
        }
        for detail in details {
            let line = Line::from(vec![
                Span::raw(" ".repeat(DETAIL_INDENT)),
                Span::styled(detail, theme::muted()),
            ]);
            wrap_one_indent(line, width, DETAIL_INDENT, &mut out);
        }
    }
    out
}

/// Draw the dialog. A no-op when it is not visible, so callers can call it
/// unconditionally.
///
/// A pure function of `(state.entries, state.last_at, area)` — two calls at
/// the same instant write byte-identical buffers.
pub fn render(state: &NoticeState, area: Rect, buf: &mut Buffer) {
    if !state.is_visible() || area.width == 0 || area.height == 0 {
        return;
    }
    let outer_w = area.width.min(MAX_WIDTH);
    // Two columns of border, and the text is laid out against what is left.
    let inner_w = outer_w.saturating_sub(2) as usize;
    if inner_w == 0 {
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, entry) in state.entries.iter().enumerate() {
        // A blank row between notices, never before the first or after the
        // last — the dialog should not open or close on empty space.
        if i > 0 {
            lines.push(Line::default());
        }
        lines.extend(notice_lines(entry, inner_w));
    }
    if lines.is_empty() {
        return;
    }

    // Height is content-driven, capped to the frame. When startup produced
    // more than fits, keep the TAIL: the newest line is the one that just
    // restarted the dwell, so it is what the user is being shown for.
    let max_inner = area.height.saturating_sub(2) as usize;
    if max_inner == 0 {
        return;
    }
    if lines.len() > max_inner {
        lines.drain(..lines.len() - max_inner);
    }
    let outer_h = (lines.len() as u16).saturating_add(2).min(area.height);

    let popup = Rect {
        x: area.x + (area.width.saturating_sub(outer_w)) / 2,
        y: area.y + (area.height.saturating_sub(outer_h)) / 2,
        width: outer_w,
        height: outer_h,
    };
    Clear.render(popup, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::accent())
        .title(" startup · any key dismisses ");
    let inner = block.inner(popup);
    block.render(popup, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    Paragraph::new(lines).render(inner, buf);
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    /// The four notices from a real launch, verbatim — the run-on sentence
    /// this module exists to break up.
    const GRAPH: &str = "✓ code graph: 14157 symbols, 5991 imports across 640 files \
                         (150 re-parsed, 490 unchanged this pass) — skipped 2 generated files";
    const RESUMABLE: &str = "◂ a previous session is resumable — ← (on an empty prompt) opens \
                             SESSIONS, ⏎ reopens one; or run `stella resume`.";

    fn buffer_rows(buf: &Buffer) -> Vec<String> {
        let area = *buf.area();
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect()
    }

    fn draw(state: &NoticeState, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| render(state, f.area(), f.buffer_mut()))
            .unwrap();
        buffer_rows(terminal.backend().buffer())
    }

    fn has_non_space(rows: &[String]) -> bool {
        rows.iter().any(|row| row.chars().any(|c| c != ' '))
    }

    /// Backdates `last_at` so the dwell reads as expired without a real sleep
    /// — the same trick `splash::tests::state_at` uses, and legal for the same
    /// reason (this module owns the private fields).
    fn aged(entries: &[&str], ms: u64) -> NoticeState {
        NoticeState {
            entries: entries.iter().map(|s| s.to_string()).collect(),
            last_at: Some(Instant::now() - Duration::from_millis(ms)),
            dismissed: false,
        }
    }

    #[test]
    fn a_fresh_notice_shows_and_expires_after_the_dwell() {
        assert!(aged(&[GRAPH], 0).is_visible());
        assert!(aged(&[GRAPH], DWELL.as_millis() as u64 - 100).is_visible());
        assert!(!aged(&[GRAPH], DWELL.as_millis() as u64 + 100).is_visible());
    }

    #[test]
    fn the_dwell_is_three_seconds() {
        assert_eq!(DWELL, Duration::from_secs(3));
    }

    #[test]
    fn an_empty_notice_state_is_invisible() {
        assert!(!NoticeState::new().is_visible());
        assert!(!has_non_space(&draw(&NoticeState::new(), 80, 24)));
    }

    #[test]
    fn blank_text_is_dropped_rather_than_shown_as_an_empty_row() {
        let mut state = NoticeState::new();
        state.push("   ");
        state.push("\n");
        assert!(state.entries().is_empty());
        assert!(!state.is_visible());
    }

    #[test]
    fn any_key_dismisses_immediately() {
        let mut state = aged(&[GRAPH], 0);
        assert!(state.is_visible());
        state.dismiss();
        assert!(!state.is_visible());
        assert!(
            !has_non_space(&draw(&state, 80, 24)),
            "a dismissed dialog must leave the frame to the deck"
        );
    }

    /// The late-arrival rule: the code-graph totals land seconds after launch,
    /// so a new line must reopen the dwell window rather than inherit an
    /// almost-expired one.
    #[test]
    fn a_new_line_restarts_the_dwell() {
        let mut state = aged(&[RESUMABLE], DWELL.as_millis() as u64 + 500);
        assert!(!state.is_visible(), "the first line has aged out");
        state.push(GRAPH);
        assert!(state.is_visible(), "the new line reopens the window");
    }

    /// …but not past an explicit dismiss. A dialog that pops back up after you
    /// waved it away is worse than one that never showed.
    #[test]
    fn a_dismissed_dialog_stays_shut_when_a_late_notice_lands() {
        let mut state = aged(&[RESUMABLE], 0);
        state.dismiss();
        state.push(GRAPH);
        assert!(!state.is_visible());
        assert_eq!(
            state.entries().len(),
            1,
            "a late notice is dropped, not queued behind the dismiss"
        );
    }

    #[test]
    fn the_run_on_graph_line_splits_into_claim_counts_and_skips() {
        let (head, details) = split_clauses(GRAPH);
        assert_eq!(
            head,
            "✓ code graph: 14157 symbols, 5991 imports across 640 files"
        );
        assert_eq!(
            details,
            vec![
                "150 re-parsed, 490 unchanged this pass".to_string(),
                "skipped 2 generated files".to_string(),
            ]
        );
    }

    #[test]
    fn the_resumable_line_splits_claim_from_its_keybindings() {
        let (head, details) = split_clauses(RESUMABLE);
        assert_eq!(head, "◂ a previous session is resumable");
        assert_eq!(details.len(), 1);
        assert!(details[0].starts_with('←'));
    }

    #[test]
    fn a_notice_with_no_em_dash_is_all_headline() {
        let (head, details) = split_clauses("◈ indexing code graph…");
        assert_eq!(head, "◈ indexing code graph…");
        assert!(details.is_empty());
    }

    /// Guard on the parenthetical lift: with nothing before the "(" there is
    /// no claim to separate, so the text stays whole.
    #[test]
    fn a_wholly_parenthesised_notice_keeps_its_text() {
        let (head, details) = split_clauses("(nothing to report)");
        assert_eq!(head, "(nothing to report)");
        assert!(details.is_empty());
    }

    #[test]
    fn details_are_indented_further_than_their_headline() {
        let lines = notice_lines(GRAPH, 68);
        let rendered: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        assert!(
            rendered[0].starts_with(&" ".repeat(HEAD_INDENT)),
            "headline sits at the head column: {:?}",
            rendered[0]
        );
        assert!(
            !rendered[0].starts_with(&" ".repeat(DETAIL_INDENT)),
            "headline is NOT at the detail column: {:?}",
            rendered[0]
        );
        for row in &rendered[1..] {
            assert!(
                row.starts_with(&" ".repeat(DETAIL_INDENT)),
                "detail rows are stepped in: {row:?}"
            );
        }
    }

    #[test]
    fn the_dialog_draws_its_content_inside_a_border() {
        let rows = draw(&aged(&[GRAPH, RESUMABLE], 0), 80, 24);
        let text = rows.join("\n");
        assert!(text.contains("code graph"), "graph notice renders:\n{text}");
        assert!(text.contains("resumable"), "resume notice renders:\n{text}");
        assert!(
            text.contains('┌') && text.contains('┘'),
            "bordered:\n{text}"
        );
        assert!(
            text.contains("any key dismisses"),
            "the dismiss affordance is stated:\n{text}"
        );
    }

    /// The property the whole design rests on: same state, same frame.
    #[test]
    fn two_renders_at_the_same_instant_are_byte_identical() {
        let state = aged(&[GRAPH, RESUMABLE], 40);
        assert_eq!(draw(&state, 80, 24), draw(&state, 80, 24));
    }

    #[test]
    fn more_notices_than_fit_keep_the_newest() {
        let many: Vec<String> = (0..40).map(|i| format!("notice number {i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let rows = draw(&aged(&refs, 0), 80, 12);
        let text = rows.join("\n");
        assert!(
            text.contains("notice number 39"),
            "newest survives:\n{text}"
        );
        assert!(
            !text.contains("notice number 0\n"),
            "oldest is clipped:\n{text}"
        );
        for row in &rows {
            assert_eq!(row.chars().count(), 80, "no row overflows the frame");
        }
    }

    #[test]
    fn render_does_not_panic_at_any_size() {
        for &(w, h) in &[
            (80_u16, 24_u16),
            (44, 14),
            (30, 5),
            (0, 0),
            (1, 1),
            (3, 3),
            (2, 2),
            (200, 60),
        ] {
            let _ = draw(&aged(&[GRAPH, RESUMABLE], 0), w, h);
            let _ = draw(&NoticeState::new(), w, h);
        }
    }
}
