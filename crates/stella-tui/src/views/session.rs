//! Session tab — the focused agent's REPL surface: identity header, the stat
//! box with its vitals gauges, any pending gate card, the transcript, and the
//! right-hand rail carrying PLAN over DONE VERIFICATION
//! ([`crate::views::plan_rail`]).
//!
//! It **reuses** the shared renderers (`render_hud`, `render_transcript`,
//! `render_ask_user`, `entry_lines`), just scoped to whichever agent
//! `ui.focused` points at. No transcript rendering is duplicated — there is
//! one implementation of "draw a session". (The scope gate renders as the
//! modal plan-review dialog, [`crate::views::scope_dialog`], not as a band.)

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use crate::deck::{AgentEntry, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::model::{FileState, TranscriptEntry};
use crate::render::{
    entry_lines, inner_height, inner_width, reasoning_is_live, render_ask_user, render_hud,
    render_transcript_window, streaming_lines,
};
use crate::theme;
use crate::transcript_nav::TurnDigest;

/// Which turns render as a one-line digest this frame, and which entries that
/// hides.
///
/// Computed fresh each frame from the transcript and the deck's fold sets, and
/// summarized into `signature` so the incremental cache invalidates the moment
/// the *set* of folded turns changes — which happens without any keypress when
/// a turn finishes while the fold-all overlay is on.
#[derive(Debug, Clone, Default)]
pub struct FoldPlan {
    /// Turn-start entry index → the digest rendered in the whole turn's place.
    digests: HashMap<usize, TurnDigest>,
    /// Entries swallowed by a folded turn (its start excluded).
    hidden: HashSet<usize>,
    signature: u64,
}

impl FoldPlan {
    /// Build the plan for `transcript` given a predicate for "is this turn
    /// folded", which the deck supplies so the exception-list semantics of
    /// `ctrl+z` live in one place.
    pub fn build(
        transcript: &[TranscriptEntry],
        is_folded: impl Fn(&crate::transcript_nav::Turn) -> bool,
    ) -> Self {
        use std::hash::{Hash, Hasher};
        let mut plan = FoldPlan::default();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for turn in crate::transcript_nav::turns(transcript) {
            if !is_folded(&turn) {
                continue;
            }
            turn.rows.start.hash(&mut hasher);
            turn.rows.end.hash(&mut hasher);
            plan.digests.insert(
                turn.rows.start,
                crate::transcript_nav::digest(transcript, &turn),
            );
            plan.hidden.extend(turn.rows.start + 1..turn.rows.end);
        }
        plan.signature = hasher.finish();
        plan
    }

    /// Whether entry `idx` renders at all.
    fn hides(&self, idx: usize) -> bool {
        self.hidden.contains(&idx)
    }
}

/// The memo key for the frame-to-frame [`FoldPlan`] cache
/// (`DeckUi::session_plan`): focused agent, transcript length, cumulative
/// front-eviction count, and the deck's fold revision.
///
/// Together these cover every input the plan reads, so the whole-transcript
/// walk in [`FoldPlan::build`] runs once per change instead of once per
/// frame. Appends and front-eviction are the only transcript mutations that
/// move turn boundaries, finish a turn, or alter a digest — streaming deltas
/// only coalesce into the tail entry's *text*, which no digest term reads —
/// and every fold-state mutation bumps `fold_rev` (including the eviction
/// pass dropping a stale fold set).
pub type FoldPlanKey = (String, usize, usize, u64);

/// The one line a folded turn renders as.
///
/// It has to answer "can I skip this?" without being unfolded, so it carries
/// what the turn was asked to do and what it cost: tool calls, distinct files
/// touched, elapsed time — and, in the danger colour, whether anything in
/// there failed. A turn with a failure count is the one a reader opens.
fn digest_line(d: &TurnDigest, folded: bool, width: usize) -> Vec<Line<'static>> {
    let chevron = if folded { "▸" } else { "▾" };
    let mut metric: Vec<Span<'static>> = Vec::new();
    if d.failures > 0 {
        metric.push(Span::styled(
            format!("{} failed · ", d.failures),
            Style::new().fg(theme::BAD),
        ));
    }
    let mut parts: Vec<String> = Vec::new();
    if d.tools > 0 {
        parts.push(format!("{} tools", d.tools));
    }
    if d.files > 0 {
        parts.push(format!("{} files", d.files));
    }
    if d.duration_ms > 0 {
        parts.push(crate::render::human_duration(d.duration_ms));
    }
    metric.push(Span::styled(
        parts.join(" · "),
        Style::new().fg(theme::MUTED),
    ));
    let left = vec![Span::styled(
        d.prompt.clone(),
        Style::new().fg(theme::VIOLET),
    )];
    let mut spans = vec![Span::styled(
        format!("{chevron} "),
        Style::new().fg(theme::VIOLET).add_modifier(Modifier::BOLD),
    )];
    spans.extend(crate::render::justify(left, metric, width, 2));
    vec![Line::from(spans)]
}

/// Everything the settled prefix was folded under. Any change invalidates it,
/// so each term is a thing that can silently alter an already-rendered row:
/// the agent, thinking/expand-all overlays and their revision, the pane width,
/// how many entries have been evicted off the front, the file-mutation count
/// (an inline diff can go stale without anything being appended), the set of
/// folded turns (which changes on its own when a turn finishes under the
/// fold-all overlay), and how many leading entries have moved into the
/// terminal's scrollback (accessible mode — those must stop being drawn).
type FoldKey = (String, bool, bool, u64, usize, usize, u64, u64, usize);

/// Incremental transcript fold for the Session tab.
///
/// Everything before the last entry is *settled* — streaming deltas only ever
/// mutate the final entry — so settled entries fold (markdown, labels, wrap)
/// exactly once and are cached with their visual-row ranges; only the tail
/// entry re-folds per frame. The cache invalidates whole when anything that
/// changes how settled entries render changes: focused agent, the thinking
/// toggle, a ctrl+o expansion, the pane width, or the retention cap evicting
/// a chunk of the front (which shifts every retained index). This turns the
/// old O(whole-history) fold per frame into O(tail) — typing latency no
/// longer grows with session length.
#[derive(Debug, Clone, Default)]
pub struct SessionFold {
    key: Option<FoldKey>,
    settled: usize,
    prefix: Vec<Line<'static>>,
    entry_rows: Vec<Range<usize>>,
    tail: Vec<Line<'static>>,
}

impl SessionFold {
    /// Bring the cache up to date for this frame. `expand_all` is the
    /// no-selection ctrl+o overlay: every expandable entry folds as if
    /// individually expanded (it participates in the cache key, so toggling
    /// it invalidates exactly once).
    /// `streaming` is the in-flight `TextDelta` preview
    /// ([`SessionModel::streaming_text`](crate::model::SessionModel)): folded
    /// into the live tail after the last entry, so it re-wraps per frame
    /// like the tail does and vanishes without residue when the
    /// authoritative `Text` clears it — never a settled entry.
    #[allow(clippy::too_many_arguments)]
    fn refresh(
        &mut self,
        agent: &str,
        transcript: &[TranscriptEntry],
        files: &[FileState],
        streaming: &str,
        thinking: bool,
        expanded: &HashSet<usize>,
        expand_all: bool,
        expanded_rev: u64,
        width: usize,
        plan: &FoldPlan,
        flushed: usize,
    ) {
        // Front-eviction shifts every retained index, so the settled prefix
        // no longer describes the entries now occupying 0..settled. The
        // marker's cumulative count grows on every pass, so keying on it
        // invalidates exactly when the front moves — the shrink check alone
        // misses an eviction whose survivors still outnumber `settled`.
        let evicted = match transcript.first() {
            Some(TranscriptEntry::Evicted { count }) => *count,
            _ => 0,
        };
        // A settled tool result's inline diff resolves against `files` at
        // fold time, and a later mutation can stale it (freshness gate in
        // `entry_lines`) without appending anything — the total mutation
        // count is the only fingerprint that moves, so it keys the cache.
        let file_gen: u64 = files.iter().map(|f| u64::from(f.changes)).sum();
        let key = (
            agent.to_string(),
            thinking,
            expand_all,
            expanded_rev,
            width,
            evicted,
            file_gen,
            plan.signature,
            flushed,
        );
        // Invalidation CLEARS the key; the commit happens after the fold loop
        // below. A panic inside `entry_lines` would otherwise leave `prefix`
        // extended with no matching `entry_rows` range and no `settled` bump,
        // and the next frame — key still matching — would resume at the same
        // index and double-append. With the commit moved after the loop, a
        // caught panic leaves `key = None` and the cache rebuilds from zero
        // (see `crate::panel_guard` for why that matters).
        if self.key.as_ref() != Some(&key) || self.settled > transcript.len().saturating_sub(1) {
            self.key = None;
            self.settled = 0;
            self.prefix.clear();
            self.entry_rows.clear();
        }
        let target = transcript.len().saturating_sub(1);
        while self.settled < target {
            let i = self.settled;
            let start = self.prefix.len();
            if i < flushed {
                // Already ordinary terminal output above this pane
                // (`accessible::Scrollback`). Drawing it again would have a
                // reader hear the conversation twice — once as it arrived in
                // scrollback, once as part of a pane that repaints. It still
                // gets a (zero-width) row range so every index-keyed
                // affordance — selection, search, scroll-into-view — stays
                // aligned with the transcript rather than shifting by
                // however much has been flushed.
                self.entry_rows.push(start..start);
                self.settled += 1;
                continue;
            }
            if let Some(d) = plan.digests.get(&i) {
                self.prefix.extend(digest_line(d, true, width));
            } else if plan.hides(i) {
                // Swallowed by the turn above. It still gets a row range —
                // the digest's — so that selecting, scrolling to, or
                // searching a hidden entry lands on the line standing in for
                // it rather than on nothing.
                let digest_rows = self
                    .entry_rows
                    .iter()
                    .rev()
                    .find(|r| !r.is_empty())
                    .cloned()
                    .unwrap_or(start..start);
                self.entry_rows.push(digest_rows);
                self.settled += 1;
                continue;
            } else {
                entry_lines(
                    &transcript[i],
                    files,
                    thinking,
                    expand_all || expanded.contains(&i),
                    false,
                    width,
                    &mut self.prefix,
                );
            }
            // The tearing window: `prefix` is extended, `entry_rows` is not.
            #[cfg(test)]
            crate::panel_guard::fail_if_armed("session fold");
            self.entry_rows.push(start..self.prefix.len());
            self.settled += 1;
        }
        self.key = Some(key);
        self.tail.clear();
        if let Some(last) = transcript.last() {
            // The tail obeys the same plan: a last entry inside a folded turn
            // must not reappear below the digest that already stands for it.
            if let Some(d) = plan.digests.get(&target) {
                self.tail.extend(digest_line(d, true, width));
            } else if !plan.hides(target) {
                entry_lines(
                    last,
                    files,
                    thinking,
                    expand_all || expanded.contains(&target),
                    reasoning_is_live(transcript, streaming),
                    width,
                    &mut self.tail,
                );
            }
        }
        streaming_lines(streaming, files, thinking, width, &mut self.tail);
    }

    /// Total visual rows (settled prefix + live tail).
    pub fn total(&self) -> usize {
        self.prefix.len() + self.tail.len()
    }

    /// The visual-row range entry `idx` occupies (the live tail entry spans
    /// everything past the prefix).
    pub fn rows_of(&self, idx: usize) -> Range<usize> {
        if idx < self.entry_rows.len() {
            self.entry_rows[idx].clone()
        } else {
            self.prefix.len()..self.total()
        }
    }

    /// Materialize just the rows in `window` — ≤ one viewport of clones.
    fn window_lines(&self, window: Range<usize>) -> Vec<Line<'static>> {
        window
            .filter_map(|r| {
                if r < self.prefix.len() {
                    self.prefix.get(r).cloned()
                } else {
                    self.tail.get(r - self.prefix.len()).cloned()
                }
            })
            .collect()
    }
}

pub fn render(model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    let Some(agent) = model.agents.get(ui.focused) else {
        empty_state(area, buf);
        return;
    };
    let sm = &agent.model;

    // Each pending gate claims its own band (0 = collapsed). Both can be
    // pending at once — nothing clears one when the other arrives — so they
    // render independently, band by band; an
    // ask-user question is never hidden behind a plan review.
    //
    // (The scope gate is the exception: it renders as the modal plan-review
    // dialog over the whole frame — `views::scope_dialog`, drawn by
    // `deck_render` — so it claims no band here.)
    let ask_h: u16 = match &sm.pending_ask_user {
        Some(p) => (p.options.len() as u16 + 5).min(12),
        None => 0,
    };
    let hunk_h: u16 = match &sm.pending_hunk_review {
        Some(p) => crate::render::hunk_review_height(p.hunks.len()),
        None => 0,
    };
    // The routing card: raised when a prompt arrives mid-turn and the deck
    // will not guess where it goes. Above the transcript with the other
    // gates, because it is the same kind of thing — work parked on an answer.
    let dispatch_h: u16 = if ui.pending_dispatch.is_some() { 7 } else { 0 };

    // The nested subagent blocks (D5), directly under the lead's identity
    // header. 0 when there are none or the focused lane is itself a
    // subagent. Full-width rows, so in accessible mode nothing here ever
    // shares a rendered row with the transcript.
    let subs_h = crate::views::subagents::band_height(model, ui);

    let hud_h = crate::render::HUD_H;

    // PLAN and DONE VERIFICATION live in the right-hand rail
    // ([`crate::views::plan_rail`]) and are up for the whole session. Too
    // narrow for a column — or accessible mode, where a side-by-side column
    // reads as interleaved lines (#1258) — stacks them above the transcript
    // instead, sized against the rows the gates left. Nothing is dropped.
    let split_rail = crate::views::plan_rail::rail_visible(ui.accessible, area.width);
    let claimed = 1 + subs_h + hud_h + ask_h + hunk_h + dispatch_h;
    let (stacked_plan_h, stacked_verify_h) = if split_rail {
        (0, 0)
    } else {
        crate::views::plan_rail::stacked_heights(area.height.saturating_sub(claimed))
    };

    let bands = Layout::vertical([
        Constraint::Length(1),                // identity header
        Constraint::Length(subs_h),           // nested subagent rows (0 = none)
        Constraint::Length(hud_h),            // stat box (+1 row for the vitals gauges)
        Constraint::Length(ask_h),            // pending ask-user (0 = collapsed)
        Constraint::Length(hunk_h),           // pending hunk review (0 = collapsed)
        Constraint::Length(dispatch_h),       // mid-turn routing (0 = collapsed)
        Constraint::Length(stacked_plan_h),   // PLAN, when stacked (0 = in the rail)
        Constraint::Min(1),                   // transcript (+ right rail on a wide frame)
        Constraint::Length(stacked_verify_h), // DONE VERIFICATION, when stacked
    ])
    .split(area);

    // On a frame that can afford it, the two panels move sideways into a
    // quarter-width column: vertical rows are the scarce resource on a
    // terminal and columns usually are not, so the rail costs the transcript
    // some right-margin clipping instead of costing it whole rows.
    let (transcript_area, rail_area) = if split_rail {
        let cols = Layout::horizontal([
            Constraint::Min(1),
            Constraint::Length(crate::views::plan_rail::rail_width(area.width)),
        ])
        .split(bands[7]);
        (cols[0], Some(cols[1]))
    } else {
        (bands[7], None)
    };

    render_header(agent, model.now_ms, bands[0], buf);
    if subs_h > 0 {
        crate::views::subagents::render(model, ui, bands[1], buf);
    }
    render_hud(&sm.hud, bands[2], buf);
    if let Some(prompt) = &sm.pending_ask_user {
        let answered = ui.ask_answered.contains(&agent.meta.id);
        render_ask_user(prompt, answered, bands[3], buf);
    }
    if let Some(proposal) = &sm.pending_hunk_review {
        let answered = ui.hunk_answered.contains(&agent.meta.id);
        crate::render::render_hunk_review(
            proposal,
            ui.hunk_marks.get(&agent.meta.id),
            answered,
            bands[4],
            buf,
        );
    }
    if let Some(pending) = &ui.pending_dispatch {
        crate::views::dispatch_card::render(pending, bands[5], buf);
    }
    let animate = !ui.no_anim;
    match rail_area {
        Some(rail) => crate::views::plan_rail::render(sm, model.now_ms, animate, rail, buf),
        None => {
            // Stacked: same two panels, same order, full width.
            crate::views::plan_rail::render_plan(&sm.plan, model.now_ms, animate, bands[6], buf);
            crate::views::plan_rail::render_verification(&sm.proof, bands[8], buf);
        }
    }

    // Transcript: fold through the incremental cache (settled entries fold
    // once; only the streaming tail re-folds per frame), then reuse the
    // line-exact scroll window over the cached rows.
    let width = inner_width(transcript_area);
    let empty = HashSet::new();
    let expanded_set = ui.expanded.get(&agent.meta.id).unwrap_or(&empty);
    // The plan is memoized on [`FoldPlanKey`] (taken out of `ui` so the
    // rebuild closure below can still read `ui`): the whole-transcript walk
    // only reruns when something it reads actually changed.
    let plan_key: FoldPlanKey = (
        agent.meta.id.clone(),
        sm.transcript.len(),
        sm.evicted_entries(),
        ui.fold_rev,
    );
    let plan = match ui.session_plan.take() {
        Some((key, plan)) if key == plan_key => plan,
        _ => FoldPlan::build(&sm.transcript, |turn| {
            crate::deck_ui::is_folded(ui, &agent.meta.id, turn)
        }),
    };
    // In accessible mode the leading entries are already in the terminal's
    // scrollback; the pane must skip exactly those, or the conversation is
    // announced twice. Zero on every ordinary session.
    let flushed = ui.scrollback.flushed_for(&agent.meta.id);
    ui.session_fold.refresh(
        &agent.meta.id,
        &sm.transcript,
        &sm.files,
        &sm.streaming_text,
        ui.thinking_expanded,
        expanded_set,
        ui.transcript_expand_all,
        ui.expanded_rev,
        width,
        &plan,
        flushed,
    );
    ui.session_plan = Some((plan_key, plan));
    let height = inner_height(transcript_area);
    let total = ui.session_fold.total();

    // A selection move from the key handler lands here, where visual-row
    // ranges are known: nudge the scroll window until the entry is visible,
    // then pin it. Follow drops even when no nudge was needed — a streaming
    // tail must not slide the highlight out of view (↓ past the tail, or
    // scrolling back to the bottom, re-arms follow).
    if let Some(sel) = ui.session_pending_scroll.take() {
        let rows = ui.session_fold.rows_of(sel);
        let current = ui.session_scroll.window(total, height);
        ui.session_scroll.top = if rows.start < current.start {
            rows.start
        } else if rows.end > current.end {
            rows.end.saturating_sub(height)
        } else {
            current.start
        };
        ui.session_scroll.follow = false;
    }

    ui.metrics.session_total = total;
    ui.metrics.session_height = height;
    let window = ui.session_scroll.window(total, height);
    let mut visible = ui.session_fold.window_lines(window.clone());
    if let Some(sel) = ui.session_selected {
        // A quiet warm background lift on the selected entry's rows.
        for r in ui.session_fold.rows_of(sel) {
            if window.contains(&r)
                && let Some(line) = visible.get_mut(r - window.start)
            {
                line.style = line.style.bg(theme::SELECT_BG);
            }
        }
    }
    // Every occurrence of a live query is lit inside the rows on screen.
    // Done here on the materialized window rather than inside the fold cache
    // so the query never becomes a cache-key term — typing must not rebuild
    // the whole transcript on every keystroke.
    if ui.search.open && !ui.search.query.is_empty() {
        highlight_matches(&mut visible, &ui.search.query);
    }
    // Contextual help, keyed to the transcript's interaction state: every
    // mode advertises its own way out (ctrl+o/Esc collapse the expand-all
    // overlay, Esc clears a highlight) and the resting state teaches the
    // scroll verbs (↑ scrolls; ⌘/⌃ ] and ⌘/⌃ [ jump to the ends).
    let hint = if ui.search.open {
        // A search bar with no match count is a search bar you cannot trust.
        let pos = if ui.search.hits.is_empty() {
            if ui.search.query.is_empty() {
                "type to search".to_string()
            } else {
                "no matches".to_string()
            }
        } else {
            format!("{}/{}", ui.search.cursor + 1, ui.search.hits.len())
        };
        format!(
            "find: {}▏ · {pos} · ⏎ next · ⌃P prev · Esc close",
            ui.search.query
        )
    } else if ui.fold_all_turns {
        "turns folded · ⌃Z or Esc unfolds · ⌃F find".to_string()
    } else if ui.transcript_expand_all {
        "all expanded · ⌃O or Esc collapses".to_string()
    } else if ui.session_selected.is_some() {
        "⌃O expand · ⌃Z fold turn · ⌃N next failure · Esc clears".to_string()
    } else {
        // `↑` selects the newest message, it does not scroll — the scroll
        // verbs are the page keys (see `handle_session_key`, which claims
        // ↑/↓ for the highlight whenever the transcript has any entries).
        //
        // `/plan` is advertised here rather than only in help because the rail
        // is a *summary*: a reader who wants a step's full text has to be
        // told, once, that there is a way to get it.
        "↑ select · ⇞⇟ scroll · ⌃F find · ⌃N failure · ⌃Z fold · ⌃O expand · /plan".to_string()
    };
    render_transcript_window(
        visible,
        window,
        total,
        ui.session_scroll.follow,
        Some(&hint),
        transcript_area,
        buf,
    );
}

/// Light every occurrence of `query` inside the rows on screen.
///
/// Splitting spans at the match keeps the row's own colours — a hit inside a
/// diff stays a diff line, a hit in a path stays a path — so the highlight
/// reads as an annotation over the transcript rather than as a different
/// rendering of it. Matching is done on each span's text, which is what the
/// reader can actually see.
fn highlight_matches(lines: &mut [Line<'static>], query: &str) {
    // Lowered once for the whole pass. Doing it per span allocated twice for
    // every span of every visible row on every keystroke, which is the kind of
    // cost that turns an incremental search into a laggy one.
    let needle = query.to_lowercase();
    for line in lines.iter_mut() {
        if !line
            .spans
            .iter()
            .any(|s| s.content.to_lowercase().contains(&needle))
        {
            continue;
        }
        let mut rebuilt: Vec<Span<'static>> = Vec::with_capacity(line.spans.len());
        for span in std::mem::take(&mut line.spans) {
            let ranges = crate::transcript_nav::match_ranges(&span.content, query);
            if ranges.is_empty() {
                rebuilt.push(span);
                continue;
            }
            let text = span.content.to_string();
            let mut at = 0usize;
            for r in ranges {
                if r.start > at {
                    rebuilt.push(Span::styled(text[at..r.start].to_string(), span.style));
                }
                rebuilt.push(Span::styled(
                    text[r.clone()].to_string(),
                    span.style.bg(theme::MATCH_BG).add_modifier(Modifier::BOLD),
                ));
                at = r.end;
            }
            if at < text.len() {
                rebuilt.push(Span::styled(text[at..].to_string(), span.style));
            }
        }
        line.spans = rebuilt;
    }
}

/// The one-line identity header: `▶ lead · running   0:00:00`. The trailing
/// slot is the **per-turn wall clock** — live while a turn is in flight, else
/// the last turn's held duration (zero before any turn), formatted `h:mm:ss`
/// and always present.
fn render_header(agent: &AgentEntry, now_ms: u64, area: Rect, buf: &mut Buffer) {
    let st = agent.status;
    let line = Line::from(vec![
        Span::styled(
            format!(" {} ", theme::status_glyph(st)),
            Style::new().fg(theme::status_color(st)),
        ),
        Span::styled(agent.meta.id.clone(), theme::accent()),
        Span::styled("  ·  ", theme::rule()),
        Span::styled(
            st.label().to_string(),
            Style::new().fg(theme::status_color(st)),
        ),
        Span::raw("   "),
        // The clock is a reading, not a brand mark — dim it so the header's
        // one accent is the agent's name.
        Span::styled(
            fmt_hms(agent.turn_clock_ms(now_ms)),
            Style::new().fg(theme::TEXT_TERTIARY),
        ),
    ]);
    Paragraph::new(line).render(area, buf);
}

/// Format a millisecond duration as `h:mm:ss` — hours un-padded, minutes and
/// seconds zero-padded to two digits. Drives the per-turn header clock.
fn fmt_hms(ms: u64) -> String {
    let secs = ms / 1000;
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    format!("{h}:{m:02}:{s:02}")
}

/// Shown when there are no agents at all.
fn empty_state(area: Rect, buf: &mut Buffer) {
    if area.height == 0 {
        return;
    }
    let mid = Rect {
        x: area.x,
        y: area.y + area.height / 2,
        width: area.width,
        height: 1,
    };
    Paragraph::new(Span::styled(
        "no active session — type a prompt and press Enter to dispatch one",
        theme::muted(),
    ))
    .alignment(Alignment::Center)
    .render(mid, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{AgentMeta, Inbound};
    use crate::model::{MAX_TRANSCRIPT_ENTRIES, SessionModel};
    use stella_protocol::{AgentEvent, ScopeProposal, TaskItem, TaskStatus};

    /// Flatten a `Buffer` to plain text (content, not ANSI — the crate-wide
    /// render-test convention).
    fn buffer_text(buf: &Buffer) -> String {
        let area = *buf.area();
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn both_pending_gates_render_at_once() {
        // Nothing clears one gate when the other arrives, so both can be
        // pending simultaneously — and both must be visible, or the user has
        // no way to see (let alone answer) the hidden one.
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::ScopeReview {
                proposal: ScopeProposal {
                    summary: "widen the refactor".into(),
                    steps: vec![],
                    estimated_files: 3,
                    estimated_cost_usd: None,
                    ..Default::default()
                },
            },
        });
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::AskUser {
                id: "q1".into(),
                question: "Which database should the cache use?".into(),
                options: vec!["sqlite".into(), "redis".into()],
            },
        });

        let mut ui = DeckUi::default();
        let area = Rect::new(0, 0, 90, 40);
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);

        // The ask-user gate renders as its band here; the scope gate renders
        // as the deck-level plan-review dialog — pending simultaneously, so
        // neither surface hides the other.
        let text = buffer_text(&buf);
        assert!(
            text.contains("Which database should the cache use?"),
            "ask-user card visible alongside the plan review:\n{text}"
        );
        assert!(
            crate::views::scope_dialog::pending(&model, &ui).is_some(),
            "the plan-review dialog is up over the deck at the same time"
        );
    }

    #[test]
    fn answered_gate_cards_flip_to_their_sent_state() {
        // The pending gates clear only on the engine's follow-on events; the
        // per-agent answered latches must flip the cards to "sent" in the
        // meantime, or they keep advertising keys that would double-submit.
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::ScopeReview {
                proposal: ScopeProposal {
                    summary: "widen the refactor".into(),
                    steps: vec![],
                    estimated_files: 3,
                    estimated_cost_usd: None,
                    ..Default::default()
                },
            },
        });
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::AskUser {
                id: "q1".into(),
                question: "Which database should the cache use?".into(),
                options: vec!["sqlite".into(), "redis".into()],
            },
        });

        let mut ui = DeckUi::default();
        ui.scope_answered.insert("lead".into());
        ui.ask_answered.insert("lead".into());
        let area = Rect::new(0, 0, 90, 40);
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);

        let text = buffer_text(&buf);
        assert!(
            crate::views::scope_dialog::pending(&model, &ui).is_none(),
            "the answered plan-review dialog drops, so its keys stop firing"
        );
        assert!(
            text.contains("answer sent — awaiting engine…"),
            "ask-user card reads answered:\n{text}"
        );
    }

    #[test]
    fn the_fold_plan_rebuilds_only_when_its_key_moves() {
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        model.apply_inbound(&Inbound::PromptStarted {
            agent: "lead".into(),
            text: "one".into(),
        });
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::Complete {
                model: "m".into(),
                cost_usd: 0.0,
            },
        });

        let mut ui = DeckUi::default();
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        let key = ui.session_plan.as_ref().expect("plan cached").0.clone();

        // Mark the cached plan (an index that never renders); an unchanged
        // frame must hand the SAME plan back rather than rebuild.
        ui.session_plan
            .as_mut()
            .unwrap()
            .1
            .digests
            .insert(usize::MAX, crate::transcript_nav::TurnDigest::default());
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        let cached = ui.session_plan.as_ref().unwrap();
        assert_eq!(cached.0, key);
        assert!(
            cached.1.digests.contains_key(&usize::MAX),
            "an unchanged frame reuses the cached plan"
        );

        // A fold-state bump rebuilds it…
        ui.fold_rev += 1;
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        let rebuilt = ui.session_plan.as_ref().unwrap();
        assert!(
            !rebuilt.1.digests.contains_key(&usize::MAX),
            "a fold toggle moves the key and rebuilds the plan"
        );

        // …and so does a transcript append.
        let key_after = rebuilt.0.clone();
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::Text { delta: "hi".into() },
        });
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        assert_ne!(
            ui.session_plan.as_ref().unwrap().0,
            key_after,
            "an append moves the key"
        );
    }

    #[test]
    fn streaming_preview_renders_in_the_session_tab_until_the_text_replaces_it() {
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        for fragment in ["streamed toke", "ns arriving"] {
            model.apply_inbound(&Inbound::Event {
                agent: "lead".into(),
                event: AgentEvent::TextDelta {
                    text: fragment.into(),
                },
            });
        }

        let mut ui = DeckUi::default();
        let area = Rect::new(0, 0, 90, 24);
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(
            text.contains("streamed tokens arriving"),
            "deltas render live, before any Text event:\n{text}"
        );

        // The authoritative Text replaces the preview — never a duplicate.
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::Text {
                delta: "streamed tokens arriving".into(),
            },
        });
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert_eq!(
            text.matches("streamed tokens arriving").count(),
            1,
            "the committed answer appears exactly once:\n{text}"
        );
    }

    #[test]
    fn fold_cache_stays_line_exact_across_front_eviction() {
        // The dangerous shape: the cache settles on a SHORT prefix, then the
        // transcript grows past the cap so a chunk of the front evicts while
        // the survivor count still exceeds `settled` — the shrink check alone
        // cannot see it, only the eviction-count key can.
        let mut model = SessionModel::new();
        let expanded = HashSet::new();
        let retry = |i: usize| AgentEvent::Retry {
            attempt: i as u32,
            reason: "r".into(),
        };
        for i in 0..1_000 {
            model.apply(&retry(i));
        }
        let mut fold = SessionFold::default();
        fold.refresh(
            "lead",
            &model.transcript,
            &model.files,
            "",
            false,
            &expanded,
            false,
            0,
            80,
            &FoldPlan::default(),
            0,
        );
        for i in 1_000..(MAX_TRANSCRIPT_ENTRIES + 50) {
            model.apply(&retry(i));
        }
        assert!(model.evicted_entries() > 0, "an eviction pass occurred");
        fold.refresh(
            "lead",
            &model.transcript,
            &model.files,
            "",
            false,
            &expanded,
            false,
            0,
            80,
            &FoldPlan::default(),
            0,
        );

        let mut fresh = SessionFold::default();
        fresh.refresh(
            "lead",
            &model.transcript,
            &model.files,
            "",
            false,
            &expanded,
            false,
            0,
            80,
            &FoldPlan::default(),
            0,
        );
        assert_eq!(fold.total(), fresh.total());
        assert_eq!(
            fold.window_lines(0..fold.total()),
            fresh.window_lines(0..fresh.total()),
            "the incrementally-maintained fold matches a from-scratch fold"
        );
    }

    #[test]
    fn a_later_mutation_leaves_the_settled_inline_diff_showing_its_own_change() {
        use stella_protocol::{FileChangeKind, ToolCall, ToolOutput};
        // A successful edit_file: ToolStart → FileChange (the engine's tap
        // fires during execution) → ToolResult. The result's inline diff
        // must render, and must go on rendering *the change that call made*
        // when the same path is edited again.
        //
        // It used to vanish instead: `FileState` kept one `latest_diff` and
        // the ref resolved only while `changes` still equalled the recorded
        // seq, so the second edit to a file silently stripped the first
        // edit's diff off its row. A session that touched one file five
        // times showed four edits with nothing under them. The path now
        // remembers its last `DIFF_HISTORY` diffs by seq, so each row
        // resolves its own — attribution, which was always the point, now
        // achieved by keeping the right diff rather than by dropping every
        // diff that might be the wrong one.
        //
        // The fold cache still keys on the total mutation count, because
        // eviction from that history *can* change how a settled entry
        // renders while nothing is appended to the transcript.
        let mut model = SessionModel::new();
        model.apply(&AgentEvent::ToolStart {
            call: ToolCall {
                call_id: "c1".into(),
                name: "edit_file".into(),
                input: serde_json::json!({"path": "src/x.rs"}),
            },
        });
        model.apply(&AgentEvent::FileChange {
            path: "src/x.rs".into(),
            kind: FileChangeKind::Modified,
            added: 1,
            removed: 0,
            diff: Some("@@ -1,1 +1,1 @@\n+first_diff_line".into()),
        });
        model.apply(&AgentEvent::ToolResult {
            call_id: "c1".into(),
            output: ToolOutput::Ok {
                content: "ok".into(),
            },
            duration_ms: 3,
            speculated: false,
        });
        let expanded = HashSet::new();
        let mut fold = SessionFold::default();
        fold.refresh(
            "lead",
            &model.transcript,
            &model.files,
            "",
            false,
            &expanded,
            false,
            0,
            120,
            &FoldPlan::default(),
            0,
        );
        let text: String = fold
            .window_lines(0..fold.total())
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.clone()))
            .collect();
        assert!(
            text.contains("first_diff_line"),
            "the fresh inline diff renders:\n{text}"
        );

        // The same path mutates again — no transcript append. The settled
        // result keeps the diff it produced and never adopts the new one.
        let len_before = model.transcript.len();
        model.apply(&AgentEvent::FileChange {
            path: "src/x.rs".into(),
            kind: FileChangeKind::Modified,
            added: 1,
            removed: 0,
            diff: Some("@@ -1,1 +1,1 @@\n+second_diff_line".into()),
        });
        assert_eq!(model.transcript.len(), len_before, "no transcript append");
        fold.refresh(
            "lead",
            &model.transcript,
            &model.files,
            "",
            false,
            &expanded,
            false,
            0,
            120,
            &FoldPlan::default(),
            0,
        );
        let text: String = fold
            .window_lines(0..fold.total())
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.clone()))
            .collect();
        assert!(
            text.contains("first_diff_line"),
            "the settled row still shows the change its own call made:\n{text}"
        );
        assert!(
            !text.contains("second_diff_line"),
            "and never adopts a later call's change:\n{text}"
        );

        // Past `DIFF_HISTORY` mutations the recorded seq falls off the end
        // of the ring and there is no diff left that belongs to this call.
        // The row degrades to naming its result — and the fold must refold
        // to show that, with the transcript still untouched.
        for i in 0..crate::model::DIFF_HISTORY {
            model.apply(&AgentEvent::FileChange {
                path: "src/x.rs".into(),
                kind: FileChangeKind::Modified,
                added: 0,
                removed: 0,
                diff: Some(format!("@@ -1,1 +1,1 @@\n+evicting_edit_{i}")),
            });
        }
        assert_eq!(model.transcript.len(), len_before, "still no append");
        fold.refresh(
            "lead",
            &model.transcript,
            &model.files,
            "",
            false,
            &expanded,
            false,
            0,
            120,
            &FoldPlan::default(),
            0,
        );
        let text: String = fold
            .window_lines(0..fold.total())
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.clone()))
            .collect();
        assert!(
            !text.contains("first_diff_line"),
            "an evicted diff stops rendering:\n{text}"
        );
        assert!(
            !text.contains("evicting_edit_"),
            "and is not replaced by a change the call never made:\n{text}"
        );

        // And the incrementally-invalidated fold stays line-exact.
        let mut fresh = SessionFold::default();
        fresh.refresh(
            "lead",
            &model.transcript,
            &model.files,
            "",
            false,
            &expanded,
            false,
            0,
            120,
            &FoldPlan::default(),
            0,
        );
        assert_eq!(
            fold.window_lines(0..fold.total()),
            fresh.window_lines(0..fresh.total()),
            "the mutation-count key refolds the settled prefix"
        );
    }

    #[test]
    fn applying_a_selection_pins_the_window_against_a_streaming_tail() {
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::Text {
                delta: "first-message".into(),
            },
        });

        // Select the (already fully visible) first entry.
        let mut ui = DeckUi {
            session_selected: Some(0),
            session_pending_scroll: Some(0),
            ..DeckUi::default()
        };
        let area = Rect::new(0, 0, 60, 12);
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        assert!(
            !ui.session_scroll.follow,
            "a selection pins the window even when no scroll nudge was needed"
        );

        // A streaming tail grows past the viewport — the pinned window must
        // keep the selected entry visible instead of following the tail.
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::Text {
                delta: "\nnoise".repeat(40),
            },
        });
        let mut buf2 = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf2);
        let text = buffer_text(&buf2);
        assert!(
            text.contains("first-message"),
            "selected entry still visible under streaming:\n{text}"
        );
    }

    fn task(id: &str, subject: &str, status: TaskStatus, owner: Option<&str>) -> TaskItem {
        TaskItem {
            id: id.into(),
            subject: subject.into(),
            description: None,
            status,
            owner: owner.map(str::to_string),
        }
    }

    /// The board reaches the frame as the rail's plan steps — every one, by
    /// name. The old strip showed `☑ 0/2 ▸ Fix the auth redirect` and
    /// suppressed the rest: a reader saw how many steps there were, never
    /// what they said.
    #[test]
    fn the_board_reaches_the_frame_as_named_plan_steps() {
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        let area = Rect::new(0, 0, 120, 24);

        // No plan: the panel is still up, and says so rather than vanishing.
        let mut ui = DeckUi::default();
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("PLAN"), "the panel is unconditional:\n{text}");
        assert!(text.contains("no plan yet"), "…and honest:\n{text}");

        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: stella_protocol::AgentEvent::TaskUpdate {
                tasks: vec![
                    task(
                        "1",
                        "Fix the auth redirect",
                        TaskStatus::InProgress,
                        Some("lead"),
                    ),
                    task("2", "Ship the fix", TaskStatus::Pending, None),
                ],
            },
        });
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(
            text.contains("working 0/2"),
            "the plan states itself:\n{text}"
        );
        assert!(text.contains("Fix the auth redirect"), "{text}");
        assert!(
            text.contains("Ship the fix"),
            "every step is listed, not just the active one:\n{text}"
        );

        // An empty snapshot clears the board.
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: stella_protocol::AgentEvent::TaskUpdate { tasks: vec![] },
        });
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        // Assert on the panel, not the bare subject: the board update is also
        // a transcript row, and that row legitimately survives a cleared
        // board — the scrollback is a record, the rail is a state.
        assert!(
            text.contains("no plan yet"),
            "a cleared board empties the rail:\n{text}"
        );
    }

    /// **The headline of this change.** On the terminal size the deck is most
    /// often run at, a turn triage waived must leave the transcript the room it
    /// used to lose to two panels with nothing to report.
    #[test]
    fn a_waived_turn_spends_no_rows_on_panels_that_have_nothing_to_say() {
        use stella_protocol::ProofStep;
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::Proof {
                step: ProofStep::Assurance {
                    witness: false,
                    verifier: false,
                },
            },
        });
        let area = Rect::new(0, 0, 80, 24);
        let mut ui = DeckUi::default();
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(
            text.contains("DONE VERIFICATION"),
            "the panel is unconditional — a waived turn is a report, not an \
             absence:\n{text}"
        );
        assert!(
            text.contains("waived"),
            "…and it says what triage decided:\n{text}"
        );
        // header(1) + stat box(3) = 4 of the 24 rows, and PLAN / DONE
        // VERIFICATION cost COLUMNS rather than rows. Before this change the
        // same turn spent 7 more rows on a band with nothing to report.
        assert_eq!(
            ui.metrics.session_height, 18,
            "the transcript must own every row the panels do not"
        );
    }

    /// The rail is columns, not rows — and it is up whenever there are columns
    /// to spare, not only when something has gone wrong.
    #[test]
    fn the_rail_is_up_wherever_the_frame_can_afford_a_column() {
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: stella_protocol::AgentEvent::TaskUpdate {
                tasks: vec![task(
                    "1",
                    "Fix the auth redirect",
                    TaskStatus::Pending,
                    None,
                )],
            },
        });

        // Wide: side by side.
        let wide = Rect::new(0, 0, 120, 24);
        let mut ui = DeckUi::default();
        let mut buf = Buffer::empty(wide);
        render(&model, &mut ui, wide, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("PLAN"), "{text}");
        assert!(text.contains("DONE VERIFICATION"), "{text}");
        assert!(
            text.contains("Fix the auth redirect"),
            "with the step named, not just a count:\n{text}"
        );

        // Too narrow for a column: the SAME two panels, stacked. The old rail
        // simply disappeared here, which taught readers that its absence meant
        // nothing was happening.
        let narrow = Rect::new(0, 0, 70, 30);
        let mut ui = DeckUi::default();
        let mut buf = Buffer::empty(narrow);
        render(&model, &mut ui, narrow, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("PLAN"), "stacked, never dropped:\n{text}");
        assert!(text.contains("DONE VERIFICATION"), "{text}");
        assert!(text.contains("Fix the auth redirect"), "{text}");
    }

    /// A settled turn of `tools` calls against `path`, `failures` of them
    /// failing, whose successful output is the distinctive `folded_away_body`.
    fn model_with_one_turn(prompt: &str, tools: usize, failures: usize) -> SessionModel {
        use stella_protocol::{ToolCall, ToolOutput};
        let mut m = SessionModel::new();
        m.push_user_prompt(prompt);
        for i in 0..tools {
            let id = format!("c{i}");
            m.apply(&AgentEvent::ToolStart {
                call: ToolCall {
                    call_id: id.clone(),
                    name: "run_command".into(),
                    input: serde_json::json!({ "path": "src/main.rs" }),
                },
            });
            m.apply(&AgentEvent::ToolResult {
                call_id: id,
                output: if i < failures {
                    ToolOutput::Error {
                        message: "exit status 1".into(),
                    }
                } else {
                    ToolOutput::Ok {
                        content: "folded_away_body".into(),
                    }
                },
                duration_ms: 20,
                speculated: false,
            });
        }
        m.apply(&AgentEvent::Complete {
            model: "m".into(),
            cost_usd: 0.0,
        });
        m
    }

    /// Fold `model` with every settled turn collapsed, and return the cache.
    fn folded(model: &SessionModel, fold_all: bool) -> SessionFold {
        let plan = FoldPlan::build(&model.transcript, |turn| fold_all && turn.finished);
        let mut fold = SessionFold::default();
        fold.refresh(
            "lead",
            &model.transcript,
            &model.files,
            "",
            false,
            &HashSet::new(),
            false,
            0,
            120,
            &plan,
            0,
        );
        fold
    }

    /// Flatten every row a fold holds to plain text.
    fn fold_text(fold: &SessionFold) -> String {
        fold.window_lines(0..fold.total())
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.clone())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_folded_turn_collapses_to_one_digest_line() {
        // The whole promise of ⌃Z is that a turn a reader has finished with
        // stops costing them screen: it must not merely gain a header, it
        // must *replace* everything it produced with one line that still
        // says what the turn was and what it cost.
        let model = model_with_one_turn("fix the auth redirect", 3, 0);
        let open = folded(&model, false);
        let shut = folded(&model, true);

        let text = fold_text(&shut);
        assert_eq!(shut.total(), 1, "the whole turn is one row now:\n{text}");
        assert!(
            text.contains("fix the auth redirect"),
            "the digest still names what was asked:\n{text}"
        );
        assert!(text.contains("3 tools"), "…and what it cost:\n{text}");
        assert!(
            !text.contains("folded_away_body"),
            "but the output it produced is gone:\n{text}"
        );
        assert!(
            open.total() > shut.total(),
            "and the row count actually collapsed ({} → {})",
            open.total(),
            shut.total()
        );
    }

    #[test]
    fn a_folded_turn_with_failures_flags_them_in_the_danger_colour() {
        // The failure count is what makes folding safe to use at all: a
        // reader skimming digests has to be able to tell which collapsed
        // turn is the one they must open, without opening any of them.
        let model = model_with_one_turn("run the suite", 3, 2);
        let fold = folded(&model, true);
        let lines = fold.window_lines(0..fold.total());
        let text = fold_text(&fold);
        assert!(text.contains("2 failed"), "the count is stated:\n{text}");

        let flagged = lines[0]
            .spans
            .iter()
            .find(|s| s.content.contains("2 failed"))
            .expect("a span carries the marker");
        assert_eq!(
            flagged.style.fg,
            Some(theme::BAD),
            "and carries the danger colour, not the muted metric one"
        );
    }

    #[test]
    fn a_hidden_entry_reports_the_digest_rows_that_stand_in_for_it() {
        // ↑/↓, ⌃N and search all address *entries*, and the renderer scrolls
        // to whatever row range the entry claims. An entry folded away has no
        // rows of its own, so it must borrow the digest's — an empty range
        // would scroll the reader to a row that is not there.
        let model = model_with_one_turn("fix the auth redirect", 3, 0);
        let fold = folded(&model, true);
        let digest_rows = fold.rows_of(0);
        assert!(!digest_rows.is_empty(), "the digest occupies real rows");
        // Entry 2 is a ToolResult swallowed by the fold (0 = prompt, 1 =
        // ToolStart), and not the tail entry — which has its own fallback.
        assert!(matches!(
            model.transcript[2],
            TranscriptEntry::ToolResult { .. }
        ));
        assert_eq!(
            fold.rows_of(2),
            digest_rows,
            "a hidden entry lands on the line standing in for it"
        );
    }

    #[test]
    fn highlighting_a_match_splits_the_span_without_editing_its_text() {
        // A highlighter that corrupts what it highlights is worse than none:
        // the reader is looking at the row *because* they searched for it, so
        // the one row they are staring at must be the one the transcript
        // actually holds — only re-styled, never re-written.
        let original = "compiling src/auth.rs and src/auth.rs again";
        let base = Style::new().fg(theme::MUTED);
        let mut lines = vec![Line::from(vec![Span::styled(original, base)])];
        highlight_matches(&mut lines, "auth");

        let rebuilt: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(rebuilt, original, "the text survives the split verbatim");
        let lit: Vec<&Span<'_>> = lines[0]
            .spans
            .iter()
            .filter(|s| s.style.bg == Some(theme::MATCH_BG))
            .collect();
        assert_eq!(lit.len(), 2, "every occurrence is lit, not just the first");
        assert!(
            lit.iter().all(|s| s.content == "auth"),
            "and only the matched substring is: {lit:?}"
        );
        assert!(
            lit.iter()
                .all(|s| s.style.add_modifier.contains(Modifier::BOLD)),
            "matches are bolded over the row's own colour"
        );
        assert!(
            lines[0]
                .spans
                .iter()
                .all(|s| s.style.fg == Some(theme::MUTED)),
            "which the unmatched fragments keep untouched"
        );
    }

    #[test]
    fn fmt_hms_formats_zero_sub_minute_and_over_an_hour() {
        // Always h:mm:ss, hours un-padded, minutes/seconds zero-padded.
        assert_eq!(fmt_hms(0), "0:00:00"); // the at-rest, pre-turn readout
        assert_eq!(fmt_hms(45_000), "0:00:45"); // < 1 min
        assert_eq!(fmt_hms(65_000), "0:01:05"); // rolls into minutes
        assert_eq!(fmt_hms(3_600_000), "1:00:00"); // exactly one hour
        assert_eq!(fmt_hms(3_661_000), "1:01:01"); // > 1 hr
        assert_eq!(fmt_hms(45_296_000), "12:34:56"); // multi-hour, sub-ms floored
    }

    #[test]
    fn header_shows_the_turn_clock_not_the_word_stella() {
        // The header shows the turn clock's h:mm:ss readout, not a workspace-name
        // title — which in the stella repo would literally read "stella".
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "stella", 0)));
        // A plain event flips the agent to `Running` (so the label reads
        // "running") without starting a turn — the clock stays at zero.
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::Text {
                delta: "working".into(),
            },
        });
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        render_header(&model.agents[0], model.now_ms, area, &mut buf);

        let text = buffer_text(&buf);
        assert!(
            !text.contains("stella"),
            "the word `stella` is gone from the header:\n{text}"
        );
        assert!(
            text.contains("0:00:00"),
            "the turn clock reads zero before any turn:\n{text}"
        );
        assert!(text.contains("running"), "status label intact:\n{text}");
    }

    /// Refresh must commit its cache key only after the fold loop finishes.
    ///
    /// Until #933 it committed *before*, so a panic between "extend `prefix`"
    /// and "record the entry's row range" left a cache the next frame happily
    /// resumed — same index, same lines appended a second time. The panel
    /// panic boundary is what makes such a panic survivable at all, which is
    /// precisely what turns a one-frame glitch into a permanent duplicate; the
    /// two changes only make sense together.
    #[test]
    fn an_interrupted_fold_rebuilds_instead_of_resuming_a_torn_prefix() {
        let transcript = vec![
            TranscriptEntry::User("first prompt".into()),
            TranscriptEntry::Text("alpha answer".into()),
            TranscriptEntry::User("second prompt".into()),
            TranscriptEntry::Text("omega answer".into()),
        ];
        let files: Vec<FileState> = Vec::new();
        let expanded = HashSet::new();
        let plan = FoldPlan::default();
        let refresh = |fold: &mut SessionFold| {
            fold.refresh(
                "lead",
                &transcript,
                &files,
                "",
                false,
                &expanded,
                false,
                0,
                60,
                &plan,
                0,
            );
        };

        let mut fold = SessionFold::default();
        let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _armed = crate::panel_guard::arm_panic("session fold");
            refresh(&mut fold);
        }));
        assert!(interrupted.is_err(), "the fold really was interrupted");
        assert!(
            !fold.prefix.is_empty() && fold.entry_rows.is_empty(),
            "and it really did leave a partial prefix with no row range"
        );
        assert!(
            fold.key.is_none(),
            "an interrupted fold leaves no key for the next frame to resume from"
        );

        // The next frame rebuilds from zero instead of folding entry 0 twice.
        refresh(&mut fold);
        let mut untorn = SessionFold::default();
        refresh(&mut untorn);
        assert_eq!(
            fold.total(),
            untorn.total(),
            "the recovered fold is the same height as one that never tore"
        );
        assert_eq!(
            fold.entry_rows, untorn.entry_rows,
            "…and every entry occupies the same rows"
        );
    }
}
