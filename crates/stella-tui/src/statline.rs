//! The statline: the deck's bottom band, **two rows** tall — a micro-label
//! row sitting directly over its bright value row, cells split by a `│`
//! hairline, with the ethos chip pinned right, over an always-on MODELS row:
//!
//! ```text
//!  MODEL                          │ EXECUTE     │ CPU         │ CONTEXT         │ SPEND │ CACHE                │ SAVED  │ WARMTH │ ENGINE   │ PIPELINE │ INBOX
//!  worker: anthropic/claude-fable-5 │ Step 4 of 5 │ ▮▮▮▮▮▮ 100% │ ▮▮▮▯▯▯ 91k/200k │ $3.77 │ 94% (7.0M rd · 0 wr) │ $18.80 │ cold   │ 0 active │ ON       │ ✉ 29
//!  MODELS  T·z-ai/glm-5.2  W·moonshotai/kimi-k3  J·anthropic/claude-opus-5
//! ```
//!
//! ## Why two rows and not one
//!
//! A one-row statline has to name each value inline (`ctx 91k/200k · cpu
//! 73%`), which is the most expensive possible way to say it: the label is
//! redrawn beside the value on every frame, competing with it for the same
//! scarce columns. Stacking the label *above* the value spends free vertical
//! space instead of scarce horizontal space, and leaves the value row
//! carrying information only. That is the room the CPU/CONTEXT meters, the
//! CACHE read/write volumes and the MODELS pins occupy — none of which fit
//! beside their own inline labels at ordinary deck widths.
//!
//! ## What leads the band
//!
//! MODEL and the stage box, not an identity. The brand glyph, the agent id
//! and a `● execute` dot led the row for a long time and none of them earned
//! it: on a single-agent session two of the three never changed, and the
//! third said in a whole cell what the stage stepper one row up already
//! says. In their place the band opens with the two things that *do* move —
//! which pin is answering (`worker: anthropic/claude-fable-5`, the model's
//! own vendor, not the gateway that proxied it) and how far through the task
//! board this stage has gotten.
//!
//! The stage box is the one cell whose *label* is live state: the stage name
//! heads it, in the stage's own color, over `Step 4 of 5` read off the task
//! board. A stage with no board reads `—` rather than inventing a step.
//!
//! ## The meters
//!
//! CPU and CONTEXT lead with a 6-cell `▮▯` bar colored by
//! [`theme::gauge_color`]: the shape reads before the digits do, so "how full
//! is the window" survives a glance that the number would not. Six cells is
//! deliberately coarse — it is a saturation gauge, not a readout, and the
//! exact figure sits immediately beside it.
//!
//! ## Degradation: the lowest-priority cell leaves first
//!
//! Every cell carries a drop priority. On a narrow row the ethos chip goes
//! first (pure chrome, before any data cell), then the lowest-priority cell
//! still standing, until the row fits. MODEL, the stage box and CONTEXT are
//! [`MUST_KEEP`] and never leave — what is answering, where it is, and how
//! much window is left. Cells leave whole; nothing is clipped mid-token.
//!
//! ## Collapse: a card on top gets a quiet floor
//!
//! When any overlay/card is open the band collapses to at most four cells
//! chosen for that context (the task card keeps `TURN` + tok/s, the scope
//! card keeps `CONTEXT` + `SPEND`, the witness panel keeps its phase).
//! [`statline_items`] is the single decision function, unit-testable without
//! a buffer.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use unicode_width::UnicodeWidthStr;

use crate::cache_panel;
use crate::deck::{PipelineRole, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::deck_ui::cards::Card;
use crate::theme;

/// The context window the CONTEXT meter divides by.
pub const CTX_WINDOW: u64 = 200_000;

/// Cells at this priority are never dropped for width: MODEL (what is
/// answering), the stage box (where the run is) and CONTEXT (how much window
/// is left). Everything else is negotiable.
pub const MUST_KEEP: u8 = 9;

/// The ethos chip, pinned right. Pure chrome, so it is the *first* thing
/// dropped when the row is tight — before any cell carrying data.
const ETHOS: &str = "↝ deterministic-first ";

/// One statline cell: a stable key (what tests assert on), the dim
/// micro-label drawn above it, its drop priority, and the styled value spans.
#[derive(Clone, Debug)]
pub struct StatItem {
    pub key: &'static str,
    pub label: &'static str,
    pub priority: u8,
    pub spans: Vec<Span<'static>>,
    /// Overrides the label row's dim for this one cell. `None` for every
    /// constant label; the stage box sets it so the live stage word carries
    /// the stage's own color instead of reading as chrome.
    pub label_color: Option<Color>,
}

impl StatItem {
    fn new(
        key: &'static str,
        label: &'static str,
        priority: u8,
        spans: Vec<Span<'static>>,
    ) -> Self {
        Self {
            key,
            label,
            priority,
            spans,
            label_color: None,
        }
    }

    /// Draw this cell's label in `color` rather than the label row's dim.
    fn with_label_color(mut self, color: Color) -> Self {
        self.label_color = Some(color);
        self
    }

    /// Display width of the value spans.
    fn value_width(&self) -> usize {
        self.spans.iter().map(|s| s.content.as_ref().width()).sum()
    }

    /// The column width the cell occupies: label and value share one column,
    /// so the wider of the two sets it.
    fn column_width(&self) -> usize {
        self.value_width().max(self.label.width())
    }
}

/// The statline's cells for this frame — THE decision function (collapse rule
/// included), pure over `(model, ui)` so it is unit-testable without a
/// buffer.
pub fn statline_items(model: &WorkspaceModel, ui: &DeckUi) -> Vec<StatItem> {
    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    let val = Style::new().fg(theme::TEXT_PRIMARY);
    let ok = Style::new().fg(theme::SUCCESS_BRIGHT);
    let focused = model.agents.get(ui.focused);

    // MODEL: which pin is serving right now, as `role: vendor/model`. This
    // is the band's identity anchor — it replaced the brand glyph, the agent
    // id and the stage dot, none of which changed often enough to earn the
    // columns. Which model is answering *does* change mid-turn, and it is the
    // thing a scored run is read against.
    let model_cell = StatItem::new("model", "MODEL", MUST_KEEP, model_spans(model, dim, val));

    // The stage box: the stage NAME is the label (top row), so the row above
    // stops repeating a constant word and starts carrying live state; the
    // value below is how far through the task board this stage has gotten.
    let stage_kind = focused.and_then(|a| a.model.hud.stage);
    let stage = StatItem::new(
        "stage",
        stage_label_upper(stage_kind),
        MUST_KEEP,
        vec![match focused.map(|a| step_of(&a.model.tasks)) {
            Some(Some(text)) => Span::styled(text, val),
            // No board built, or an empty one: the stage name above is the
            // whole story, and an invented "Step 1 of 1" would not be.
            _ => Span::styled("—", dim),
        }],
    )
    .with_label_color(
        stage_kind
            .map(theme::stage_color)
            .unwrap_or(theme::TEXT_TERTIARY),
    );

    // SPEND: the *session* total across every agent. The live per-turn figure
    // deliberately lives one row up in `render_composer_footer` — the two
    // numbers answer different questions and move at different speeds.
    let mut spend_spans = vec![Span::styled(format!("${:.2}", model.total_cost()), ok)];
    if let Some(cap) = model.budget_cap_usd.filter(|cap| *cap > 0.0) {
        spend_spans.push(Span::styled(format!(" of ${cap:.2}"), dim));
    }
    let spend = StatItem::new("spend", "SPEND", 6, spend_spans);

    // ── the collapse rule: a card on top gets at most four cells ────────────
    if let Some(context) = open_surface(ui) {
        let mut items = vec![model_cell, stage];
        match context {
            // `/plan` is one card where `/tasks`, `/scope` and `/witness` used
            // to be three, so it gets one arm carrying what all three needed:
            // the turn's cost and where the plan stands.
            Some(Card::Plan) => {
                let turn_cost = focused.map_or(0.0, |a| a.model.hud.turn_spent_usd());
                items.push(StatItem::new(
                    "turn",
                    "TURN",
                    6,
                    vec![Span::styled(format!("${turn_cost:.2}"), ok)],
                ));
                let progress = focused
                    .map(|a| {
                        let (done, total) = a.model.plan.progress();
                        format!("{} {done}/{total}", a.model.plan.state.label())
                    })
                    .unwrap_or_else(|| "no plan".to_string());
                items.push(StatItem::new(
                    "plan",
                    "PLAN",
                    MUST_KEEP,
                    vec![Span::styled(progress, theme::accent())],
                ));
            }
            // Every other card — and every non-card overlay — keeps the
            // context meter and the session spend: the quiet floor.
            _ => {
                items.push(ctx_item(model, ui, val));
                items.push(spend);
            }
        }
        return items;
    }

    // ── the full band ───────────────────────────────────────────────────────
    let cpu = f64::from(model.global_cpu_pct);
    let mut items = vec![
        model_cell,
        stage,
        StatItem::new(
            "cpu",
            "CPU",
            5,
            vec![
                Span::styled(
                    meter_bar(cpu / 100.0),
                    Style::new().fg(theme::gauge_color(cpu / 100.0)),
                ),
                Span::styled(format!(" {cpu:>3.0}%"), val),
            ],
        ),
        ctx_item(model, ui, val),
        spend,
        StatItem::new(
            "cache",
            "CACHE",
            4,
            cache_spans(
                model.cache_hit_tokens(),
                model.total_cache_write_tokens(),
                model.total_input_tokens(),
                dim,
                val,
            ),
        ),
        StatItem::new(
            "saved",
            "SAVED",
            3,
            saved_spans(
                model.total_cache_savings_usd(),
                model.total_input_tokens() > 0,
            ),
        ),
        StatItem::new(
            "warmth",
            "WARMTH",
            3,
            warmth_spans(focused.and_then(|a| a.cache_warmth_secs(model.now_ms))),
        ),
        StatItem::new(
            "engine",
            "ENGINE",
            4,
            vec![Span::styled(
                format!("{} active", model.active_count()),
                val,
            )],
        ),
        StatItem::new(
            "pipeline",
            "PIPELINE",
            3,
            // ON when the session drives the staged pipeline, OFF for the raw
            // engine loop.
            if model.pipeline {
                vec![Span::styled("ON", ok)]
            } else {
                vec![Span::styled("OFF", dim)]
            },
        ),
    ];

    // LANES: only once this session has spawned subagents — a solo run has
    // nothing to disambiguate and pays no columns for the cell. Priority 3,
    // *below* CACHE, even though it is the wider cell: at 150 columns a tie
    // at 4 was resolved by declaration order and cost the cache readout its
    // place, and the lane split is already spelled out in the Session tab's
    // nested subagent rows while the cache figures are nowhere else on the
    // deck.
    let subs = model.subagent_count();
    if subs > 0 {
        items.push(StatItem::new(
            "lanes",
            "LANES",
            3,
            vec![
                Span::styled(format!("✦ {} lead", model.lead_count()), theme::accent()),
                Span::styled(" · ", dim),
                Span::styled(format!("◆ {subs} sub"), Style::new().fg(theme::SUBAGENT)),
            ],
        ));
    }

    // PR: only once a Pr event has been observed. Failing CI raises the drop
    // priority the same way unread mail does — a red ✗ must survive a narrow
    // row.
    if let Some(pr) = &model.pr {
        let failing = pr.ci == Some(stella_protocol::CiStatus::Failing);
        let key = if failing { "pr-failing" } else { "pr" };
        items.push(StatItem::new(
            key,
            "PR",
            if failing { 8 } else { 5 },
            pr_spans(pr),
        ));
    }

    // INBOX: persist-until-read notifications. The badge is the always-on
    // surface; `/inbox` opens the overlay that clears it.
    let unread = ui.notifications.iter().filter(|n| !n.read).count();
    items.push(StatItem::new(
        "inbox",
        "INBOX",
        if unread > 0 { 8 } else { 2 },
        vec![if unread > 0 {
            Span::styled(
                format!("✉ {unread}"),
                Style::new()
                    .fg(theme::WARNING_BRIGHT)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled("—", dim)
        }],
    ));
    items
}

/// The MODEL cell's spans: `worker: anthropic/claude-fable-5` — the role
/// word dimmed, the slug bright.
///
/// The role shown is whichever one most recently *served* a call
/// ([`WorkspaceModel::active_role`]), falling back to the worker pin and then
/// to any pin at all, so the cell names something real from the first call
/// onward. With no pin observed yet it reads `—`, never a configured default
/// dressed up as evidence.
fn model_spans(model: &WorkspaceModel, dim: Style, val: Style) -> Vec<Span<'static>> {
    let role = model
        .active_role
        .filter(|r| model.role_pins.contains_key(r))
        .or_else(|| {
            PipelineRole::ORDER
                .iter()
                .copied()
                .find(|r| *r == PipelineRole::Worker && model.role_pins.contains_key(r))
        })
        .or_else(|| {
            PipelineRole::ORDER
                .iter()
                .copied()
                .find(|r| model.role_pins.contains_key(r))
        });
    let Some(role) = role else {
        return vec![Span::styled("—", dim)];
    };
    let slug = model
        .role_pins
        .get(&role)
        .map(vendor_slug)
        .unwrap_or_else(|| "—".into());
    vec![
        Span::styled(format!("{}: ", role_word(role)), dim),
        Span::styled(slug, val),
    ]
}

/// The pipeline role as the deck says it out loud: triage / worker / verify.
///
/// `Judge` reads "verify" because that is the stage word the stepper and the
/// witness panel already use for the same step; "judge" is the internal role
/// name, and two words for one thing is one too many on a glanceable row.
fn role_word(role: PipelineRole) -> &'static str {
    match role {
        PipelineRole::Triage => "triage",
        PipelineRole::Worker => "worker",
        PipelineRole::Verifier => "verify",
    }
}

/// A pin's slug with the API gateway stripped: the *model's own* vendor, not
/// whoever proxied the call.
///
/// `RolePin::slug` answers "where did this call go", which is the right
/// question for routing and the wrong one for a glanceable cell — every row
/// of a session reads `openrouter/…`, spending columns on a constant. When
/// the model string already carries its vendor (`z-ai/glm-5.2`) that prefix
/// IS the identity, so the provider is dropped; when it does not
/// (`claude-fable-5`) the provider is the vendor (`anthropic`) and stays.
fn vendor_slug(pin: &crate::deck::RolePin) -> String {
    if pin.model.contains('/') || pin.provider.is_empty() {
        pin.model.clone()
    } else {
        format!("{}/{}", pin.provider, pin.model)
    }
}

/// The stage name, uppercased for the label row it now heads. `None` — no
/// stage observed yet — reads `IDLE`.
fn stage_label_upper(stage: Option<stella_protocol::StageKind>) -> &'static str {
    use stella_protocol::StageKind as S;
    match stage {
        None => "IDLE",
        Some(S::Triage) => "TRIAGE",
        Some(S::ContextRecall) => "CONTEXT RECALL",
        Some(S::Plan) => "PLAN",
        Some(S::ScopeReview) => "SCOPE REVIEW",
        Some(S::Witness) => "WITNESS",
        Some(S::Execute) => "EXECUTE",
        Some(S::Verify) => "VERIFY",
        Some(S::Verdict) => "VERDICT",
        Some(S::Reflect) => "REFLECT",
        Some(S::ContextWrite) => "CONTEXT WRITE",
        Some(S::Complete) => "COMPLETE",
    }
}

/// How far through the task board the session has gotten: `Step 4 of 5`.
///
/// The step is the position of the task actually being worked, not the count
/// finished — those differ the moment a task is cancelled or the board is
/// worked out of order, and the question the cell answers is "where am I",
/// not "how many are done". With nothing in progress the last finished
/// position stands in, so a board between steps does not blink back to zero.
/// `None` for an empty board: there is no step to be on.
fn step_of(tasks: &[stella_protocol::TaskItem]) -> Option<String> {
    use stella_protocol::TaskStatus;
    if tasks.is_empty() {
        return None;
    }
    let total = tasks.len();
    let step = tasks
        .iter()
        .position(|t| t.status == TaskStatus::InProgress)
        .map(|i| i + 1)
        .unwrap_or_else(|| tasks.iter().filter(|t| !t.status.is_open()).count());
    Some(format!("Step {step} of {total}"))
}

/// The CONTEXT cell: the 6-cell meter, then `used/window` in compact tokens.
fn ctx_item(model: &WorkspaceModel, ui: &DeckUi, val: Style) -> StatItem {
    // Current window occupancy = the latest call's prompt size, not the
    // session's cumulative input (which dwarfs the window after a few turns).
    let used = model.agents.get(ui.focused).map_or(0, |a| a.context_tokens);
    let frac = (used as f64 / CTX_WINDOW as f64).min(1.0);
    StatItem::new(
        "ctx",
        "CONTEXT",
        MUST_KEEP,
        vec![
            Span::styled(meter_bar(frac), Style::new().fg(theme::gauge_color(frac))),
            Span::styled(format!(" {}/{}", fmt_k(used), fmt_k(CTX_WINDOW)), val),
        ],
    )
}

/// A compact 6-cell utilization meter for a `[0, 1]` fraction.
fn meter_bar(frac: f64) -> String {
    const CELLS: usize = 6;
    let filled = (frac.clamp(0.0, 1.0) * CELLS as f64).round() as usize;
    (0..CELLS)
        .map(|i| if i < filled { '▮' } else { '▯' })
        .collect()
}

/// CACHE cell: hit% then the compact read/write token volumes behind it, or
/// the no-data dash before any input is metered.
fn cache_spans(
    cache_read: u64,
    cache_write: u64,
    total_input: u64,
    dim: Style,
    val: Style,
) -> Vec<Span<'static>> {
    match cache_panel::hit_pct(cache_read, total_input) {
        None => vec![Span::styled("—", val)],
        Some(pct) => vec![
            Span::styled(format!("{pct}%"), val),
            Span::styled(
                format!(
                    " ({} rd · {} wr)",
                    cache_panel::fmt_tokens(cache_read),
                    cache_panel::fmt_tokens(cache_write)
                ),
                dim,
            ),
        ],
    }
}

/// SAVED cell: session dollars saved by caching, danger-colored when the
/// write premium outran the reads (the low-hit incident). `metered` gates the
/// dash — no input yet shows `—`, never a misleading `$0.00`.
fn saved_spans(savings_usd: f64, metered: bool) -> Vec<Span<'static>> {
    if !metered {
        return vec![Span::styled("—", Style::new().fg(theme::TEXT_PRIMARY))];
    }
    let color = if savings_usd < 0.0 {
        theme::DANGER_BRIGHT
    } else {
        theme::SUCCESS_BRIGHT
    };
    vec![Span::styled(
        cache_panel::fmt_savings(savings_usd),
        Style::new().fg(color),
    )]
}

/// WARMTH cell: countdown until the focused agent's cached prefix expires —
/// danger once cold, warning under a minute, success while comfortably warm,
/// dim `—` when there is no warm prefix to preserve.
fn warmth_spans(remaining_secs: Option<u64>) -> Vec<Span<'static>> {
    let color = match remaining_secs {
        Some(0) => theme::DANGER_BRIGHT,
        Some(s) if s < 60 => theme::WARNING_BRIGHT,
        Some(_) => theme::SUCCESS_BRIGHT,
        None => theme::TEXT_TERTIARY,
    };
    vec![Span::styled(
        cache_panel::fmt_warmth(remaining_secs),
        Style::new().fg(color),
    )]
}

/// The MODELS row: the pin serving each of triage / worker / judge, with the
/// role that most recently served drawn in the brand accent.
///
/// Its own row because the cell row cannot hold it — three `provider/model`
/// slugs measured 210 columns inside the cell row against the 160 the row is
/// designed around, and the ethos chip is dropped before any data cell, so no
/// cell priority could have saved it. A dedicated row is what makes all three
/// visible at every width instead of none of them.
///
/// A role that has not served yet reads `—` rather than borrowing another
/// role's pin. In a scored run "the judge is pinned to X" and "the judge ran
/// on X" are different claims, and only the second is evidence.
fn models_spans(model: &WorkspaceModel, highlight: bool) -> Vec<Span<'static>> {
    let label = Style::new().fg(theme::TEXT_TERTIARY);
    let val = Style::new().fg(theme::TEXT_PRIMARY);
    let active = Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD);

    let mut spans = vec![Span::styled(" MODELS  ", label)];
    for (i, role) in PipelineRole::ORDER.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", label));
        }
        let is_active = highlight && model.active_role == Some(*role);
        spans.push(Span::styled(
            format!("{}·", role.initial()),
            if is_active { active } else { label },
        ));
        match model.role_pins.get(role) {
            Some(pin) => {
                let style = if is_active {
                    active
                } else if pin.served {
                    val
                } else {
                    // Configured but not yet reached. Drawn at label weight so
                    // intent never reads as evidence — a judge that has not run
                    // must not look like one that has.
                    label
                };
                // Same gateway-stripped form the MODEL cell uses, so the two
                // surfaces name a pin identically.
                spans.push(Span::styled(vendor_slug(pin), style));
            }
            None => spans.push(Span::styled("—", label)),
        }
    }
    spans
}

/// Which card/overlay is on top, for the collapse rule — the card enum for
/// the named contexts, with `None` standing in for "some other
/// overlay" too (they share the quiet floor).
fn open_surface(ui: &DeckUi) -> Option<Option<Card>> {
    if let Some(card) = ui.cards.open {
        return Some(Some(card));
    }
    let other_overlay = ui.help_open
        || ui.queue_open
        || ui.graph_picker_open
        || ui.sessions_open
        || ui.inbox_open
        || ui.context_open
        || ui.inspect_open;
    other_overlay.then_some(None)
}

/// Render the statline band: the label row over the value row, the always-on
/// MODELS row beneath them, and the low-hit-rate diagnosis on a fourth row
/// when one is earned and the band has the height.
pub fn render(model: &WorkspaceModel, ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let top_y = area.y;
    let bot_y = area.y + (area.height.saturating_sub(1)).min(1);

    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    let sep = Style::new().fg(theme::HAIRLINE);
    let items = statline_items(model, ui);

    // Fit: drop the ethos chip, then the lowest-priority non-must-keep cell,
    // until the band fits (or only must-keep cells remain — then accept a
    // clip rather than dropping a load-bearing cell).
    let ethos_w = ETHOS.width();
    let mut kept = vec![true; items.len()];
    let mut ethos_on = true;
    loop {
        // One leading space opens the row; every later cell is opened by the
        // 3-column " │ " hairline that separates it from the one before.
        let shown = (0..items.len()).filter(|&i| kept[i]).count();
        let cells_w: usize = (0..items.len())
            .filter(|&i| kept[i])
            .map(|i| items[i].column_width())
            .sum::<usize>()
            + if shown > 0 { 1 + (shown - 1) * 3 } else { 0 };
        let need = cells_w + if ethos_on { ethos_w } else { 0 };
        if need <= area.width as usize {
            break;
        }
        if ethos_on {
            ethos_on = false;
            continue;
        }
        match (0..items.len())
            .filter(|&i| kept[i] && items[i].priority < MUST_KEEP)
            .min_by_key(|&i| items[i].priority)
        {
            Some(i) => kept[i] = false,
            None => break,
        }
    }

    // Both rows are built cell-by-cell with the same leader, so a label can
    // only ever land in the same column as its own value.
    let mut labels: Vec<Span<'static>> = Vec::new();
    let mut values: Vec<Span<'static>> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if !kept[i] {
            continue;
        }
        let cw = item.column_width();
        let lead = if labels.is_empty() { " " } else { " │ " };
        labels.push(Span::styled(lead, sep));
        values.push(Span::styled(lead, sep));
        let label_style = item.label_color.map_or(dim, |c| Style::new().fg(c));
        labels.push(Span::styled(format!("{:<cw$}", item.label), label_style));
        // Value, right-padded into the same column width so the labels above
        // stay aligned with the values below.
        let pad = cw.saturating_sub(item.value_width());
        values.extend(item.spans.iter().cloned());
        if pad > 0 {
            values.push(Span::raw(" ".repeat(pad)));
        }
    }

    let row = |y: u16| Rect {
        x: area.x,
        y,
        width: area.width,
        height: 1,
    };
    Paragraph::new(Line::from(labels)).render(row(top_y), buf);
    Paragraph::new(Line::from(values)).render(row(bot_y), buf);
    if ethos_on {
        render_right(
            vec![Span::styled(
                ETHOS,
                Style::new().fg(theme::VIOLET).add_modifier(Modifier::BOLD),
            )],
            row(bot_y),
            buf,
        );
    }

    // MODELS is permanent by design: which pins are serving triage, worker
    // and judge is the thing a scored run is read against, so it must never
    // be the row that got dropped. Guarded on height so a caller handing this
    // function a bare 2-row area gets the pair, not a clipped third row.
    if area.height >= 3 {
        Paragraph::new(Line::from(models_spans(model, model.active_count() > 0)))
            .render(row(top_y + 2), buf);
    }

    // The low-hit-rate diagnosis: full-sentence and byte-identical to what
    // `stella stats` prints for the same cause. Only present when the caller
    // reserved the row AND the focused agent earned it.
    if area.height >= 4
        && let Some(cause) = model
            .agents
            .get(ui.focused)
            .and_then(|a| a.cache_diagnosis(cache_panel::LOW_HIT_RATE_THRESHOLD))
    {
        Paragraph::new(Line::from(cache_panel::diagnosis_spans(cause))).render(row(top_y + 3), buf);
    }
}

/// Render `spans` flush to the right edge of `area`.
fn render_right(spans: Vec<Span<'static>>, area: Rect, buf: &mut Buffer) {
    let w = spans
        .iter()
        .map(|s| s.content.as_ref().width())
        .sum::<usize>()
        .min(area.width as usize) as u16;
    if w == 0 {
        return;
    }
    Paragraph::new(Line::from(spans)).render(
        Rect {
            x: area.x + area.width - w,
            width: w,
            ..area
        },
        buf,
    );
}

/// The PR cell's spans: `⇢ #183 open` (or the URL tail when the monitor
/// parsed no number) colored by PR status, plus a CI glyph once a verdict has
/// been observed — `✓` passing, `✗` failing (bold), `◌` pending / `…`
/// running (dim).
fn pr_spans(pr: &crate::deck::PrInfo) -> Vec<Span<'static>> {
    use stella_protocol::{CiStatus, PrStatus};
    let status_color = match pr.status {
        PrStatus::Draft => theme::WARNING,
        PrStatus::Open => theme::ACCENT_DEEP,
        PrStatus::Merged => theme::ACCENT,
        PrStatus::Closed => theme::DANGER,
    };
    let status_style = Style::new().fg(status_color);
    let ident = match pr.number {
        Some(n) => format!("⇢ #{n}"),
        // No parsed number — the URL tail still identifies the PR.
        None => format!(
            "⇢ {}",
            pr.url.rsplit('/').find(|s| !s.is_empty()).unwrap_or("pr")
        ),
    };
    let mut spans = vec![
        Span::styled(ident, status_style.add_modifier(Modifier::BOLD)),
        Span::styled(
            format!(" {}", crate::textline::pr_status_label(pr.status)),
            status_style,
        ),
    ];
    if let Some(ci) = pr.ci {
        let (glyph, style) = match ci {
            CiStatus::Passing => ("✓", Style::new().fg(theme::OK)),
            CiStatus::Failing => (
                "✗",
                Style::new().fg(theme::BAD).add_modifier(Modifier::BOLD),
            ),
            CiStatus::Pending => ("◌", Style::new().fg(theme::TEXT_TERTIARY)),
            CiStatus::Running => ("…", Style::new().fg(theme::TEXT_TERTIARY)),
        };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(glyph, style));
    }
    spans
}

/// Format a token count compactly: `42k`, `1.2k`, `950`.
fn fmt_k(n: u64) -> String {
    if n >= 10_000 {
        format!("{}k", n / 1000)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use stella_protocol::{AgentEvent, StageKind};

    use crate::envelope::{AgentMeta, Inbound};

    fn running_model() -> WorkspaceModel {
        let mut m = WorkspaceModel::new();
        m.now_ms = 10_000;
        m.apply_inbound(&Inbound::Register(
            AgentMeta::new("lead", "goal", 0).with_role("lead"),
        ));
        m.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::Stage {
                name: StageKind::Execute,
            },
        });
        m
    }

    fn keys(items: &[StatItem]) -> Vec<&'static str> {
        items.iter().map(|i| i.key).collect()
    }

    #[test]
    fn the_full_band_carries_every_labeled_cell() {
        let model = running_model();
        let ui = DeckUi::default();
        let items = statline_items(&model, &ui);
        for key in [
            "model", "stage", "cpu", "ctx", "spend", "cache", "saved", "warmth", "engine",
            "pipeline", "inbox",
        ] {
            assert!(
                keys(&items).contains(&key),
                "missing {key}: {:?}",
                keys(&items)
            );
        }
    }

    #[test]
    fn every_cell_labels_itself_for_the_row_above() {
        let model = running_model();
        let items = statline_items(&model, &DeckUi::default());
        for item in &items {
            assert!(!item.label.is_empty(), "{} must label its column", item.key);
            assert_eq!(
                item.label.to_uppercase(),
                item.label,
                "{} label is drawn uppercase",
                item.key
            );
        }
    }

    #[test]
    fn the_stage_box_heads_itself_with_the_live_stage_name() {
        let model = running_model();
        let items = statline_items(&model, &DeckUi::default());
        let stage = items.iter().find(|i| i.key == "stage").expect("stage");
        assert_eq!(stage.label, "EXECUTE", "the stage name IS the label");
        assert!(
            stage.label_color.is_some(),
            "the live label carries the stage's own color, not the row's dim"
        );
        // No board on this fixture, so no step is invented.
        assert_eq!(stage.spans[0].content.as_ref(), "—");
    }

    #[test]
    fn the_step_counter_reads_the_board_position_not_the_done_count() {
        use stella_protocol::{TaskItem, TaskStatus};
        let task = |id: &str, status| TaskItem {
            id: id.into(),
            subject: format!("task {id}"),
            description: None,
            status,
            owner: None,
        };
        // Working the 4th of 5 — even though only two before it are done.
        let board = vec![
            task("1", TaskStatus::Completed),
            task("2", TaskStatus::Cancelled),
            task("3", TaskStatus::Completed),
            task("4", TaskStatus::InProgress),
            task("5", TaskStatus::Pending),
        ];
        assert_eq!(step_of(&board).as_deref(), Some("Step 4 of 5"));

        // Between steps: the last finished position stands in rather than
        // blinking back to zero.
        let idle = vec![
            task("1", TaskStatus::Completed),
            task("2", TaskStatus::Completed),
            task("3", TaskStatus::Pending),
        ];
        assert_eq!(step_of(&idle).as_deref(), Some("Step 2 of 3"));

        // No board is no step.
        assert_eq!(step_of(&[]), None);
    }

    #[test]
    fn the_model_cell_names_the_vendor_not_the_gateway() {
        use crate::deck::RolePin;
        // A gateway-proxied pin: the model string already carries its vendor,
        // so `openrouter/` is dropped.
        assert_eq!(
            vendor_slug(&RolePin {
                provider: "openrouter".into(),
                model: "z-ai/glm-5.2".into(),
                served: true,
            }),
            "z-ai/glm-5.2"
        );
        // A direct pin: the provider IS the vendor and stays.
        assert_eq!(
            vendor_slug(&RolePin {
                provider: "anthropic".into(),
                model: "claude-fable-5".into(),
                served: true,
            }),
            "anthropic/claude-fable-5"
        );
        // Legacy events carry no provider.
        assert_eq!(
            vendor_slug(&RolePin {
                provider: String::new(),
                model: "glm-5.2".into(),
                served: true,
            }),
            "glm-5.2"
        );
    }

    #[test]
    fn the_role_word_is_the_one_the_deck_says_out_loud() {
        assert_eq!(role_word(PipelineRole::Triage), "triage");
        assert_eq!(role_word(PipelineRole::Worker), "worker");
        // `Judge` reads "verify" — the stage word the stepper already uses.
        assert_eq!(role_word(PipelineRole::Verifier), "verify");
    }

    #[test]
    fn the_meter_fills_proportionally_and_clamps() {
        assert_eq!(meter_bar(0.0), "▯▯▯▯▯▯");
        assert_eq!(meter_bar(0.5), "▮▮▮▯▯▯");
        assert_eq!(meter_bar(1.0), "▮▮▮▮▮▮");
        // Over-full never overflows the six cells, and a negative fraction
        // never underflows the padding subtraction.
        assert_eq!(meter_bar(4.2), "▮▮▮▮▮▮");
        assert_eq!(meter_bar(-1.0), "▯▯▯▯▯▯");
    }

    #[test]
    fn cpu_and_context_lead_with_a_meter() {
        let model = running_model();
        let items = statline_items(&model, &DeckUi::default());
        for key in ["cpu", "ctx"] {
            let cell = items.iter().find(|i| i.key == key).expect(key);
            let first = cell.spans[0].content.as_ref().to_string();
            assert_eq!(
                first.chars().count(),
                6,
                "{key} leads with the 6-cell meter, got {first:?}"
            );
            assert!(
                first.chars().all(|c| c == '▮' || c == '▯'),
                "{key} meter uses ▮/▯, got {first:?}"
            );
        }
    }

    #[test]
    fn the_cache_cell_carries_its_read_write_volumes() {
        let spans = cache_spans(150_000, 40_000, 200_000, Style::new(), Style::new());
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "75% (150.0K rd · 40.0K wr)");
        // Before any input is metered the cell is a dash, never 0%.
        let none = cache_spans(0, 0, 0, Style::new(), Style::new());
        assert_eq!(none[0].content.as_ref(), "—");
    }

    #[test]
    fn only_the_load_bearing_cells_are_undroppable() {
        let model = running_model();
        let items = statline_items(&model, &DeckUi::default());
        let pinned: Vec<&str> = items
            .iter()
            .filter(|i| i.priority >= MUST_KEEP)
            .map(|i| i.key)
            .collect();
        assert_eq!(pinned, vec!["model", "stage", "ctx"]);
    }

    #[test]
    fn each_card_collapses_the_band_to_at_most_four_cells() {
        use crate::deck_ui::cards::Card;
        let model = running_model();
        for card in [Card::Plan, Card::Models, Card::Budget] {
            let mut ui = DeckUi::default();
            ui.cards.raise(card);
            let items = statline_items(&model, &ui);
            assert!(
                items.len() <= 4,
                "{card:?} must collapse to ≤4 cells, got {:?}",
                keys(&items)
            );
            assert_eq!(items[0].key, "model", "the model anchor survives");
        }
    }

    #[test]
    fn the_task_card_collapse_keeps_turn_cost() {
        use crate::deck_ui::cards::Card;
        let model = running_model();
        let mut ui = DeckUi::default();
        ui.cards.raise(Card::Plan);
        let items = statline_items(&model, &ui);
        assert!(keys(&items).contains(&"turn"), "{:?}", keys(&items));
    }

    /// The plan card's collapse names where the plan stands. This used to be
    /// three assertions across three cards; `/plan` is one card now, and the
    /// verification phase it also carried lives permanently in the rail's
    /// DONE VERIFICATION panel rather than in a collapsed statline cell.
    #[test]
    fn the_plan_collapse_names_where_the_plan_stands() {
        use crate::deck_ui::cards::Card;
        let model = running_model();
        let mut ui = DeckUi::default();
        ui.cards.raise(Card::Plan);
        let items = statline_items(&model, &ui);
        assert!(keys(&items).contains(&"plan"), "{:?}", keys(&items));
        assert!(keys(&items).contains(&"turn"), "{:?}", keys(&items));
    }

    #[test]
    fn any_other_overlay_also_collapses() {
        let model = running_model();
        let mut ui = DeckUi::default();
        ui.help_open = true;
        assert!(statline_items(&model, &ui).len() <= 4);
    }
}
