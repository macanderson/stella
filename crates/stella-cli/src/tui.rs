//! Terminal UI — streaming text, tool-call cards, cost tracking.
//!
//! Designed for speed and engagement: tool calls appear as cards with
//! status, and the model's response prints as soon as it's ready.
//!
//! `render_event` is the TUI's one entry point onto `stella_core::Engine`'s
//! event stream (L-T1: the TUI renders exclusively
//! from `AgentEvent`s — no panel owns state that isn't reconstructible by
//! replaying the event log). It's deliberately a thin dispatcher onto the
//! existing per-kind print helpers below, not a rewrite of them.
//!
//! There is deliberately no animated "thinking" spinner: the Phase 0/1
//! version had one, but it only ticked a fixed 3 frames *before* dispatching
//! the network call, then froze for the entire real wait — a decorative
//! pre-roll, not a live indicator. A correct live spinner would need its own
//! concurrent task racing the event-draining task below, both writing to the
//! terminal — real interleaving risk for a cosmetic win. `Stage::Execute`
//! below gives one clean, immediate "thinking" line instead. There is no
//! decorative "rocket"/spinner animation either — activity is reported by the
//! command deck's honest run progress bar (`stella_tui::progress`), never by a
//! cosmetic character-noise loop.

use std::io::{self, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use colored::{Color, ColoredString, Colorize};
use stella_protocol::{AgentEvent, BudgetMode, StageKind};
use stella_tui::textline::{self, EventLine, Tone, fmt_cost};

/// Truncate `s` to at most `max` characters, appending `…` when it was
/// shortened. Char-boundary-safe: operates on `char`s, never byte indices,
/// so it can never panic on multi-byte UTF-8 the way a `&s[..57]` byte-slice
/// does (a slice at a non-char boundary is an immediate panic — e.g. any
/// tool input or output with an accented letter or emoji at the cut point).
fn truncate_with_ellipsis(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    } else {
        s.to_string()
    }
}

/// Selectable accent palette — `/color` switches the session's accent so
/// multiple terminal windows running stella are visually distinct at a
/// glance (see [`set_accent`], and [`rename_tab`] for the `/rename` sibling).
// The brand entry is first, and so the default. The rest are personalisation
// only — `/color` exists so several terminal windows running stella can be told
// apart at a glance, which needs hues that are *distinct* rather than on-brand.
// `colored`'s named ANSI colors are the portable stand-ins, and the nearest one
// to the brand's Phosphor Gold (`theme::ACCENT`, `#FFB81A`) is bright-yellow.
//
// The default used to be bright-blue (and bright-cyan before that): the
// accent trailed the identity through recolour after recolour because
// nothing asserted which hue the default resolves to. The comet kit ends
// that — the brand slot is `gold` now, and the test below pins it.
//
// The old slugs are all kept as personalisation: `sky` and `azure` still
// land on bright-blue (the named set holds no second distinct blue), and
// `/color sky` keeps working for anyone whose muscle memory types it — it
// just no longer claims to be the brand.
const PALETTE: [(&str, Color); 7] = [
    ("gold", Color::BrightYellow),
    ("sky", Color::BrightBlue),
    ("cyan", Color::BrightCyan),
    ("azure", Color::BrightBlue),
    ("violet", Color::BrightMagenta),
    ("magenta", Color::Magenta),
    ("mint", Color::Green),
];

static ACCENT: AtomicUsize = AtomicUsize::new(0);

/// The session accent color (defaults to the brand gold, `PALETTE[0]`).
pub fn accent() -> Color {
    PALETTE[ACCENT.load(Ordering::Relaxed) % PALETTE.len()].1
}

/// Set the accent by name; returns false (and prints the options) on an
/// unknown name.
pub fn set_accent(name: &str) -> bool {
    if let Some(index) = PALETTE.iter().position(|(n, _)| *n == name.to_lowercase()) {
        ACCENT.store(index, Ordering::Relaxed);
        true
    } else {
        let names: Vec<&str> = PALETTE.iter().map(|(n, _)| *n).collect();
        println!(
            "  unknown color `{name}` — pick one of: {}",
            names.join(", ")
        );
        false
    }
}

/// Rename the terminal tab/window via the OSC 0 escape — running several
/// stella windows side by side, each can carry its own title.
///
/// Control characters are dropped before interpolation: the title is free
/// text, and an embedded `ESC`/`BEL` would terminate this OSC string early
/// and let the remainder be read by the terminal as its own command sequence.
/// Printable text is passed through unchanged, so a normal `/rename` is
/// unaffected.
pub fn rename_tab(title: &str) {
    let safe: String = title.chars().filter(|c| !c.is_control()).collect();
    print!("\x1b]0;{safe}\x07");
    let _ = io::stdout().flush();
}

/// Render the Files Touched panel: one `[C|R|U|D]` badge per file, in
/// first-touch order, sourced from the registry's CRUD ledger.
pub fn files_touched_panel(entries: &[(String, String)]) {
    if entries.is_empty() {
        return;
    }
    println!(
        "\n  {} {}",
        "─".repeat(3).dimmed(),
        "Files Touched".color(accent()).bold()
    );
    let width = entries.iter().map(|(_, ops)| ops.len()).max().unwrap_or(1);
    for (path, ops) in entries {
        let colored_ops: String = ops
            .chars()
            .map(|op| match op {
                'C' => "C".green().to_string(),
                'R' => "R".bright_magenta().to_string(),
                'U' => "U".yellow().to_string(),
                'D' => "D".red().to_string(),
                other => other.to_string(),
            })
            .collect();
        println!(
            "  [{colored_ops}]{} {}",
            " ".repeat(width - ops.len()),
            path
        );
    }
}

/// Pretty per-verb rendering for the CRUD file tools; returns false (so the
/// caller falls back to the generic key=value card) for anything else.
fn file_tool_card(name: &str, input: &serde_json::Value) -> bool {
    let Some(path) = input.get("path").and_then(|v| v.as_str()) else {
        return false;
    };
    match name {
        "read_file" => {
            let range = match (
                input.get("offset").and_then(|v| v.as_u64()),
                input.get("limit").and_then(|v| v.as_u64()),
            ) {
                (Some(offset), Some(limit)) => format!(" [{offset}..+{limit}]"),
                (Some(offset), None) => format!(" [{offset}..]"),
                _ => String::new(),
            };
            println!(
                "  {} {} {}{}",
                "▷".bright_magenta(),
                "read".bright_magenta(),
                path,
                range.dimmed()
            );
        }
        "write_file" => {
            let lines = input
                .get("content")
                .and_then(|v| v.as_str())
                .map(|content| content.lines().count())
                .unwrap_or(0);
            println!(
                "  {} {} {} {}",
                "✚".green(),
                "write".green(),
                path,
                format!("({lines} lines)").dimmed()
            );
        }
        "edit_file" => {
            println!("  {} {} {}", "±".yellow(), "edit".yellow(), path);
            if let Some(old) = input.get("old_string").and_then(|v| v.as_str()) {
                let old_line = truncate_with_ellipsis(old.lines().next().unwrap_or(""), 70);
                println!("    {} {}", "-".red(), old_line.red().dimmed());
            }
            if let Some(new) = input.get("new_string").and_then(|v| v.as_str()) {
                let new_line = truncate_with_ellipsis(new.lines().next().unwrap_or(""), 70);
                println!("    {} {}", "+".green(), new_line.green());
            }
        }
        "delete_file" => {
            println!("  {} {} {}", "✖".red(), "delete".red(), path);
        }
        _ => return false,
    }
    true
}

/// Print a tool-call card: name, input summary, status. The CRUD file tools
/// get a dedicated per-verb card (`file_tool_card`) while they're running;
/// everything else gets the generic key=value summary below.
pub fn tool_call_card(name: &str, input: &serde_json::Value, status: &str) {
    if status == "running" && file_tool_card(name, input) {
        return;
    }
    let icon = match status {
        "running" => "▶".bright_red(),
        "ok" => "✓".green(),
        "error" => "✗".red(),
        _ => "·".dimmed(),
    };
    let input_str = if input.is_object() {
        // Show key fields compactly
        if let Some(obj) = input.as_object() {
            let summary: Vec<String> = obj
                .iter()
                .take(3)
                .map(|(k, v)| {
                    let val_str = if let Some(s) = v.as_str() {
                        truncate_with_ellipsis(s, 57)
                    } else {
                        v.to_string()
                    };
                    format!("{}={}", k.bright_magenta(), val_str)
                })
                .collect();
            summary.join(" ")
        } else {
            input.to_string()
        }
    } else {
        input.to_string()
    };

    // The tool name takes the session accent, matching the deck: it is the one
    // token in a transcript that carries the brand hue, because the names are
    // what a reader scans a scrollback *for*. It was `bright_yellow` — this
    // file's own stand-in for the retired gold — which survived the recolour
    // because no "gold" string named it.
    println!(
        "  {} {}({})",
        icon,
        name.color(accent()),
        input_str.dimmed()
    );
}

/// Print a tool result summary.
pub fn tool_result_card(_name: &str, output: &str, is_error: bool, duration: Duration) {
    let icon = if is_error { "✗".red() } else { "✓".green() };
    let label = if is_error {
        "error".red()
    } else {
        "ok".green()
    };
    let preview = output.lines().next().unwrap_or("(empty)");
    let preview = truncate_with_ellipsis(preview, 77);
    println!(
        "    {} {} in {:.0}ms — {}",
        icon,
        label,
        duration.as_secs_f64() * 1000.0,
        preview.dimmed()
    );
}

/// Print a section header.
pub fn section_header(title: &str) {
    println!(
        "\n{} {}",
        "─".dimmed().repeat(3),
        title.color(accent()).bold()
    );
}

/// Print the assistant's complete response (after streaming).
pub fn assistant_response(text: &str) {
    if !text.is_empty() {
        println!("\n{}", text);
    }
}

/// Print a cost summary for a turn. `AgentEvent::Complete` (the source of
/// this data under `stella_core::Engine`) carries `model`/`cost_usd` only —
/// no per-turn token breakdown — so this is deliberately narrow. Real
/// per-role token/cost accounting lives in `BudgetTick` (`render_event`
/// below), which fires after every call, not just at turn-end.
pub fn cost_summary(cost_usd: f64, model: &str, elapsed: Duration) {
    println!(
        "\n  {} {} · {} · {:.1}s",
        "◆".dimmed(),
        model.bright_magenta(),
        fmt_cost(cost_usd),
        elapsed.as_secs_f64(),
    );
}

/// The end-of-run recap (settings `enable_recap`): a deterministic synthesis
/// of the outcome and what changed, printed after the file and cost panels in
/// text mode. No model call — it reads the run's own facts. Complements the
/// file-list panel with the "so what": did it complete, was it verified, and
/// a created/modified/deleted tally.
pub fn recap_panel(
    status: &stella_pipeline::PipelineStatus,
    verdict: Option<&stella_pipeline::Verdict>,
    files: &[(String, String)],
) {
    use stella_pipeline::PipelineStatus;
    let headline = match status {
        PipelineStatus::Completed => match verdict {
            Some(v) if v.passed && v.deterministic => {
                "✓ completed — verified (deterministic)".to_string()
            }
            Some(v) if v.passed => "✓ completed — verified".to_string(),
            _ => "✓ completed".to_string(),
        },
        PipelineStatus::VerificationFailed { verdict } => {
            format!("✗ verification failed — {}", verdict.summary)
        }
        PipelineStatus::Aborted { reason, .. } => format!("⚠ aborted — {reason}"),
    };

    let mut created = 0usize;
    let mut modified = 0usize;
    let mut deleted = 0usize;
    for (_, op) in files {
        match op.to_ascii_lowercase().as_str() {
            o if o.contains("creat") || o.contains("add") => created += 1,
            o if o.contains("delet") || o.contains("remov") => deleted += 1,
            _ => modified += 1,
        }
    }
    let changes = if files.is_empty() {
        "no files changed".to_string()
    } else {
        let mut parts = Vec::new();
        if created > 0 {
            parts.push(format!("{created} created"));
        }
        if modified > 0 {
            parts.push(format!("{modified} modified"));
        }
        if deleted > 0 {
            parts.push(format!("{deleted} deleted"));
        }
        parts.join(" · ")
    };

    println!("\n  {} Recap", "◆".dimmed());
    println!("    {headline}");
    println!("    {changes}");
}

/// Print the welcome banner: the STELLA block-letter logomark swept with the
/// stellar gradient, then the session info line. (The previous banner was a
/// hand-pasted figlet that had drifted into garbage — it did not actually
/// spell "Stella" — which is exactly why the wordmark below is *composed*
/// from per-letter glyph data, the same defense the TS CLI's `banner.tsx`
/// uses: composition can't misspell.)
pub fn welcome_banner(provider: &str, model: &str, workspace: &str) {
    println!();
    for line in wordmark_lines() {
        println!("  {line}");
    }
    println!(
        "\n  {} {}",
        "✦".color(accent()),
        "a fast, BYOK, model-agnostic coding agent".dimmed()
    );
    println!(
        "  {} {} · {} · {}",
        "◆".color(accent()),
        format!("{provider}/{model}").bold(),
        workspace.dimmed(),
        "type your prompt, Ctrl+D to exit".dimmed(),
    );
    println!("  {}\n", "─".repeat(60).dimmed());
}

/// Render one `AgentEvent` from `stella_core::Engine::run_turn`'s stream.
/// `ToolStart`/`ToolResult` are intentionally a no-op here: `ToolResult`
/// doesn't carry the tool's name (only `call_id`), so the call site keeps a
/// small `call_id -> name` map and calls `tool_call_card`/`tool_result_card`
/// directly instead of routing those two through this function — see
/// `agent.rs`'s event-draining task. Every other event kind (including
/// `Text` — the engine emits one per step, not just at turn-end, since a
/// step with tool calls can still carry commentary text) is rendered here.
///
/// Wording comes from `stella_tui::textline` — the one event→text table
/// both this surface and the deck consume (issue #66); the arms below carry
/// only this surface's *policy* (what to suppress, what goes to stderr) and
/// styling. A new annotation variant needs a `textline` entry, nothing here.
pub fn render_event(event: &AgentEvent) {
    match event {
        AgentEvent::Stage {
            name: StageKind::Execute,
        } => {
            println!("  {}", "thinking…".dimmed());
        }
        AgentEvent::Stage { .. } | AgentEvent::ToolStart { .. } | AgentEvent::ToolResult { .. } => {
            // Complete-stage and ToolStart/ToolResult: handled inline at the
            // call site or by a more specific event (see the module doc).
        }
        AgentEvent::Text { text } => assistant_response(text),
        AgentEvent::Reasoning { .. } => {}
        AgentEvent::BudgetTick {
            mode: BudgetMode::Off,
            ..
        } => {
            // Unmetered sessions stay quiet about spend.
        }
        AgentEvent::Complete { .. } => {
            // The call site prints `cost_summary` from `TurnOutcome`
            // directly (it has the same model/cost_usd this event carries,
            // plus wall-clock elapsed time this event doesn't).
        }
        AgentEvent::AskUser {
            question, options, ..
        } => {
            // The interactive ask_user tool prints the numbered options and
            // collects the answer itself (it owns stdin while the turn is
            // in flight); this render arm exists for replay/stream-json
            // consumers of the event log. The binding free-text contract
            // (every question always offers a type-your-own option) is
            // enforced at the prompt site, not here.
            println!("  {}", styled_event_line(&textline::ask_user(question)));
            for (i, option) in options.iter().enumerate() {
                println!("    {} {}", format!("{})", i + 1).dimmed(), option);
            }
        }
        AgentEvent::Error { .. } => {
            // Errors belong on stderr — routing is policy, wording is shared.
            if let Some(line) = textline::event_line(event) {
                eprintln!("  {}", styled_event_line(&line));
            }
        }
        other => {
            if let Some(line) = textline::event_line(other) {
                println!("  {}", styled_event_line(&line));
            }
        }
    }
}

/// A shared [`EventLine`] in this surface's dress: the glyph carries the
/// tone via `colored` (bold when `strong`), the detail tail is dimmed.
fn styled_event_line(line: &EventLine) -> String {
    let glyph = match line.tone {
        Tone::Info => line.glyph.bright_magenta(),
        Tone::Success => line.glyph.green(),
        Tone::Warn => line.glyph.yellow(),
        Tone::Error => line.glyph.red(),
        Tone::Muted => line.glyph.dimmed(),
    };
    let glyph: ColoredString = if line.strong { glyph.bold() } else { glyph };
    match &line.detail {
        Some(detail) => format!("{} {} {}", glyph, line.body, detail.dimmed()),
        None => format!("{} {}", glyph, line.body),
    }
}

// ── STELLA logomark ─────────────────────────────────────────────────────────
//
// 5-row block glyphs, kept as literals so the wordmark renders identically
// across terminals (no figlet dependency) — the same composition scheme as
// the TS CLI's `apps/cli/src/tui/banner.tsx`. Each glyph is a fixed-width
// column of 5 rows; the word is composed by joining glyph rows with one
// space so letters never touch.

const GLYPH_ROWS: usize = 5;

fn glyph(ch: char) -> Option<[&'static str; GLYPH_ROWS]> {
    match ch {
        'S' => Some(["███████", "██     ", "███████", "     ██", "███████"]),
        'T' => Some(["███████", "   ██  ", "   ██  ", "   ██  ", "   ██  "]),
        'E' => Some(["███████", "██     ", "█████  ", "██     ", "███████"]),
        'L' => Some(["██     ", "██     ", "██     ", "██     ", "███████"]),
        'A' => Some([" █████ ", "██   ██", "███████", "██   ██", "██   ██"]),
        _ => None,
    }
}

/// Compose a word into its 5-row block-letter form. Unknown chars are
/// skipped, so the output can never contain a garbled letter.
fn compose_wordmark(text: &str) -> Vec<String> {
    let letters: Vec<[&'static str; GLYPH_ROWS]> = text
        .chars()
        .flat_map(|c| glyph(c.to_ascii_uppercase()))
        .collect();
    (0..GLYPH_ROWS)
        .map(|row| letters.iter().map(|g| g[row]).collect::<Vec<_>>().join(" "))
        .collect()
}

/// The `(r, g, b)` behind a theme token, for the two banners in this crate
/// that write raw truecolor escapes rather than ratatui cells.
///
/// The fallback is unreachable for a brand token — every one of them is a
/// `Color::Rgb` — and exists only because `Color` is an open enum. It is
/// deliberately a neutral grey rather than a guessed brand hue: a token that
/// somehow stopped being truecolor should look obviously wrong, not
/// plausibly on-brand.
pub(crate) const fn token_rgb(color: ratatui::style::Color) -> (u8, u8, u8) {
    match color {
        ratatui::style::Color::Rgb(r, g, b) => (r, g, b),
        _ => (0x80, 0x80, 0x80),
    }
}

/// Stellar gradient — one restrained brand sweep, deep → bright, left to right
/// across the wordmark.
///
/// Read out of [`stella_tui::theme`] rather than written here. The literals
/// this replaced were two recolours stale: they still held the pale "sky" pair
/// (`#38BDF8`/`#7DD3FC`) from an identity that had since gone to terminal green
/// and then to electric blue. Nothing caught it because the only guard was a
/// test asserting the same literals back — a copy checking itself. The banner
/// writes raw truecolor and so cannot use ratatui `Color`s directly, but it can
/// read them, which is the difference between a copy and a reference.
const STELLAR_STOPS: [(u8, u8, u8); 2] = [
    token_rgb(stella_tui::theme::ACCENT_DEEP),
    token_rgb(stella_tui::theme::ACCENT),
];

/// Color at horizontal position `t` ∈ `[0,1]` along the stellar gradient.
fn stellar_color_at(t: f64) -> (u8, u8, u8) {
    let clamped = t.clamp(0.0, 1.0);
    let segments = (STELLAR_STOPS.len() - 1) as f64;
    let scaled = clamped * segments;
    let idx = (scaled.floor() as usize).min(STELLAR_STOPS.len() - 2);
    let local = scaled - idx as f64;
    let from = STELLAR_STOPS[idx];
    let to = STELLAR_STOPS[idx + 1];
    let mix = |a: u8, b: u8| -> u8 { (a as f64 + (b as f64 - a as f64) * local).round() as u8 };
    (mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2))
}

/// The STELLA wordmark, each row colored column-by-column along the
/// gradient. Returns display-ready strings (ANSI truecolor when the terminal
/// supports it — `colored` handles the no-color fallback itself).
fn wordmark_lines() -> Vec<String> {
    let rows = compose_wordmark("STELLA");
    let width = rows.first().map(|r| r.chars().count()).unwrap_or(1).max(2);
    rows.iter()
        .map(|row| {
            row.chars()
                .enumerate()
                .map(|(i, ch)| {
                    if ch == ' ' {
                        ch.to_string()
                    } else {
                        let (r, g, b) = stellar_color_at(i as f64 / (width - 1) as f64);
                        ch.to_string().truecolor(r, g, b).to_string()
                    }
                })
                .collect::<String>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── shared event lines ─────────────────────────────────────────────────

    // Strips ANSI so assertions pin visible text regardless of whether
    // `colored` detects a tty in the test harness.
    use stella_tui::strip_ansi;

    #[test]
    fn styled_event_line_composes_glyph_body_detail_with_single_spaces() {
        // Wording is pinned byte-exactly in `stella_tui::textline`'s own
        // fixtures; this guards the composition this surface adds around it
        // (the old renderers wrote `"  {glyph} {body} {detail}"`).
        assert_eq!(
            strip_ansi(&styled_event_line(&textline::retry(2, "rate limited"))),
            "↻ retry #2: rate limited"
        );
        assert_eq!(
            strip_ansi(&styled_event_line(&textline::budget_tick(0.42, None))),
            "$ spend: $0.4200"
        );
    }

    // ── wordmark ───────────────────────────────────────────────────────────

    #[test]
    fn wordmark_composes_six_letters_into_five_equal_width_rows() {
        let rows = compose_wordmark("STELLA");
        assert_eq!(rows.len(), GLYPH_ROWS);
        let width = rows[0].chars().count();
        // 6 glyphs × 7 columns + 5 separator spaces.
        assert_eq!(width, 6 * 7 + 5);
        for row in &rows {
            assert_eq!(row.chars().count(), width, "ragged wordmark row");
            assert!(row.chars().all(|c| c == '█' || c == ' '));
        }
    }

    #[test]
    fn wordmark_skips_unknown_characters_instead_of_garbling() {
        // The old banner shipped a hand-pasted figlet that silently drifted
        // into not spelling "Stella" — composition skips anything it has no
        // glyph for, so a typo shrinks the mark instead of corrupting it.
        assert_eq!(compose_wordmark("S?T"), compose_wordmark("ST"));
    }

    /// The banner writes raw truecolor, so it converts rather than renders —
    /// but it must convert the LIVE brand tokens, never a copy of their values.
    ///
    /// This assertion used to read `(0x38, 0xBD, 0xF8)` / `(0x7D, 0xD3, 0xFC)`
    /// on both sides: the constant checked against itself, which is no check at
    /// all. It passed happily through two whole recolours while the banner kept
    /// painting a retired palette. Comparing against `theme::` is what makes it
    /// a guard.
    #[test]
    fn stellar_stops_are_the_live_brand_tokens_not_a_copy_of_them() {
        assert_eq!(
            STELLAR_STOPS[0],
            token_rgb(stella_tui::theme::ACCENT_DEEP),
            "the gradient's deep stop must BE theme::ACCENT_DEEP"
        );
        assert_eq!(
            STELLAR_STOPS[1],
            token_rgb(stella_tui::theme::ACCENT),
            "the gradient's bright stop must BE theme::ACCENT"
        );
        // And the conversion has to actually convert — a token silently
        // falling through to the neutral grey would satisfy the equalities
        // above while painting the wordmark grey.
        assert_ne!(STELLAR_STOPS[0], (0x80, 0x80, 0x80));
        assert_ne!(STELLAR_STOPS[1], (0x80, 0x80, 0x80));
        assert_ne!(
            STELLAR_STOPS[0], STELLAR_STOPS[1],
            "a gradient needs two distinct stops"
        );
    }

    /// Gold is the brand, so it is the default accent -- `/color` with no
    /// argument, and a fresh session, must land on it.
    #[test]
    fn default_accent_is_gold() {
        assert_eq!(PALETTE[0].0, "gold");
        assert_eq!(ACCENT.load(Ordering::Relaxed) % PALETTE.len(), 0);
        // …and it must be the BRAND hue, not merely first in the list. The
        // default trailed the identity through recolour after recolour
        // because nothing asserted which colour the default slug actually
        // resolves to. Bright-yellow is the nearest named ANSI stand-in for
        // `theme::ACCENT` (Phosphor Gold `#FFB81A`).
        assert_eq!(PALETTE[0].1, Color::BrightYellow);
        assert_eq!(accent(), Color::BrightYellow);
        // The old brand slug survives as personalisation, then the process
        // global returns to the brand default for any test that reads it.
        assert!(set_accent("sky"));
        assert_eq!(PALETTE[ACCENT.load(Ordering::Relaxed)].0, "sky");
        assert!(set_accent("gold"));
        assert_eq!(accent(), Color::BrightYellow);
    }

    #[test]
    fn stellar_gradient_hits_its_endpoint_stops_exactly() {
        assert_eq!(stellar_color_at(0.0), STELLAR_STOPS[0]);
        assert_eq!(
            stellar_color_at(1.0),
            STELLAR_STOPS[STELLAR_STOPS.len() - 1]
        );
        // Out-of-range positions clamp instead of extrapolating.
        assert_eq!(stellar_color_at(-1.0), STELLAR_STOPS[0]);
        assert_eq!(
            stellar_color_at(2.0),
            STELLAR_STOPS[STELLAR_STOPS.len() - 1]
        );
    }
}
