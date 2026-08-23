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

use crate::model::{FileState, TranscriptEntry};
use crate::render::row::*;
use crate::textline::{
    budget_mode_label, ci_status_label, media_kind_label, media_state_label, pr_status_label,
    stage_label, steering_withheld,
};
use crate::theme;

// The context-recall table. Split out rather than grown here: it is the one
// entry kind that lays out a *grid* — fitted columns, a heading, a rule, a
// second grid for the budget legs — and that machinery is a concern of its own,
// with an invariant (`cell` returns exactly its column's width) that only holds
// if nothing outside it composes a recall row by hand. Keeping it in a child
// module means it reaches this file's private `quiet`/`value`/`plural` without
// widening anything, exactly as `entry` itself reaches `render`'s.
mod recall;
use recall::recall_lines;

// The tool-call block — the argument body under a call head, and the result's
// metric row, preview and inline diff. Split out on `recall`'s test: it stacks
// four rendering decisions that are each their own concern, and it is the arm
// that grows whenever a tool gains one (#4230). Its module doc carries the
// argument.
mod tool;

// Pure content builders (unit-tested directly)

/// What a transcript row needs to know beyond its own entry.
///
/// Both fields exist because a row is not a function of its entry alone. An
/// inline diff resolves against the draw-side file ledger; and a tool **head**
/// states a size nothing has measured at the moment it dispatches — the
/// emitter's `(added, removed)` arrives with the call's *result*, a later entry,
/// and is only recorded once the turn boundary has measured the tree (#4154).
///
/// Bundled rather than passed as two more positional arguments: [`entry_lines`]
/// already carries three booleans, and the pair travels together everywhere —
/// pairing one entry's ledger with another entry's tail is not a call a caller
/// should be able to make by ordering its arguments wrongly.
#[derive(Clone, Copy, Default)]
pub(crate) struct EntryView<'a> {
    /// The draw-side file ledger every diff and delta resolves against.
    pub files: &'a [FileState],
    /// The lane's entries *after* the one being rendered, in order.
    pub following: &'a [TranscriptEntry],
}

impl<'a> EntryView<'a> {
    /// The view entry `idx` is rendered against.
    pub fn at(files: &'a [FileState], transcript: &'a [TranscriptEntry], idx: usize) -> Self {
        Self {
            files,
            following: transcript.get(idx.saturating_add(1)..).unwrap_or_default(),
        }
    }

    /// A view with no tail, for a row that has no later entry to consult: the
    /// streaming answer preview, which is not in the transcript at all.
    pub fn of(files: &'a [FileState]) -> Self {
        Self {
            files,
            following: &[],
        }
    }
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
    entry_lines(
        &preview,
        EntryView::of(files),
        expand_thinking,
        false,
        false,
        width,
        out,
    );
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
    view: EntryView<'_>,
    expand_thinking: bool,
    expanded: bool,
    live: bool,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    if !v2_rows(entry, view, expanded, width, out) {
        entry_body(entry, view, expand_thinking, expanded, live, width, out);
    }
    if closes_block(entry) {
        push_gap(out);
    }
}

/// SPEC 6 rows for the entries the v2 transcript owns; `false` leaves the entry
/// to the v1 renderer below.
///
/// Two arms, deliberately: a tool call's **head** and compaction's one quiet
/// line. Everything else still renders v1 until its phase lands, which is why
/// this is a router rather than a replacement — a half-migrated transcript must
/// still draw every row it holds, not drop the ones v2 has no arm for yet.
///
/// A tool **result** is pointedly not here. Its body already carries syntax
/// highlighting in the file's own language, inline word-level diffs, a line
/// number gutter and a truncation notice naming the key that reveals the rest
/// (#4019, #4020, #4036) — and SPEC 6.4 keeps every one of them. Routing the
/// result through a v2 renderer that has not been taught them yet would delete
/// working features to make the screen look newer, which is a regression
/// wearing a redesign's clothes. The result row is restyled in P2, where the
/// highlighter it needs is built, not here.
///
/// **A router still owes every feature of the arm it intercepts.** This one did
/// not: taking the `ToolStart` head made the whole v1 arm unreachable, and the
/// `ctrl+o` argument view living in its second half went with it — silently,
/// because a dead *match arm* is invisible to `dead-code-allows` and to
/// `module-reachability`, which see items. The row rendered identically
/// expanded and collapsed for as long as nobody pressed the key (#4157). Hence
/// `expanded` here: a router that takes a head takes the body under it too.
///
/// The head's size column is the other thing this arm owes, and it is why the
/// router takes a [`EntryView`] rather than the entry alone: a `ToolStart` is
/// drawn at dispatch, when nothing has measured the change it is about to make,
/// so the number can only come from the paired result and the ledger behind it
/// (#4154). Resolving it here rather than inside the projection keeps
/// [`crate::v2::transcript_source::head_rows`] a pure function of what is known
/// about one call. (Spelled in full: the `v2` alias below is a `use` inside the
/// body, and rustdoc resolves a link against the module, not the function.)
fn v2_rows(
    entry: &TranscriptEntry,
    view: EntryView<'_>,
    expanded: bool,
    width: usize,
    out: &mut Vec<Line<'static>>,
) -> bool {
    use crate::v2::transcript_source as v2;
    match entry {
        TranscriptEntry::ToolStart {
            call_id,
            name,
            input,
            raw,
            path,
        } => {
            let measured = v2::measured_delta(call_id, view.following, view.files);
            out.extend(v2::head_rows(name, path.as_deref(), input, measured, width));
            if expanded {
                tool::argument_rows(v2::head_metal(name), raw, width, out);
            }
            true
        }
        TranscriptEntry::Compaction {
            before_tokens,
            after_tokens,
            evicted,
            deduped,
        } => {
            out.extend(v2::compaction_rows(
                *before_tokens,
                *after_tokens,
                *evicted,
                *deduped,
            ));
            true
        }
        TranscriptEntry::Complete {
            cost_usd,
            turn,
            receipt,
            ..
        } => {
            out.extend(v2::turn_end_rows(*turn, *cost_usd, receipt, width));
            true
        }
        // Only the boundary that *opens* the turn. A later stage of the same
        // turn falls through to the plain section rule below, which is what
        // keeps SPEC 6.1's labelled rule one-per-turn rather than one-per-stage
        // (see `model::TurnOpening`).
        TranscriptEntry::Stage {
            name,
            opens: Some(opening),
        } => {
            out.extend(v2::turn_begin_rows(
                opening.turn,
                stage_label(name),
                opening.model.as_deref(),
                opening.budget_usd,
                opening.queued_steer.as_deref(),
                width,
            ));
            true
        }
        _ => false,
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
    view: EntryView<'_>,
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
        // Reached only for a boundary *inside* a turn — the one that opens the
        // turn is claimed by the v2 router above, which draws SPEC 6.1's
        // labelled rule instead.
        TranscriptEntry::Stage { name, .. } => {
            // A section rule, not a row — see `push_rule`. Quiet, in the rule
            // tone with a muted lowercase word: SPEC 6.1 makes the *turn* the
            // transcript's rhythm, and its opening rule (drawn by the v2
            // router above) already names the stage the turn opened in. A
            // later stage of the same turn is a sub-heading, and a bold
            // uppercase one outshouted the turn rules it sits between.
            push_rule(
                &stage_label(name).to_lowercase(),
                Style::new().fg(stella_tui_theme::token::MUTED),
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
        TranscriptEntry::ToolResult { .. } => tool::result_body(entry, view, expanded, width, out),
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
        // Composed from `textline::steering_withheld`, the same wording the
        // plain door prints (#4463). Rebuilt here it would be a second copy
        // of a sentence whose whole point is naming the *right* remedy, and
        // the two surfaces would drift on the day one of them was corrected.
        TranscriptEntry::SteeringWithheld {
            withheld_by,
            memories,
            records,
            skills,
            commands,
            agents,
        } => {
            let line = steering_withheld(
                *withheld_by,
                &[
                    (*memories, "memory", "memories"),
                    (*records, "context record", "context records"),
                    (*skills, "skill", "skills"),
                    (*commands, "command", "commands"),
                    (*agents, "agent", "agents"),
                ],
            );
            let mut content = vec![Span::styled(line.body, value())];
            if let Some(detail) = line.detail {
                content.push(Span::styled(format!("  ·  {detail}"), quiet()));
            }
            push_note("⚠ steering", loud(theme::WARNING), content, width, out);
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
            // A dispatch is the single most consequential row in a transcript
            // — a whole other agent starts working here, with its own budget
            // and its own tool calls — and it rendered in the quiet tier, the
            // same one the bookkeeping notes use. It gets the delegation class
            // hue, the same one `delegate`/`task_*` wear on the call rows around
            // it, so the hand-off and the board it moves read as one family.
            None => push_note(
                "⤷ sub-agent",
                loud(crate::tool_class::ToolClass::Delegate.color()),
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
        // Owned by the v2 router above, which returns `true` for it and so
        // never falls through to here — the closing rule and receipt of
        // SPEC 6.1. The arm exists because the match must stay exhaustive, and
        // it draws nothing rather than panicking: if the router is ever
        // narrowed, a missing turn rule is a visible gap a reader can report,
        // where an `unreachable!()` would take the session down mid-frame.
        TranscriptEntry::Complete { .. } => {}
        // Also the router's — head *and*, on ctrl+o, the argument object under
        // it. This one **delegates** rather than drawing nothing, which is the
        // difference between the two arms and the lesson of #4157: the row a
        // gap here would cost is the call itself, the single most load-bearing
        // row in the transcript, and the previous version of this arm sat here
        // looking live while the router quietly took its `expanded` half away.
        // Delegating means there is one implementation to keep correct and no
        // second one to rot.
        TranscriptEntry::ToolStart { .. } => {
            v2_rows(entry, view, expanded, width, out);
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
        PrStatus::Open => theme::ACCENT,
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
