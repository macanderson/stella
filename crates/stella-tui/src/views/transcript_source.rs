//! The live projection behind [`super::transcript`] — SPEC 6 over the deck's
//! own [`crate::TranscriptEntry`] stream.
//!
//! ## Why the pair maps to two calls, not one
//!
//! SPEC 6.2 describes an event as one thing: a head, a body, a footer, all
//! sharing a rail. The deck's transcript records it as two entries — a
//! [`crate::TranscriptEntry::ToolStart`] when the call is dispatched and a
//! [`crate::TranscriptEntry::ToolResult`] when it returns — because the head has to
//! render before the result exists, and a transcript that waited for the
//! result would show nothing at all while a two-minute `cargo test` ran.
//!
//! This module projects the **head** only. The result row stays on the long-form
//! renderer until P2, because that row already carries syntax highlighting in
//! the file's own language, word-level inline diffs, a line-number gutter and a
//! truncation notice naming the key that reveals the rest (#4019, #4020,
//! #4036). SPEC 6.4 keeps every one of them, and a projected result row that has
//! not been taught them yet would delete working features to make the screen
//! look newer. P2 builds the highlighter; the row is restyled there.
//!
//! ## Open vocabulary
//!
//! An unrecognised tool is [`EventKind::Other`], never a missing row. The deck
//! gains MCP tools and workspace custom tools at runtime, so a renderer with an
//! arm per tool is a renderer that silently drops the ones a user added — the
//! reasoning [`stella_transcript::ToolKind`] already states.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use stella_tui_theme::token;

use super::transcript::{
    Event, EventKind, Extent, Receipt, Subject, Touched, TurnHead, event_rows, receipt, turn_begin,
    turn_end,
};
use crate::model::{FileState, GraphFact, ReadSize, TranscriptEntry};

/// What is known about one call at the moment its head renders — each field
/// filled by its own resolver, or its `None`/default while nothing has
/// answered.
///
/// Bundled the way `render::EntryView` bundles the draw-side pair (named
/// rather than linked: that type is crate-private, and this item is public):
/// the facts travel together into every head, and a positional list this long
/// is a call site that can pair one call's scope with another call's timing
/// by ordering its arguments wrongly.
#[derive(Default)]
pub struct CallFacts {
    /// What the emitter measured for this call — the paths it claimed and the
    /// delta summed across them ([`measured_scope`]).
    pub scope: Option<Touched>,
    /// The line coverage a read reported for itself ([`read_size`]) — a
    /// separate channel, because a read's number is a coverage the producer
    /// states, never a delta a mutation stamps.
    pub read: Option<ReadSize>,
    /// Wall time from the call's paired result ([`call_duration`]), rendered
    /// `⚡3ms`. `None` — still in flight, failed, or never answered — renders
    /// no metric at all.
    pub duration_ms: Option<u64>,
    /// The delegate that made the call, `None` for the lead's own — carried
    /// straight from `TranscriptEntry::ToolStart` so a fan-out call renders
    /// visibly apart from the lead's (#4699).
    pub sub_agent_id: Option<String>,
    /// What the code graph said about the call's path ([`graph_fact`]).
    /// `None` — no index, no fact published, nothing answered yet — renders
    /// no graph line, never a zero the check did not measure (#5034).
    pub graph: Option<GraphFact>,
    /// Whether the reader has expanded this entry (`ctrl+o`). A read head is
    /// folded until they do; every other kind ignores it here (its reveal is
    /// the argument body the caller hangs beneath).
    pub expanded: bool,
}

/// The metal-bearing head of a dispatched call (SPEC 6.2).
///
/// Always at least one row: a tool with no recognised verb still names itself,
/// because a call that rendered nothing would be a call the reader cannot see
/// happened.
#[must_use]
pub fn head_rows(
    name: &str,
    path: Option<&str>,
    input: &str,
    facts: CallFacts,
    width: usize,
) -> Vec<Line<'static>> {
    let kind = kind_for(name, facts.scope, facts.read);
    let mut event = Event::new(kind, subject_for(name, path, input));
    // A read folds by default (SPEC 6.3): `▸ read … · ↵ open` until the
    // reader opens it. Every other head draws its kind glyph — its body is
    // the result row beneath, so the head itself has no fold state. This
    // used to be `Some(false)` across the board, which made the live path
    // the one place a read never folded (#5030).
    event.collapsed = Some(matches!(event.kind, EventKind::Read { .. }) && !facts.expanded);
    event.duration_ms = facts.duration_ms.unwrap_or(0);
    event.sub_agent_id = facts.sub_agent_id;
    match facts.graph {
        // SPEC 6.3's write footer. A dim trailing line, because it reports
        // what the call already did rather than qualifying what it says.
        Some(GraphFact::RegisteredModule) => {
            event.footer = Some("  registered in graph as module node".to_string());
        }
        // SPEC 6.3's delete body: the count the check measured before the
        // unlink, and `det` — the boolean SPEC §5 reserves for a call that
        // reached no model, which a graph query never does.
        Some(GraphFact::InboundRefs(inbound)) => {
            let noun = if inbound == 1 { "ref" } else { "refs" };
            event.body = vec![Line::from(vec![
                Span::styled("  graph check: ", Style::new().fg(token::MUTED)),
                Span::styled(
                    format!("{inbound} inbound {noun} · det"),
                    Style::new().fg(token::SILVER),
                ),
            ])];
        }
        None => {}
    }
    event_rows(&event, width)
}

/// The rail metal [`head_rows`] draws this call in (SPEC 6.2) — silver-dim for
/// a read, gold for anything that acts, red for a delete.
///
/// Exposed because the head is only the first row of a call's block: the rows
/// the deck hangs *under* it — the expanded argument object — have to reproduce
/// the same rail, and deriving the colour a second time from the tool name is
/// how the two ends of one block come to disagree about what metal it is. One
/// derivation, read by both.
#[must_use]
pub fn head_metal(name: &str) -> Color {
    // The metal is a fact about the verb, so the extent is irrelevant here and
    // an unmeasured one is the right argument rather than a placeholder: a
    // block's rail must not change colour when its size arrives.
    kind_for(name, None, None).metal()
}

/// What the emitter measured for the call `call_id` dispatched — the paths it
/// claimed and the delta summed across them — or `None` while nothing has
/// measured it.
///
/// `following` is the rest of the lane's transcript *after* the head, and
/// `files` the draw-side ledger. Two hops:
///
/// 1. **The references, by `call_id`.** A [`TranscriptEntry::ToolResult`] carries an
///    [`crate::model::InlineDiffRef`] per change its own call produced, and
///    shares its `call_id` with the head. The scan stops at the turn's
///    closing entry: a result cannot land after the turn that dispatched it
///    completed, so this bounds the walk by turn length rather than by session
///    length, and an unanswered head (a cancelled call) costs one turn's scan.
/// 2. **The measurement, through those references.**
///    `render::resolve_inline_delta_total` (crate-private, so there is no link
///    to it) reads [`crate::model::FileState::delta_at`] for each — the counts
///    the *emitter* measured — and sums them, so the head states **the whole
///    call**, every path it claimed, and not the first one it happened to lead
///    with (#4214). Never the tool's input, never a recount of the rendered
///    diff: the first is what #2290 established as the defect (`edit_file` with
///    `replace_all` makes an input-derived number wrong outright), and the
///    second counts a bounded rendering of the changed region rather than the
///    change.
///
/// `None` therefore covers three cases — the call has not returned, it
/// failed (a failed mutation stamps no reference), or the turn boundary has not
/// measured the tree yet — and each of them renders as no column at all.
///
/// The **scope** rides back with the delta rather than being resolved a second
/// time, because the two are one reading: the file count is over the same
/// references the delta is summed across, and a head stating three files beside
/// a delta summed over two is a row that contradicts itself (#4319).
#[must_use]
pub fn measured_scope(
    call_id: &str,
    following: &[TranscriptEntry],
    files: &[FileState],
) -> Option<Touched> {
    following
        .iter()
        .take_while(|e| !matches!(e, TranscriptEntry::Complete { .. }))
        .find_map(|e| match e {
            TranscriptEntry::ToolResult {
                call_id: cid, diff, ..
            } if cid == call_id => Some(diff.as_slice()),
            _ => None,
        })
        .filter(|refs| !refs.is_empty())
        .map(|refs| Touched {
            files: crate::model::distinct_diff_paths(refs),
            extent: crate::render::resolve_inline_delta_total(refs, files)
                .map_or_else(Extent::default, |(added, removed)| {
                    Extent::delta(added, removed)
                }),
        })
}

/// The line coverage the call `call_id` reported for itself, or `None` while
/// nothing has.
///
/// The same bounded scan as [`measured_scope`] — the pair by `call_id`, ended
/// at the turn's closing entry — but a different channel at the join: the
/// number comes off the entry's own [`ReadSize`] carrier, which the fold fills
/// from the tool result's structured `data` (`read_file`'s
/// `lines_shown`/`lines_total`, #4297). Never through an inline-diff
/// reference: a read stamps none, correctly, because it changes nothing —
/// which is why the two resolvers are two functions rather than one with a
/// mode.
///
/// `None` here means the call has not returned, it failed, or its tool
/// predates the payload — and each renders as no column.
#[must_use]
pub fn read_size(call_id: &str, following: &[TranscriptEntry]) -> Option<ReadSize> {
    following
        .iter()
        .take_while(|e| !matches!(e, TranscriptEntry::Complete { .. }))
        .find_map(|e| match e {
            TranscriptEntry::ToolResult {
                call_id: cid,
                read_size,
                ..
            } if cid == call_id => Some(*read_size),
            _ => None,
        })
        .flatten()
}

/// What the code graph said about the call `call_id`'s own path, or `None`
/// while nothing has said anything (#5034).
///
/// The same bounded scan as [`read_size`], off the entry's own [`GraphFact`]
/// carrier, which the fold fills from the tool result's structured `data`.
/// `None` covers every reason there is no fact — the call has not returned,
/// it failed, this workspace has no code graph, the tool publishes none —
/// and every one of them renders as no line at all.
#[must_use]
pub fn graph_fact(call_id: &str, following: &[TranscriptEntry]) -> Option<GraphFact> {
    following
        .iter()
        .take_while(|e| !matches!(e, TranscriptEntry::Complete { .. }))
        .find_map(|e| match e {
            TranscriptEntry::ToolResult {
                call_id: cid,
                graph,
                ..
            } if cid == call_id => Some(*graph),
            _ => None,
        })
        .flatten()
}

/// Wall time of the call `call_id`, from its paired result — or `None` while
/// nothing has answered it.
///
/// The same bounded scan as [`measured_scope`] and [`read_size`], read off the
/// result entry's own `duration_ms`. A zero reading maps to `None` rather than
/// to a `⚡0ms` metric: the emitters stamp zero for a synthetic echo (a gate
/// answer, a demo reply), and "answered instantly" and "not a timed call" are
/// not worth a column that cannot tell them apart.
#[must_use]
pub fn call_duration(call_id: &str, following: &[TranscriptEntry]) -> Option<u64> {
    following
        .iter()
        .take_while(|e| !matches!(e, TranscriptEntry::Complete { .. }))
        .find_map(|e| match e {
            TranscriptEntry::ToolResult {
                call_id: cid,
                duration_ms,
                ..
            } if cid == call_id => Some(*duration_ms),
            _ => None,
        })
        .filter(|ms| *ms > 0)
}

/// What SPEC 6.3's model footer can say without inventing a number.
///
/// `irreducible generation` is a **fact about the call**, not a measurement:
/// this work reached a model, so it was not the deterministic path SPEC 1
/// prefers, and that is exactly what the deck's `$0.00 · det` gate row says in
/// the other direction. It needs nothing behind it, so it stays on the row the
/// way `new file` and `git-backed · u undo` stay on theirs when their counts
/// have not arrived.
///
/// SPEC 6.3's second clause — `n of m budgeted model calls this turn` — is
/// **elided**, because nothing in this workspace budgets model calls per turn.
/// `EngineConfig::max_steps` is the nearest number and its own doc calls it a
/// "hard backstop on step count … never the *primary* stuck-loop defense", so
/// `3 of 200 budgeted` would report a backstop as a plan. `n` alone cannot
/// stand in either: `n of m` is one reading, and half of it says something
/// else. #5234 is where a real per-turn model-call budget is tracked; when one
/// exists, the clause comes back here.
const MODEL_FOOTER: &str = "   irreducible generation";

/// One settled model call (SPEC 6.3) — `◐ model <activity> · tok/s` over the
/// gold-bright rail that marks generation, with `MODEL_FOOTER` under it.
///
/// `activity` is the wire's own word for the call's role and `None` for a role
/// this build cannot identify; the head then names no activity rather than one
/// nothing recorded. `tokens_per_sec` is `None` for a call whose rate has no
/// inputs to divide — see [`crate::model::TranscriptEntry::Model`], whose fold
/// resolves both.
#[must_use]
pub fn model_rows(
    activity: Option<&str>,
    tokens_per_sec: Option<u32>,
    duration_ms: u64,
    sub_agent_id: Option<String>,
    width: usize,
) -> Vec<Line<'static>> {
    let mut event = Event::new(
        EventKind::Model { tokens_per_sec },
        activity.unwrap_or_default(),
    );
    event.duration_ms = duration_ms;
    event.sub_agent_id = sub_agent_id;
    event.footer = Some(MODEL_FOOTER.to_string());
    event_rows(&event, width)
}

/// One dim line, no rail (SPEC 6.3).
#[must_use]
pub fn compaction_rows(
    before: u64,
    after: u64,
    evicted: usize,
    deduped: usize,
) -> Vec<Line<'static>> {
    event_rows(
        &Event::new(
            EventKind::Compaction {
                from_tokens: before,
                to_tokens: after,
                evicted: evicted as u32,
                deduped: deduped as u32,
            },
            String::new(),
        ),
        0,
    )
}

/// Map a wire tool name onto SPEC 6.3's event vocabulary.
///
/// The same six names [`stella_transcript::ToolKind::from_name`]
/// recognises, so the two renderers of the same transcript cannot disagree
/// about what a `bash` row is.
///
/// `scope` is what the emitter measured for this call once [`measured_scope`]
/// has resolved it, and `None` for as long as nothing has — which is every head
/// at the moment it dispatches, because the tool has not returned and no
/// `FileChange` has been emitted. An unmeasured kind renders **no size column
/// at all**: filling the fields with zeros instead put `edit <path> +0 -0` over
/// every real edit in the deck — a row asserting the change was empty, on the
/// one screen a reader consults to find out what changed (#4150).
///
/// Which half of the measurement a kind states is a property of the verb, and
/// lives here so one measurement cannot be read two ways: an edit states both
/// sides (they are one reading), a write states what it wrote, a deletion what
/// it removed. `run` and an unrecognised tool state the **scope** as well,
/// because their subject cell is a command line or a tool name rather than a
/// path, so the counts alone would not say what they are counts of (#4319).
///
/// A read states its **coverage**, and it arrives on the separate `read`
/// channel, never through `scope`: only a *mutation* stamps the inline-diff
/// reference [`measured_scope`] resolves through, so `scope` is `None` for a
/// read on every live path and always was. #4180 removed the `Extent` the read
/// once misfiled its size under; #4297 gave the number a real producer
/// ([`read_size`]) and the column its own field.
fn kind_for(name: &str, scope: Option<Touched>, read: Option<ReadSize>) -> EventKind {
    let extent = scope.map_or_else(Extent::default, |scope| scope.extent);
    match name {
        "read_file" => EventKind::Read { lines: read },
        "edit_file" => EventKind::Edit { extent },
        "write_file" => EventKind::Write {
            extent: Extent {
                added: extent.added,
                removed: None,
            },
        },
        "delete_file" => EventKind::Delete {
            extent: Extent {
                added: None,
                removed: extent.removed,
            },
        },
        "bash" => EventKind::Run { touched: scope },
        // Never mapped onto one of the five above: a server's `read_file` is
        // not necessarily this workspace's `read`, and a familiar verb on a row
        // that did something else is the one error a transcript cannot afford.
        // It keeps its name as the subject instead — see `subject_for`.
        //
        // The class is the one thing that *can* be said about it without
        // claiming a verb, and it comes off the same gate-enforced catalog the
        // policy layer reads rather than a second table here, so a tool cannot
        // be filed under one family for `tools.<group>: "off"` and a different
        // one on screen. It renders as a glyph, never as a hue (#4125).
        _ => EventKind::Other {
            class: crate::tool_class::classify(name),
            touched: scope,
        },
    }
}

/// The object of the verb: the path for a file tool, the command for `bash`,
/// the raw input for anything else.
///
/// [`Subject::is_path`] leaves with the text it describes, out of the one match
/// that chose it, so the two cannot come to disagree. Downstream the only
/// evidence left is a `/`, and re-deriving the answer from one would emphasise
/// a fragment of `sed -n '1,20p' foo/bar.rs` — a command line that names no
/// file this row touched (#4168).
fn subject_for(name: &str, path: Option<&str>, input: &str) -> Subject {
    match (name, path) {
        // The one arm that yields a path, and the only one that may: the caller
        // handed us `path`, so this is not a guess about the string's shape.
        (_, Some(p)) if !p.is_empty() => Subject::path(p),
        // A command line, not a file — even when it contains a slash.
        ("bash", _) => first_line(input).into(),
        _ => {
            // A tool this host has no verb for is named by its own label, which
            // for an MCP tool is its trailing segment: the row read
            // `mcp__fs__read_file apps/page.tsx`, and `mcp__fs__` is how the
            // call was *addressed*, not what it did.
            //
            // Stripped rather than translated. A server's `read_file` is not
            // necessarily this workspace's `read`, so the name survives and
            // only the routing goes — the most a caller can do without
            // asking the server.
            let label = stella_tools::catalog::label_for(name);
            let head = first_line(input);
            // A raw input blob is not a path even when it contains one: the
            // subject here opens with the tool's own name, so there is no file
            // identity for a basename to carry.
            if head.is_empty() {
                label.into()
            } else {
                format!("{label} {head}").into()
            }
        }
    }
}

/// The first line of a blob, for a row that has exactly one.
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("").trim_end()
}

/// A turn's opening rule (SPEC 6.1).
///
/// One row, and only on the stage boundary that **opens** the turn — see
/// [`crate::model::TurnOpening`] for why a wrapped run's four inner stages do
/// not each announce the same turn number.
///
/// `queued_steer` names the mid-turn steer **a person** made that this turn
/// consumed — the visible payoff the composer's `⏎ queue (never blocks)`
/// promises. It is back-filled onto [`crate::model::TurnOpening`] by the fold,
/// gated on [`stella_protocol::SteerCause::is_from_a_person`], so the engine's
/// own loop and stall nudges never reach it: a rule labelling a stall-rung
/// auto-steer as something the user queued would be worse than the blank it
/// replaces, which is why this stayed unfed until the cause was on the wire
/// (#4185, #3622).
///
/// The steer also keeps its own `(steered mid-turn)` transcript row
/// ([`crate::TranscriptEntry::User`]). The two are not one fact said twice:
/// the row is *when* it landed, the rule is *what this turn opened by
/// consuming* — the same relationship `model` has with the closing receipt.
#[must_use]
pub fn turn_begin_rows(
    turn: u32,
    stage: &str,
    model: Option<&str>,
    budget_usd: Option<f64>,
    queued_steer: Option<&str>,
    width: usize,
) -> Vec<Line<'static>> {
    vec![turn_begin(
        &TurnHead {
            number: turn,
            stage: stage.to_string(),
            model: model.map(str::to_string),
            budget_usd,
            queued_steer: queued_steer.map(str::to_string),
        },
        width,
    )]
}

/// A turn's closing rule and its receipt (SPEC 6.1).
///
/// Two rows, always: the boundary is the transcript's rhythm — SPEC 2 makes the
/// turn its unit — so it renders whether or not the receipt has anything but
/// money to say.
///
/// Everything the receipt cannot source is elided rather than zeroed: a
/// receipt reading `0 tok · 0/0 tests` would be measurements nobody took, on
/// the one line whose whole job is to be the settled account of a turn.
///
/// `tally` is [`crate::model::TurnReceipt`] — counted at fold time and stamped
/// onto the closing entry, because this renderer sees one entry and no session
/// state. Tests are the one field still absent, and
/// [`crate::model::TurnCounters`] states what would have to exist to feed it.
#[must_use]
pub fn turn_end_rows(
    turn: u32,
    cost_usd: f64,
    tally: &crate::model::TurnReceipt,
    width: usize,
) -> Vec<Line<'static>> {
    vec![
        turn_end(turn, tally.elapsed_ms.map(human_elapsed).as_deref(), width),
        receipt(&Receipt {
            spend_usd: cost_usd,
            tokens: tally.tokens,
            files: tally.files,
            memories: tally.memories,
            ..Receipt::default()
        }),
    ]
}

/// A turn's wall clock, in the deck's own duration wording.
fn human_elapsed(ms: u64) -> String {
    crate::render::human_duration(ms)
}

#[cfg(test)]
mod tests;
