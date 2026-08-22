// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The metric block at the head of the `?` help overlay.
//!
//! SPEC 5 (`design/tui-v2/SPEC.md`) re-homes five cells the v1 two-row status
//! wall carried — MODEL detail, CPU, MEM, WARMTH and ENGINE — to **two**
//! surfaces: the AGENTS tab and behind `?`. The AGENTS half shipped in
//! [`crate::views::agents`] (the `CPU%` / `MEM` / `Warmth` columns and the
//! active/total count on the EXECUTIONS header); this module is the other half
//! (#4188), and SPEC 11's one-line description of `?` — "help, full metric
//! detail" — is what it makes true. Until it existed, `?` was a keybinding
//! sheet and the second home SPEC 5 promised did not render.
//!
//! Its own module for the same reason [`super::footer`] and [`super::parked`]
//! are: `deck_render.rs` is the file in this crate that keeps arriving at the
//! file-size ratchet, so new drawing lands beside it rather than in it.
//!
//! Everything here is read straight off the fold — [`WorkspaceModel`] and the
//! driver's engine snapshot — so the block cannot disagree with the AGENTS tab
//! about a number they both render.

use ratatui::text::{Line, Span};

use crate::cache_panel;
use crate::deck::{AgentEntry, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::envelope::EngineConfigState;
use crate::theme;

/// Width of the label column.
///
/// Deliberately narrower than the shortcut sheet's own key column below it
/// (13): the MODEL rows carry a `provider/slug` **and** the roles riding it,
/// and the help popup is capped at 68 columns by `super::render_help`.
const LABEL_W: usize = 9;

/// Shown wherever a metric has no value yet — the same em-dash
/// [`cache_panel::fmt_warmth`] uses for a session with no cache to time, so
/// the column reads consistently.
const ABSENT: &str = "—";

/// The metric block drawn above the shortcut sections — its blank separator
/// and heading included, so the caller is one `extend` and the overlay's own
/// layout stays in one place.
///
/// CPU, MEM and WARMTH are the **focused** agent's, which is the same agent
/// the statline and the SESSION transcript are about; ENGINE and the model
/// wiring are session-wide.
pub(super) fn rows(model: &WorkspaceModel, ui: &DeckUi) -> Vec<Line<'static>> {
    let focused = model.agents.get(ui.focused);
    let mut lines = vec![
        Line::default(),
        Line::from(Span::styled("  session metrics", theme::heading())),
        row("cpu", cpu(focused)),
        row("mem", mem(focused)),
        row(
            "warmth",
            cache_panel::fmt_warmth(focused.and_then(|a| a.cache_warmth_secs(model.now_ms))),
        ),
        // The cell ENGINE became — SPEC 5 §9.5 puts the same pair on the
        // EXECUTIONS header, where the lane split sits beside it.
        row(
            "engine",
            format!(
                "{} active / {} total",
                model.active_count(),
                model.agents.len()
            ),
        ),
    ];
    lines.extend(model_rows(ui.engine.pristine.as_ref(), focused));
    lines
}

/// The MODEL detail rows: which pin is serving each role.
///
/// This is the cell the status bar cannot carry. The bar shows one slug —
/// whichever pin is answering right now — and a reader who wants to know that
/// the verifier is on a different model than the worker has, since the v1 wall
/// went away, had nowhere to look (#4188).
///
/// Grouped by model rather than listed per role, because the ordinary posture
/// is several roles inheriting `default_model`: one line per *distinct* pin
/// says "these four are the same and that one is not" in the shape of the
/// block, and keeps the whole overlay inside a 40-row terminal.
///
/// Read from the **pristine** snapshot — the last configuration the driver
/// reported — not from [`crate::views::engine::EngineOverlay::state`], which
/// may hold unsaved edits from the SETTINGS editor. A metric view reports what
/// is running; an edit that has not been saved is not.
fn model_rows(
    config: Option<&EngineConfigState>,
    focused: Option<&AgentEntry>,
) -> Vec<Line<'static>> {
    // Before the first engine snapshot lands there is no wiring to show, so
    // fall back to the one slug the deck does know: the focused agent's route.
    let fallback = || {
        vec![row(
            "model",
            focused
                .and_then(|a| a.meta.model.clone())
                .unwrap_or_else(|| ABSENT.to_string()),
        )]
    };
    let Some(config) = config else {
        return fallback();
    };

    // First-appearance order, so the block is a stable function of the
    // snapshot and a golden frame can pin it.
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for wiring in &config.roles {
        match groups.iter_mut().find(|(slug, _)| *slug == wiring.model) {
            Some((_, roles)) => roles.push(&wiring.role),
            None => groups.push((&wiring.model, vec![&wiring.role])),
        }
    }
    if groups.is_empty() {
        return fallback();
    }

    groups
        .into_iter()
        .enumerate()
        .map(|(i, (slug, roles))| {
            // Only the first row is labelled; the rest are a hanging indent
            // under it, the way the shortcut sections read.
            row(
                if i == 0 { "model" } else { "" },
                format!("{slug}  {}", roles.join(" · ")),
            )
        })
        .collect()
}

fn cpu(focused: Option<&AgentEntry>) -> String {
    focused.map_or_else(
        || ABSENT.to_string(),
        |entry| format!("{:.0}%", entry.res.cpu_pct),
    )
}

fn mem(focused: Option<&AgentEntry>) -> String {
    focused.map_or_else(
        || ABSENT.to_string(),
        |entry| humanize_bytes(entry.res.mem_bytes),
    )
}

/// Bytes → `"148M"`, binary (1024) units, whole numbers only.
///
/// A second copy of `views::agents::humanize_bytes`, which is private to that
/// module. MEM has to read identically on both of the surfaces SPEC 5 sends it
/// to, and a differently-rounded second rendering of the same `u64` is worse
/// than the duplication; the fix is to give it one home both can call, which
/// is a change to a file this module does not own.
fn humanize_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut val = bytes as f64;
    let mut unit = 0;
    while val >= 1024.0 && unit < UNITS.len() - 1 {
        val /= 1024.0;
        unit += 1;
    }
    format!("{val:.0}{}", UNITS[unit])
}

/// One `label  value` row, aligned on [`LABEL_W`]. An empty label draws the
/// hanging indent for a continuation row.
fn row(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {label:<LABEL_W$} "), theme::accent()),
        Span::styled(value, theme::body()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::RoleWiringRow;

    fn wiring(role: &str, model: &str) -> RoleWiringRow {
        RoleWiringRow {
            role: role.to_string(),
            model: model.to_string(),
            effort: "medium".to_string(),
            thinking: "thinking on".to_string(),
            source: "default_model".to_string(),
            next_session: None,
        }
    }

    fn text(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Roles sharing a pin collapse onto one row; a role with its own pin gets
    /// its own. This is the whole point of the MODEL block — "the verifier is
    /// somewhere else" has to be visible at a glance.
    #[test]
    fn model_rows_group_roles_by_the_pin_serving_them() {
        let config = EngineConfigState {
            roles: vec![
                wiring("default", "zai/glm-5.2-air"),
                wiring("worker", "zai/glm-5.2-air"),
                wiring("verifier", "anthropic/claude-opus-5"),
            ],
            ..Default::default()
        };
        let rows: Vec<String> = model_rows(Some(&config), None).iter().map(text).collect();
        assert_eq!(rows.len(), 2, "two distinct pins, two rows: {rows:?}");
        assert!(
            rows[0].contains("zai/glm-5.2-air  default · worker"),
            "{rows:?}"
        );
        assert!(rows[0].starts_with("  model"), "{rows:?}");
        assert!(
            rows[1].contains("anthropic/claude-opus-5  verifier"),
            "{rows:?}"
        );
        assert!(
            rows[1].trim_start().starts_with("anthropic"),
            "the continuation row hangs under the label: {rows:?}"
        );
    }

    /// No engine snapshot yet — the block still says something rather than
    /// rendering a blank row, and what it says is the one slug the deck knows.
    #[test]
    fn model_rows_fall_back_to_the_focused_agents_route() {
        let rows: Vec<String> = model_rows(None, None).iter().map(text).collect();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].contains(ABSENT), "{rows:?}");
    }

    #[test]
    fn mem_reads_in_binary_units() {
        assert_eq!(humanize_bytes(148 * 1024 * 1024), "148M");
        assert_eq!(humanize_bytes(0), "0B");
    }
}
