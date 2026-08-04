//! The statline: the deck's bottom row, regrouped into four labeled zones
//! separated by a single `│` divider in the theme's divider color (D1):
//!
//! ```text
//! ✦ triage (glm-5.1-fast) ● execute 1/3 │ ctx 35k/200k (78% hit) · cpu 73% · 3 sessions │ turn $0.99 · run $1.00 │        ✉ 29 · queue empty
//! └─ zone A: identity ────────┘      step#/steps  └─ zone B: resources ──────────────┘            └─ zone C: money ───────┘ └─ zone D: attention (right-aligned)
//! ```
//!
//! Zone A is identity (brand glyph + focused lane + the pin it runs on +
//! stage dot + step counter), B is resources (`ctx` with its cache hit-rate ·
//! `cpu` · live lane count), C is money (`turn` · `run [of cap]`), D is
//! attention (`✉` unread + queue status, right-aligned, plus `✦/◆` lane
//! counts when subagents exist). WARMTH, ENGINE, PIPELINE and the cache
//! `saved` figure live in the context overlay's SESSION VITALS —
//! diagnostics, not glanceables — and the MODELS row is gone outright
//! (routing lives on the scope card and `/models`; zone A names only the pin
//! the focused lane runs on).
//!
//! The lane's free-form `role` left this row with the regroup. For the lead
//! it never rendered anyway — the driver registers id and role both as
//! `"lead"`, and the cell was suppressed as a repeat — and for a subagent it
//! restated what `sub:` in the id and zone D's `◆ N sub` already say. What it
//! cost was legibility: `✦ triage lead (glm-5.1-fast)` is two bare words
//! space-joined with no way to tell which is the lane.
//!
//! ## Zone A joins with a space, everywhere else with `·`
//!
//! Identity is one phrase, not a list: `✦ triage (glm-5.1-fast) ● execute
//! 1/3` reads as a sentence about who is working, on what, and where they
//! are. Zones B/C/D are enumerations of independent facts and keep the ` · `
//! join. `separator` is the single place that rule lives, shared by the
//! renderer and the width estimator so they can never disagree.
//!
//! ## What the numbers actually are
//!
//! Every figure is folded state, never a guess (the project thesis: *report*
//! state, don't fake it).
//!
//! - `(glm-5.1-fast)` is [`crate::envelope::AgentMeta::model`]: the route the
//!   driver registered the lane on, replaced on every metered call by the
//!   provider that actually *served* it. Rendered whole, `provider/model`
//!   included — `zai/glm-5.1-fast` and `openrouter/glm-5.1-fast` are
//!   different runs with different latencies and quantizations, and a row
//!   that printed them identically would hide the confound a head-to-head
//!   exists to measure. Absent only when nothing has been registered.
//! - `1/3` is the focused lane's task board: closed steps over total,
//!   byte-identical to the `☑ 1/3` the work rail shows, so the two surfaces
//!   can never disagree. Absent when the board is empty — there is no plan
//!   progress fraction anywhere in the model to synthesize one from.
//! - `3 sessions` is the count of **registered lanes** (`agents.len()`) — how
//!   much work is live right now. Not the SESSIONS overlay's saved-session
//!   registry, which is a driver snapshot that does not exist until asked
//!   for and would read `0 sessions` for most of a run.
//! - `(78% hit)` rides the `ctx` meter because it qualifies the number beside
//!   it — how much of that context came back from cache. Omitted entirely
//!   before any input is metered, never shown as `(—% hit)`.
//!
//! ## Degradation: zones drop whole, they never squish
//!
//! On a narrow row, items are dropped in a fixed order (`cpu`, then the rest
//! of zone B, then zone C's `turn` and `run`, then zone A's `steps` and
//! `model`, then `queue`, `stage`, lane counts — zone A's brand and zone D's
//! inbox badge survive to the narrowest widths). Nothing is elided
//! mid-token, and a dropped zone takes its divider with it, so two zones
//! never share a divider-less boundary.
//!
//! ## Collapse: a card on top gets a quiet floor
//!
//! When any overlay/card is open the row collapses to at most four items
//! chosen for that context (the task card keeps `turn $` + tok/s, the scope
//! card keeps `ctx` + `run of cap`, the witness panel keeps its phase).
//! [`statline_items`] is the single decision function, unit-testable
//! without a buffer.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use unicode_width::UnicodeWidthStr;

use crate::cache_panel;
use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;
use crate::deck_ui::cards::Card;
use crate::textline::stage_label;
use crate::theme;

/// The context window the `ctx` meter divides by.
pub(crate) const CTX_WINDOW: u64 = 200_000;

/// The four zones. Order is display order; D is right-aligned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatZone {
    Identity,
    Resources,
    Money,
    Attention,
}

/// One statline item: a stable key (what tests assert on), its zone, and the
/// styled spans that render it.
#[derive(Clone, Debug)]
pub struct StatItem {
    pub key: &'static str,
    pub zone: StatZone,
    pub spans: Vec<Span<'static>>,
}

impl StatItem {
    fn new(key: &'static str, zone: StatZone, spans: Vec<Span<'static>>) -> Self {
        Self { key, zone, spans }
    }

    /// Display width of the item's spans.
    fn width(&self) -> usize {
        self.spans.iter().map(|s| s.content.as_ref().width()).sum()
    }
}

/// The fixed order items are dropped in when the row is too narrow. Earlier
/// entries go first; anything not listed survives to the narrowest widths
/// (`agent` — the brand-glyph identity — `inbox`, and a failing-CI PR
/// badge).
///
/// Zone A's extras are ordered by what a reader loses least: `steps` first
/// (the work rail shows the same counter), then `model` — the slug is the one
/// thing on this row that answers "what am I paying for".
const DROP_ORDER: [&str; 9] = [
    "cpu", "sessions", "ctx", "turn", "run", "steps", "model", "queue", "pr",
];

/// The separator between two adjacent left-side items: `│` across a zone
/// boundary, ` · ` between items of one zone — except zone A, which joins
/// with a plain space so the identity reads as one phrase rather than a
/// list. Shared by [`render`] and [`row_width`]: a width estimate that
/// disagreed with the renderer would drop items the row had space for.
fn separator(prev: StatZone, next: StatZone) -> &'static str {
    if prev != next {
        " │ "
    } else if next == StatZone::Identity {
        " "
    } else {
        " · "
    }
}

/// The statline's items for this frame — THE decision function (collapse
/// rule included), pure over `(model, ui)` so it is unit-testable without a
/// buffer.
pub fn statline_items(model: &WorkspaceModel, ui: &DeckUi) -> Vec<StatItem> {
    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    let primary = Style::new().fg(theme::TEXT_PRIMARY);
    let ok = Style::new().fg(theme::SUCCESS_BRIGHT);
    let focused = model.agents.get(ui.focused);

    // ── zone A: identity ────────────────────────────────────────────────
    let agent_name = focused
        .map(|a| a.meta.id.clone())
        .unwrap_or_else(|| "—".into());
    let agent = StatItem::new(
        "agent",
        StatZone::Identity,
        vec![
            Span::styled("✦ ", theme::accent()),
            Span::styled(agent_name, theme::accent()),
        ],
    );
    // The pin this lane runs on — the registered route until a metered call
    // replaces it with whatever actually served. Absent only when nothing has
    // been registered, which is the pre-session state, not a routing claim.
    let served_model = focused.and_then(|a| a.meta.model.clone()).map(|slug| {
        StatItem::new(
            "model",
            StatZone::Identity,
            vec![
                Span::styled("(", dim),
                Span::styled(slug, Style::new().fg(theme::TEXT_SECONDARY)),
                Span::styled(")", dim),
            ],
        )
    });
    let stage_kind = focused.and_then(|a| a.model.hud.stage);
    let stage = StatItem::new(
        "stage",
        StatZone::Identity,
        vec![
            Span::styled(
                "● ",
                Style::new().fg(stage_kind
                    .map(theme::stage_color)
                    .unwrap_or(theme::TEXT_TERTIARY)),
            ),
            Span::styled(
                stage_kind.map(stage_label).unwrap_or("idle").to_string(),
                primary,
            ),
        ],
    );
    // `step#/steps` — the focused lane's task board, closed over total. The
    // same figure `views::work_rail` prints as `☑ 1/3`, derived the same way
    // from the same field, so the two can never tell different stories about
    // one board. An empty board yields no item: there is no plan-progress
    // fraction in the model, and `0/0` would be an invented one.
    let steps = focused
        .map(|a| &a.model.tasks)
        .filter(|tasks| !tasks.is_empty())
        .map(|tasks| {
            let done = tasks.iter().filter(|t| !t.status.is_open()).count();
            StatItem::new(
                "steps",
                StatZone::Identity,
                vec![Span::styled(format!("{done}/{}", tasks.len()), dim)],
            )
        });

    // ── zone C values shared with the collapsed forms ───────────────────
    let turn_cost = focused.map_or(0.0, |a| a.model.hud.turn_spent_usd());
    let turn = StatItem::new(
        "turn",
        StatZone::Money,
        vec![
            Span::styled("turn ", dim),
            Span::styled(format!("${turn_cost:.2}"), ok),
        ],
    );
    let mut run_spans = vec![
        Span::styled("run ", dim),
        Span::styled(format!("${:.2}", model.total_cost()), primary),
    ];
    if let Some(cap) = model.budget_cap_usd.filter(|cap| *cap > 0.0) {
        run_spans.push(Span::styled(format!(" of ${cap:.2}"), dim));
    }
    let run = StatItem::new("run", StatZone::Money, run_spans);

    // ── the collapse rule: a card on top gets at most four items ────────
    if let Some(context) = open_surface(ui) {
        let mut items = vec![agent, stage];
        match context {
            Card::Tasks => {
                items.push(turn);
                let (rate, contributors) = model.combined_tok_per_s();
                if let Some(rate) = rate {
                    let text = if contributors > 1 {
                        format!("{rate} tok/s combined")
                    } else {
                        format!("{rate} tok/s")
                    };
                    items.push(StatItem::new(
                        "toks",
                        StatZone::Attention,
                        vec![Span::styled(text, dim)],
                    ));
                }
            }
            Card::Witness => {
                let phase = focused
                    .map(|a| crate::views::witness_card::phase_label(&a.model.proof))
                    .unwrap_or_else(|| "witness".to_string());
                items.push(StatItem::new(
                    "witness",
                    StatZone::Attention,
                    vec![Span::styled(phase, theme::accent())],
                ));
                items.push(run);
            }
            // The scope card — and every other overlay — keeps the resource
            // meter and the run figure: the quiet floor.
            _ => {
                items.push(ctx_item(model, ui, dim, primary));
                items.push(run);
            }
        }
        return items;
    }

    // ── the full row ────────────────────────────────────────────────────
    let mut items = vec![agent];
    items.extend(served_model);
    items.push(stage);
    items.extend(steps);

    items.push(ctx_item(model, ui, dim, primary));
    let cpu = f64::from(model.global_cpu_pct);
    items.push(StatItem::new(
        "cpu",
        StatZone::Resources,
        vec![
            Span::styled("cpu ", dim),
            Span::styled(format!("{cpu:.0}%"), primary),
        ],
    ));
    // How many lanes are live. A resource in the same sense `cpu` is: it is
    // what the machine is carrying right now, and it is the number that
    // explains a `combined` token rate or a run cost climbing faster than one
    // turn can account for.
    let lanes = model.agents.len();
    items.push(StatItem::new(
        "sessions",
        StatZone::Resources,
        vec![
            Span::styled(lanes.to_string(), primary),
            Span::styled(if lanes == 1 { " session" } else { " sessions" }, dim),
        ],
    ));

    items.push(turn);
    items.push(run);

    // ── zone D: attention (right-aligned) ───────────────────────────────
    let subs = model.subagent_count();
    if subs > 0 {
        items.push(StatItem::new(
            "lanes",
            StatZone::Attention,
            vec![
                Span::styled(format!("✦ {} lead", model.lead_count()), theme::accent()),
                Span::styled(" · ", dim),
                Span::styled(format!("◆ {subs} sub"), Style::new().fg(theme::SUBAGENT)),
            ],
        ));
    }
    // The PR badge, once a Pr event has been observed. Failing CI takes a
    // survivor key — a red ✗ must outlive a narrow row the way unread mail
    // does.
    if let Some(pr) = &model.pr {
        let key = if pr.ci == Some(stella_protocol::CiStatus::Failing) {
            "pr-failing"
        } else {
            "pr"
        };
        items.push(StatItem::new(key, StatZone::Attention, pr_spans(pr)));
    }
    let unread = ui.notifications.iter().filter(|n| !n.read).count();
    items.push(StatItem::new(
        "inbox",
        StatZone::Attention,
        vec![if unread > 0 {
            Span::styled(
                format!("✉ {unread}"),
                theme::accent().add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled("✉ 0", dim)
        }],
    ));
    let queued = model.queue.pending();
    items.push(StatItem::new(
        "queue",
        StatZone::Attention,
        vec![if queued > 0 {
            Span::styled(format!("{queued} queued"), primary)
        } else {
            Span::styled("queue empty", dim)
        }],
    ));
    items
}

/// The `ctx used/cap (N% hit)` item — used and hit-rate in primary, the cap
/// and the qualifier dimmed.
///
/// The hit rate rides this item rather than standing beside it as its own
/// `cache N%` cell: it is a statement *about* the number to its left — how
/// much of the context in flight came back from cache rather than being
/// re-sent — and reads as one fact. The read/write volumes and the savings
/// figure stay in the context overlay's cache detail.
fn ctx_item(model: &WorkspaceModel, ui: &DeckUi, dim: Style, primary: Style) -> StatItem {
    let used = model.agents.get(ui.focused).map_or(0, |a| a.context_tokens);
    let mut spans = vec![
        Span::styled("ctx ", dim),
        Span::styled(fmt_k(used), primary),
        Span::styled(format!("/{}", fmt_k(CTX_WINDOW)), dim),
    ];
    // Omitted outright before any input is metered — a parenthetical that
    // said `(—% hit)` would be noise claiming to be a measurement.
    if let Some(pct) = cache_panel::hit_pct(model.cache_hit_tokens(), model.total_input_tokens()) {
        spans.push(Span::styled(" (", dim));
        spans.push(Span::styled(format!("{pct}%"), primary));
        spans.push(Span::styled(" hit)", dim));
    }
    StatItem::new("ctx", StatZone::Resources, spans)
}

/// Which card/overlay is on top, for the collapse rule — the card enum for
/// the three named contexts, `Card::Scope` standing in for "some other
/// overlay" too (they share the quiet floor).
fn open_surface(ui: &DeckUi) -> Option<Card> {
    if let Some(card) = ui.cards.open {
        return Some(card);
    }
    let other_overlay = ui.help_open
        || ui.queue_open
        || ui.graph_picker_open
        || ui.sessions_open
        || ui.inbox_open
        || ui.context_open
        || ui.inspect_open
        || ui.state_open;
    other_overlay.then_some(Card::Scope)
}

/// Render the statline band: the zoned row, plus the low-hit-rate diagnosis
/// on a second row when one is earned and the band has the height.
pub fn render(model: &WorkspaceModel, ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let mut items = statline_items(model, ui);
    let width = area.width as usize;

    // Zones drop whole: remove items in the fixed order until the row fits.
    let mut drop_at = 0usize;
    while row_width(&items) > width && drop_at < DROP_ORDER.len() {
        let key = DROP_ORDER[drop_at];
        items.retain(|item| item.key != key);
        drop_at += 1;
    }
    // Below even that, shed the remaining non-survivors back-to-front
    // (lane counts, stage) — brand, inbox and a failing-CI badge go last.
    while row_width(&items) > width && items.len() > 2 {
        let Some(victim) = items
            .iter()
            .rposition(|i| !matches!(i.key, "agent" | "inbox" | "pr-failing"))
        else {
            break;
        };
        items.remove(victim);
    }

    // Left: zones A/B/C in order. Right: zone D.
    let sep = Style::new().fg(theme::HAIRLINE);
    let mut left: Vec<Span<'static>> = vec![Span::raw(" ")];
    let mut last_zone: Option<StatZone> = None;
    for item in items.iter().filter(|i| i.zone != StatZone::Attention) {
        if let Some(prev) = last_zone {
            left.push(Span::styled(separator(prev, item.zone), sep));
        }
        left.extend(item.spans.iter().cloned());
        last_zone = Some(item.zone);
    }
    let mut right: Vec<Span<'static>> = Vec::new();
    for (i, item) in items
        .iter()
        .filter(|i| i.zone == StatZone::Attention)
        .enumerate()
    {
        if i > 0 {
            right.push(Span::styled(" · ", sep));
        }
        right.extend(item.spans.iter().cloned());
    }
    if !right.is_empty() {
        right.push(Span::raw(" "));
    }

    let row = Rect { height: 1, ..area };
    Paragraph::new(Line::from(left)).render(row, buf);
    let right_w = right
        .iter()
        .map(|s| s.content.as_ref().width())
        .sum::<usize>()
        .min(width) as u16;
    if right_w > 0 {
        Paragraph::new(Line::from(right)).render(
            Rect {
                x: area.x + area.width - right_w,
                y: area.y,
                width: right_w,
                height: 1,
            },
            buf,
        );
    }

    // The low-hit-rate diagnosis keeps its own second row when earned.
    if area.height >= 2
        && let Some(cause) = model
            .agents
            .get(ui.focused)
            .and_then(|a| a.cache_diagnosis(cache_panel::LOW_HIT_RATE_THRESHOLD))
    {
        Paragraph::new(Line::from(cache_panel::diagnosis_spans(cause))).render(
            Rect {
                x: area.x,
                y: area.y + 1,
                width: area.width,
                height: 1,
            },
            buf,
        );
    }
}

/// The rendered width of `items`: leading pad + spans + separators (per
/// [`separator`] on the left; ` · ` + trailing pad on the right).
fn row_width(items: &[StatItem]) -> usize {
    let mut w = 1; // leading pad
    let mut last_zone: Option<StatZone> = None;
    let mut right_items = 0usize;
    for item in items {
        if item.zone == StatZone::Attention {
            right_items += 1;
            w += item.width();
            continue;
        }
        if let Some(prev) = last_zone {
            w += separator(prev, item.zone).width();
        }
        w += item.width();
        last_zone = Some(item.zone);
    }
    if right_items > 0 {
        w += (right_items - 1) * 3 + 1 /* trailing pad */ + 2 /* breathing gap */;
    }
    w
}

/// The PR badge's spans: `⇢ #183 open` (or the URL tail when the monitor
/// parsed no number) colored by PR status, plus a CI glyph once a verdict
/// has been observed — `✓` passing, `✗` failing (bold), `◌` pending / `…`
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

    /// A model with everything zone A and B can show: a routed pin, a
    /// three-step board with one closed, and metered input with cache reads.
    fn furnished_model() -> WorkspaceModel {
        use stella_protocol::{TaskItem, TaskStatus};
        let mut m = running_model();
        m.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::TaskUpdate {
                tasks: vec![
                    TaskItem {
                        id: "1".into(),
                        subject: "one".into(),
                        description: None,
                        status: TaskStatus::Completed,
                        owner: None,
                    },
                    TaskItem {
                        id: "2".into(),
                        subject: "two".into(),
                        description: None,
                        status: TaskStatus::InProgress,
                        owner: None,
                    },
                    TaskItem {
                        id: "3".into(),
                        subject: "three".into(),
                        description: None,
                        status: TaskStatus::Pending,
                        owner: None,
                    },
                ],
            },
        });
        let a = &mut m.agents[0];
        a.meta.model = Some("glm-5.1-fast".into());
        a.tokens_in = 100_000;
        a.cache_read_tokens = 78_000;
        m
    }

    #[test]
    fn the_full_row_carries_all_four_zones() {
        let model = furnished_model();
        let ui = DeckUi::default();
        let items = statline_items(&model, &ui);
        for key in [
            "agent", "model", "stage", "steps", "ctx", "cpu", "sessions", "turn", "run", "inbox",
            "queue",
        ] {
            assert!(
                keys(&items).contains(&key),
                "missing {key}: {:?}",
                keys(&items)
            );
        }
        // WARMTH / ENGINE / PIPELINE / MODELS left the statline outright, and
        // `cache` folded into `ctx` while `saved` moved to SESSION VITALS —
        // none of them may come back as their own cell.
        for gone in ["warmth", "engine", "pipeline", "models", "cache", "saved"] {
            assert!(!keys(&items).contains(&gone), "{gone} must not return");
        }
    }

    /// The spec row, rendered: `✦ lead (glm-5.1-fast) ● execute 1/3 │ ctx
    /// …/200k (78% hit) · cpu 0% · 1 session │ turn $0.00 · run $0.00 │ …`.
    #[test]
    fn the_row_reads_as_the_documented_layout() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let model = furnished_model();
        let ui = DeckUi::default();
        let area = Rect::new(0, 0, 200, 1);
        let mut buf = Buffer::empty(area);
        render(&model, &ui, area, &mut buf);
        let text: String = (0..area.width)
            .map(|x| buf.cell((x, 0)).map(|c| c.symbol()).unwrap_or(" "))
            .collect();

        // Zone A is one phrase: space-joined, no `·` anywhere inside it.
        assert!(
            text.contains("✦ lead (glm-5.1-fast) ● execute 1/3 │"),
            "zone A reads as one phrase:\n{text}"
        );
        // Zone B: the hit rate rides ctx, and the lane count is singular at 1.
        assert!(
            text.contains("(78% hit) · cpu 0% · 1 session │"),
            "zone B carries the hit rate and lane count:\n{text}"
        );
        // Zone C ends at `run` — `saved` is gone from this row.
        assert!(text.contains("turn $0.00 · run $0.00"), "zone C:\n{text}");
        assert!(!text.contains("saved"), "saved left zone C:\n{text}");
        // Zone D holds the right edge.
        assert!(text.trim_end().ends_with("queue empty"), "zone D:\n{text}");
    }

    #[test]
    fn the_lane_count_is_pluralized_and_the_unmetered_row_hides_the_hit_rate() {
        // A bare session: nothing metered, so no `(N% hit)` parenthetical —
        // never `(—% hit)`.
        let model = running_model();
        let ui = DeckUi::default();
        let items = statline_items(&model, &ui);
        let ctx = items.iter().find(|i| i.key == "ctx").expect("ctx");
        let ctx_text: String = ctx.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(!ctx_text.contains("hit"), "no hit rate yet: {ctx_text}");
        // …and no board yet, so no step counter.
        assert!(!keys(&items).contains(&"steps"), "{:?}", keys(&items));

        let mut two = model.clone();
        two.apply_inbound(&Inbound::Register(AgentMeta::new("sub", "child", 0)));
        let items = statline_items(&two, &ui);
        let lanes = items
            .iter()
            .find(|i| i.key == "sessions")
            .expect("sessions");
        let text: String = lanes.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(text, "2 sessions");
    }

    #[test]
    fn the_lane_role_never_reaches_the_row() {
        // A subagent lane is the case where id and role genuinely differ. The
        // row names the lane and the pin; `subagent` is already told by the
        // `sub:` id and zone D's `◆ N sub`, and space-joined into zone A it
        // read as a second, unlabelled name.
        let mut model = running_model();
        model.apply_inbound(&Inbound::Register(
            AgentMeta::new("sub:auth", "child", 0).with_role("subagent"),
        ));
        let mut ui = DeckUi::default();
        ui.focused = 1;
        let items = statline_items(&model, &ui);
        assert!(!keys(&items).contains(&"role"), "{:?}", keys(&items));
        let text: String = items
            .iter()
            .flat_map(|i| i.spans.iter())
            .map(|s| s.content.to_string())
            .collect();
        assert!(!text.contains("subagent"), "no role label: {text}");
    }

    #[test]
    fn a_lane_with_no_routed_call_names_no_model() {
        // The pin is an observation, not an intention: a lane that has not
        // called anything shows no slug rather than the session default.
        let model = running_model();
        let ui = DeckUi::default();
        assert!(
            !keys(&statline_items(&model, &ui)).contains(&"model"),
            "an unrouted lane names no model"
        );
    }

    #[test]
    fn the_step_counter_matches_the_work_rail() {
        // One number, one derivation: the statline and the work rail read the
        // same field the same way, so they cannot disagree about one board.
        let model = furnished_model();
        let ui = DeckUi::default();
        let items = statline_items(&model, &ui);
        let steps = items.iter().find(|i| i.key == "steps").expect("steps");
        let text: String = steps.spans.iter().map(|s| s.content.to_string()).collect();
        let tasks = &model.agents[0].model.tasks;
        let done = tasks.iter().filter(|t| !t.status.is_open()).count();
        assert_eq!(text, format!("{done}/{}", tasks.len()));
        assert_eq!(text, "1/3");
    }

    #[test]
    fn each_card_collapses_the_row_to_at_most_four_items() {
        use crate::deck_ui::cards::Card;
        let model = running_model();
        for card in [
            Card::Tasks,
            Card::Scope,
            Card::Witness,
            Card::Models,
            Card::Budget,
        ] {
            let mut ui = DeckUi::default();
            ui.cards.raise(card);
            let items = statline_items(&model, &ui);
            assert!(
                items.len() <= 4,
                "{card:?} must collapse to ≤4 items, got {:?}",
                keys(&items)
            );
            assert_eq!(items[0].key, "agent", "the brand identity survives");
        }
    }

    #[test]
    fn the_task_card_collapse_keeps_turn_cost() {
        use crate::deck_ui::cards::Card;
        let model = running_model();
        let mut ui = DeckUi::default();
        ui.cards.raise(Card::Tasks);
        let items = statline_items(&model, &ui);
        assert!(keys(&items).contains(&"turn"), "{:?}", keys(&items));
    }

    #[test]
    fn the_witness_collapse_names_the_phase() {
        use crate::deck_ui::cards::Card;
        let model = running_model();
        let mut ui = DeckUi::default();
        ui.cards.raise(Card::Witness);
        let items = statline_items(&model, &ui);
        assert!(keys(&items).contains(&"witness"), "{:?}", keys(&items));
        assert!(keys(&items).contains(&"run"), "{:?}", keys(&items));
    }

    #[test]
    fn any_other_overlay_also_collapses() {
        let model = running_model();
        let mut ui = DeckUi::default();
        ui.help_open = true;
        assert!(statline_items(&model, &ui).len() <= 4);
    }
}
