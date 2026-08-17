//! The **plain surface** — the line-oriented renderer: streaming text,
//! tool-call cards, inline diffs, stage rules, cost tracking.
//!
//! This module was called `tui` until #2421, which was the one thing it is
//! not: there is no screen here, no layout and no input loop, just `colored`
//! and `println!` writing lines that scroll. The Command Deck in
//! [`stella_tui`] is the TUI. This is what runs when the deck cannot — and
//! that is not a rare fallback: `stella run` never opens the deck at all
//! (`main.rs`'s `Command::Run` arm), every supervised child writes here
//! because its stdout is a console *file* rather than a terminal, and
//! `stella daemon logs`/`attach` replay those bytes. For a great many runs
//! this surface is the only transcript that ever exists.
//!
//! The name follows the decision that selects it:
//! [`crate::term_policy::plain_fallback`], `--plain`, `STELLA_PLAIN`.
//!
//! `render_event` is this surface's one entry point onto `stella_core::Engine`'s
//! event stream (L-T1: it renders exclusively from `AgentEvent`s — no panel
//! owns state that isn't reconstructible by replaying the event log). It's
//! deliberately a thin dispatcher onto the per-kind print helpers below, not a
//! rewrite of them.
//!
//! # What is shared with the deck, and what is not
//!
//! Both surfaces fold the same event stream, so they always agree on *what
//! happened*; they disagree on paint, and the split is deliberate:
//!
//! - **Shared**, because two copies drift: the event→text wording
//!   ([`stella_tui::textline`], #66), the stage vocabulary
//!   ([`stella_tui::textline::stage_label`]), the markdown parse
//!   ([`stella_tui::markdown`]) and the diff layout ([`stella_tui::diff`]).
//! - **Not shared**, because the media genuinely differ: this surface writes
//!   into the user's own scrollback and paints no ground, so prose keeps the
//!   terminal's default foreground instead of the deck's explicit
//!   `theme::INK` — inheriting that would fight a light terminal profile.
//!   The deck can also expand a diff in place (ctrl+o); a scrollback line
//!   cannot be revisited, so this surface commits to one capped rendering.
//!
//! There is deliberately no animated "thinking" spinner: the Phase 0/1
//! version had one, but it only ticked a fixed 3 frames *before* dispatching
//! the network call, then froze for the entire real wait — a decorative
//! pre-roll, not a live indicator. A correct live spinner would need its own
//! concurrent task racing the event-draining task below, both writing to the
//! terminal — real interleaving risk for a cosmetic win. [`stage_rule`] gives
//! one clean, immediate line per stage instead (it replaced a lone dim
//! `thinking…` on `Execute` in #2421). There is no
//! decorative "rocket"/spinner animation either — activity is reported by the
//! command deck's honest run progress bar (`stella_tui::progress`), never by a
//! cosmetic character-noise loop.

use std::io::{self, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use colored::{Color, ColoredString, Colorize};
use stella_protocol::{AgentEvent, BudgetMode, FileChangeKind, StageKind};
use stella_tui::ansi::AnsiPalette;
use stella_tui::render::{INLINE_DIFF_CAP, THINKING_ROWS};
use stella_tui::textline::{self, EventLine, Tone, fmt_cost};

/// Width of the stage rules and the banner's closing rule.
///
/// A constant rather than the terminal's width: this surface's output is
/// routinely *not* going to a terminal at all (a supervised run's console
/// file, a pipe, CI), and a rule sized to whatever `stella` happened to see at
/// launch would make two runs of the same task diff against each other for no
/// reason. The banner already picked 60 for the same reason.
const RULE_WIDTH: usize = 60;

/// How this surface dresses the shared renderers' output.
///
/// The colour decision is `colored`'s, not ours — it already folds `NO_COLOR`,
/// `CLICOLOR_FORCE`, `TERM=dumb` (via [`crate::term_policy`]) and a non-tty
/// stream, and a second opinion here is how the two would drift apart.
///
/// [`stella_tui::theme::INK`] is transparent because this surface paints no
/// ground: the deck asserts an explicit white for prose over its own black
/// frame, and inheriting that would put our idea of "text" on a reader's light
/// terminal profile. The structure around the prose keeps its colours.
fn palette() -> AnsiPalette {
    if colored::control::SHOULD_COLORIZE.should_colorize() {
        AnsiPalette::colored().with_transparent_fg(stella_tui::theme::INK)
    } else {
        AnsiPalette::monochrome()
    }
}

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
///
/// `agent.rs` imports this **by name** rather than reaching for it through the
/// module path. That is not style: `plain::accent()` is two characters longer
/// than the `tui::accent()` it replaced in #2421, which was enough to make
/// rustfmt wrap a call site and push that god file two lines over its ceiling.
/// A rename must not cost a baseline bump.
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

/// Print a tool-call card: name, input summary, status.
pub fn tool_call_card(name: &str, input: &serde_json::Value, status: &str) {
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

/// Print the assistant's complete response (after streaming), rendered as
/// markdown.
///
/// Agent responses are markdown-capable and this surface used to print them
/// with a bare `println!`, so headings arrived as `### text`, emphasis as
/// literal asterisks and code blocks as rows of backticks. That is the
/// transcript a `stella run` leaves behind, and the *only* one a supervised
/// run leaves behind — the deck's readers were never the ones who needed this
/// most.
///
/// The parse is [`stella_tui::markdown`]'s, the same one the deck folds with:
/// a second markdown implementation here would be a second set of edge cases
/// (the `snake_case`-outranks-`_emphasis_` rule alone is a page of reasoning)
/// drifting out of step with the first.
pub fn assistant_response(text: &str) {
    if text.is_empty() {
        return;
    }
    println!();
    for line in stella_tui::markdown::render_ansi(text, &palette()) {
        println!("{line}");
    }
}

/// Print a cost summary for a turn. `AgentEvent::TurnComplete` (the source of
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
/// of the outcome, printed after the cost panel in text mode. No model call —
/// it reads the run's own facts: did it complete, and was it verified.
pub fn recap_panel(
    status: &stella_pipeline::PipelineStatus,
    verdict: Option<&stella_pipeline::Verdict>,
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
        // #2569: the recap's own question is "did it complete, was it
        // verified" — so the one status that answers "yes, and no" gets its
        // own line rather than borrowing the tick.
        PipelineStatus::Unverified { verdict } => {
            format!("? completed — UNVERIFIED: {}", verdict.summary)
        }
        PipelineStatus::VerificationFailed { verdict } => {
            format!("✗ verification failed — {}", verdict.summary)
        }
        PipelineStatus::Aborted { reason, .. } => format!("⚠ aborted — {reason}"),
    };

    println!("\n  {} Recap", "◆".dimmed());
    println!("    {headline}");
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

/// Print a stage divider — the plain surface's form of the deck's section
/// rule.
///
/// Until #2421 this surface printed one dim `thinking…` for `Execute` and
/// nothing whatever for the other eleven stages, so a staged `stella run` — the
/// default path — rendered as an undifferentiated wall of tool calls with no
/// sign that triage, planning, witness authoring, verification and the verdict
/// had each happened. The stage vocabulary is
/// [`stella_tui::textline::stage_label`], shared with the deck, so the two
/// surfaces cannot come to call the same stage different things (#1465 was
/// exactly that bug, five surfaces wide).
///
/// The label alone, without the word "stage": the divider already says what
/// kind of thing this is.
pub fn stage_rule(stage: StageKind) {
    println!("\n{}", stage_rule_line(stage));
}

/// [`stage_rule`]'s composition, kept pure so the layout is testable without
/// capturing stdout — the same split [`crate::term_policy`] uses for its
/// decisions, and for the same reason.
fn stage_rule_line(stage: StageKind) -> String {
    let label = textline::stage_label(stage);
    // "  " + "──" + " " + label + " " — what the trailing rule has to clear.
    let used = label.chars().count() + 5;
    let tail = RULE_WIDTH.saturating_sub(used);
    format!(
        "  {} {} {}",
        "──".dimmed(),
        label.bold(),
        "─".repeat(tail).dimmed()
    )
}

/// The chain of thought accumulated since the last non-`Reasoning` event.
///
/// `Reasoning` arrives as token-sized deltas, so it has to be coalesced before
/// it can be measured or previewed; the deck coalesces into a
/// `TranscriptEntry::Reasoning` it can re-render, and this surface cannot
/// re-render anything it has already written, so it buffers until the thought
/// is over and prints once.
///
/// A `Mutex<String>` rather than a channel or a field because `render_event`
/// is a free function called from a single event-draining task — the same
/// reason [`ACCENT`] is a static. Poisoning is recovered from rather than
/// propagated: a panic elsewhere must not turn the transcript off.
static REASONING: Mutex<String> = Mutex::new(String::new());

/// Buffer a reasoning delta (see [`REASONING`]).
fn reasoning_delta(delta: &str) {
    let mut buf = match REASONING.lock() {
        Ok(buf) => buf,
        Err(poisoned) => poisoned.into_inner(),
    };
    buf.push_str(delta);
}

/// Print the buffered thought, if any, and clear it.
///
/// Bounded to [`THINKING_ROWS`] — the deck's own preview depth, shared rather
/// than re-chosen — and dimmed. Reasoning was dropped entirely here before
/// #2421, with no comment saying why, which is the signature of an omission
/// rather than a policy: every other suppression in [`render_event`] states
/// its reason. Printing it *whole* is the other wrong answer, since a long
/// thought is the largest text a turn produces and this surface is often
/// writing to a log file.
fn flush_reasoning() {
    let mut buf = match REASONING.lock() {
        Ok(buf) => buf,
        Err(poisoned) => poisoned.into_inner(),
    };
    if buf.trim().is_empty() {
        buf.clear();
        return;
    }
    let text = std::mem::take(&mut *buf);
    drop(buf);
    for line in reasoning_block(&text) {
        println!("{line}");
    }
}

/// [`flush_reasoning`]'s composition, kept pure (see [`stage_rule_line`]).
///
/// Blank lines are dropped rather than previewed: in a window this small a
/// paragraph break costs a row of thought and says nothing — the same call the
/// deck makes.
fn reasoning_block(text: &str) -> Vec<String> {
    let rows: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let total = rows.len();
    let mut out = vec![format!(
        "  {} {}",
        "✻".dimmed(),
        format!("{total} lines").dimmed()
    )];
    for row in rows.iter().take(THINKING_ROWS) {
        out.push(format!("    {}", row.trim_end().dimmed().italic()));
    }
    if let Some(folded) = total.checked_sub(THINKING_ROWS).filter(|n| *n > 0) {
        out.push(format!("    {}", format!("⋯ {folded} more").dimmed()));
    }
    out
}

/// Print a file mutation and the diff it produced, capped.
///
/// This is the surface's answer to "what actually changed", and until #2421 it
/// had none: the row said `± write src/foo.rs` and stopped, while the diff rode
/// the very same event unread. The deck has shown a capped inline diff for some
/// time; the surface that a `stella run` and every supervised run actually
/// write to had never shown one.
///
/// Layout comes from [`stella_tui::diff`] — the one implementation of "how a
/// diff looks" — in its inline form, which drops the file header and the
/// counts footer because the row above already carries both.
///
/// [`INLINE_DIFF_CAP`] bounds it, and the withheld count is *printed*: a
/// truncation nobody announces reads as the whole change. Unlike the deck
/// there is no ctrl+o here — a scrollback line cannot be revisited — so the
/// cap is the final answer and has to say so.
pub fn file_change_card(
    path: &str,
    kind: FileChangeKind,
    added: u32,
    removed: u32,
    diff: Option<&str>,
) {
    for line in file_change_lines(path, kind, added, removed, diff) {
        println!("{line}");
    }
}

/// [`file_change_card`]'s composition, kept pure (see [`stage_rule_line`]).
fn file_change_lines(
    path: &str,
    kind: FileChangeKind,
    added: u32,
    removed: u32,
    diff: Option<&str>,
) -> Vec<String> {
    let line = textline::file_change(path, kind);
    // Counts ride the event from the emitter's own LCS measurement. They are
    // never recounted from the diff text: `changed_region_diff` is a bounded,
    // coarse rendering of the changed span and disagrees with the real delta
    // by any shared line.
    let counts = if added == 0 && removed == 0 {
        String::new()
    } else {
        format!(" {}", format!("+{added} −{removed}").dimmed())
    };
    let mut out = vec![format!("  {}{}", styled_event_line(&line), counts)];

    let Some(diff) = diff.filter(|d| !d.trim().is_empty()) else {
        return out;
    };
    let (body, hidden) =
        stella_tui::diff::body_lines_inline_ansi(diff, Some(path), INLINE_DIFF_CAP, &palette());
    out.extend(body.into_iter().map(|row| format!("    {row}")));
    if hidden > 0 {
        out.push(format!(
            "    {}",
            format!("⋯ {hidden} more line{}", if hidden == 1 { "" } else { "s" }).dimmed()
        ));
    }
    out
}

/// Render one `AgentEvent` from `stella_core::Engine::run_turn`'s stream.
///
/// `ToolStart`/`ToolResult` are intentionally a no-op here: `ToolResult`
/// doesn't carry the tool's name (only `call_id`), so the call site keeps a
/// small `call_id -> name` map and calls `tool_call_card`/`tool_result_card`
/// directly instead of routing those two through this function — see
/// `agent.rs`'s event-draining task. Every other event kind (including
/// `Text` — the engine emits one per step, not just at turn-end, since a
/// step with tool calls can still carry commentary text) is rendered here.
///
/// **Everything shared is shared, and the rest is policy.** Wording comes from
/// `stella_tui::textline` (the one event→text table both surfaces consume,
/// #66), stage names from its `stage_label`, prose from
/// `stella_tui::markdown`, diffs from `stella_tui::diff` (#2421). What the
/// arms below own is only this surface's *policy* — what to suppress, what
/// goes to stderr, how deep to preview — and its styling. A new annotation
/// variant needs a `textline` entry, nothing here.
///
/// Every suppression states its reason. An arm that silently drops an event
/// is how `Reasoning` went unrendered for as long as it did.
pub fn render_event(event: &AgentEvent) {
    // A thought ends the instant anything else lands — the same positional
    // rule the deck uses (`render::entry::reasoning_is_live`) rather than a
    // liveness flag or a timer. Doing it here, once, is what keeps every arm
    // below from having to remember it.
    if !matches!(event, AgentEvent::Reasoning { .. }) {
        flush_reasoning();
    }
    match event {
        AgentEvent::Stage { name, .. } => stage_rule(*name),
        AgentEvent::ToolStart { .. } | AgentEvent::ToolResult { .. } => {
            // Handled inline at the call site, which holds the `call_id ->
            // name` correlation this event pair needs (see the module doc).
        }
        AgentEvent::Text { text } => assistant_response(text),
        AgentEvent::FileChange {
            path,
            kind,
            added,
            removed,
            diff,
        } => file_change_card(path, *kind, *added, *removed, diff.as_deref()),
        AgentEvent::Reasoning { delta } => reasoning_delta(delta),
        AgentEvent::BudgetTick {
            mode: BudgetMode::Off,
            ..
        } => {
            // Unmetered sessions stay quiet about spend.
        }
        AgentEvent::TurnComplete { .. } => {
            // The call site prints `cost_summary` from `TurnOutcome`
            // directly (it has the same model/cost_usd this event carries,
            // plus wall-clock elapsed time this event doesn't).
        }
        AgentEvent::AskUser {
            question, options, ..
        } => {
            // The approvals plane's question io prints the numbered options
            // and collects the answer itself (it owns stdin while the turn
            // is in flight); this render arm exists for replay/stream-json
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

    // ── #2421: what the plain surface stopped throwing away ────────────────
    //
    // Four witnesses, one per gap. Each asserts on the *visible* text, so a
    // restyling does not break them but a silent drop does. On `main` before
    // #2421 every one of them fails, and fails for the right reason: the
    // renderer it exercises did not exist.

    /// Prose is markdown, and this surface used to print it raw.
    #[test]
    fn assistant_prose_is_rendered_not_printed_with_its_delimiters() {
        let out = stella_tui::markdown::render_ansi(
            "## Heading\n\nSome **bold** and `code`.\n\n- one\n- two\n",
            &AnsiPalette::colored().with_transparent_fg(stella_tui::theme::INK),
        );
        let visible: String = out
            .iter()
            .map(|l| strip_ansi(l).into_owned())
            .collect::<Vec<_>>()
            .join("\n");

        // The delimiters are consumed, not shown.
        assert!(
            !visible.contains("**") && !visible.contains('`') && !visible.contains("## "),
            "markdown delimiters survived into the output:\n{visible}"
        );
        // …and the content they wrapped is still there.
        for word in ["Heading", "bold", "code", "one", "two"] {
            assert!(visible.contains(word), "lost {word:?} from:\n{visible}");
        }
        // A bullet became a glyph rather than vanishing.
        assert!(visible.contains('•'), "no bullet glyph in:\n{visible}");
    }

    /// Prose must NOT be given a foreground: this surface paints no ground, so
    /// the deck's explicit `theme::INK` would fight a light terminal profile.
    /// The structure around it still gets colour — that is the whole point of
    /// the transparency, and a palette that simply dropped colour would pass a
    /// weaker version of this test while losing the headings.
    #[test]
    fn prose_keeps_the_readers_foreground_while_structure_keeps_colour() {
        let palette = AnsiPalette::colored().with_transparent_fg(stella_tui::theme::INK);
        let plain = stella_tui::markdown::render_ansi("just prose", &palette);
        assert_eq!(plain, vec!["just prose".to_string()], "prose was tinted");

        let heading = stella_tui::markdown::render_ansi("# Title", &palette);
        assert!(
            heading[0].contains('\u{1b}'),
            "the heading lost its styling too: {:?}",
            heading[0]
        );
    }

    /// Every stage draws a rule. Before #2421 only `Execute` printed anything
    /// at all, and what it printed was the word "thinking…".
    #[test]
    fn every_stage_draws_a_rule_naming_itself() {
        for (stage, label) in [
            (StageKind::Triage, "triage"),
            (StageKind::Plan, "plan"),
            (StageKind::Witness, "witness"),
            (StageKind::Execute, "execute"),
            (StageKind::Verdict, "verdict"),
        ] {
            let line = strip_ansi(&stage_rule_line(stage)).into_owned();
            assert!(line.contains(label), "stage rule lost its label: {line:?}");
            assert!(line.contains('─'), "stage rule drew no rule: {line:?}");
            // The vocabulary is `textline`'s, not a local copy — #1465 was
            // five surfaces disagreeing about one stage's name.
            assert!(
                line.contains(textline::stage_label(stage)),
                "stage rule stopped using the shared label: {line:?}"
            );
        }
    }

    /// A chain of thought is previewed and *counted*, never dropped and never
    /// dumped whole.
    #[test]
    fn reasoning_is_previewed_bounded_and_says_how_much_it_withheld() {
        let thought: String = (1..=12).map(|n| format!("step {n}\n")).collect();
        let block = reasoning_block(&thought);
        let visible: Vec<String> = block.iter().map(|l| strip_ansi(l).into_owned()).collect();

        assert!(
            visible[0].contains("12 lines"),
            "no total in the header: {:?}",
            visible[0]
        );
        // Header + the preview + the fold marker, and nothing beyond.
        assert_eq!(visible.len(), THINKING_ROWS + 2, "preview is unbounded");
        assert!(visible[1].contains("step 1"));
        assert!(
            visible
                .last()
                .unwrap()
                .contains(&format!("{} more", 12 - THINKING_ROWS)),
            "withheld count not stated: {:?}",
            visible.last()
        );
        // The 12th step must not have reached the terminal.
        assert!(
            !visible.iter().any(|l| l.contains("step 12")),
            "the whole thought was dumped"
        );
    }

    /// Blank lines cost a row of thought and say nothing, so they are dropped
    /// before the budget is spent — the same call the deck makes.
    #[test]
    fn reasoning_spends_its_budget_on_content_not_paragraph_breaks() {
        let spaced = "a\n\n\nb\n\n\nc\n";
        let visible: Vec<String> = reasoning_block(spaced)
            .iter()
            .map(|l| strip_ansi(l).into_owned())
            .collect();
        assert!(visible[0].contains("3 lines"), "blanks were counted");
        assert_eq!(visible.len(), 4, "blanks were previewed: {visible:?}");
    }

    /// The headline gap: a mutation prints the diff that rode its own event.
    #[test]
    fn a_file_change_prints_its_diff_and_its_measured_counts() {
        let diff = "@@ -1,3 +1,3 @@\n fn main() {\n-    let x = 1;\n+    let x = 2;\n }\n";
        let visible: Vec<String> =
            file_change_lines("src/main.rs", FileChangeKind::Modified, 1, 1, Some(diff))
                .iter()
                .map(|l| strip_ansi(l).into_owned())
                .collect();
        let all = visible.join("\n");

        assert!(all.contains("src/main.rs"), "path missing:\n{all}");
        // Counts come off the event, not off the diff text.
        assert!(all.contains("+1 −1"), "measured counts missing:\n{all}");
        // The actual change, both sides of it.
        assert!(all.contains("let x = 1;"), "removed line missing:\n{all}");
        assert!(all.contains("let x = 2;"), "added line missing:\n{all}");
        assert!(
            visible.len() > 1,
            "the row printed but the diff did not:\n{all}"
        );
    }

    /// A big diff is capped and *says* it was capped. An unannounced
    /// truncation reads as the whole change, and unlike the deck there is no
    /// ctrl+o here to prove otherwise.
    #[test]
    fn a_large_diff_is_capped_and_names_the_lines_it_withheld() {
        let mut diff = String::from("@@ -1,80 +1,80 @@\n");
        for n in 0..80 {
            diff.push_str(&format!("-old {n}\n+new {n}\n"));
        }
        let visible: Vec<String> =
            file_change_lines("big.rs", FileChangeKind::Modified, 80, 80, Some(&diff))
                .iter()
                .map(|l| strip_ansi(l).into_owned())
                .collect();

        // One header row, at most the shared cap of diff rows, one fold note.
        assert!(
            visible.len() <= INLINE_DIFF_CAP + 2,
            "diff flooded the transcript: {} lines",
            visible.len()
        );
        assert!(
            visible.last().unwrap().contains("more line"),
            "truncated silently: {:?}",
            visible.last()
        );
    }

    /// A read is not a change: no counts, no diff, just the row.
    #[test]
    fn a_read_prints_one_row_with_no_diff_and_no_counts() {
        let visible: Vec<String> =
            file_change_lines("src/lib.rs", FileChangeKind::Read, 0, 0, None)
                .iter()
                .map(|l| strip_ansi(l).into_owned())
                .collect();
        assert_eq!(visible.len(), 1, "a read grew a body: {visible:?}");
        assert!(!visible[0].contains('+'), "a read reported a delta");
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
