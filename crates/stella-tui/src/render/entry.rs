//! The transcript's pure content builders — `(model) -> Vec<Line>`.
//!
//! Split out of `render.rs` when that file crossed the 1500-line guard. This is
//! a **pure move**: every function below is byte-identical to the one it
//! replaced, and `render` re-exports the two `pub(crate)` entry points, so no
//! call site changed.
//!
//! The cut follows the concern rather than the line count. Everything here
//! turns model state into `Line`s and touches no `Frame`, `Buffer`, or `Rect`;
//! everything left in `render.rs` draws. `render/row.rs` remains the *word*
//! layer beneath this one — `entry_lines` composes the sentence, `row` renders
//! the phrase.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use stella_protocol::{CiStatus, PrStatus, SubAgentStatus};

use crate::model::{FileState, TranscriptEntry};
use crate::render::row::*;
use crate::textline::{
    self, budget_mode_label, ci_status_label, media_kind_label, media_state_label, pr_status_label,
    stage_label,
};
// Still owned by the parent: `resolve_inline_diff` reads the draw-side file
// list and `INLINE_DIFF_CAP` bounds it. A child module may reach a private
// parent item, so the move needed no visibility change.
use super::{INLINE_DIFF_CAP, resolve_inline_diff};
use crate::{diff, theme};

// Pure content builders (unit-tested directly)

/// Fold the in-flight answer preview
/// ([`SessionModel::streaming_text`](crate::model::SessionModel::streaming_text))
/// as a trailing agent block, or nothing at all when there is none.
///
/// Both transcript renderers end on exactly this step, so it lives here rather
/// than twice: neither of `entry_lines`' two view flags means anything for a
/// preview (it cannot be selected, and tail-follow is a *thinking* affordance),
/// and getting that pair wrong in one caller and not the other is the kind of
/// drift that only shows up mid-stream.
pub(crate) fn streaming_lines(
    streaming: &str,
    files: &[FileState],
    expand_thinking: bool,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    if streaming.is_empty() {
        return;
    }
    let preview = TranscriptEntry::Text(streaming.to_string());
    entry_lines(&preview, files, expand_thinking, false, false, width, out);
}

/// Whether the trailing transcript entry is a thought *still being written* —
/// the one thing a collapsed thinking block needs in order to follow its tail
/// instead of showing its head (see the `TranscriptEntry::Reasoning` arm of
/// [`entry_body`]).
///
/// Positional, so it stays a pure function of the fold with no liveness flag to
/// set and no timer to reset. A thought stops being live the instant anything
/// lands after it, and *everything* that ends one appends its own entry: a tool
/// call, the authoritative `Text`, `Complete`, `Error`. The streaming answer
/// preview is the one end that has no entry yet — it is the earliest evidence
/// the thought is over, arriving a beat before the `Text` that coalesces it.
pub(crate) fn reasoning_is_live(transcript: &[TranscriptEntry], streaming: &str) -> bool {
    matches!(transcript.last(), Some(TranscriptEntry::Reasoning(_))) && streaming.is_empty()
}

/// Whether an entry closes a readable block, and so is followed by a spacer.
///
/// Trailing rather than leading, which is what lets the rhythm stay
/// entry-local: a leading gap would have to know what preceded it, and the
/// deck's incremental fold renders each entry in isolation. Two entries are
/// deliberately *not* block-closing — a [`TranscriptEntry::ToolStart`], whose
/// result belongs directly beneath it, and [`TranscriptEntry::Evicted`], the
/// one-line note that opens the scrollback. A consequence worth keeping: a
/// batch of parallel `ToolStart`s renders as a tight block, which is exactly
/// how a fan-out should read.
fn closes_block(entry: &TranscriptEntry) -> bool {
    !matches!(
        entry,
        TranscriptEntry::ToolStart { .. } | TranscriptEntry::Evicted { .. }
    )
}

/// `live` marks the one entry that is still being written to — the trailing
/// entry of an in-flight turn, per [`reasoning_is_live`]. Only a collapsed
/// reasoning block reads it, and only to follow its tail instead of showing its
/// head; every settled entry passes `false`.
pub(crate) fn entry_lines(
    entry: &TranscriptEntry,
    files: &[FileState],
    expand_thinking: bool,
    expanded: bool,
    live: bool,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    entry_body(entry, files, expand_thinking, expanded, live, width, out);
    if closes_block(entry) {
        push_gap(out);
    }
}

/// The label style for a system note that wants to be *found*: errors, held
/// scopes, questions, failed verdicts. Bold and hued, because the whole point
/// of the row is that a scan should stop on it.
fn loud(color: Color) -> Style {
    Style::new().fg(color).add_modifier(Modifier::BOLD)
}

/// The label style for a system note that is context rather than event —
/// recall, memory writes, compaction, fallbacks, media, commits.
///
/// These used to be hued too, and collectively they were most of the colour on
/// screen: a transcript where recall, spend and compaction all shout as loudly
/// as an error reads as a list of problems. They are bookkeeping. They get a
/// dim label, and the reader's eye is left free for the rows that matter.
fn quiet() -> Style {
    Style::new().fg(theme::TEXT_TERTIARY)
}

/// The value half of a system note. Always white.
///
/// This is the rule that keeps the deck legible: **the label is coloured, the
/// value is read.** Before the split, `push_note` was routinely handed one
/// style for both halves, so every note row was a single saturated hue end to
/// end and the accent stopped meaning anything. A colour earns its place by
/// being rare.
fn value() -> Style {
    Style::new().fg(theme::INK)
}

/// `1 frame` / `3 frames` — a count that reads as English. The transcript used
/// to render "1 frames".
fn plural(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

/// Visual rows a collapsed reasoning block spends on content.
///
/// A *row* budget, not a line budget: a chain of thought is usually a handful
/// of paragraph-long logical lines, so a five-*line* window on a wrapped
/// thought is most of the pane, and how much of it you got would depend on how
/// the model happened to punctuate. Counting rows is also what keeps the block
/// the same height whether it is following a live thought or previewing a
/// settled one — the stream stopping must not resize the block under the
/// reader.
pub const THINKING_ROWS: usize = 5;

/// The fold marker on a collapsed reasoning block. Sits *below* a settled
/// preview (there is more after what you can see) and *above* a live tail
/// (there is more before it) — so the newest text is always the last row of a
/// thought still being written.
const THINKING_FOLD_HINT: &str = "⋯ ctrl+o expands this thought · ctrl+r all";

fn entry_body(
    entry: &TranscriptEntry,
    files: &[FileState],
    expand_thinking: bool,
    expanded: bool,
    live: bool,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    match entry {
        TranscriptEntry::Evicted { count } => out.push(Line::from(Span::styled(
            format!("… {count} earlier entries evicted"),
            Style::new()
                .fg(theme::TEXT_TERTIARY)
                .add_modifier(Modifier::ITALIC),
        ))),
        TranscriptEntry::User(text) => {
            // The one transcript entry rendered in a single color end to end:
            // the `[user]:` tag and every line of the prompt ride the same
            // violet as the composer's keybind glyphs and the
            // "deterministic-first" chip (`deck_render`) — the interactive-
            // chrome accent, never the brand gold. Rendered as plain lines
            // (not markdown) so nothing tints part of the prompt a 2nd color.
            let violet = Style::new().fg(theme::VIOLET);
            let lines: Vec<Line<'static>> = text
                .split('\n')
                .map(|l| Line::from(Span::styled(l.to_owned(), violet)))
                .collect();
            push_row_block(Rail::User, lines, width, out);
        }
        TranscriptEntry::Stage(name) => {
            // A section rule, not a row — see `push_rule`. The word "stage" is
            // dropped with it: the label *is* the stage, and prefixing every
            // one with its own type name was three columns spent restating
            // what the divider already says.
            push_rule(
                stage_label(*name),
                Style::new()
                    .fg(theme::TEXT_SECONDARY)
                    .add_modifier(Modifier::BOLD),
                width,
                out,
            );
        }
        TranscriptEntry::Text(text) => {
            push_row_block(Rail::Agent, crate::markdown::render(text), width, out);
        }
        TranscriptEntry::Reasoning(text) => {
            let total_lines = text.lines().count().max(1);
            let show_all = expand_thinking || expanded;
            let chevron = if show_all { "⏶" } else { "⏵" };
            // Dim, not tinted. Reasoning is the agent talking to itself; it
            // is the *least* load-bearing text on screen, so it wears the
            // quiet warm neutral, never a hue.
            let header_style = quiet();
            let reasoning_style = Style::new()
                .fg(theme::TEXT_TERTIARY)
                .add_modifier(Modifier::ITALIC);
            let mut block = vec![Line::from(Span::styled(
                format!("{total_lines} lines"),
                header_style,
            ))];
            if show_all {
                for l in text.split('\n') {
                    block.push(Line::from(Span::styled(l.to_owned(), reasoning_style)));
                }
            } else {
                // Pre-wrap every non-blank line to the note's body column so
                // the budget below is spent in visual rows. Blank lines are
                // dropped rather than wrapped: in a window this small a
                // paragraph break costs a row of thought and says nothing.
                let mut rows: Vec<Line<'static>> = Vec::new();
                for l in text.lines().filter(|l| !l.trim().is_empty()) {
                    wrap_one_indent(
                        Line::from(Span::styled(l.to_owned(), reasoning_style)),
                        width.saturating_sub(LEAD),
                        0,
                        &mut rows,
                    );
                }
                let folded = rows.len().saturating_sub(THINKING_ROWS);
                let hint = Line::from(Span::styled(
                    THINKING_FOLD_HINT,
                    Style::new().fg(theme::TEXT_TERTIARY),
                ));
                if live {
                    // Follow the tail. A long turn is mostly spent inside one
                    // thought, and a preview frozen on its opening lines reads
                    // as a stalled session — the newest row is both the most
                    // informative one and the only proof anything is moving.
                    if folded > 0 {
                        block.push(hint);
                    }
                    block.extend(rows.split_off(folded));
                } else {
                    // Settled: the head, which is where a thought states what
                    // it is about. Scrollback is read top-down.
                    rows.truncate(THINKING_ROWS);
                    block.extend(rows);
                    if folded > 0 {
                        block.push(hint);
                    }
                }
            }
            push_note_block(
                &format!("{chevron} thinking"),
                header_style,
                block,
                width,
                out,
            );
        }
        TranscriptEntry::ToolStart {
            name,
            input,
            raw,
            path,
            ..
        } => {
            // `name` then `argument`, the name soft-padded to a common column
            // so arguments line up across a run of calls. Soft, not hard: a
            // long MCP name (`mcp__github__create_pull_request`) overruns the
            // column rather than being truncated, since the tool's identity
            // outranks the alignment it would cost.
            // The tool name is the one thing in the transcript that carries
            // the full brand accent. Everything a session did, it did through a
            // tool call, so the names are the index to the whole scrollback —
            // and they are the only rows a reader scans *for* rather than
            // reads. The argument beside it stays white/dim (`path_spans`), so
            // the accent marks the verb and never the object.
            let mut left = vec![Span::styled(
                pad_name(name),
                Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
            )];
            left.extend(path_spans(input, path.is_some()));
            push_row(Rail::Call, left, width, out);
            if expanded {
                // ctrl+o: the full argument object, pretty-printed and dim.
                // An over-budget argument may not parse (char-capped raw) —
                // show it wrapped rather than clipped at the pane edge.
                let pretty = serde_json::from_str::<serde_json::Value>(raw)
                    .and_then(|v| serde_json::to_string_pretty(&v))
                    .unwrap_or_else(|_| raw.clone());
                for l in pretty.lines() {
                    push_detail_line(l, width, out);
                }
            }
        }
        TranscriptEntry::ToolResult {
            ok,
            full,
            duration_ms,
            speculated,
            diff,
            ..
        } => {
            let rail = if *ok { Rail::Result } else { Rail::Fail };
            let dim = Style::new().fg(theme::MUTED);
            let total = full.lines().count();
            // ⚡ marks a speculated result: the duration overlapped the
            // model's own streaming instead of following it.
            let dur = if *speculated {
                format!("⚡{}", human_duration(*duration_ms))
            } else {
                human_duration(*duration_ms)
            };
            let inline = diff.as_ref().and_then(|d| resolve_inline_diff(d, files));
            // The delta the emitter measured for this very mutation, carried
            // alongside its diff — not a recount of the rendered hunk, which is
            // a bounded view of the changed region and reports a smaller number.
            let inline_delta = diff
                .as_ref()
                .and_then(|d| super::resolve_inline_delta(d, files));

            // The right-hand metric column. A diff states its own size in
            // added/removed lines, which is the honest unit for an edit —
            // "42 lines of output" would describe the tool's chatter, not the
            // change. Everything else reports output size, and only when
            // there is more than the one line already shown.
            let mut metric: Vec<Span<'static>> = Vec::new();
            if inline.is_some() {
                let (added, removed) = inline_delta.unwrap_or((0, 0));
                metric.push(Span::styled(
                    format!("+{added}"),
                    Style::new().fg(theme::OK),
                ));
                metric.push(Span::styled(" ".to_string(), dim));
                metric.push(Span::styled(
                    format!("−{removed}"),
                    Style::new().fg(theme::BAD),
                ));
                metric.push(Span::styled(" · ".to_string(), dim));
            } else if total > 1 && !expanded {
                // `⋯` is the one glyph this UI uses for "there is more behind
                // this", so it carries the ctrl+o affordance the removed hint
                // row used to spell out — at no extra row.
                metric.push(Span::styled(format!("⋯ {} · ", plural_lines(total)), dim));
            }
            metric.push(Span::styled(dur, dim));

            if expanded {
                push_row(
                    rail,
                    justify(vec![], metric, width, rail.indent()),
                    width,
                    out,
                );
                for l in full.lines() {
                    push_detail_line(l, width, out);
                }
            } else {
                // With a diff below, a prose summary ("Applied edit to
                // src/agent.rs") would restate the call row above it and the
                // diff under it in the same breath. The row carries only its
                // metrics and gets out of the way.
                let shown: Vec<&str> = if inline.is_some() {
                    Vec::new()
                } else {
                    // A failure never collapses to a single line. The point of
                    // reading a transcript at the moment something breaks is to
                    // see *why*, and a one-line preview of a stack trace is a
                    // prompt to go hunting rather than an answer.
                    let budget = if *ok { 1 } else { FAIL_PREVIEW };
                    full.lines().skip(salient_line(full)).take(budget).collect()
                };
                let head: Vec<Span<'static>> = match shown.first() {
                    Some(l) => vec![Span::styled(
                        l.trim_end().to_owned(),
                        if *ok {
                            dim
                        } else {
                            Style::new().fg(theme::BAD)
                        },
                    )],
                    None => Vec::new(),
                };
                push_row(
                    rail,
                    justify(head, metric, width, rail.indent()),
                    width,
                    out,
                );
                for l in shown.iter().skip(1) {
                    push_detail_line(l.trim_end(), width, out);
                }
                // Only a failure earns the "there is more" row: a successful
                // result already states its size in the metric column, and
                // saying it twice is how a dense layout turns back into a
                // sparse one. Anchoring mid-output also means the count is
                // "everything but the window", not "everything after it".
                let hidden = total.saturating_sub(shown.len());
                if hidden > 0 && !*ok {
                    push_detail_line(&format!("⋯ {} · ctrl+o", plural_lines(hidden)), width, out);
                }
            }
            // The mutation's diff, inline under the result — GitHub-PR style
            // via `crate::diff` (the one implementation of "how a diff
            // looks"), gated on freshness: a later mutation of the same path
            // bumps `FileState::changes` past the recorded seq and the diff
            // no longer belongs to this call, so it is hidden rather than
            // misattributed. Collapsed shows at most [`INLINE_DIFF_CAP`]
            // styled lines; ctrl+o reveals the whole diff.
            if let (Some(dref), Some(d)) = (diff.as_ref(), inline) {
                // No path header and no counts footer here, unlike the
                // standalone viewer: the call row above already names the file
                // and the metric column already states `+n −m`, so both rules
                // would be the same facts a second time — four rows of chrome
                // around what is often a two-row change.
                let cap = if expanded {
                    usize::MAX
                } else {
                    INLINE_DIFF_CAP
                };
                let (body, hidden) = diff::body_lines_inline(d, Some(&dref.path), cap);
                for line in body {
                    push_diff_line(line, out);
                }
                if hidden > 0 {
                    push_diff_line(
                        Line::from(Span::styled(
                            format!("⋯ {} · ctrl+o", plural_lines(hidden)),
                            Style::new().fg(theme::MUTED),
                        )),
                        out,
                    );
                }
            }
        }
        TranscriptEntry::Retry { attempt, reason } => {
            push_note(
                "↻ retry",
                loud(theme::WARNING_BRIGHT),
                vec![
                    Span::styled(format!("#{attempt} "), quiet()),
                    Span::styled(reason.clone(), value()),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::Parked {
            description,
            poll_interval_secs,
            deadline_secs,
        } => {
            push_note(
                "⏳ parked",
                loud(theme::ACCENT),
                vec![
                    Span::styled(format!("until {description} "), value()),
                    Span::styled(
                        format!(
                            "· every {poll_interval_secs}s, up to {deadline_secs}s, no model calls"
                        ),
                        quiet(),
                    ),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::Woken { reason, polls_used } => {
            push_note(
                "▶ woke",
                loud(theme::ACCENT),
                vec![
                    Span::styled(
                        format!("after {} ", plural(*polls_used, "probe", "probes")),
                        value(),
                    ),
                    Span::styled(
                        match reason.as_str() {
                            "changed" => "· the watched state changed".to_string(),
                            "deadline_expired" => {
                                "· the deadline expired with no change".to_string()
                            }
                            other => format!("· {other}"),
                        },
                        quiet(),
                    ),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::Compaction {
            before_tokens,
            after_tokens,
            evicted,
            deduped,
        } => {
            push_note(
                "⇣ compacted",
                quiet(),
                vec![
                    Span::styled(format!("{before_tokens}→{after_tokens} tok"), value()),
                    Span::styled(
                        format!("  ·  {evicted} evicted · {deduped} deduped"),
                        quiet(),
                    ),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::BudgetTick {
            spent_usd,
            limit_usd,
            mode,
        } => {
            let limit = limit_usd.map(|l| format!("/${l:.2}")).unwrap_or_default();
            let style = Style::new().fg(theme::WARNING);
            push_note(
                "◇ spend",
                style,
                vec![Span::styled(
                    format!("${spent_usd:.4}{limit} ({})", budget_mode_label(*mode)),
                    style,
                )],
                width,
                out,
            );
        }
        TranscriptEntry::ProviderFallback { from, to, reason } => {
            push_note(
                "⚡ fallback",
                loud(theme::WARNING),
                vec![
                    Span::styled(format!("{from} → {to}"), value()),
                    Span::styled(format!("  ·  {reason}"), quiet()),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::ContextRecall {
            frames,
            tokens,
            latency_ms,
            used_ann_index,
            providers,
            budget,
        } => {
            recall_lines(
                frames,
                *tokens,
                *latency_ms,
                *used_ann_index,
                providers,
                budget.as_ref(),
                expanded,
                width,
                out,
            );
        }
        TranscriptEntry::ContextWrite {
            provider,
            upserts,
            superseded,
        } => {
            push_note(
                "✎ memory",
                quiet(),
                vec![
                    Span::styled(plural(u64::from(*upserts), "fact", "facts"), value()),
                    Span::styled(
                        format!("  ·  {superseded} superseded → {provider}"),
                        quiet(),
                    ),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::MediaProgress {
            artifact_id,
            kind,
            state,
        } => {
            push_note(
                "🎞 media",
                quiet(),
                vec![
                    Span::styled(
                        format!("{} {}", media_kind_label(*kind), media_state_label(state)),
                        value(),
                    ),
                    Span::styled(format!("  ·  {artifact_id}"), quiet()),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::MediaComplete { label, path, kind } => {
            push_note(
                "🎨 media",
                quiet(),
                vec![
                    Span::styled(format!("{} {label}", media_kind_label(*kind)), value()),
                    Span::styled(format!("  ·  {path}"), quiet()),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::Verdict {
            passed,
            summary,
            deterministic,
        } => {
            // Passing is [`theme::OK`], not the accent: a verdict is an
            // outcome, and outcomes are status-coloured. The accent means
            // "active", which a settled verdict by definition is not.
            let (glyph, color) = if *passed {
                ("✓", theme::OK)
            } else {
                ("✗", theme::DANGER)
            };
            let tag = if *deterministic {
                "deterministic"
            } else {
                "model-verifier"
            };
            push_note(
                &format!("{glyph} verdict"),
                loud(color),
                vec![
                    Span::styled(summary.clone(), value()),
                    Span::styled(format!("  ·  {tag}"), quiet()),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::GoalVerdict {
            met,
            round,
            reasoning,
        } => {
            let (glyph, color) = if *met {
                ("✓", theme::OK)
            } else {
                ("○", theme::WARN)
            };
            push_note(
                &format!("{glyph} goal"),
                loud(color),
                vec![
                    Span::styled(
                        if *met { "met" } else { "not yet met" }.to_string(),
                        loud(color),
                    ),
                    Span::styled(format!("  {reasoning}"), value()),
                    Span::styled(format!("  ·  round {round}"), quiet()),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::SubAgent {
            agent_id,
            finished,
            instruction_preview,
            write_access,
        } => match finished {
            None => push_note(
                "⤷ sub-agent",
                quiet(),
                vec![
                    Span::styled(agent_id.clone(), value()),
                    Span::styled(
                        format!("  {}", if *write_access { "write" } else { "read-only" }),
                        quiet(),
                    ),
                    Span::styled(format!("  {instruction_preview}"), quiet()),
                ],
                width,
                out,
            ),
            Some(summary) => {
                let (glyph, color) = match summary.status {
                    SubAgentStatus::Completed => ("✓", theme::OK),
                    SubAgentStatus::Incomplete => ("○", theme::WARN),
                    SubAgentStatus::Refused => ("✗", theme::BAD),
                };
                push_note(
                    &format!("{glyph} sub-agent"),
                    loud(color),
                    vec![
                        Span::styled(agent_id.clone(), value()),
                        Span::styled(
                            match &summary.reason {
                                Some(reason) => format!("  {reason}"),
                                None => "  done".to_string(),
                            },
                            value(),
                        ),
                        // The saving is the point of the primitive, so it is
                        // on the row rather than only in the journal.
                        Span::styled(
                            format!(
                                "  ·  {} step{}  ·  {} msgs absorbed  ·  ${:.4}",
                                summary.steps,
                                if summary.steps == 1 { "" } else { "s" },
                                summary.absorbed_messages,
                                summary.cost_usd
                            ),
                            quiet(),
                        ),
                    ],
                    width,
                    out,
                );
            }
        },
        TranscriptEntry::ScopeReview {
            summary,
            steps,
            estimated_files,
        } => {
            push_note(
                "⏸ plan",
                loud(theme::WARNING_BRIGHT),
                vec![
                    Span::styled(summary.clone(), value()),
                    Span::styled(
                        format!(
                            "  ·  {} · ~{}",
                            plural(*steps as u64, "step", "steps"),
                            plural(u64::from(*estimated_files), "file", "files")
                        ),
                        quiet(),
                    ),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::HunkReview { tool, hunks, files } => {
            push_note(
                "⏸ hunks",
                loud(theme::WARNING_BRIGHT),
                vec![
                    Span::styled(tool.clone(), value()),
                    Span::styled(
                        format!(
                            "  ·  {} · {}",
                            plural(*hunks as u64, "hunk", "hunks"),
                            plural(*files as u64, "file", "files")
                        ),
                        quiet(),
                    ),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::AskUser { question, options } => {
            push_note(
                "? ask",
                loud(theme::WARNING_BRIGHT),
                vec![
                    Span::styled(question.clone(), value()),
                    Span::styled(
                        format!(
                            "  ·  {} + free text",
                            plural(*options as u64, "option", "options")
                        ),
                        quiet(),
                    ),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::Commit { sha, message } => {
            let short = sha.chars().take(9).collect::<String>();
            push_note(
                "● commit",
                quiet(),
                vec![
                    Span::styled(format!("{short}  "), quiet()),
                    Span::styled(message.clone(), value()),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::Pr {
            url,
            status,
            number,
            ci,
        } => {
            let style = Style::new()
                .fg(pr_status_color(*status))
                .add_modifier(Modifier::BOLD);
            let mut spans = vec![Span::styled(
                format!("[{}] ", pr_status_label(*status)),
                style,
            )];
            if let Some(n) = number {
                spans.push(Span::styled(format!("#{n} "), style));
            }
            if let Some(ci) = ci {
                spans.push(Span::styled(
                    format!("ci {} ", ci_status_label(*ci)),
                    Style::new().fg(ci_status_color(*ci)),
                ));
            }
            spans.push(Span::styled(
                url.clone(),
                Style::new().fg(theme::TEXT_TERTIARY),
            ));
            push_note("⇢ pr", style, spans, width, out);
        }
        TranscriptEntry::TaskUpdate {
            done,
            total,
            active,
        } => {
            let mut spans = vec![Span::styled(format!("{done}/{total}"), value())];
            if let Some(subject) = active {
                spans.push(Span::styled(format!("  ·  {subject}"), quiet()));
            }
            push_note("☰ plan", loud(theme::VIOLET), spans, width, out);
        }
        TranscriptEntry::Error { message, retryable } => {
            push_note(
                "✗ error",
                loud(theme::DANGER),
                vec![
                    Span::styled(message.clone(), Style::new().fg(theme::DANGER)),
                    Span::styled(
                        if *retryable { "  ·  retryable" } else { "" }.to_string(),
                        quiet(),
                    ),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::Complete { model, cost_usd } => {
            // The turn's receipt, and now the *only* spend line in the
            // transcript — the per-call `BudgetTick` rows that used to print
            // four or five running subtotals per turn are gauge-only (see
            // `SessionModel::apply`). Because it is the one line, it can afford
            // to be the definite one: green for a settled amount, and the model
            // that actually answered spelled out beside it rather than left to
            // the statline.
            push_note(
                "✓ cost",
                loud(theme::SUCCESS_BRIGHT),
                vec![
                    Span::styled(
                        textline::fmt_cost(*cost_usd),
                        Style::new()
                            .fg(theme::SUCCESS_BRIGHT)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("  ·  {model}"), quiet()),
                ],
                width,
                out,
            );
        }
    }
}

// ── Context recall ──────────────────────────────────────────────────────────
//
// A recall is a handful of records with four attributes each — a small table —
// and it used to render as its labels comma-joined into a paragraph that
// wrapped mid-word at the pane edge. The reader could not tell where one record
// ended and the next began, could not tell an 800-token episodic memory from a
// 60-token graph symbol, and had no way to ask for more: `ctrl+o` did not apply
// to the entry, and there was nothing left in the read-model for it to reveal.
//
// So: a header row that summarises, and one aligned row per frame. Alignment is
// the whole point — with the per-frame cost in a right-hand column, the one
// frame eating two thirds of the budget is visible by scanning a strip of
// digits, which is a finding no paragraph can carry.

/// Frames shown before the fold. Sized so the collapsed block is no taller
/// than the wrapped paragraph it replaces (four rows, in the pane that
/// prompted this) while being scannable rather than run together.
const RECALL_PREVIEW: usize = 3;

/// Widest a citation label grows before it is elided. Labels are heterogeneous
/// by nature — `fn review` beside a whole recalled user prompt — so the column
/// is capped rather than sized to the widest, which one episodic memory would
/// otherwise push past the pane edge on its own.
const RECALL_LABEL_MAX: usize = 34;

/// Narrowest a label column shrinks to before the location is dropped instead.
/// Below this a label is not a citation, it is an initial.
const RECALL_LABEL_MIN: usize = 12;

/// Widest a location grows before it is left-elided.
const RECALL_LOCATION_MAX: usize = 38;

/// Narrowest a location column is worth keeping. A `path:line` shorter than
/// this is all ellipsis and no path.
const RECALL_LOCATION_MIN: usize = 14;

/// The kind column, wide enough for the longest protocol kind (`snippet`).
const RECALL_KIND_COL: usize = 8;

/// Column for a recall's second-level detail — a frame's provenance chain, a
/// budget leg. One step past [`BODY`], so the hierarchy reads without a rule.
const RECALL_DETAIL: usize = BODY + 2;

/// The provider column in the expanded budget breakdown, sized for the two
/// legs that ship (`workspace-memory`, `code-graph`).
const RECALL_LEG_COL: usize = 18;

/// The transcript rows for one context recall.
#[allow(clippy::too_many_arguments)]
fn recall_lines(
    frames: &[crate::model::RecalledFrameRow],
    tokens: u32,
    latency_ms: u32,
    used_ann_index: Option<bool>,
    providers: &[(String, u32)],
    budget: Option<&crate::model::RecallBudget>,
    expanded: bool,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    let dim = Style::new().fg(theme::MUTED);

    // Header: the two numbers that are always worth having, then the two that
    // say whether recall was the reason the turn felt slow.
    let mut head = vec![Span::styled(
        format!(
            "{} · {tokens} tok",
            plural(frames.len() as u64, "frame", "frames")
        ),
        value(),
    )];
    // `0` means *not measured* on the wire, never "instant" — printing `0ms`
    // would invent a measurement. Omitting it says only what is known.
    if latency_ms > 0 {
        head.push(Span::styled(
            format!("  ·  {}", human_duration(u64::from(latency_ms))),
            quiet(),
        ));
    }
    // Tri-state, and rendered tri-state: `ann` (the index fired), `scan` (it
    // did not), nothing at all (no recall path reported). A `bool` here would
    // print `scan` on every real turn and read as "the index never fires".
    if let Some(ann) = used_ann_index {
        head.push(Span::styled(
            if ann { "  ·  ann" } else { "  ·  scan" }.to_string(),
            quiet(),
        ));
    }
    push_note("◉ recalled", quiet(), head, width, out);

    let shown = if expanded {
        frames.len()
    } else {
        RECALL_PREVIEW.min(frames.len())
    };
    // One column layout for the whole block, computed once. Per-row layout
    // would let each row pick a different label width and the table would stop
    // being a table — alignment is the entire reason this is not a paragraph.
    let cols = RecallColumns::fit(&frames[..shown], width);
    for frame in &frames[..shown] {
        // Deliberately **no rail glyph**. `Rail::Result`'s `⎿` means "the
        // outcome of the call above", and borrowing it here would put five
        // false hits per turn into the margin a reader scans for "what ran and
        // how did it go". Recall is bookkeeping — the same judgement `quiet()`
        // documents — so it takes the subordinate body column and recedes.
        push_recall_row(
            justify(
                recall_frame_spans(frame, &cols),
                vec![Span::styled(
                    format!("{:>w$} tok", frame.tokens, w = cols.metric_digits),
                    dim,
                )],
                width,
                BODY,
            ),
            BODY,
            width,
            out,
        );
        // Expanded only: the provenance chain, which is the whole reason a
        // detail view exists here. `provider ← source` is deliberately two
        // fields — an adapter fronting another store (`workspace-memory` over
        // `stella-context`) is exactly the case a single field would hide.
        if expanded {
            push_recall_row(
                vec![Span::styled(recall_provenance(frame), dim)],
                RECALL_DETAIL,
                width,
                out,
            );
        }
    }

    if !expanded {
        let hidden = frames.len() - shown;
        // `⋯` is this UI's one glyph for "there is more behind this", and it
        // carries the ctrl+o affordance everywhere else in the file. The row
        // is emitted even at zero hidden frames, because provenance and the
        // budget report are *always* behind the fold and an affordance nobody
        // knows about is the same as no affordance.
        //
        // The hidden frames' token cost rides on it, and that is the whole
        // reason the fold can afford to keep the host's render order rather
        // than promoting the expensive frames into the preview. In the recall
        // that prompted this work the two folded frames carried 908 of 1155
        // tokens — reordering would have shown the outlier while lying about
        // what the model actually saw; naming the number shows it and does not.
        let more = if hidden > 0 {
            let cost: u32 = frames[shown..].iter().map(|f| f.tokens).sum();
            format!("⋯ {hidden} more · {cost} tok · ctrl+o for provenance and budget")
        } else {
            "⋯ ctrl+o for provenance and budget".to_string()
        };
        push_recall_row(
            vec![Span::styled(more, Style::new().fg(theme::TEXT_TERTIARY))],
            BODY,
            width,
            out,
        );
        return;
    }

    // The bill. One row per leg rather than one long joined line: the legs are
    // the same shape as the frame rows above them, so the same scan works on
    // both, and a joined line wrapped at any ordinary pane width — splitting
    // `2 rejected` across two rows, which is precisely the phrase this exists
    // to surface.
    //
    // When a budget report is present it *subsumes* the provider mix: the mix
    // counts frames that won fusion and reached the prompt, the report counts
    // what each leg served and what the host rejected. Showing both puts two
    // similar-looking counts side by side that mean different things, and the
    // report is the strict superset.
    match budget {
        Some(b) => {
            push_recall_row(
                vec![Span::styled(
                    format!("budget {} of {} tok", b.consumed, b.requested),
                    dim,
                )],
                BODY,
                width,
                out,
            );
            for (provider, served, rejected, tok) in &b.providers {
                let counts = if *rejected > 0 {
                    format!("{served} served · {rejected} rejected · {tok} tok")
                } else {
                    format!("{served} served · {tok} tok")
                };
                push_recall_row(
                    vec![Span::styled(
                        format!("{} {counts}", pad_to(provider, RECALL_LEG_COL)),
                        dim,
                    )],
                    RECALL_DETAIL,
                    width,
                    out,
                );
            }
        }
        // No report — say which legs the frames came from, which is all the
        // event carries when no CGP host produced a usage report.
        None if !providers.is_empty() => {
            let mix = providers
                .iter()
                .map(|(p, n)| format!("{p} {n}"))
                .collect::<Vec<_>>()
                .join(" · ");
            push_recall_row(
                vec![Span::styled(format!("via {mix}"), dim)],
                BODY,
                width,
                out,
            );
        }
        None => {}
    }
}

/// Emit one recall row at `indent`, wrapping to the same column.
///
/// [`push_detail_line`] would do this but takes a `&str` in one fixed style,
/// and a recall row is a styled composite — a tertiary kind, a white citation,
/// a dim path, a dim metric flushed right.
fn push_recall_row(
    spans: Vec<Span<'static>>,
    indent: usize,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    let mut row = vec![Span::raw(" ".repeat(indent))];
    row.extend(spans);
    wrap_one_indent(Line::from(row), width, indent, out);
}

/// The left half of a frame row: `kind`, then the citation, then where it came
/// from in the tree.
///
/// The kind is the field the old rendering lost and the one that changes how a
/// row is read — a `memory` and a `symbol` cost the prompt the same tokens and
/// mean entirely different things about what retrieval did.
fn recall_frame_spans(
    frame: &crate::model::RecalledFrameRow,
    cols: &RecallColumns,
) -> Vec<Span<'static>> {
    let dim = Style::new().fg(theme::MUTED);
    // An empty kind is a stream recorded before the field existed. `frame`
    // says that honestly; a blank column would read as a rendering bug.
    let kind = if frame.kind.is_empty() {
        "frame"
    } else {
        frame.kind.as_str()
    };
    let mut spans = vec![Span::styled(
        pad_to(kind, RECALL_KIND_COL),
        Style::new().fg(theme::TEXT_TERTIARY),
    )];
    let label = elide(&frame.label, cols.label);
    let Some(uri) = frame.uri.as_deref().filter(|_| cols.location > 0) else {
        spans.push(Span::styled(label, value()));
        return spans;
    };
    spans.push(Span::styled(pad_to(&label, cols.label + 2), value()));
    // The location, elided from the *left*: a recall row is actionable because
    // of its filename and line, and clipping from the right — which is what the
    // pane edge did — removes exactly that and keeps the repo prefix every row
    // already shares.
    spans.push(Span::styled(recall_location(uri, cols.location), dim));
    spans
}

/// The column widths one recall block renders at.
///
/// Computed from the pane rather than fixed, because the two elastic columns
/// fail in opposite directions when space runs out. A citation label is prose
/// and elides gracefully — half of `create a new minor release…` still says
/// what it is. A location does not: it is a `path:line`, and the tail is the
/// entire point. So the location is budgeted **first** and the label absorbs
/// the pressure.
///
/// Leaving this to [`justify`] is the bug this type exists to fix. `justify`
/// truncates its left column from the *right* to make room for the metric,
/// which is exactly backwards here: in a 100-column deck pane it cut
/// `crates/stella-core/src/driver.rs:88` down to
/// `crates/stella-core/src/driver.r…`, deleting the filename and line while
/// keeping the repo prefix every row on screen already shares.
struct RecallColumns {
    label: usize,
    /// `0` when the pane cannot afford a location column that would still say
    /// something — dropped whole rather than rendered as an ellipsis.
    location: usize,
    /// Digits the token counts are right-aligned within, so the metric reads
    /// as one strip a reader can run an eye down.
    metric_digits: usize,
}

impl RecallColumns {
    fn fit(frames: &[crate::model::RecalledFrameRow], width: usize) -> Self {
        let metric_digits = frames
            .iter()
            .map(|f| f.tokens.to_string().len())
            .max()
            .unwrap_or(1);
        // What `justify` will leave the left column: the pane, less the body
        // indent, less the metric and the one space it insists on.
        let avail = width
            .saturating_sub(BODY)
            .saturating_sub(metric_digits + " tok".len() + 1);
        let rest = avail.saturating_sub(RECALL_KIND_COL);

        // The widest location any frame in *this* block needs, so a block of
        // short paths does not reserve a column for a long one that is absent.
        let widest = frames
            .iter()
            .filter_map(|f| f.uri.as_deref())
            .map(|u| recall_location(u, RECALL_LOCATION_MAX).chars().count())
            .max()
            .unwrap_or(0);

        // The label keeps its minimum before the location gets anything; below
        // that the location is dropped entirely rather than both being starved
        // into two columns of ellipsis.
        let for_location = rest.saturating_sub(RECALL_LABEL_MIN + 2);
        let location = if widest == 0 || for_location < RECALL_LOCATION_MIN {
            0
        } else {
            widest.min(for_location)
        };
        let gap = usize::from(location > 0) * 2;
        let label = rest
            .saturating_sub(location + gap)
            .clamp(RECALL_LABEL_MIN, RECALL_LABEL_MAX);
        Self {
            label,
            location,
            metric_digits,
        }
    }
}

/// The dim provenance line under an expanded frame row.
fn recall_provenance(frame: &crate::model::RecalledFrameRow) -> String {
    let mut parts = Vec::new();
    if frame.provider.is_empty() {
        parts.push(frame.source.clone());
    } else if frame.provider == frame.source || frame.source.is_empty() {
        parts.push(frame.provider.clone());
    } else {
        parts.push(format!("{} ← {}", frame.provider, frame.source));
    }
    if let Some(m) = &frame.method {
        parts.push(m.clone());
    }
    if let Some(id) = &frame.id {
        parts.push(id.clone());
    }
    // A missing digest is not nothing: per the context-reuse spec such a frame
    // is *not verifiable* and a host must re-query rather than reuse it. Saying
    // so is the point of showing the field at all.
    match &frame.digest {
        Some(d) => parts.push(short_digest(d)),
        None => parts.push("unverifiable (no digest)".to_string()),
    }
    parts.join(" · ")
}

/// `sha256:9f2c1ab…` — enough to compare two frames by eye, short enough to
/// share a row with the rest of the provenance chain.
fn short_digest(digest: &str) -> String {
    match digest.split_once(':') {
        Some((algo, hex)) => format!("{algo}:{}…", &hex[..hex.len().min(7)]),
        None => format!("{}…", &digest[..digest.len().min(14)]),
    }
}

/// A frame's URI as a location a reader can act on, within `cap` columns.
///
/// Left-elided, which is the opposite of what the pane edge did to it: the tail
/// (`…/command_deck/hunk_gate.rs:32`) is the part that identifies the frame,
/// and the head is a repo prefix every row on screen already shares.
fn recall_location(uri: &str, cap: usize) -> String {
    // Strip a scheme so `file:///…` and a bare path render alike; the scheme
    // is never the discriminating part of a recall row.
    let path = uri.split_once("://").map_or(uri, |(_, rest)| rest);
    if path.chars().count() <= cap {
        return path.to_string();
    }
    let tail: String = path
        .chars()
        .skip(path.chars().count() - cap.saturating_sub(1))
        .collect();
    // Cut on a separator so the elision lands between path segments rather
    // than mid-directory-name — but only when that leaves a real path behind.
    // Snapping to a separator that sits near the end would trade a readable
    // `…re/src/driver.rs:88` for a useless `…/driver.rs:88`… of the wrong
    // width, so the snap is taken only in the leading third.
    match tail.find('/').filter(|cut| *cut <= cap / 3) {
        Some(cut) => format!("…{}", &tail[cut..]),
        None => format!("…{tail}"),
    }
}

/// Truncate to `cap` display columns with a trailing `…`, never mid-wrap.
fn elide(text: &str, cap: usize) -> String {
    if text.chars().count() <= cap {
        return text.to_string();
    }
    let kept: String = text.chars().take(cap.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// Right-pad to a soft column. Soft, like [`pad_name`]: an over-wide field
/// overruns rather than truncating, since identity outranks alignment.
fn pad_to(text: &str, col: usize) -> String {
    let w = UnicodeWidthStr::width(text);
    format!("{text}{}", " ".repeat(col.saturating_sub(w).max(1)))
}

fn pr_status_color(status: PrStatus) -> Color {
    // A ramp toward the brand accent as the PR matures, so the `[⇢ pr]:`
    // gutter reads with the rest of the transcript: warning-orange draft, deep
    // gold while open, full gold on merge, danger on close. (The "ember"
    // family this comment used to name was retired with the aurora→gold
    // recolour — see `theme`'s palette-law test.)
    match status {
        PrStatus::Draft => theme::WARNING,
        PrStatus::Open => theme::ACCENT_DEEP,
        PrStatus::Merged => theme::ACCENT,
        PrStatus::Closed => theme::DANGER,
    }
}

fn ci_status_color(status: CiStatus) -> Color {
    match status {
        CiStatus::Pending => theme::TEXT_TERTIARY,
        CiStatus::Running => theme::WARNING_BRIGHT,
        CiStatus::Passing => theme::OK,
        CiStatus::Failing => theme::BAD,
    }
}
