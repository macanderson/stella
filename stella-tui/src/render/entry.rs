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

use stella_protocol::{CiStatus, PrStatus, SubAgentStatus};

use crate::model::{FileState, SessionModel, TranscriptEntry};
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

/// The full visual-line list for the transcript. Each entry is rendered with
/// per-entry wrapping so continuation lines respect the label column. An
/// in-flight streaming preview (`SessionModel::streaming_text`) renders as a
/// live trailing agent entry — it is not a transcript entry, so the
/// authoritative `Text` event replaces it without leaving a duplicate row.
pub(crate) fn transcript_lines(
    model: &SessionModel,
    expand_thinking: bool,
    width: usize,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let live = reasoning_is_live(&model.transcript, &model.streaming_text);
    let last = model.transcript.len().saturating_sub(1);
    for (i, entry) in model.transcript.iter().enumerate() {
        entry_lines(
            entry,
            &model.files,
            expand_thinking,
            expand_thinking,
            live && i == last,
            width,
            &mut out,
        );
    }
    streaming_lines(
        &model.streaming_text,
        &model.files,
        expand_thinking,
        width,
        &mut out,
    );
    out
}

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
const THINKING_ROWS: usize = 5;

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
            labels,
        } => {
            let cited = labels.join(", ");
            push_note(
                "◉ recalled",
                quiet(),
                vec![
                    Span::styled(
                        format!(
                            "{} · {tokens} tok",
                            plural(*frames as u64, "frame", "frames")
                        ),
                        value(),
                    ),
                    Span::styled(format!("  ·  {cited}"), quiet()),
                ],
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
        TranscriptEntry::JudgeVerdict {
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
                "model-judge"
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
                "⏸ scope",
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
            push_note("☰ tasks", loud(theme::VIOLET), spans, width, out);
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
