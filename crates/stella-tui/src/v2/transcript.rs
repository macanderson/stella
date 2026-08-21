//! The v2 transcript — SPEC 6, turn boundaries and event anatomy.
//!
//! ## The shape
//!
//! A turn is a labelled rule, a run of events, a closing rule and a one-line
//! receipt:
//!
//! ```text
//! ── turn 14 · execute · kimi-k3 · budget $0.60 ───────────────────────────
//!  │ ✦ skill oxagen-feature · auto                              1.2k tok
//!  │   injected 10-layer feature contract · used 42× this repo
//!  │ ▸ read …/lifecycle.rs · 221 lines                    ⚡3ms · ↵ open
//!  │ ● edit …/self_driving_cmd.rs +3 -1              ⚡2ms · → task 3
//! ── turn 14 done · 0:42 ──────────────────────────────────────────────────
//!    receipt $0.11 · 18k tok · det 86% · 4/4 tests · 2 files · ↵ audit
//! ```
//!
//! ## Why a rail and a glyph, never a colour alone
//!
//! SPEC 13 requires every state to be legible without colour, and the
//! degradation map means a 16-colour terminal collapses the two metals onto
//! adjacent ANSI slots. So the rail carries the metal, the glyph carries the
//! kind, and the two are redundant on purpose: drop the colour and `✗ delete`
//! still reads as a deletion.
//!
//! ## Purity
//!
//! Every function here is a projection of owned data onto `Line<'static>`. No
//! clock, no filesystem, no `WorkspaceModel` — the live projection lives in
//! [`super::transcript_source`], the same split [`super::status_bar`] and
//! [`super::status_source`] use, and for the same reason: it is what lets the
//! goldens below be fixture data all the way down.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use stella_tui_theme::{glyph, token};

/// Cells the coloured rail occupies at the head of every event row (SPEC 6.2).
pub const RAIL_W: usize = 2;

/// A file event's size, as **measured**.
///
/// Every field here is an `Option` for one reason: a head row renders the
/// moment its call dispatches, and nothing has been measured yet at that
/// moment. A zero is not the honest stand-in — `+0 -0` beside a path asserts
/// that the edit changed nothing, which is a louder and entirely different
/// claim than "not measured yet", and the same substitution already shipped
/// once as a defect in the files panel (see [`crate::deck::FileLedger`], whose
/// counts stopped being re-derived for exactly this reason — #2290). `None`
/// renders as no column at all.
///
/// A measured one is filled in after the fact, not at dispatch:
/// [`super::transcript_source::measured_delta`] resolves the emitter's counts
/// through the call's own result once the turn boundary has measured the tree,
/// and the deck's settled-prefix fold re-renders the row when that lands
/// (#4154). So `None` is the state of every head at the moment it is drawn, and
/// of any head whose call failed, was cancelled, or changed nothing — three
/// facts a zero would misreport as one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Extent {
    /// Lines added, for an edit or a new file.
    pub added: Option<u32>,
    /// Lines removed, for an edit or a deletion.
    pub removed: Option<u32>,
}

impl Extent {
    /// A measured `(added, removed)` pair.
    #[must_use]
    pub fn delta(added: u32, removed: u32) -> Self {
        Self {
            added: Some(added),
            removed: Some(removed),
        }
    }

    /// A measured one-sided count — a read's or a new file's line count on the
    /// `added` side, a deletion's on the `removed` side.
    #[must_use]
    pub fn added(lines: u32) -> Self {
        Self {
            added: Some(lines),
            removed: None,
        }
    }

    /// The removed-side counterpart of [`Extent::added`].
    #[must_use]
    pub fn removed(lines: u32) -> Self {
        Self {
            added: None,
            removed: Some(lines),
        }
    }
}

/// What kind of thing happened — the sole input to an event's glyph and metal.
///
/// A *visual* taxonomy, not a mirror of the engine's event enum, for the reason
/// [`stella_transcript::ToolKind`] gives: the engine gains event kinds every
/// release and a renderer that needs an arm per kind silently drops the ones it
/// has not heard of. Anything unrecognised is [`EventKind::Other`] and renders
/// as a plain muted row, which is the correct degradation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// `▸ read <path> · <n> lines` — folded by default. The count rides
    /// `Extent::added` and has no producer today: only a *mutation* stamps the
    /// inline-diff reference the head's measurement is resolved through, so a
    /// read states its path and nothing about its size (#4177).
    Read { extent: Extent },
    /// `● edit <path> +a -b`, the counts absent until the edit returns.
    Edit { extent: Extent },
    /// `＋ write <path> · new file · n lines`, the count on `Extent::added`.
    Write { extent: Extent },
    /// `✗ delete <path> · -n lines · git-backed · u undo`, the count on
    /// `Extent::removed`.
    Delete { extent: Extent },
    /// `● run <cmd>`.
    Run,
    /// `✦ skill <name> · auto|/cmd · n tok`.
    Skill { trigger: String, tokens: u32 },
    /// `◆ memory logged · mem_id`.
    Memory,
    /// `◇ gate <name> · state` — always priced, `$0.00` when deterministic.
    Gate { state: String, deterministic: bool },
    /// `◐ model <activity> · tok/s`.
    Model { tokens_per_sec: u32 },
    /// `↓ compacted 74k→69k · 0 evicted · 0 deduped` — one dim line, no rail.
    Compaction {
        from_tokens: u64,
        to_tokens: u64,
        evicted: u32,
        deduped: u32,
    },
    /// Anything this renderer has not been taught.
    Other,
}

impl EventKind {
    /// The rail metal (SPEC 6.2): read silver-dim, edit/write/run/gate gold,
    /// delete red, skill/memory silver, model gold_bright.
    #[must_use]
    pub fn metal(&self) -> Color {
        match self {
            EventKind::Read { .. } => token::MUTED,
            EventKind::Edit { .. }
            | EventKind::Write { .. }
            | EventKind::Run
            | EventKind::Gate { .. } => token::GOLD,
            EventKind::Delete { .. } => token::RED,
            EventKind::Skill { .. } | EventKind::Memory => token::SILVER,
            EventKind::Model { .. } => token::GOLD_BRIGHT,
            // An unrecognised tool — an MCP server's, a workspace custom one —
            // is still *stella acting*, which SPEC 2 says is gold. Dim is the
            // bookkeeping tier and belongs to compaction alone: a call the
            // renderer has not been taught is not thereby less of an action,
            // and dimming it would hide exactly the rows a user added.
            EventKind::Other => token::GOLD,
            EventKind::Compaction { .. } => token::DIM,
        }
    }

    /// The head glyph (SPEC 4). A collapsed event takes `▸` regardless of kind
    /// — the toggle state outranks the kind on the one cell that shows it.
    #[must_use]
    pub fn head_glyph(&self, collapsed: bool) -> char {
        if collapsed {
            return glyph::COLLAPSED;
        }
        match self {
            EventKind::Write { .. } => glyph::WRITE,
            EventKind::Delete { .. } => glyph::FAILED,
            EventKind::Skill { .. } => glyph::SKILL,
            EventKind::Memory => glyph::MEMORY,
            EventKind::Gate { .. } => glyph::GATE,
            EventKind::Model { .. } => glyph::RUNNING,
            EventKind::Compaction { .. } => '↓',
            _ => '●',
        }
    }

    /// The verb as the head says it.
    #[must_use]
    pub fn verb(&self) -> &'static str {
        match self {
            EventKind::Read { .. } => "read",
            EventKind::Edit { .. } => "edit",
            EventKind::Write { .. } => "write",
            EventKind::Delete { .. } => "delete",
            EventKind::Run => "run",
            EventKind::Skill { .. } => "skill",
            EventKind::Memory => "memory",
            EventKind::Gate { .. } => "gate",
            EventKind::Model { .. } => "model",
            EventKind::Compaction { .. } => "compacted",
            EventKind::Other => "",
        }
    }

    /// Whether this kind folds by default (SPEC 6.3: reads collapse, edits
    /// expand).
    #[must_use]
    pub fn collapses_by_default(&self) -> bool {
        matches!(self, EventKind::Read { .. })
    }
}

/// One transcript event, already projected — no borrowed model state.
#[derive(Clone, Debug)]
pub struct Event {
    pub kind: EventKind,
    /// The object of the verb: a path, a command line, a skill name.
    pub subject: String,
    /// Wall time, rendered `⚡3ms`. Zero suppresses the metric.
    pub duration_ms: u64,
    /// The task this event is attributed to, rendered `→ task 3` when a plan
    /// is active (SPEC 6.2). Attribution is what makes per-task cost free.
    pub task: Option<u32>,
    /// Whether the user has folded it. `None` takes the kind's default.
    pub collapsed: Option<bool>,
    /// Rows under the head, already rendered. Only drawn when expanded.
    pub body: Vec<Line<'static>>,
    /// A dim trailing line under the body (SPEC 6.3's footers).
    pub footer: Option<String>,
}

impl Event {
    /// A minimal event; the builders below are for tests and the live source.
    #[must_use]
    pub fn new(kind: EventKind, subject: impl Into<String>) -> Self {
        Self {
            kind,
            subject: subject.into(),
            duration_ms: 0,
            task: None,
            collapsed: None,
            body: Vec::new(),
            footer: None,
        }
    }

    /// Whether this event draws its body.
    #[must_use]
    pub fn is_collapsed(&self) -> bool {
        self.collapsed
            .unwrap_or_else(|| self.kind.collapses_by_default())
    }
}

/// The label on a turn's opening rule (SPEC 6.1).
#[derive(Clone, Debug)]
pub struct TurnHead {
    pub number: u32,
    pub stage: String,
    pub model: String,
    pub budget_usd: f64,
    /// The steer this turn consumed, if any. Rendered `queued: "…"` so
    /// queue-never-blocks has a visible payoff (SPEC 6.1).
    pub queued_steer: Option<String>,
}

/// The one receipt line under a turn's closing rule (SPEC 6.1).
#[derive(Clone, Debug, Default)]
pub struct Receipt {
    pub spend_usd: f64,
    /// Tokens this turn spent. `None` when nothing has counted them.
    ///
    /// Optional for the same reason [`Receipt::det_pct`] is: `StepUsage` is a
    /// metering record the deck deliberately does not fold (it would
    /// double-count the spend the budget gauge already tracks), so a turn's
    /// token total has no source in the session model today. A receipt that
    /// printed `0 tok` would be stating a measurement nobody took.
    pub tokens: Option<u64>,
    /// The deterministic share of the turn's work. This is `det %`'s home —
    /// SPEC 5 removed it from the status bar and named the receipt instead.
    pub det_pct: Option<u32>,
    pub tests_passed: u32,
    pub tests_total: u32,
    pub files: u32,
    pub memories: u32,
}

/// A full-width rule with an embedded label: `── turn 14 · execute ──────`.
///
/// The label is drawn in `text` over a `rule`-coloured line so the boundary
/// reads as structure rather than as content, and the trailing rule always
/// reaches the right edge — a rule that stops short reads as a truncated line.
fn labelled_rule(label: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let rule = Style::new().fg(token::RULE);
    let mut spans = vec![Span::styled("── ", rule)];
    let used: usize = label.iter().map(Span::width).sum::<usize>() + 3;
    spans.extend(label);
    if used + 1 < width {
        spans.push(Span::styled(" ", rule));
        spans.push(Span::styled("─".repeat(width - used - 1), rule));
    }
    Line::from(spans)
}

/// The opening rule of a turn (SPEC 6.1).
#[must_use]
pub fn turn_begin(head: &TurnHead, width: usize) -> Line<'static> {
    let text = Style::new().fg(token::TEXT);
    let dim = Style::new().fg(token::DIM);
    let mut label = vec![
        Span::styled("turn ", dim),
        Span::styled(head.number.to_string(), Style::new().fg(token::GOLD)),
        Span::styled(format!(" {}", head.stage), text),
        Span::styled(" · ", dim),
        Span::styled(head.model.clone(), text),
    ];
    if head.budget_usd > 0.0 {
        label.push(Span::styled(" · budget ", dim));
        label.push(Span::styled(
            format!("${:.2}", head.budget_usd),
            Style::new().fg(token::GOLD),
        ));
    }
    if let Some(steer) = &head.queued_steer {
        label.push(Span::styled(" · queued: ", dim));
        label.push(Span::styled(format!("\"{steer}\""), text));
    }
    labelled_rule(label, width)
}

/// The closing rule of a turn (SPEC 6.1). `elapsed` is pre-formatted — this
/// module has no clock.
#[must_use]
pub fn turn_end(number: u32, elapsed: Option<&str>, width: usize) -> Line<'static> {
    let dim = Style::new().fg(token::DIM);
    let mut label = vec![
        Span::styled("turn ", dim),
        Span::styled(number.to_string(), Style::new().fg(token::GOLD)),
        Span::styled(" done", Style::new().fg(token::TEXT)),
    ];
    // Elided rather than rendered as `0:00`, which is a duration nobody
    // measured. The deck folds no per-turn clock today; when it does, this
    // fills in without the rule changing shape.
    if let Some(elapsed) = elapsed {
        label.push(Span::styled(format!(" · {elapsed}"), dim));
    }
    labelled_rule(label, width)
}

/// The receipt line under a closing rule (SPEC 6.1).
///
/// Money is gold everywhere it appears (SPEC 5); a full test suite is the one
/// green on the row, and only when it actually passed — a partial pass is not
/// a pass and must not borrow the metal that says one.
#[must_use]
pub fn receipt(r: &Receipt) -> Line<'static> {
    let dim = Style::new().fg(token::DIM);
    let text = Style::new().fg(token::TEXT);
    let mut spans = vec![
        Span::styled("   receipt ", dim),
        Span::styled(format!("${:.2}", r.spend_usd), Style::new().fg(token::GOLD)),
    ];
    if let Some(tokens) = r.tokens {
        spans.push(Span::styled(" · ", dim));
        spans.push(Span::styled(fmt_tokens(tokens), text));
        spans.push(Span::styled(" tok", dim));
    }
    if let Some(det) = r.det_pct {
        spans.push(Span::styled(" · det ", dim));
        spans.push(Span::styled(format!("{det}%"), text));
    }
    if r.tests_total > 0 {
        let all_passed = r.tests_passed == r.tests_total;
        spans.push(Span::styled(" · ", dim));
        spans.push(Span::styled(
            format!("{}/{} tests", r.tests_passed, r.tests_total),
            Style::new().fg(if all_passed { token::GREEN } else { token::RED }),
        ));
    }
    if r.files > 0 {
        spans.push(Span::styled(" · ", dim));
        spans.push(Span::styled(plural(r.files, "file"), text));
    }
    if r.memories > 0 {
        spans.push(Span::styled(" · ", dim));
        spans.push(Span::styled(plural(r.memories, "memory"), text));
    }
    spans.push(Span::styled(" · ↵ audit", dim));
    Line::from(spans)
}

/// Every row an event owns: head, body when expanded, then footer.
///
/// Compaction is the one kind with no rail — SPEC 6.3 calls it "deliberately
/// quiet", and a rail is how this transcript says *something happened here*.
#[must_use]
pub fn event_rows(event: &Event, width: usize) -> Vec<Line<'static>> {
    if let EventKind::Compaction {
        from_tokens,
        to_tokens,
        evicted,
        deduped,
    } = &event.kind
    {
        let dim = Style::new().fg(token::DIM);
        return vec![Line::from(vec![Span::styled(
            format!(
                "   ↓ compacted {}→{} · {evicted} evicted · {deduped} deduped",
                fmt_tokens(*from_tokens),
                fmt_tokens(*to_tokens)
            ),
            dim,
        )])];
    }

    let metal = event.kind.metal();
    let mut rows = vec![head_row(event, metal, width)];
    if !event.is_collapsed() {
        for body in &event.body {
            let mut spans = vec![rail_span(metal)];
            spans.extend(body.spans.iter().cloned());
            rows.push(Line::from(spans));
        }
    }
    if let Some(footer) = &event.footer {
        rows.push(Line::from(vec![
            rail_span(metal),
            Span::styled(footer.clone(), Style::new().fg(token::DIM)),
        ]));
    }
    rows
}

/// The 2-cell coloured rail every row of an event carries (SPEC 6.2).
pub fn rail_span(metal: Color) -> Span<'static> {
    Span::styled(" │", Style::new().fg(metal))
}

/// `<rail> <glyph> <verb> <subject> … <metrics right-aligned>`.
fn head_row(event: &Event, metal: Color, width: usize) -> Line<'static> {
    let dim = Style::new().fg(token::DIM);
    let text = Style::new().fg(token::TEXT);

    let mut left = vec![
        rail_span(metal),
        Span::styled(
            format!(" {} ", event.kind.head_glyph(event.is_collapsed())),
            Style::new().fg(metal),
        ),
    ];
    // `Other` has no verb — the tool's own name is the whole head. Composed as
    // one branch rather than two independent pushes so the separator belongs to
    // whichever part precedes the subject: an empty verb must not leave a
    // double space, and an empty span would still occupy an index that
    // `render/tests/palette.rs` counts past to reach the subject.
    if !event.kind.verb().is_empty() {
        left.push(Span::styled(
            event.kind.verb().to_string(),
            Style::new().fg(metal).add_modifier(Modifier::BOLD),
        ));
        if !event.subject.is_empty() {
            left.push(Span::styled(format!(" {}", event.subject), text));
        }
    } else if !event.subject.is_empty() {
        left.push(Span::styled(event.subject.clone(), text));
    }
    left.extend(kind_detail(&event.kind));

    let right = metrics(event);
    let left_w: usize = left.iter().map(Span::width).sum();
    let right_w: usize = right.iter().map(Span::width).sum();

    let mut spans = left;
    if right_w > 0 && left_w + right_w < width {
        spans.push(Span::styled(" ".repeat(width - left_w - right_w), dim));
        spans.extend(right);
    }
    Line::from(spans)
}

/// The kind-specific tail of a head line (SPEC 6.3's per-event columns).
fn kind_detail(kind: &EventKind) -> Vec<Span<'static>> {
    let dim = Style::new().fg(token::DIM);
    let text = Style::new().fg(token::TEXT);
    match kind {
        EventKind::Read { extent } => extent
            .added
            .map(|lines| vec![Span::styled(format!(" · {lines} lines"), dim)])
            .unwrap_or_default(),
        // The separator spaces stay neutral: colour marks the count, not the
        // padding around it, and a red cell is a scarce thing on this screen
        // (prompt.md rule 5) that must not be spent on whitespace.
        //
        // Both halves or neither: an edit's two numbers are one measurement,
        // and `+3` alone would read as an addition-only change rather than as
        // half a reading.
        EventKind::Edit { extent } => match (extent.added, extent.removed) {
            (Some(added), Some(removed)) => vec![
                Span::raw(" "),
                Span::styled(format!("+{added}"), Style::new().fg(token::GREEN)),
                Span::raw(" "),
                Span::styled(format!("-{removed}"), Style::new().fg(token::RED)),
            ],
            _ => Vec::new(),
        },
        // `new file` is a fact about the call, not a measurement, so it stays
        // on the row when the line count has not arrived.
        EventKind::Write { extent } => vec![Span::styled(
            match extent.added {
                Some(lines) => format!(" · new file · {lines} lines"),
                None => " · new file".to_string(),
            },
            dim,
        )],
        // Likewise the undo affordance: a reader needs to know the deletion is
        // recoverable whether or not its size has been counted yet.
        EventKind::Delete { extent } => vec![Span::styled(
            match extent.removed {
                Some(lines) => format!(" · -{lines} lines · git-backed · u undo"),
                None => " · git-backed · u undo".to_string(),
            },
            dim,
        )],
        EventKind::Skill { trigger, tokens } => vec![
            Span::styled(format!(" · {trigger}"), dim),
            Span::styled(format!(" · {} tok", fmt_tokens(u64::from(*tokens))), dim),
        ],
        EventKind::Gate {
            state,
            deterministic,
        } => {
            let mut spans = vec![Span::styled(format!(" · {state}"), text)];
            if *deterministic {
                spans.push(Span::styled(" · $0.00 · det", dim));
            }
            spans
        }
        EventKind::Model { tokens_per_sec } => {
            vec![Span::styled(format!(" · {tokens_per_sec} tok/s"), dim)]
        }
        _ => Vec::new(),
    }
}

/// The right-aligned metric group: wall time, then the task tag.
fn metrics(event: &Event) -> Vec<Span<'static>> {
    let dim = Style::new().fg(token::DIM);
    let mut spans = Vec::new();
    if event.duration_ms > 0 {
        spans.push(Span::styled(format!("⚡{}ms", event.duration_ms), dim));
    }
    if matches!(event.kind, EventKind::Read { .. }) && event.is_collapsed() {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ", dim));
        }
        spans.push(Span::styled("↵ open", dim));
    }
    if let Some(task) = event.task {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ", dim));
        }
        spans.push(Span::styled(
            format!("→ task {task}"),
            Style::new().fg(token::MUTED),
        ));
    }
    spans
}

/// `18k`, `1.2k`, `940` — the compact token count every metric row uses.
fn fmt_tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=9_999 => format!("{:.1}k", n as f64 / 1000.0),
        _ => format!("{}k", n / 1000),
    }
}

/// `1 file` / `2 files`, `1 memory` / `2 memories`.
fn plural(n: u32, word: &str) -> String {
    if n == 1 {
        format!("{n} {word}")
    } else if word == "memory" {
        format!("{n} memories")
    } else {
        format!("{n} {word}s")
    }
}

#[cfg(test)]
mod tests;
