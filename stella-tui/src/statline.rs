//! The statline: the deck's bottom row, regrouped into four labeled zones
//! separated by a single `│` divider in the theme's divider color (D1):
//!
//! ```text
//! ✦ lead · lead · ● execute │ ctx 35k/200k · cpu 73% · cache 85% │ turn $0.99 · run $1.00 · saved $2.87 │   ✉ 29 · queue empty
//! └─ zone A: identity ──────┘└─ zone B: resources ─────────────┘└─ zone C: money ─────────────────────┘ └─ zone D: attention ─┘
//! ```
//!
//! Zone A is identity (brand glyph + focused agent + role + stage dot), B is
//! resources (`ctx` · `cpu` · `cache` hit-rate), C is money (`turn` · `run
//! [of cap]` · `saved`), D is attention (`✉` unread + queue status,
//! right-aligned, plus `✦/◆` lane counts when subagents exist). WARMTH,
//! ENGINE and PIPELINE left this row for the context overlay / SETTINGS —
//! diagnostics, not glanceables — and the MODELS row is gone outright
//! (routing lives on the scope card and `/models`).
//!
//! ## Degradation: zones drop whole, they never squish
//!
//! On a narrow row, items are dropped in a fixed order (`cpu`, then the rest
//! of zone B, then `turn`+`saved`, then `run`, `role`, `queue`, `stage`,
//! lane counts — zone A's brand and zone D's inbox badge survive to the
//! narrowest widths). Nothing is elided mid-token, and a dropped zone takes
//! its divider with it, so two zones never share a divider-less boundary.
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
const DROP_ORDER: [&str; 9] = [
    "cpu", "cache", "ctx", "turn", "saved", "run", "role", "queue", "pr",
];

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
            Span::styled(agent_name.clone(), theme::accent()),
        ],
    );
    // The role, dimmed — suppressed when it just repeats the id.
    let role = focused
        .map(|a| a.meta.role.clone())
        .filter(|role| !role.is_empty() && *role != agent_name)
        .map(|role| StatItem::new("role", StatZone::Identity, vec![Span::styled(role, dim)]));
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
    items.extend(role);
    items.push(stage);

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
    // The hit rate only — the read/write volumes live in the context
    // overlay's cache detail now.
    let hit = cache_panel::hit_pct(model.cache_hit_tokens(), model.total_input_tokens());
    items.push(StatItem::new(
        "cache",
        StatZone::Resources,
        vec![
            Span::styled("cache ", dim),
            match hit {
                Some(pct) => Span::styled(format!("{pct}%"), primary),
                None => Span::styled("—", dim),
            },
        ],
    ));

    items.push(turn);
    items.push(run);
    items.push(StatItem::new(
        "saved",
        StatZone::Money,
        vec![
            Span::styled("saved ", dim),
            Span::styled(
                cache_panel::fmt_savings(model.total_cache_savings_usd()),
                if model.total_cache_savings_usd() < 0.0 {
                    Style::new().fg(theme::DANGER_BRIGHT)
                } else {
                    ok
                },
            ),
        ],
    ));

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

/// The `ctx used/cap` item — used in primary, `/cap` dimmed.
fn ctx_item(model: &WorkspaceModel, ui: &DeckUi, dim: Style, primary: Style) -> StatItem {
    let used = model.agents.get(ui.focused).map_or(0, |a| a.context_tokens);
    StatItem::new(
        "ctx",
        StatZone::Resources,
        vec![
            Span::styled("ctx ", dim),
            Span::styled(fmt_k(used), primary),
            Span::styled(format!("/{}", fmt_k(CTX_WINDOW)), dim),
        ],
    )
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
        match last_zone {
            Some(zone) if zone == item.zone => left.push(Span::styled(" · ", sep)),
            Some(_) => left.push(Span::styled(" │ ", sep)),
            None => {}
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

/// The rendered width of `items`: leading pad + spans + separators (` · `
/// within a zone, ` │ ` between zones on the left; ` · ` + trailing pad on
/// the right).
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
        if last_zone.is_some() {
            w += 3; // " · " or " │ "
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
    fn the_full_row_carries_all_four_zones() {
        let model = running_model();
        let ui = DeckUi::default();
        let items = statline_items(&model, &ui);
        for key in [
            "agent", "stage", "ctx", "cpu", "cache", "turn", "run", "saved", "inbox", "queue",
        ] {
            assert!(
                keys(&items).contains(&key),
                "missing {key}: {:?}",
                keys(&items)
            );
        }
        // WARMTH / ENGINE / PIPELINE / MODELS left the statline outright.
        for gone in ["warmth", "engine", "pipeline", "models"] {
            assert!(!keys(&items).contains(&gone), "{gone} must not return");
        }
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
