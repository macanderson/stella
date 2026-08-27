//! The transcript — SPEC 6, turn boundaries and event anatomy.
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
//!  │ ▸ read …/lifecycle.rs                                ⚡3ms · ↵ open
//!  │ ● edit …/self_driving_cmd.rs +3 -1              ⚡2ms · → task 3
//! ── turn 14 done · 0:42 ──────────────────────────────────────────────────
//!    receipt $0.11 · 18k tok · 4/4 tests · 2 files · ↵ audit
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
use stella_protocol::MemoryClass;
use stella_tui_theme::{glyph, token};

use crate::model::ReadSize;
use crate::tool_class::ToolClass;

/// Cells the coloured rail occupies at the head of every event row (SPEC 6.2).
pub const RAIL_W: usize = 2;

/// A file event's size, as **measured**.
///
/// Every field here is an `Option` for one reason: a head row renders the
/// moment its call dispatches, and nothing has been measured yet at that
/// moment. A zero is the wrong stand-in — `+0 -0` beside a path asserts
/// that the edit changed nothing, which is a louder and entirely different
/// claim than "not measured yet", and the same substitution already shipped
/// once as a defect in the files panel (see [`crate::deck::FileLedger`], whose
/// counts stopped being re-derived for exactly this reason — #2290). `None`
/// renders as no column at all.
///
/// A measured one is filled in after the fact, not at dispatch:
/// [`super::transcript_source::measured_scope`] resolves the emitter's counts
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

    /// A measured one-sided count — a new file's line count on the `added`
    /// side, a deletion's on the `removed` side.
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

/// What a call did to the work tree, for a head whose subject names no path.
///
/// [`Extent`] alone is enough for `edit`/`write`/`delete`, because their
/// subject cell *is* the path and the counts are unambiguously that file's.
/// `run` and an unrecognised tool have no such cell — the subject is a command
/// line or a tool name — so a bare `+12 -4` there states a number without
/// saying what it is a number *of*. The file count is what supplies that, and
/// it is the same count the result row beneath already states, resolved from
/// the same references (#4319).
///
/// `Some` means the call returned having claimed at least one change; the
/// extent inside can still be unmeasured, exactly as it can for an edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Touched {
    /// Distinct paths the call claimed — of paths, not of claims, so two edits
    /// to one file are one file.
    pub files: usize,
    /// The delta summed across every one of them (#4214), or unmeasured.
    pub extent: Extent,
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
    /// `▸ read <path> · n lines` — folded by default.
    ///
    /// The size is **not** an [`Extent`]: a read measures coverage, not a
    /// delta, and the number never resolves through the inline-diff reference
    /// only a *mutation* stamps — a read emits no `FileChange`, correctly,
    /// since it changes nothing. #4180 removed the `Extent` this kind once
    /// carried for exactly that reason: the column was expressible,
    /// unreachable on every live path, and reached by nothing but a fixture.
    /// #4297 earned it back by giving the number a real producer — `read_file`
    /// reports `lines_shown`/`lines_total` as structured wire data, and
    /// [`super::transcript_source::read_size`] resolves it through the call's
    /// own result. `None` is every head at dispatch, every failed read, and
    /// every result written before the payload existed; it renders as no
    /// column at all.
    Read { lines: Option<ReadSize> },
    /// `● edit <path> +a -b`, both counts or neither — they are one reading.
    /// Absent until the emitter has measured the change (#4154), which is the
    /// turn boundary rather than the moment the call returns.
    Edit { extent: Extent },
    /// `+ write <path> · new file · n lines`, the count on `Extent::added`.
    Write { extent: Extent },
    /// `✗ delete <path> · -n lines · git-backed · u undo`, the count on
    /// `Extent::removed`.
    Delete { extent: Extent },
    /// `● run <cmd> · n files +a -b`.
    ///
    /// A `bash` call claims its own measured changes (#4213), so a `sed -i` or
    /// a codemod folds real `FileChange`s under it and the row claims them.
    /// The head resolved that measurement and threw it away until #4319: the
    /// result row beneath stated `n files · +a −b` while its own head was
    /// sizeless, which reads as a command that changed nothing.
    Run { touched: Option<Touched> },
    /// `✦ skill <name> · auto|/cmd · n tok`.
    Skill { trigger: String, tokens: u32 },
    /// `◆ memory logged`, with the memory's own id in the metric group and
    /// the [`memory_log_body`] ladder beneath it.
    MemoryLog { memory_id: String },
    /// `◆ memory promoted OBSERVATION → RULE · conf 0.87 · audit event <id> ·
    /// now prompt-injected` — **one row**, which SPEC 6.3 states as a note on
    /// the event rather than as a shape a renderer can choose.
    ///
    /// So everything it says rides the head: a body row would put the audit
    /// handle on a second line, and a promotion is the quietest event that
    /// changes what steers every later turn. It earns a line, not a block.
    MemoryPromote {
        from: MemoryClass,
        to: MemoryClass,
        /// `0..=100`, rendered `conf 0.87`.
        confidence: u8,
        audit_event_id: String,
    },
    /// `◇ gate <name> · state` — always priced, `$0.00` when deterministic.
    Gate { state: String, deterministic: bool },
    /// `◐ model <activity> · tok/s`.
    ///
    /// The rate is an `Option` for the reason [`Extent`]'s counts are: a model
    /// call whose provider vouched for no usage envelope, or whose wall clock
    /// nobody took, has no rate — and `0 tok/s` states that the model generated
    /// nothing per second, which is a louder claim than "not measured".
    /// [`super::transcript_source::model_rows`] names the three absences it
    /// covers.
    Model { tokens_per_sec: Option<u32> },
    /// `↓ compacted 74k→69k · 0 evicted · 0 deduped` — one dim line, no rail.
    Compaction {
        from_tokens: u64,
        to_tokens: u64,
        evicted: u32,
        deduped: u32,
    },
    /// Anything this renderer has not been taught — an MCP server's tool, a
    /// workspace custom tool, a built-in with no verb of its own.
    ///
    /// The `class` is the only thing that distinguishes one such row from
    /// another, and it is carried as a **glyph**, never as a hue. Every
    /// `Other` rails gold because every one of them is *stella acting*
    /// ([`EventKind::metal`]), which under class-coloured names left
    /// `get_state`, `mcp__github__create_pull_request` and `delegate`
    /// rendering identically — one `●` on one gold rail, the confusion #4125
    /// was filed about. Restoring the distinction as a third colour channel
    /// would erode SPEC 2's two-metal rule; restoring it as shape does not,
    /// which is what SPEC 2's "never colour alone" already asks for.
    ///
    /// `touched` is the same column [`EventKind::Run`] carries and for the
    /// same reason: `apply_edits` is not one of the five names either, so the
    /// canonical multi-path write lands here (#4319). A tool that touches no
    /// file — `task_create`, `get_state`, `delegate` — claims nothing and
    /// renders no column.
    Other {
        class: ToolClass,
        touched: Option<Touched>,
    },
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
            | EventKind::Run { .. }
            | EventKind::Gate { .. } => token::GOLD,
            EventKind::Delete { .. } => token::RED,
            EventKind::Skill { .. }
            | EventKind::MemoryLog { .. }
            | EventKind::MemoryPromote { .. } => token::SILVER,
            EventKind::Model { .. } => token::GOLD_BRIGHT,
            // An unrecognised tool — an MCP server's, a workspace custom one —
            // is still *stella acting*, which SPEC 2 says is gold. Dim is the
            // bookkeeping tier and belongs to compaction alone: a call the
            // renderer has not been taught is not thereby less of an action,
            // and dimming it would hide exactly the rows a user added.
            EventKind::Other { .. } => token::GOLD,
            EventKind::Compaction { .. } => token::DIM,
        }
    }

    /// The head glyph (SPEC 4). A collapsed event takes `▸` regardless of kind
    /// — the toggle state outranks the kind on the one cell that shows it.
    ///
    /// [`EventKind::Other`] is the one arm that reads a second field: every
    /// such row rails gold, so the glyph is the only cell left that can say
    /// which of them this is (#4125).
    #[must_use]
    pub fn head_glyph(&self, collapsed: bool) -> char {
        if collapsed {
            return glyph::COLLAPSED;
        }
        match self {
            EventKind::Write { .. } => glyph::WRITE,
            EventKind::Delete { .. } => glyph::FAILED,
            EventKind::Skill { .. } => glyph::SKILL,
            EventKind::MemoryLog { .. } | EventKind::MemoryPromote { .. } => glyph::MEMORY,
            EventKind::Gate { .. } => glyph::GATE,
            EventKind::Model { .. } => glyph::RUNNING,
            EventKind::Compaction { .. } => glyph::COMPACTED,
            EventKind::Other { class, .. } => match class {
                ToolClass::Inspect => glyph::TOOL_INSPECT,
                ToolClass::Mutate => glyph::TOOL_MUTATE,
                ToolClass::Execute => glyph::TOOL_EXECUTE,
                ToolClass::Delegate => glyph::TOOL_DELEGATE,
            },
            // Named rather than a wildcard, so a kind added to the vocabulary
            // is an `E0004` here and has to state its own head (#4320).
            EventKind::Read { .. } | EventKind::Edit { .. } | EventKind::Run { .. } => glyph::EVENT,
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
            EventKind::Run { .. } => "run",
            EventKind::Skill { .. } => "skill",
            EventKind::MemoryLog { .. } | EventKind::MemoryPromote { .. } => "memory",
            EventKind::Gate { .. } => "gate",
            EventKind::Model { .. } => "model",
            EventKind::Compaction { .. } => "compacted",
            EventKind::Other { .. } => "",
        }
    }

    /// Whether this kind folds by default (SPEC 6.3: reads collapse, edits
    /// expand).
    #[must_use]
    pub fn collapses_by_default(&self) -> bool {
        matches!(self, EventKind::Read { .. })
    }
}

/// The object of a head's verb, and whether it is a **path**.
///
/// The flag rides the subject rather than being re-derived from the string at
/// render time, because "contains a slash" is not the same question. A `bash`
/// head's subject is a command line, and `sed -n '1,20p' foo/bar.rs` contains a
/// slash while being no more a path than `grep -r foo/ .` is. Brightening the
/// tail of either would spend the row's emphasis on the text least able to use
/// it.
///
/// The distinction is free where it is decided: `transcript_source::subject_for`
/// (module-private, so there is no link to it) already branches on
/// `path: Option<&str>` and falls back to the raw input only when there is
/// none, so it *knows* — it simply used to discard the answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Subject {
    /// The rendered text.
    pub text: String,
    /// Whether [`Self::text`] is a filesystem path, and so gets the
    /// dim-directory / bright-basename split (SPEC 6.2).
    pub is_path: bool,
}

impl Subject {
    /// A subject that is a path.
    #[must_use]
    pub fn path(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_path: true,
        }
    }
}

/// Anything string-shaped is a **non-path** subject, which is the default:
/// a command line, a skill name or a tool's own label is the common case, and
/// the emphasis split is the exception that has to be asked for.
impl<T: Into<String>> From<T> for Subject {
    fn from(text: T) -> Self {
        Self {
            text: text.into(),
            is_path: false,
        }
    }
}

/// One transcript event, already projected — no borrowed model state.
#[derive(Clone, Debug)]
pub struct Event {
    pub kind: EventKind,
    /// The object of the verb: a path, a command line, a skill name.
    ///
    /// [`Subject::is_path`] is carried from the producer rather than re-derived
    /// here: the only signal left at this point is a `/`, and
    /// `sed -n '1,20p' foo/bar.rs` has one without naming any file this row
    /// touched. The projection knows the answer for free —
    /// [`super::transcript_source`]'s head takes the call's own `path` and falls
    /// back to the raw input only when there is none — so the fact travels
    /// instead of being guessed where it can no longer be checked (#4168).
    pub subject: Subject,
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
    /// The delegate that made this call, rendered `↳ d:1` (#4699). `None` is
    /// the lead's own call — the ordinary case, and drawn with no tag at all
    /// rather than a "lead" label nobody needs on every other row.
    pub sub_agent_id: Option<String>,
}

impl Event {
    /// A minimal event; the builders below are for tests and the live source.
    #[must_use]
    pub fn new(kind: EventKind, subject: impl Into<Subject>) -> Self {
        Self {
            kind,
            subject: subject.into(),
            duration_ms: 0,
            task: None,
            collapsed: None,
            body: Vec::new(),
            footer: None,
            sub_agent_id: None,
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
///
/// Three of the four fields beside the number are optional, and for the reason
/// [`Receipt`]'s are: the rule states what something observed and elides the
/// rest. A turn opens *before* most of what describes it is known — the first
/// turn of a session has not been told which model answered, and a run with no
/// budget armed has no ceiling to print — so the alternative to eliding is a
/// rule that opens by naming a model nobody routed to and a `$0.00` nobody set.
#[derive(Clone, Debug)]
pub struct TurnHead {
    pub number: u32,
    pub stage: String,
    /// The model that is answering, when the session knows it yet.
    pub model: Option<String>,
    /// The spend ceiling in force. `None` is **no budget armed**, which is a
    /// different fact from `Some(0.0)` — a run capped at nothing.
    pub budget_usd: Option<f64>,
    /// The steer this turn consumed, if any. Rendered `queued: "…"` so
    /// queue-never-blocks has a visible payoff (SPEC 6.1).
    pub queued_steer: Option<String>,
}

/// The one receipt line under a turn's closing rule (SPEC 6.1).
///
/// Every field is fed from [`crate::model::TurnCounters`], counted at fold time
/// and stamped onto the closing entry — except the tests, which have no source
/// at all. A field with nothing behind it elides rather than printing a zero.
#[derive(Clone, Debug, Default)]
pub struct Receipt {
    pub spend_usd: f64,
    /// Tokens this turn spent, summed from its `StepUsage` events.
    ///
    /// Only the **token** fields of that event are folded. Its `cost_usd` is
    /// not, and must not be: the deck's spend comes from `BudgetTick`, so
    /// folding both would double-count it.
    ///
    /// `None` is "no usage event arrived" — never `Some(0)` from an absence.
    pub tokens: Option<u64>,
    /// Tests this turn ran and passed. **Nothing counts these.**
    ///
    /// They stay `0`/`0`, which elides, until an event states the numbers.
    /// `AgentEvent::Verdict` carries a pass/fail and prose, not counts, and a
    /// `bash` call running `cargo test` is opaque to the fold — its output is a
    /// `ToolResult` string, and parsing one would be a scraper guessing at a
    /// harness rather than a measurement. Feeding this needs either a
    /// verification plugin reporting its `EvidenceSet` per check, or a
    /// test-runner tool that returns structured results.
    pub tests_passed: u32,
    pub tests_total: u32,
    /// Distinct paths this turn changed, from its `FileChange` events. A real
    /// `0` — every mutation emits one, so nothing counted is nothing changed.
    pub files: u32,
    /// Memories written, summed over the turn's `ContextWrite` upserts.
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
    ];
    if let Some(model) = &head.model {
        label.push(Span::styled(" · ", dim));
        label.push(Span::styled(model.clone(), text));
    }
    if let Some(budget) = head.budget_usd {
        label.push(Span::styled(" · budget ", dim));
        label.push(Span::styled(
            format!("${budget:.2}"),
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
    // Elided rather than rendered as `0:00`, which would be a duration nobody
    // measured. Fed from `crate::model::TurnReceipt::elapsed_ms`, which the
    // deck stamps on the way past — the fold itself may not read a clock
    // (L-T1), so `None` here is a turn the deck never timed.
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
/// Compaction is the one kind with no rail — SPEC 6.3 calls it `deliberately
/// quiet`, and a rail is how this transcript says *something happened here*.
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

/// The subject, split so a **path**'s basename carries the emphasis.
///
/// In a scan down the margin the eye is hunting the file *identity*; the
/// directory is context that only matters once the file is found, so it
/// recedes. Without this a column of calls in one workspace reads as a column
/// of near-identical `crates/stella-tui/src/render/…` strings, all the same
/// weight, differing only in the last few cells (#4168).
///
/// Non-path text is left as one unemphasised span. The subject of
/// a `bash` head is a command, not a file, and brightening its tail would spend
/// the contrast on the rows least able to use it — the rule the deleted row encoded
/// in `render::row::path_spans` before it was deleted, third arm included.
///
/// `lead` is the separator owned by whatever precedes the subject, carried into
/// the first span rather than pushed as one of its own: an empty span would
/// still occupy an index the palette tests count past.
fn subject_spans(subject: &Subject, lead: &str) -> Vec<Span<'static>> {
    let text = Style::new().fg(token::TEXT);
    // Byte-index slicing is safe on the result of `rfind('/')`: `/` is ASCII,
    // so `cut` and `cut + 1` are both char boundaries whatever else the path
    // holds.
    // A trailing separator has no basename to emphasise, so a directory renders
    // whole rather than as a bright empty span.
    match subject
        .text
        .rfind('/')
        .filter(|cut| subject.is_path && cut + 1 < subject.text.len())
    {
        Some(cut) => vec![
            Span::styled(
                format!("{lead}{}", &subject.text[..=cut]),
                Style::new().fg(token::DIM),
            ),
            Span::styled(subject.text[cut + 1..].to_owned(), text),
        ],
        // A basename with no directory is already the identity — bright, whole,
        // and nothing to recede. A non-path renders exactly as it did before.
        _ => vec![Span::styled(format!("{lead}{}", subject.text), text)],
    }
}

/// `<rail> <glyph> <verb> <subject> … <metrics right-aligned>`.
fn head_row(event: &Event, metal: Color, width: usize) -> Line<'static> {
    let dim = Style::new().fg(token::DIM);

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
        if !event.subject.text.is_empty() {
            left.extend(subject_spans(&event.subject, " "));
        }
    } else if !event.subject.text.is_empty() {
        left.extend(subject_spans(&event.subject, ""));
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
        // `n of m` when the read was truncated, `n lines` when it was whole —
        // so a partial read is visibly partial rather than silently
        // understating the file or overstating what entered context. The
        // number is the producer's own (`read_file`'s structured data,
        // #4297), and its absence renders as no column at all.
        EventKind::Read { lines } => match lines {
            Some(l) if l.shown != l.total => vec![Span::styled(
                format!(" · {} of {} lines", l.shown, l.total),
                dim,
            )],
            Some(l) => vec![Span::styled(format!(" · {} lines", l.total), dim)],
            None => Vec::new(),
        },
        // The separator spaces stay neutral: colour marks the count, not the
        // padding around it, and a red cell is a scarce thing on this screen
        // (prompt.md rule 5) that must not be spent on whitespace.
        //
        // An edit's two numbers are one measurement, so both render or neither:
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
        EventKind::Model { tokens_per_sec } => match tokens_per_sec {
            Some(rate) => vec![Span::styled(format!(" · {rate} tok/s"), dim)],
            None => Vec::new(),
        },
        EventKind::Run { touched } | EventKind::Other { touched, .. } => touched_detail(*touched),
        // The whole promotion on one row (SPEC 6.3): where it moved, on what
        // confidence, and the record that makes the move auditable.
        //
        // The confidence is the one green cell. It is the number that decided
        // the promotion, and green is what this scheme spends on a threshold
        // that was met — the same reading `receipt`'s test count takes, and
        // the reason it is not spent on the rungs beside it.
        EventKind::MemoryPromote {
            from,
            to,
            confidence,
            audit_event_id,
        } => vec![
            Span::styled(
                format!(" {} → {}", class_label(*from), class_label(*to)),
                Style::new().fg(token::MUTED),
            ),
            Span::styled(
                format!(" · conf {}", fmt_confidence(*confidence)),
                Style::new().fg(token::GREEN),
            ),
            // `now prompt-injected` is the consequence of the move: an
            // observation is recalled, a rule is injected as an instruction,
            // so this is the row where something the loop inferred starts
            // steering every later turn.
            Span::styled(
                format!(" · audit event {audit_event_id} · now prompt-injected"),
                dim,
            ),
        ],
        // Named rather than swept up by a wildcard, so adding an `EventKind`
        // is an `E0004` here the way it already is in `head_glyph` (#4320).
        // A kind that earns no detail column says so; it does not fall
        // through. All three carry what they have to say elsewhere: the
        // memory log in its body and its own id in the metric group, the
        // other two in the head's subject.
        EventKind::MemoryLog { .. } | EventKind::Compaction { .. } => Vec::new(),
    }
}

/// A rung as the ladder spells it — `OBSERVATION`.
///
/// Upper-cased from the wire spelling rather than tabulated a second time: a
/// second table is a second thing to keep in step with [`MemoryClass::LADDER`],
/// and it would say the same words louder.
fn class_label(class: MemoryClass) -> String {
    class.as_str().to_ascii_uppercase()
}

/// `0.62` — a `0..=100` confidence as the two-decimal fraction every SPEC 6.3
/// memory row states.
fn fmt_confidence(confidence: u8) -> String {
    format!("{:.2}", f64::from(confidence) / 100.0)
}

/// The ladder's own separator. The character is [`glyph::COLLAPSED`]'s, and
/// this is not that constant: there it means *this row is folded* and a reader
/// can press it open, here it means *the next rung up*. One glyph, two
/// vocabularies — renaming the fold marker must not silently reword the
/// ladder.
const LADDER_STEP: &str = " ▸ ";

/// `OBSERVATION ▸ RULE ▸ FACT`, with `current` lit and the rest receding.
///
/// Every rung renders, always. A row that named only the class it is on would
/// say where the memory is and not where it can go — and *where it can go* is
/// the whole reason the footer beneath states a threshold.
fn ladder_spans(current: MemoryClass) -> Vec<Span<'static>> {
    let dim = Style::new().fg(token::DIM);
    let mut spans = Vec::new();
    for rung in MemoryClass::LADDER {
        if !spans.is_empty() {
            spans.push(Span::styled(LADDER_STEP, dim));
        }
        spans.push(Span::styled(
            class_label(rung),
            if rung == current {
                Style::new().fg(token::TEXT)
            } else {
                dim
            },
        ));
    }
    spans
}

/// The two rows under a logged memory's head (SPEC 6.3): the lesson quoted
/// verbatim, then the ladder with its metrics.
///
/// The text is quoted rather than merely indented because it is the only
/// content on this screen written by a *model* about the user's own work, and
/// the quotes are what say so — the rest of a transcript row is the deck's own
/// words about what happened.
///
/// `kind` is the producer's own word for the sort of memory this is and is
/// rendered as given: that vocabulary belongs to whatever wrote the memory and
/// gains entries without this renderer's leave, so translating it here is how
/// the two come to disagree.
#[must_use]
pub fn memory_log_body(
    text: &str,
    class: MemoryClass,
    confidence: u8,
    kind: &str,
    decays: bool,
) -> Vec<Line<'static>> {
    let muted = Style::new().fg(token::MUTED);
    // The rail is two cells with no trailing space of its own, so every body
    // row owns the gap between it and its own first glyph — the quoted line
    // below does the same.
    let mut ladder = vec![Span::raw(" ")];
    ladder.extend(ladder_spans(class));
    // Every rung renders on every row, so the ladder is one fixed width and a
    // constant gap is all the alignment the metrics beside it need: a run of
    // memory rows states its confidences down a straight edge whichever rung
    // each of them is lit on.
    ladder.push(Span::styled(" ".repeat(LADDER_METRIC_GAP), muted));
    let mut metrics = format!(" conf {} · kind {kind}", fmt_confidence(confidence));
    // Stated only when true. `does not decay` on every durable memory would be
    // a column that says nothing on the rows that carry it most.
    if decays {
        metrics.push_str(" · decays");
    }
    ladder.push(Span::styled(metrics, muted));
    vec![
        Line::from(vec![Span::styled(
            format!(" \"{text}\""),
            Style::new().fg(token::TEXT),
        )]),
        Line::from(ladder),
    ]
}

/// Cells between the ladder and the metrics beside it.
const LADDER_METRIC_GAP: usize = 6;

/// The dim trailing line under a logged memory (SPEC 6.3's footer).
///
/// `promotes to <rung> at <conf>` elides at the top of the ladder, where there
/// is no rung above to name — a fact still states its affordances, because a
/// reader can reject a memory whatever rung it reached.
///
/// SPEC 6.3 lists `e edit` here too. It is absent because it is unrouted:
/// pressing `e` on a memory row does nothing, and an affordance a row promises
/// and the deck does not answer is worse than one it never offered. #5231 is
/// where it lands.
#[must_use]
pub fn memory_log_footer(class: MemoryClass, promotes_at: u8) -> String {
    match class.next() {
        Some(next) => format!(
            " promotes to {} at {} · e edit · x reject",
            class_label(next),
            fmt_confidence(promotes_at)
        ),
        None => " e edit · x reject".to_string(),
    }
}

/// The size column for a head whose subject names no path (#4319).
///
/// The result row's own rule, not a second one:
/// `render::entry::tool` states the file count only above N=1 — a `1 files`
/// chip over the overwhelmingly common single-path row is a column that never
/// varies — and states the delta only when it has been measured. A head that
/// spelled the scope differently from the row two lines under it would be two
/// answers to one question, which is the failure #4214 already paid for once.
fn touched_detail(touched: Option<Touched>) -> Vec<Span<'static>> {
    let dim = Style::new().fg(token::DIM);
    let Some(touched) = touched else {
        return Vec::new();
    };
    let mut spans = Vec::new();
    if touched.files > 1 {
        spans.push(Span::styled(format!(" · {} files", touched.files), dim));
    }
    // The two numbers are one reading, so an edit's rule applies here too:
    // both or neither, and a measurement the emitter never took renders as no
    // column rather than a zero (#4150, #4156).
    if let (Some(added), Some(removed)) = (touched.extent.added, touched.extent.removed) {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("+{added}"),
            Style::new().fg(token::GREEN),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("-{removed}"),
            Style::new().fg(token::RED),
        ));
    }
    spans
}

/// The right-aligned metric group: wall time, then the task tag.
fn metrics(event: &Event) -> Vec<Span<'static>> {
    let dim = Style::new().fg(token::DIM);
    let mut spans = Vec::new();
    if event.duration_ms > 0 {
        spans.push(Span::styled(format!("⚡{}ms", event.duration_ms), dim));
    }
    // The memory's own id, right-aligned and whole (SPEC 6.3's `· mem_id`).
    // Whole rather than elided, because it is the handle
    // `stella memory forget` / `restore` take: a row that shortened it would
    // make the one identifier on it unusable everywhere except the row itself.
    if let EventKind::MemoryLog { memory_id } = &event.kind {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ", dim));
        }
        spans.push(Span::styled(memory_id.clone(), dim));
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
    if let Some(agent) = &event.sub_agent_id {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ", dim));
        }
        spans.push(Span::styled(
            format!("↳ {agent}"),
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
