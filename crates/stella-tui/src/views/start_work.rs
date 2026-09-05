// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The ISSUES tab's start-work overlay — SPEC 8.2, rendering `10-start-work`.
//!
//! ```text
//! ┌ w start work ──────────────────────────────────────────────┐
//! │ w start work #151 dedup digest persists across CI runs     │
//! │ draft plan r1 · built from issue text + graph + memory     │
//! │────────────────────────────────────────────────────────────│
//! │ sources  issue #151 · graph: 2 coupled files               │
//! │          crates/stella-store/src/seen.rs                   │
//! │          RULE dedup-keys "keys must be stable across runs" │
//! │────────────────────────────────────────────────────────────│
//! │ ○ 1 read the seen-set write path  read only · no contract  │
//! │ ○ 2 persist the digest set                    graph · det  │
//! │    done means: seen.rs is changed on the branch            │
//! │ ◇ 3 verify · 5 gates                        blocks merge   │
//! │────────────────────────────────────────────────────────────│
//! │ estimate  ~$0.40  ~60k tok  ~8 min                         │
//! │ a approve and start   e edit tasks   x cancel              │
//! │ nothing runs before approval                               │
//! └────────────────────────────────────────────────────────────┘
//! ```
//!
//! Centred, unlike the command palette one file over: SPEC 8.2 asks for a
//! `Clear` over a centred `Rect`, and #5048's move to an anchored popup is an
//! argument about *completions*, which belong beside the letters that produced
//! them. This overlay answers a key pressed on a row, not a word being typed,
//! and it is the tab's second centred card — the send-to-prompt confirmation
//! in [`super::issues_tab`] is the first.
//!
//! # Why there is no `det %`
//!
//! The estimate line reads `~$ · ~tok · ~minutes`, which is SPEC 8.2 item 5.
//! The `det est 84%` in the SVG rendering predates SPEC §1's amendment:
//! nothing in this workspace measures the deterministic share of a turn, and
//! the project does not build a metric to satisfy a layout. The `det` tags
//! that remain — one per task contract — are booleans the mechanism defines.
//!
//! # Why the estimate can be absent
//!
//! Every term is priced against calls this workspace has actually recorded,
//! so a workspace with no history for the session's model has no estimate to
//! give and the line says that instead of showing a number nobody measured.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
use stella_tui_theme::token;

use crate::render::columns;
use crate::start_work::{StartWork, StartWorkDraft};

/// The overlay's share of the deck's width, and the two bounds on it.
///
/// The floor is the contract rows: they are the widest content and the ones a
/// reader is being asked to approve. The ceiling is the same rows read from
/// the other side — a task subject at the margin and its `unit · det` tag at
/// the far edge of a 200-column terminal are no longer one row to a human eye.
const WIDTH_SHARE: u16 = 5;
const WIDTH_MIN: u16 = 56;
const WIDTH_MAX: u16 = 100;

/// Longest a quoted RULE runs, in display columns, before it is cut.
const RULE_QUOTE_COLS: usize = 44;

/// Draw the overlay over `area`. Nothing here reads the panel's mode — the
/// caller decides the overlay is open, exactly as the send-to-prompt
/// confirmation does.
pub fn render(panel: &StartWork, area: Rect, buf: &mut Buffer) {
    if area.width < 8 || area.height < 6 {
        return;
    }
    let width = (area.width * WIDTH_SHARE / 6)
        .min(WIDTH_MAX)
        .max(WIDTH_MIN.min(area.width))
        .min(area.width);
    let lines = body(panel, width.saturating_sub(2) as usize);
    let height = ((lines.len() as u16) + 2).min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    Clear.render(popup, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(gold_bold())
        .title(Span::styled(" w start work ", gold_bold()));
    Paragraph::new(lines).block(block).render(popup, buf);
}

fn gold_bold() -> Style {
    Style::new().fg(token::GOLD).add_modifier(Modifier::BOLD)
}

/// Every row inside the border, for a content width of `inner`.
fn body(panel: &StartWork, inner: usize) -> Vec<Line<'static>> {
    let muted = Style::new().fg(token::MUTED);
    let mut lines = Vec::new();
    let Some(draft) = &panel.draft else {
        lines.push(Line::from(Span::styled(
            format!(" {}", panel.issue_key),
            Style::new().fg(token::TEXT),
        )));
        lines.push(Line::from(Span::styled(
            format!(
                " {}",
                panel
                    .error
                    .clone()
                    .unwrap_or_else(|| "drafting the plan…".to_string())
            ),
            muted,
        )));
        lines.push(Line::from(Span::styled(
            " x cancel",
            Style::new().fg(token::GOLD),
        )));
        return lines;
    };
    header(draft, inner, &mut lines);
    lines.push(rule(inner));
    sources(draft, inner, &mut lines);
    lines.push(rule(inner));
    tasks(panel, draft, inner, &mut lines);
    lines.push(rule(inner));
    estimate(draft, &mut lines);
    lines.push(actions(panel));
    if let Some(error) = &panel.error {
        lines.push(Line::from(Span::styled(
            format!(" {error}"),
            Style::new().fg(token::RED),
        )));
    }
    lines.push(Line::from(Span::styled(
        " nothing runs before approval",
        Style::new().fg(token::DIM),
    )));
    lines
}

/// SPEC 8.2 item 2 — the verb, the issue, and what the draft was built from.
fn header(draft: &StartWorkDraft, inner: usize, lines: &mut Vec<Line<'static>>) {
    let head = format!("{} {}", draft.issue_key, draft.issue_title);
    lines.push(Line::from(vec![
        Span::styled(" w start work ", gold_bold()),
        Span::styled(
            truncate(&head, inner.saturating_sub(14)),
            Style::new().fg(token::TEXT),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        " draft plan r1 · built from issue text + graph + memory",
        Style::new().fg(token::MUTED),
    )));
}

/// SPEC 8.2 item 3 — exactly what was used, and nothing that was not.
///
/// A source the driver could not read is *absent*, never zero: `graph: 0
/// coupled files` and "there is no code-graph index" are different statements,
/// and only one of them is about the issue.
fn sources(draft: &StartWorkDraft, inner: usize, lines: &mut Vec<Line<'static>>) {
    let muted = Style::new().fg(token::MUTED);
    let mut used = vec![format!("issue {}", draft.issue_key)];
    if !draft.sources.coupled_files.is_empty() {
        used.push(format!(
            "graph: {} coupled file{}",
            draft.sources.coupled_files.len(),
            plural(draft.sources.coupled_files.len())
        ));
    }
    lines.push(Line::from(vec![
        Span::styled(" sources  ", Style::new().fg(token::TEXT)),
        Span::styled(truncate(&used.join(" · "), inner.saturating_sub(10)), muted),
    ]));
    for path in &draft.sources.coupled_files {
        lines.push(Line::from(Span::styled(
            format!("          {}", truncate(path, inner.saturating_sub(10))),
            Style::new().fg(token::DIM),
        )));
    }
    for applied in &draft.sources.rules {
        let quoted = format!(
            "RULE {} \"{}\"",
            applied.id,
            truncate(&flatten(&applied.text), RULE_QUOTE_COLS)
        );
        lines.push(Line::from(Span::styled(
            format!("          {}", truncate(&quoted, inner.saturating_sub(10))),
            muted,
        )));
    }
}

/// SPEC 8.2 item 4 — the task list with its contract previews, then the
/// verify task that blocks the merge.
fn tasks(panel: &StartWork, draft: &StartWorkDraft, inner: usize, lines: &mut Vec<Line<'static>>) {
    for (i, task) in draft.tasks.iter().enumerate() {
        let dropped = panel.dropped.contains(&i);
        let cursor = if panel.editing && panel.sel == i {
            "▸"
        } else {
            " "
        };
        let subject = if dropped {
            format!("{} — taken out", task.subject)
        } else {
            task.subject.clone()
        };
        let subject_style = if dropped {
            Style::new()
                .fg(token::DIM)
                .add_modifier(Modifier::CROSSED_OUT)
        } else {
            Style::new().fg(token::TEXT)
        };
        let left = format!("{cursor}○ {} {subject}", i + 1);
        // A read-only task states that it has no contract rather than leaving
        // the column blank: a blank one reads as a contract nobody wrote. A
        // task the human took out shows no contract at all — its check is not
        // going to run, and a `done means` line under it would describe work
        // the approval will not carry.
        let right = match (&task.contract, dropped) {
            (_, true) => "not in the plan".to_string(),
            (None, _) => "read only · no contract".to_string(),
            (Some(contract), _) => format!(
                "{} · {}",
                contract.mechanism,
                if contract.deterministic {
                    "det"
                } else {
                    "model"
                }
            ),
        };
        lines.push(row(&left, &right, subject_style, inner));
        if let Some(contract) = task.contract.as_ref().filter(|_| !dropped) {
            lines.push(Line::from(Span::styled(
                format!(
                    "    done means: {}",
                    truncate(&contract.done_means, inner.saturating_sub(16))
                ),
                Style::new().fg(token::MUTED),
            )));
        }
    }
    let verify = format!(
        " ◇ {} verify · {} gates",
        draft.tasks.len() + 1,
        draft.gates
    );
    lines.push(row(&verify, "blocks merge", gold_bold(), inner));
}

/// SPEC 8.2 item 5 — `~$ · ~tok · ~minutes`, or the reason there is none.
fn estimate(draft: &StartWorkDraft, lines: &mut Vec<Line<'static>>) {
    let label = Span::styled(" estimate ", Style::new().fg(token::TEXT));
    let Some(estimate) = &draft.estimate else {
        lines.push(Line::from(vec![
            label,
            Span::styled(
                " no recorded calls to price this model against",
                Style::new().fg(token::MUTED),
            ),
        ]));
        return;
    };
    lines.push(Line::from(vec![
        label,
        Span::styled(
            format!(" ~${:.2}", estimate.usd),
            Style::new().fg(token::GOLD),
        ),
        Span::styled(
            format!("  ~{} tok", compact(estimate.tokens)),
            Style::new().fg(token::TEXT),
        ),
        Span::styled(
            format!("  ~{} min", estimate.minutes),
            Style::new().fg(token::TEXT),
        ),
    ]));
}

/// SPEC 8.2 item 6 — the action row. `e` is lit while it is holding the task
/// list open, so the key that changes the mode says which mode it is in.
fn actions(panel: &StartWork) -> Line<'static> {
    let muted = Style::new().fg(token::MUTED);
    let editing = panel.editing;
    let mut spans = vec![
        Span::styled(" a", gold_bold()),
        Span::styled(" approve and start", Style::new().fg(token::GOLD)),
        Span::styled("   e", if editing { gold_bold() } else { muted }),
        Span::styled(
            if editing {
                " done editing"
            } else {
                " edit tasks"
            },
            muted,
        ),
    ];
    if editing {
        spans.push(Span::styled("   ↑↓", muted));
        spans.push(Span::styled(" task", Style::new().fg(token::DIM)));
        spans.push(Span::styled("   space", muted));
        spans.push(Span::styled(" take out", Style::new().fg(token::DIM)));
    }
    spans.push(Span::styled("   x", muted));
    spans.push(Span::styled(" cancel", Style::new().fg(token::DIM)));
    Line::from(spans)
}

/// One task row: `left` at the margin and `right` pushed to the far edge, so
/// the contract column lines up however long the subjects are.
///
/// One column short of the border on each side. Both margins are the same
/// choice: a tag butted against the frame reads as part of it, and a subject
/// that runs into its own tag reads as one string.
fn row(left: &str, right: &str, left_style: Style, inner: usize) -> Line<'static> {
    let field = inner.saturating_sub(1);
    // `left` is a task subject or the verify line, written by a model or
    // a user. So its elide, and the fill after it, both spend `field` in
    // display columns.
    let right_w = columns::width(right);
    let left = truncate(left, field.saturating_sub(right_w + 2));
    let gap = field
        .saturating_sub(columns::width(&left))
        .saturating_sub(right_w);
    Line::from(vec![
        Span::styled(left, left_style),
        Span::raw(" ".repeat(gap)),
        Span::styled(right.to_string(), Style::new().fg(token::MUTED)),
    ])
}

fn rule(inner: usize) -> Line<'static> {
    Line::from(Span::styled("─".repeat(inner), Style::new().fg(token::DIM)))
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Collapse a rule's own line breaks — a quoted RULE rides one row.
fn flatten(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `60k`, `1.2M` — the token figure at a glance, never a false precision on
/// an estimate.
fn compact(tokens: u64) -> String {
    match tokens {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0),
        n if n >= 1_000 => format!("{}k", n / 1_000),
        n => n.to_string(),
    }
}

/// Cut to `max` display columns, with an ellipsis when it cuts.
fn truncate(s: &str, max: usize) -> String {
    columns::head(s, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::start_work::{DraftContract, DraftEstimate, DraftRule, DraftSources, DraftTask};

    fn draft() -> StartWorkDraft {
        StartWorkDraft {
            issue_key: "#151".into(),
            issue_title: "dedup digest persists across CI runs".into(),
            sources: DraftSources {
                coupled_files: vec!["src/seen.rs".into(), "src/ci.rs".into()],
                rules: vec![DraftRule {
                    id: "dedup-keys".into(),
                    text: "dedup keys must be stable\nacross runs".into(),
                }],
            },
            tasks: vec![
                DraftTask {
                    subject: "read the seen-set write path".into(),
                    contract: None,
                },
                DraftTask {
                    subject: "persist the digest set".into(),
                    contract: Some(DraftContract {
                        done_means: "the file exists after a run".into(),
                        mechanism: "graph".into(),
                        deterministic: true,
                    }),
                },
            ],
            gates: 5,
            estimate: Some(DraftEstimate {
                usd: 0.4,
                tokens: 60_000,
                minutes: 8,
            }),
        }
    }

    fn panel() -> StartWork {
        let mut panel = StartWork::default();
        panel.open("#151", 1);
        panel.draft = Some(draft());
        panel
    }

    fn text(panel: &StartWork) -> String {
        body(panel, 70)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_overlay_names_every_source_it_used() {
        let drawn = text(&panel());
        assert!(drawn.contains("issue #151"), "{drawn}");
        assert!(drawn.contains("graph: 2 coupled files"), "{drawn}");
        assert!(drawn.contains("src/seen.rs"), "{drawn}");
        assert!(
            drawn.contains("RULE dedup-keys \"dedup keys must be stable across runs\""),
            "the rule's own text, flattened onto one row: {drawn}"
        );
    }

    #[test]
    fn a_read_only_task_declares_no_contract_and_a_diff_task_declares_one() {
        let drawn = text(&panel());
        assert!(drawn.contains("read only · no contract"), "{drawn}");
        assert!(
            drawn.contains("done means: the file exists after a run"),
            "{drawn}"
        );
        assert!(drawn.contains("graph · det"), "{drawn}");
    }

    #[test]
    fn the_final_task_is_verify_and_it_blocks_the_merge() {
        let drawn = text(&panel());
        assert!(drawn.contains("◇ 3 verify · 5 gates"), "{drawn}");
        assert!(drawn.contains("blocks merge"), "{drawn}");
    }

    /// SPEC §1: no `det %`, on this line or anywhere.
    #[test]
    fn the_estimate_line_prices_and_times_and_reports_no_ratio() {
        let drawn = text(&panel());
        assert!(drawn.contains("~$0.40"), "{drawn}");
        assert!(drawn.contains("~60k tok"), "{drawn}");
        assert!(drawn.contains("~8 min"), "{drawn}");
        assert!(
            !drawn.contains("det est"),
            "the SVG's `det est 84%` is the metric SPEC §1 refuses: {drawn}"
        );
    }

    #[test]
    fn an_unpriceable_workspace_says_so_rather_than_showing_a_zero() {
        let mut panel = panel();
        panel.draft.as_mut().unwrap().estimate = None;
        let drawn = text(&panel);
        assert!(
            drawn.contains("no recorded calls to price this model against"),
            "{drawn}"
        );
        assert!(!drawn.contains("~$0.00"), "{drawn}");
    }

    #[test]
    fn the_footer_and_action_row_are_spec_8_2s() {
        let drawn = text(&panel());
        assert!(drawn.contains("a approve and start"), "{drawn}");
        assert!(drawn.contains("e edit tasks"), "{drawn}");
        assert!(drawn.contains("x cancel"), "{drawn}");
        assert!(drawn.contains("nothing runs before approval"), "{drawn}");
    }

    /// A task taken out shows no contract: its check is not going to run.
    #[test]
    fn a_task_taken_out_says_so_and_drops_its_contract() {
        let mut panel = panel();
        panel.editing = true;
        panel.sel = 1;
        panel.toggle();
        let drawn = text(&panel);
        assert!(
            drawn.contains("persist the digest set — taken out"),
            "{drawn}"
        );
        assert!(drawn.contains("not in the plan"), "{drawn}");
        assert!(
            !drawn.contains("done means: the file exists after a run"),
            "a dropped task keeps no contract preview: {drawn}"
        );
        panel.sel = 0;
        panel.toggle();
        let drawn = text(&panel);
        assert!(
            drawn.contains("read the seen-set write path — taken out"),
            "{drawn}"
        );
        assert!(
            drawn.contains("▸○ 1"),
            "the edit cursor sits on it: {drawn}"
        );
    }

    #[test]
    fn a_draft_that_has_not_arrived_draws_the_wait_not_an_empty_plan() {
        let mut panel = StartWork::default();
        panel.open("#151", 1);
        let drawn = text(&panel);
        assert!(drawn.contains("drafting the plan…"), "{drawn}");
        assert!(drawn.contains("x cancel"), "{drawn}");
        assert!(!drawn.contains("verify"), "no plan is drawn yet: {drawn}");
    }

    #[test]
    fn a_failed_draft_shows_what_stopped_it() {
        let mut panel = StartWork::default();
        panel.open("#151", 1);
        panel.error = Some("no tracker connected".into());
        assert!(text(&panel).contains("no tracker connected"));
    }

    /// `truncate` spends `max` in display columns, never bytes or chars.
    ///
    /// Renamed from `truncate_cuts_on_characters_not_bytes`. That old name
    /// kept 3 whole glyphs at `max` 4 — a char count. Here `max` is a
    /// column budget, and a CJK glyph is 2 columns, so only one glyph
    /// plus the ellipsis fits.
    #[test]
    fn truncate_spends_its_budget_in_display_columns() {
        assert_eq!(truncate("日本語のテキスト", 4), "日…");
        assert_eq!(truncate("short", 40), "short");
    }

    #[test]
    fn compact_reads_at_a_glance() {
        assert_eq!(compact(999), "999");
        assert_eq!(compact(60_000), "60k");
        assert_eq!(compact(1_250_000), "1.2M");
    }

    /// A CJK task subject keeps its contract tag flush at the row's own
    /// width, instead of drifting past it.
    ///
    /// `left` here is 20 CJK glyphs — 20 chars, 40 columns. A char-counted
    /// gap sees "20" where the row spent 40, so old code pads with 20
    /// extra columns it never had, and the row runs past `inner`.
    #[test]
    fn a_wide_character_subject_keeps_the_tag_flush_at_the_rows_width() {
        let left = "圈".repeat(20);
        let right = "graph · det";
        let inner = 60;
        let line = row(&left, right, Style::new(), inner);
        assert!(
            line.width() <= inner,
            "row overran its {inner}-column budget: {line:?}"
        );
        assert_eq!(
            line.spans.last().map(|s| s.content.as_ref()),
            Some(right),
            "the tag should render whole: {line:?}"
        );
    }
}
