//! The unified stage stepper + progress row that sits directly above the
//! composer — the deck's sole activity indicator (it replaced the garble
//! spinner, and then absorbed the separate stage word-list):
//!
//! ```text
//! ✓ plan   ▸ execute   ████████░░░░░░░░ 50%   · verify            43 tok/s
//! ```
//!
//! Completed stages lead with `✓`, the active stage carries `▸` plus the
//! determinate track and its percent, pending stages trail dimmed, and the
//! live token rate holds the right edge.
//!
//! ## Honesty
//!
//! Every mark on this bar is bound to real run state; nothing performs activity
//! it can't substantiate (the project thesis: *report* state, don't fake it).
//! Concretely:
//!
//! - The **determinate fill** is a *stage-position* readout, not a fabricated
//!   completion percentage. There is no progress fraction anywhere in the model
//!   (`Hud.stage` is a categorical [`StageKind`], not a monotonic 0→N counter),
//!   so the bar maps the real current stage onto the three display phases
//!   `plan → execute → verify`: completed phases fill solid, the active phase
//!   fills to its midpoint, and the percent is derived from that position. It
//!   moves only when the engine actually emits a new `Stage` event.
//! - The **shimmer** (a light band sweeping the filled region) is the *only*
//!   indeterminate cue, and it signals liveness (`AgentStatus::Running`) —
//!   never progress. It rides *on top of* the determinate fill and never
//!   advances it; it is a scrubbed `theme::lighten` toward white, gated on
//!   `no_anim`.
//! - The fill rides the brand **gold** gradient (deep gold → Phosphor Gold)
//!   — activity is the accent, so the deck's sole activity indicator is
//!   unmistakable against the quiet warm-neutral chrome everywhere else.
//! - **tok/s** is the focused agent's *live turn* rate — output tokens since
//!   the turn began over the turn's own elapsed; it is omitted (not guessed)
//!   whenever there's nothing real to divide, including a running lane with
//!   no turn clock. **ETA** is always omitted — the planner exposes no
//!   estimate to substantiate one.
//! - On **failure** the fill freezes at the stage the run reached and the head
//!   turns crimson; on **completion** it reads a full success-green track.
//!
//! ## Cost
//!
//! This module never spins a timer of its own: the shimmer/pulse are pure
//! functions of `model.now_ms`, so the bar repaints exactly when the deck loop
//! already redraws (`deck_shell`'s ~30 fps tick, or an inbound event) and
//! renders identically on replay. `--no-anim` (and `NO_COLOR`) freeze the
//! motion to a static frame for CI and recordings.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use stella_protocol::StageKind;

use crate::deck::{AgentEntry, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::envelope::AgentStatus;
use crate::theme::{self, ColorMode};

/// The shimmer sweep's period, in ms — one pass of the light band across the
/// filled region (a brisker sweep than before for a livelier read).
const SHIMMER_PERIOD_MS: u64 = 1_100;

/// The three display phases the bar collapses the real [`StageKind`] pipeline
/// onto. The engine's ten stages are conditional and unordered-in-advance, so a
/// literal per-stage bar would lie about totals; these three are the stable
/// spine every turn actually walks.
const PHASE_LABELS: [&str; 3] = ["plan", "execute", "verify"];

/// One display phase's state, left → right across the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegState {
    /// The run has moved past this phase.
    Done,
    /// The run is in this phase right now.
    Active,
    /// The run has not reached this phase.
    Pending,
}

/// The run's lifecycle, as the bar reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPhase {
    /// Nothing is running — a flat dim track, no shimmer, no head.
    Idle,
    /// A turn is in flight (`Running`/`WaitingInput`/`Paused`).
    Running,
    /// The turn finished cleanly — a full success-green track.
    Complete,
    /// The turn failed — fill frozen at the failure point, crimson head.
    Error,
}

/// The bar's fully-derived, render-ready state — a pure function of the model
/// (see [`ProgressState::derive`]) so it is unit-testable without a terminal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProgressState {
    pub phase: RunPhase,
    pub segments: [SegState; 3],
    /// Fill fraction of the track, `[0, 1]` — stage position, not fabricated %.
    pub fill: f64,
    /// The percent shown at the right, derived from `fill`.
    pub pct: u8,
    /// Real tokens/sec of the focused agent's live turn, or `None` when
    /// there's nothing honest to divide.
    pub tok_per_s: Option<u64>,
    /// Whether the shimmer / head-pulse should move this frame.
    pub animate: bool,
}

/// Which display phase (0=plan, 1=execute, 2=verify) a real stage belongs to.
fn stage_phase(stage: StageKind) -> usize {
    match stage {
        // Witness authoring is pre-execution work: it groups with planning on
        // the 3-segment display (the bar must not jump to "verify" before the
        // worker has run).
        StageKind::Triage
        | StageKind::ContextRecall
        | StageKind::Plan
        | StageKind::ScopeReview
        | StageKind::Witness => 0,
        StageKind::Execute => 1,
        StageKind::Verify | StageKind::Verdict | StageKind::Reflect | StageKind::ContextWrite => 2,
        // Complete is handled via `Hud.complete`; treat as end-of-verify.
        StageKind::Complete => 2,
    }
}

/// The honest fill for an active phase: prior phases full, this phase to its
/// midpoint. Phase 0 → 1/6, phase 1 → 1/2, phase 2 → 5/6.
fn phase_fill(active: usize) -> f64 {
    (active as f64 + 0.5) / PHASE_LABELS.len() as f64
}

impl ProgressState {
    /// The idle bar — nothing running.
    fn idle() -> Self {
        Self {
            phase: RunPhase::Idle,
            segments: [SegState::Pending; 3],
            fill: 0.0,
            pct: 0,
            tok_per_s: None,
            animate: false,
        }
    }

    /// Derive the bar from the focused agent's real run state. `no_anim` forces
    /// a static frame (CI / recordings).
    pub fn derive(agent: Option<&AgentEntry>, now_ms: u64, no_anim: bool) -> Self {
        let Some(agent) = agent else {
            return Self::idle();
        };
        let hud = &agent.model.hud;
        let complete = hud.complete || agent.status == AgentStatus::Done;
        let error = matches!(agent.status, AgentStatus::Failed | AgentStatus::Killed);

        // Idle: no turn in flight and nothing to show — a flat track, exactly
        // like having no agent at all. Keyed on the header clock
        // (`turn_started_ms`), the one honest "a turn is running" signal: it is
        // set on `PromptStarted` and cleared by `end_turn` on completion. Status
        // alone is unreliable here — `WaitingInput` is `is_active()` yet is also
        // the post-command resting state, which would otherwise strand the bar
        // mid-fill after a handled command (e.g. `/init`) finishes.
        if !complete && !error && hud.stage.is_none() && agent.turn_started_ms.is_none() {
            return Self::idle();
        }

        let active = hud.stage.map(stage_phase);

        if complete {
            return Self {
                phase: RunPhase::Complete,
                segments: [SegState::Done; 3],
                fill: 1.0,
                pct: 100,
                tok_per_s: None,
                animate: false,
            };
        }

        // Running or failed: fill to the reached stage position (frozen there on
        // error). With no stage yet but an active status, we're at the very
        // start of plan.
        let active = active.unwrap_or(0);
        let fill = phase_fill(active);
        let segments = std::array::from_fn(|i| {
            use std::cmp::Ordering::*;
            match i.cmp(&active) {
                Less => SegState::Done,
                Equal => SegState::Active,
                Greater => SegState::Pending,
            }
        });

        let running = agent.status == AgentStatus::Running;
        // The LIVE turn's rate: tokens emitted since the turn began over the
        // turn's own elapsed. Cumulative session tokens over agent lifetime
        // would decay toward the session average and misreport the run on
        // screen. A running lane without a turn clock (no `PromptStarted` —
        // e.g. a worker lane) omits the figure rather than dressing that
        // average up as a rate.
        let tok_per_s = if running {
            agent.turn_started_ms.and_then(|start| {
                let elapsed_ms = now_ms.saturating_sub(start);
                let turn_tokens = agent.tokens_out.saturating_sub(agent.turn_start_tokens_out);
                (elapsed_ms > 0 && turn_tokens > 0)
                    .then(|| turn_tokens.saturating_mul(1000) / elapsed_ms)
            })
        } else {
            None
        };

        Self {
            phase: if error {
                RunPhase::Error
            } else {
                RunPhase::Running
            },
            segments,
            fill,
            pct: (fill * 100.0).round() as u8,
            tok_per_s,
            // Motion is liveness: only while genuinely Running, never when
            // paused / awaiting input / failed, and never under `--no-anim`.
            animate: running && !no_anim,
        }
    }
}

/// Render the progress row for the focused agent into `area` (one row).
pub fn render(model: &WorkspaceModel, ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let mut state = ProgressState::derive(model.agents.get(ui.focused), model.now_ms, ui.no_anim);
    // With more than one lane emitting, the honest figure is the workspace's
    // summed rate, labelled `combined` (D5) — a single lane keeps its own
    // unlabelled rate.
    let (combined_rate, contributors) = model.combined_tok_per_s();
    let combined = contributors > 1;
    if combined && state.phase == RunPhase::Running {
        state.tok_per_s = combined_rate;
    }
    render_state(&state, model.now_ms, ui.color_mode, combined, area, buf);
}

/// One stepper element: `✓ plan` done (success), `▸ execute` active (accent,
/// bold — `✗` on error), `· verify` pending (dim). The unified stepper row
/// interleaves these with the track (D2).
fn stepper_span(state: &ProgressState, i: usize) -> (Span<'static>, usize) {
    let name = PHASE_LABELS[i];
    let (glyph, color, bold) = match state.segments[i] {
        SegState::Done => ("✓", theme::SUCCESS_BRIGHT, false),
        SegState::Active if state.phase == RunPhase::Error => ("✗", theme::DANGER, true),
        SegState::Active => ("▸", theme::ACCENT, true),
        SegState::Pending => ("·", theme::TEXT_DIM, false),
    };
    let mut style = Style::default().fg(color);
    if bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    (
        Span::styled(format!("{glyph} {name}"), style),
        2 + name.chars().count(),
    )
}

/// The right-edge telemetry: `NN tok/s` dim while running (` combined` when
/// more than one lane contributes), `idle` / `100% · done` / `failed` for the
/// other phases. The percent lives beside the track now, not out here.
fn telemetry_line(state: &ProgressState, combined: bool) -> (Vec<Span<'static>>, usize) {
    match state.phase {
        RunPhase::Idle => (
            vec![Span::styled("idle", Style::default().fg(theme::TEXT_DIM))],
            4,
        ),
        RunPhase::Complete => (
            vec![Span::styled(
                "100% · done",
                Style::default().fg(theme::SUCCESS_BRIGHT),
            )],
            11,
        ),
        RunPhase::Error => (
            vec![Span::styled(
                "failed",
                Style::default()
                    .fg(theme::DANGER)
                    .add_modifier(Modifier::BOLD),
            )],
            6,
        ),
        RunPhase::Running => match state.tok_per_s {
            Some(tps) => {
                let text = if combined {
                    format!("{tps} tok/s combined")
                } else {
                    format!("{tps} tok/s")
                };
                let w = text.chars().count();
                (
                    vec![Span::styled(text, Style::default().fg(theme::TEXT_DIM))],
                    w,
                )
            }
            None => (Vec::new(), 0),
        },
    }
}

/// Paint the derived state into `area` as the unified stepper row (D2):
///
/// ```text
/// ✓ plan   ▸ execute   ████████░░░░░░░░ 50%   · verify            43 tok/s
/// ```
///
/// Completed stages lead, the determinate track and its percent follow the
/// ACTIVE stage's label, pending stages trail, and the live token rate holds
/// the right edge. On a narrow row the stage labels drop first, then the
/// telemetry — the track itself survives to the narrowest widths. Split out
/// from the frame composer so tests can drive it with a hand-built
/// [`ProgressState`] and a fixed clock.
fn render_state(
    state: &ProgressState,
    now_ms: u64,
    mode: ColorMode,
    combined: bool,
    area: Rect,
    buf: &mut Buffer,
) {
    let y = area.y;
    let total = area.width as usize;
    let (telem, telem_w) = telemetry_line(state, combined);

    // Idle keeps its flat dim groove across the row — a stepper with no run
    // to step through would be noise.
    if state.phase == RunPhase::Idle {
        let bar_w = total.saturating_sub(telem_w + 1);
        render_track(state, now_ms, mode, area.x, y, bar_w as u16, buf);
        render_right_telem(telem, telem_w, area, buf);
        return;
    }

    // The stepper: labels before the track (done + active), labels after
    // (pending). On Complete every label is done and the full green track
    // trails them.
    const GAP: usize = 3;
    let active = state
        .segments
        .iter()
        .position(|s| *s == SegState::Active)
        .unwrap_or(PHASE_LABELS.len() - 1);
    let mut before: Vec<Span<'static>> = Vec::new();
    let mut before_w = 0usize;
    let mut after: Vec<Span<'static>> = Vec::new();
    let mut after_w = 0usize;
    for i in 0..PHASE_LABELS.len() {
        let (span, w) = stepper_span(state, i);
        if i <= active {
            if before_w > 0 {
                before.push(Span::raw(" ".repeat(GAP)));
                before_w += GAP;
            }
            before.push(span);
            before_w += w;
        } else {
            after.push(Span::raw(" ".repeat(GAP)));
            after.push(span);
            after_w += GAP + w;
        }
    }

    // The percent readout beside the track (running/error only — Complete's
    // lives in the right-edge telemetry as `100% · done`).
    let pct_text = match state.phase {
        RunPhase::Running | RunPhase::Error => format!(" {}%", state.pct),
        _ => String::new(),
    };
    let pct_w = pct_text.chars().count();

    /// The track never renders thinner than this in the full layout.
    const MIN_TRACK: usize = 8;
    /// And never fatter than this — slack goes to breathing room instead.
    const MAX_TRACK: usize = 24;

    let fixed = before_w + 1 + pct_w + after_w + telem_w + if telem_w > 0 { 2 } else { 0 };
    if total >= fixed + MIN_TRACK {
        // Full layout: stepper labels + track + percent + telemetry.
        let track_w = (total - fixed).min(MAX_TRACK);
        let mut x = area.x;
        let line_w = (before_w).min(total) as u16;
        Paragraph::new(Line::from(before)).render(
            Rect {
                x,
                y,
                width: line_w,
                height: 1,
            },
            buf,
        );
        x += line_w + 1;
        render_track(state, now_ms, mode, x, y, track_w as u16, buf);
        x += track_w as u16;
        let mut tail: Vec<Span<'static>> = Vec::new();
        if !pct_text.is_empty() {
            tail.push(Span::styled(
                pct_text,
                Style::default().fg(theme::TEXT_PRIMARY),
            ));
        }
        tail.extend(after);
        let tail_w = (pct_w + after_w).min((area.x + area.width).saturating_sub(x) as usize);
        if tail_w > 0 {
            Paragraph::new(Line::from(tail)).render(
                Rect {
                    x,
                    y,
                    width: tail_w as u16,
                    height: 1,
                },
                buf,
            );
        }
        render_right_telem(telem, telem_w, area, buf);
    } else if total >= pct_w + telem_w + 2 + 6 {
        // Narrow: drop the labels; keep track + percent + telemetry.
        let track_w = total - pct_w - telem_w - if telem_w > 0 { 2 } else { 0 };
        render_track(state, now_ms, mode, area.x, y, track_w as u16, buf);
        if pct_w > 0 {
            Paragraph::new(Line::from(Span::styled(
                pct_text,
                Style::default().fg(theme::TEXT_PRIMARY),
            )))
            .render(
                Rect {
                    x: area.x + track_w as u16,
                    y,
                    width: pct_w as u16,
                    height: 1,
                },
                buf,
            );
        }
        render_right_telem(telem, telem_w, area, buf);
    } else {
        // Narrowest: the track alone — the load-bearing element.
        render_track(state, now_ms, mode, area.x, y, area.width, buf);
    }
}

/// Right-align the telemetry spans on the row (no-op when empty).
fn render_right_telem(telem: Vec<Span<'static>>, telem_w: usize, area: Rect, buf: &mut Buffer) {
    if telem_w == 0 || telem_w > area.width as usize {
        return;
    }
    let x = area.x + (area.width as usize - telem_w) as u16;
    Paragraph::new(Line::from(telem)).render(
        Rect {
            x,
            y: area.y,
            width: telem_w as u16,
            height: 1,
        },
        buf,
    );
}

/// Paint just the fill track (gradient fill, dim groove, shimmer) into
/// `[x, x+w)` on row `y`. The shimmer is the one permitted motion — a
/// scrubbed `theme::lighten` toward white over the gradient, never
/// accumulated state — and it stays gated on `no_anim` via
/// [`ProgressState::animate`].
fn render_track(
    state: &ProgressState,
    now_ms: u64,
    mode: ColorMode,
    x: u16,
    y: u16,
    w: u16,
    buf: &mut Buffer,
) {
    let w = w as usize;
    if w == 0 {
        return;
    }
    let truecolor = mode.is_truecolor();
    let fill_cells = (state.fill * w as f64).round() as usize;

    // Shimmer: a light band whose center sweeps left→right within the filled
    // region only. A pure function of the clock — no persisted state.
    let shimmer_center = if state.animate && fill_cells > 0 {
        let t = (now_ms % SHIMMER_PERIOD_MS) as f64 / SHIMMER_PERIOD_MS as f64;
        Some(t * fill_cells as f64)
    } else {
        None
    };
    let head = fill_cells.saturating_sub(1); // last filled cell

    for i in 0..w {
        let Some(cell) = buf.cell_mut((x + i as u16, y)) else {
            continue;
        };

        if i < fill_cells {
            // The fill is a glyph (not a background), so the bar's *shape* reads
            // even under `NO_COLOR`, where every color drops to the terminal
            // default; the brand gradient rides the glyph's foreground.
            let t = if w > 1 {
                i as f64 / (w - 1) as f64
            } else {
                0.0
            };
            let mut fg = if truecolor {
                theme::brand_gradient(t)
            } else {
                theme::ACCENT
            };

            // The shimmer, scrubbed off the clock. On non-truecolor a
            // lightened RGB has no indexed fallback, so it degrades to a
            // single highlight cell on the solid ACCENT fill.
            if let Some(center) = shimmer_center {
                if truecolor {
                    let d = (i as f64 - center).abs();
                    if d < 2.5 {
                        fg = theme::lighten(fg, 0.45 * (1.0 - d / 2.5));
                    }
                } else if i == center.round() as usize {
                    fg = theme::ACCENT;
                }
            }

            // The frontier cell turns crimson on failure — the fill freezes at
            // the stage the run reached and the head says why it stopped.
            if i == head && state.phase == RunPhase::Error {
                fg = theme::DANGER;
            }

            // A completed run reads as a solid success-green bar.
            if state.phase == RunPhase::Complete {
                fg = theme::SUCCESS_BRIGHT;
            }
            cell.set_symbol("█");
            cell.set_fg(fg);
        } else {
            // Unfilled track — a dim groove.
            cell.set_symbol("░");
            cell.set_fg(theme::TEXT_DIM);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{AgentMeta, Inbound};
    use stella_protocol::AgentEvent;

    fn agent_running(stage: StageKind) -> WorkspaceModel {
        let mut m = WorkspaceModel::new();
        m.now_ms = 10_000;
        m.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        m.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::Stage { name: stage },
        });
        m
    }

    fn focused(m: &WorkspaceModel) -> Option<&AgentEntry> {
        m.agents.first()
    }

    #[test]
    fn no_agent_is_idle() {
        let s = ProgressState::derive(None, 0, false);
        assert_eq!(s.phase, RunPhase::Idle);
        assert_eq!(s.fill, 0.0);
        assert!(!s.animate);
    }

    #[test]
    fn stage_maps_to_the_right_phase_and_fill() {
        let plan = agent_running(StageKind::Plan);
        let s = ProgressState::derive(focused(&plan), plan.now_ms, false);
        assert_eq!(s.phase, RunPhase::Running);
        assert_eq!(s.segments[0], SegState::Active);
        assert!(
            (s.fill - 1.0 / 6.0).abs() < 1e-9,
            "plan → 1/6, got {}",
            s.fill
        );

        let exec = agent_running(StageKind::Execute);
        let s = ProgressState::derive(focused(&exec), exec.now_ms, false);
        assert_eq!(s.segments[0], SegState::Done);
        assert_eq!(s.segments[1], SegState::Active);
        assert_eq!(s.segments[2], SegState::Pending);
        assert!((s.fill - 0.5).abs() < 1e-9);
        assert_eq!(s.pct, 50);

        let verify = agent_running(StageKind::Verify);
        let s = ProgressState::derive(focused(&verify), verify.now_ms, false);
        assert_eq!(s.segments[2], SegState::Active);
        assert!((s.fill - 5.0 / 6.0).abs() < 1e-9);
    }

    fn lead_registered(now_ms: u64) -> WorkspaceModel {
        let mut m = WorkspaceModel::new();
        m.now_ms = now_ms;
        m.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        m
    }

    #[test]
    fn command_in_flight_with_no_stage_reads_in_progress_not_idle() {
        // A driver command (e.g. /init) emits no Stage events, but PromptStarted
        // starts the clock — the bar must show the default in-progress state
        // (plan), never a stale fill and never idle.
        let mut m = lead_registered(5_000);
        m.apply_inbound(&Inbound::PromptStarted {
            agent: "lead".into(),
            text: "/init".into(),
        });
        let s = ProgressState::derive(focused(&m), m.now_ms, false);
        assert_eq!(s.phase, RunPhase::Running, "clock running ⇒ in-progress");
        assert_eq!(
            s.segments[0],
            SegState::Active,
            "default in-progress = plan"
        );
        assert!(
            (s.fill - 1.0 / 6.0).abs() < 1e-9,
            "restarts at the beginning"
        );
    }

    #[test]
    fn resting_after_a_command_reads_idle_not_stranded() {
        // When a handled command completes, the clock stops (WaitingInput →
        // end_turn) and, with no stage/complete, the bar returns to idle — it is
        // never left frozen mid-fill even though WaitingInput is `is_active()`.
        let mut m = lead_registered(5_000);
        m.apply_inbound(&Inbound::PromptStarted {
            agent: "lead".into(),
            text: "/init".into(),
        });
        m.apply_inbound(&Inbound::Status {
            agent: "lead".into(),
            status: AgentStatus::WaitingInput,
        });
        let s = ProgressState::derive(focused(&m), m.now_ms, false);
        assert_eq!(s.phase, RunPhase::Idle, "clock stopped + no stage ⇒ idle");
        assert_eq!(s.fill, 0.0);
    }

    #[test]
    fn new_prompt_after_completion_restarts_from_the_beginning() {
        // A completed turn leaves the bar full-green; the NEXT submission must
        // reset it to the in-progress start, not resume frozen at verify/100%.
        let mut m = agent_running(StageKind::Verify);
        m.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::Complete {
                model: "glm-5.2".into(),
                cost_usd: 0.1,
            },
        });
        assert_eq!(
            ProgressState::derive(focused(&m), m.now_ms, false).phase,
            RunPhase::Complete,
            "precondition: full-green"
        );
        m.apply_inbound(&Inbound::PromptStarted {
            agent: "lead".into(),
            text: "another".into(),
        });
        let s = ProgressState::derive(focused(&m), m.now_ms, false);
        assert_eq!(
            s.phase,
            RunPhase::Running,
            "reset to in-progress, not stale complete"
        );
        assert!(
            (s.fill - 1.0 / 6.0).abs() < 1e-9,
            "back to the plan-phase start, got {}",
            s.fill
        );
    }

    #[test]
    fn completion_fills_green_to_100() {
        let mut m = agent_running(StageKind::Verify);
        m.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::Complete {
                model: "glm-5.2".into(),
                cost_usd: 0.1,
            },
        });
        let s = ProgressState::derive(focused(&m), m.now_ms, false);
        assert_eq!(s.phase, RunPhase::Complete);
        assert_eq!(s.fill, 1.0);
        assert_eq!(s.pct, 100);
        assert!(s.segments.iter().all(|&x| x == SegState::Done));
        assert!(!s.animate, "a finished run does not shimmer");
    }

    #[test]
    fn failure_freezes_and_stops_motion() {
        let mut m = agent_running(StageKind::Execute);
        m.apply_inbound(&Inbound::Status {
            agent: "lead".into(),
            status: AgentStatus::Failed,
        });
        let s = ProgressState::derive(focused(&m), m.now_ms, false);
        assert_eq!(s.phase, RunPhase::Error);
        // Frozen at the execute position — not advanced, not zeroed.
        assert!((s.fill - 0.5).abs() < 1e-9);
        assert!(!s.animate);
    }

    #[test]
    fn tok_per_s_is_the_live_turns_rate_or_omitted() {
        // A later turn in a long session: only tokens since ITS PromptStarted
        // count, over ITS elapsed — dividing the session's cumulative output
        // by the agent's lifetime would report the average, not the rate.
        let mut m = agent_running(StageKind::Execute);
        if let Some(a) = m.agents.first_mut() {
            a.tokens_out = 9_000; // prior turns' output
        }
        m.apply_inbound(&Inbound::PromptStarted {
            agent: "lead".into(),
            text: "go".into(),
        });
        if let Some(a) = m.agents.first_mut() {
            a.tokens_out += 500; // this turn's output so far
        }
        m.now_ms += 10_000; // 10s into the turn
        let s = ProgressState::derive(focused(&m), m.now_ms, false);
        assert_eq!(s.tok_per_s, Some(50), "500 turn tokens over 10 turn secs");

        // No tokens emitted this turn yet → omitted, never guessed.
        let mut plain = agent_running(StageKind::Execute);
        plain.apply_inbound(&Inbound::PromptStarted {
            agent: "lead".into(),
            text: "go".into(),
        });
        let s = ProgressState::derive(focused(&plain), plain.now_ms + 1_000, false);
        assert_eq!(s.tok_per_s, None);

        // Running with tokens but NO turn clock (a lane that never saw
        // `PromptStarted`) → omitted rather than a lifetime average.
        let mut clockless = agent_running(StageKind::Execute);
        if let Some(a) = clockless.agents.first_mut() {
            a.tokens_out = 500;
            a.meta.started_ms = 0;
        }
        clockless.now_ms = 10_000;
        let s = ProgressState::derive(focused(&clockless), clockless.now_ms, false);
        assert_eq!(s.tok_per_s, None);
    }

    #[test]
    fn no_anim_forces_a_static_frame() {
        let exec = agent_running(StageKind::Execute);
        let s = ProgressState::derive(focused(&exec), exec.now_ms, true);
        assert!(!s.animate, "--no-anim freezes the shimmer/pulse");
    }

    #[test]
    fn renders_without_panic_at_narrow_and_wide_widths() {
        for w in [8u16, 20, 40, 80, 200] {
            let exec = agent_running(StageKind::Execute);
            let state = ProgressState::derive(focused(&exec), exec.now_ms, false);
            let area = Rect::new(0, 0, w, 1);
            let mut buf = Buffer::empty(area);
            render_state(&state, 1234, ColorMode::Truecolor, false, area, &mut buf);
            // The bar painted a filled glyph for a mid-run state on any width.
            let filled = (0..w).any(|x| buf.cell((x, 0)).is_some_and(|c| c.symbol() == "█"));
            assert!(filled, "width {w} should paint a fill");
        }
    }

    #[test]
    fn non_truecolor_fill_uses_named_tokens_never_an_interpolated_rgb() {
        let exec = agent_running(StageKind::Execute);
        let state = ProgressState::derive(focused(&exec), exec.now_ms, false);
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        // The track itself (not the labels/telemetry) is what must degrade
        // cleanly — an interpolated gradient RGB has no indexed fallback.
        render_track(&state, 1234, ColorMode::Ansi256, 0, 0, 40, &mut buf);
        let allowed = [
            theme::ACCENT,
            theme::ACCENT,
            theme::TEXT_DIM,
            theme::HAIRLINE,
            ratatui::style::Color::Reset,
        ];
        for x in 0..40 {
            if let Some(c) = buf.cell((x, 0)) {
                assert!(allowed.contains(&c.fg), "unexpected fg {:?} at x={x}", c.fg);
            }
        }
    }

    #[test]
    fn no_color_keeps_the_bar_shape() {
        // Under NO_COLOR every color drops to the terminal default, but the
        // fill glyph must survive so the determinate bar still conveys progress.
        let exec = agent_running(StageKind::Execute);
        let state = ProgressState::derive(focused(&exec), exec.now_ms, false);
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        render_state(&state, 1234, ColorMode::Truecolor, false, area, &mut buf);
        theme::degrade_buffer(&mut buf, ColorMode::None);
        let filled = (0..40)
            .filter(|&x| buf.cell((x, 0)).is_some_and(|c| c.symbol() == "█"))
            .count();
        let track = (0..40)
            .filter(|&x| buf.cell((x, 0)).is_some_and(|c| c.symbol() == "░"))
            .count();
        assert!(filled > 0, "the fill shape survives NO_COLOR");
        assert!(track > 0, "the track shape survives NO_COLOR");
        // …and every color really was stripped to the default.
        assert!(
            (0..40).all(|x| buf
                .cell((x, 0))
                .is_some_and(|c| c.fg == ratatui::style::Color::Reset)),
            "NO_COLOR leaves no residual color"
        );
    }
}
