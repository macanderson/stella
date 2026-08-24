//! Emit the command deck as a **frame film** — a deterministic, 60 fps stream
//! of styled character grids plus a camera track — for rendering a hero video.
//!
//! ```sh
//! cargo run -p stella-tui --example deck_film > film.jsonl
//! scripts/render-deck-film.py film.jsonl -o docs/demo/stella-deck.mp4
//! ```
//!
//! ## Why a film and not a screen recording
//!
//! A screen recording of the deck is a recording of *a terminal*: its font, its
//! colour handling, its window size, its scrollback, and whatever the compositor
//! did to the pixels. None of that is Stella, and none of it is reproducible —
//! re-shooting the same take next month gives a different video. This example
//! goes the other way and records the only thing that is actually ours: the
//! output of [`render_deck`], which AGENTS.md already relies on being a pure
//! function of `(model, ui)`. The same property that makes the golden-frame
//! suite possible (`tests/deck_render_snapshots.rs`) makes a byte-identical
//! re-render possible, so the video is a build artifact rather than a take.
//!
//! What this costs, stated plainly: the film shows the deck's **real render
//! over the scripted [`scenario`] session**, not a live agent run against a
//! provider. Every glyph is the deck's own — the layouts, the palette, the
//! panel geometry, the diff rendering — and the session driving them is the
//! same fixture the render tests assert against. It is a UI film, and calling
//! it a recording of an agent solving a task would be a lie. A real-run film
//! is #3556; it needs a provider key and a captured event log, and it reuses
//! every stage below except the fixture.
//!
//! ## The wire format
//!
//! One JSON object per line. The first is a header; the rest are frames, one
//! per output frame at [`FPS`], in order:
//!
//! ```text
//! {"kind":"header","fps":60,"cols":114,"rows":29,"frames":2982}
//! {"kind":"frame","i":0,"cam":[1.42,0.5,0.46],"fade":0.0,"grid":7,"rows":[[…]]}
//! {"kind":"frame","i":1,"cam":[1.41,0.5,0.46],"fade":0.05,"grid":7}
//! ```
//!
//! `grid` is a content id: a frame repeats the previous grid by citing its id
//! and omitting `rows`, which is what keeps a 50-second film in single-digit
//! megabytes rather than tens of gigabytes. The renderer caches by that id.
//! A row is a list of style runs, `[fg, bg, mods, text]`, because a terminal row
//! is mostly long stretches of one style and per-cell encoding is ~30x larger
//! for no gain. Colours are `"RRGGBB"` or `null` (the terminal default); `mods`
//! is the bitmask in [`mod_bits`].
//!
//! `cam` is `[zoom, cx, cy]`: at zoom 1.0 the grid spans the frame's width,
//! above it the view narrows, centred on `(cx, cy)` in normalised grid
//! coordinates. The renderer resolves that against a supersampled raster rather
//! than by scaling a finished frame — see its header for why that is the whole
//! point of the effect.
//!
//! ## Determinism
//!
//! Same rules as the golden suite, for the same reason: a fixed pid, a fixed
//! `now_ms`, and explicit resource samples. `--verify` renders the film twice
//! and compares byte for byte, which is the check that keeps this honest as the
//! deck changes underneath it.
//!
//! One clock read survives and is declared rather than hidden: the launch mark
//! animates off `SplashState::elapsed()`, which `splash.rs` floors onto a 40 ms
//! grid. Backdating a fresh state per frame and rendering it immediately lands
//! well inside a bucket, so the splash shot is deterministic in practice — and
//! is the one part of the film that is not deterministic *by construction*.

use std::collections::HashMap;
use std::fmt::Write as _;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};

use stella_tui::scenario::{demo_engine_config, demo_graph, demo_inbound};
use stella_tui::v2::engine_panel::EngineTab;
use stella_tui::{
    AgentScope, AgentVersionInfo, DeckTab, DeckUi, EngineRole, InstalledAgentEntry, IssueRow,
    McpServerInfo, ResourceSample, SettingsPane, SkillRow, SkillScope, SkillsView, ToolDenial,
    ToolPolicyState, ToolRow, ToolScope, WorkspaceModel, render_deck,
};

// ───────────────────────────── film geometry ─────────────────────────────

/// Output frame rate. Sixty, because the camera moves: at 30 a slow push-in
/// judders visibly on text, which is the one thing this film is made of.
const FPS: u32 = 60;

/// The recorded grid. Both numbers are required, and both are solved
/// rather than chosen.
///
/// A monospace face pins two ratios, and both are measurements: JetBrains Mono
/// advances **0.600 em** per column and stands **1.32 em** per line box
/// (ascent + descent — not the 1.0 em an em-square suggests). A terminal cell
/// must be at least as tall as that line box or consecutive rows draw into each
/// other, so the cell's shape is not free: `cell_h >= 2.2 x cell_w`. Put that
/// against a 1920x1080 frame the deck should fill and the grid's proportions
/// fall out of it:
///
/// ```text
/// 16/9 = (cols x 0.600 S) / (rows x k S)   ->   cols/rows = 2.963 k
/// ```
///
/// where `k` is the leading in ems — so a grid that fills the frame is one with
/// roughly **four columns per row**. `scripts/render-deck-film.py` solves the
/// rest: the row count fixes the cell height, the largest size whose line box
/// fits fixes the font, and its advance fixes the cell width.
///
/// **114x29** is that solution at the width the deck itself needs (below), and
/// lands within 0.14% of 16:9 — cell 42x93 px at the renderer's 2.5x
/// supersample, font size 70, line box 92 px inside a 93 px cell, advance
/// exactly 42 px. The residual is about a pixel and a half of ground at the top
/// and bottom, in the deck's own ink.
///
/// **Why not narrower.** Narrower is bigger type, and 100x25 is an exact 0.00%
/// fit — but the deck's composer footer does not budget its `esc / > steer this
/// turn` hint against the right-aligned counter, so below ~108 columns the two
/// abut and the row reads `steer thturn $0.0390`. That is a real defect
/// (#3591), not a film problem, and the film must not ship a frame that
/// displays it. 114 clears it with room.
///
/// **Why not wider.** 160 was the first cut's answer and it is what produced
/// the video this replaced: a 3.2:1 strip stranded in a 16:9 frame, the deck
/// squeezed into a band with the rest of the frame empty. At 114 the AGENTS
/// dashboard still renders its compact column set, which is what a viewer on a
/// normal terminal sees anyway.
const COLS: u16 = 114;
const ROWS: u16 = 29;

/// `--extents` only: try a different grid without rebuilding. The film itself
/// always uses [`COLS`]x[`ROWS`] — a grid is a geometry decision with an aspect
/// consequence (see [`ROWS`]), not a runtime knob.
fn probe_rows() -> u16 {
    std::env::var("DECK_FILM_ROWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(ROWS)
}

/// `--extents` only, as [`probe_rows`] — the width half.
fn probe_cols() -> u16 {
    std::env::var("DECK_FILM_COLS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(COLS)
}

/// The fixed pid every agent is stamped with. Never `std::process::id()`: the
/// AGENTS dashboard's CPU%/MEM columns sample whatever pid they are given, and
/// a live one makes the film differ per run.
const FIXTURE_PID: u32 = 424_242;

/// Fade-to-black at each end, in seconds. The film is published on a `loop`
/// attribute, so the last frame cuts straight to the first; landing both on
/// black is what turns that cut into a beat instead of a jump.
const FADE: f64 = 0.45;

// ───────────────────────────── the camera ─────────────────────────────

/// One camera keyframe: at `at` (seconds into the shot), the grid is drawn at
/// `zoom` times fit-to-frame, centred on `(cx, cy)` in normalised grid
/// coordinates.
///
/// Zoom 1.0 is the whole deck filling the frame, because the grid is exactly
/// 16:9 (see [`COLS`]). Above it the view narrows on **both** axes, so `cx` and
/// `cy` both bite — which is why the tracks below pick a corner with
/// [`left`]/[`top`] rather than drifting around the middle, and why no shot
/// here needs the big 1.9x pushes the letterboxed cut used to need to fill the
/// frame.
#[derive(Clone, Copy, Debug)]
struct Key {
    at: f64,
    zoom: f64,
    cx: f64,
    cy: f64,
}

const fn key(at: f64, zoom: f64, cx: f64, cy: f64) -> Key {
    Key { at, zoom, cx, cy }
}

/// The camera centre that puts the grid's **left** edge on the frame's left
/// edge at `zoom`.
///
/// The deck is left- and top-anchored: tab bar, table headers, tree gutters and
/// every list start at column zero. A push-in centred anywhere else slices the
/// first glyph of every row it crosses, which reads as a broken render rather
/// than as a camera move — so a shot that wants to be *in* on left-anchored
/// content asks for `left(zoom)` rather than guessing a centre.
fn left(zoom: f64) -> f64 {
    0.5 / zoom
}

/// The camera centre that puts the grid's **top** edge on the frame's top edge
/// at `zoom` — the vertical half of [`left`].
///
/// This is what a push-in on a sparse tab needs. The deck draws its chrome and
/// its lists from row 0 down and its status block along the bottom; the rows in
/// between are what the scripted fixture leaves empty. A centred push frames
/// exactly those, so a shot that goes close goes close on the *top*.
fn top(zoom: f64) -> f64 {
    0.5 / zoom
}

/// Cubic ease-in-out over `[0, 1]`.
///
/// Linear interpolation is what makes a zoom look like a slider being dragged;
/// easing both ends is what makes it look like a camera. Cubic rather than
/// quadratic because the deck's text is high-frequency detail — the gentler
/// start hides the first few frames of resampling.
fn ease(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let u = -2.0 * t + 2.0;
        1.0 - u * u * u / 2.0
    }
}

/// The camera at `t` seconds into a shot, interpolating between its keyframes.
///
/// Before the first key and after the last, the camera holds that key — a shot
/// may specify a single key and simply sit still.
fn camera_at(keys: &[Key], t: f64) -> (f64, f64, f64) {
    let Some(first) = keys.first() else {
        return (1.0, 0.5, 0.5);
    };
    if t <= first.at {
        return (first.zoom, first.cx, first.cy);
    }
    for pair in keys.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if t < b.at {
            let span = (b.at - a.at).max(f64::EPSILON);
            let f = ease((t - a.at) / span);
            return (
                a.zoom + (b.zoom - a.zoom) * f,
                a.cx + (b.cx - a.cx) * f,
                a.cy + (b.cy - a.cy) * f,
            );
        }
    }
    let last = keys[keys.len() - 1];
    (last.zoom, last.cx, last.cy)
}

// ───────────────────────────── the shot list ─────────────────────────────

/// What the deck is showing during a shot. Applied to a fresh `DeckUi` every
/// frame, so a shot never inherits the previous one's cursor state.
#[derive(Clone, Copy, Debug)]
enum Scene {
    /// The launch mark, at `elapsed_ms` into its assemble timeline.
    Splash,
    /// A deck tab. `sel` drives whichever cursor that tab owns.
    Tab(DeckTab),
    /// The SESSION tab with lane `focused` selected — `0` is the lead, whose
    /// view carries the sub-agent band; a sub-agent's own transcript fills the
    /// tab instead, which is what `⏎ focus` on a lane does.
    SessionLane(usize),
    /// The FILES tab with the diff pane open under row `sel` — the PR-style
    /// view `⏎` toggles on a row.
    FilesDiff,
    /// The SETTINGS tab's TOOLS pane — the session's tool surface.
    SettingsTools,
    /// The SETTINGS tab's AGENTS pane: the `agent_engine_config` editor, on
    /// `EngineTab` and row `sel`.
    EngineConfig(EngineTab),
}

/// One shot: a scene, a duration, a camera track, and how much of the scripted
/// session has streamed in by the end of it.
struct Shot {
    /// Shown in `--shots`; not rendered into the film.
    label: &'static str,
    scene: Scene,
    secs: f64,
    keys: Vec<Key>,
    /// Cursor row within the scene's list, as `(start, end)` — stepped over the
    /// shot so a list visibly reads itself rather than sitting frozen.
    sel: (usize, usize),
    /// How many of the scripted session's events have folded in by the shot's
    /// start and end. Ramping this is what makes a SESSION shot *stream*
    /// instead of cut; a count rather than a share because the story beats
    /// are specific events — a shot that should end on the edit's inline diff
    /// has to end on exactly the `ToolResult` that claims it, not on "about
    /// half". [`EVENTS`] names the indices the shot list uses.
    session: (usize, usize),
}

fn shot(
    label: &'static str,
    scene: Scene,
    secs: f64,
    keys: Vec<Key>,
    sel: (usize, usize),
    session: (usize, usize),
) -> Shot {
    Shot {
        label,
        scene,
        secs,
        keys,
        sel,
        session,
    }
}

/// Indices into the scripted session, in play order — the beats the shot
/// list is cut on. Each is the count of events folded in *after* the named
/// one, so `session: (EVENTS.lead_planned, EVENTS.edit_done)` streams from
/// the lead's plan to the edit's result inclusive.
///
/// Pinned against the fixture by `film()`: a fixture edit that moves an event
/// fails the run by name rather than quietly cutting a shot on the wrong beat.
struct Events {
    /// Lead registered, context recalled, plan stated, task board posted.
    lead_planned: usize,
    /// Both sub-agents registered and running.
    subs_dispatched: usize,
    /// The lead's `edit_file` result, with the `FileChange` inside its window
    /// — the event the transcript's inline diff is claimed on.
    edit_done: usize,
    /// The board moved: the triggers task done, the workflows task active.
    board_moved: usize,
    /// `sub:auth` asked its question, having searched and written the route.
    auth_asked: usize,
    /// Everything: `sub:ci` verified, committed and opened the PR, and the
    /// lead proposed the larger refactor that now waits at the gate.
    all: usize,
}

const EVENTS: Events = Events {
    lead_planned: 8,
    subs_dispatched: 12,
    edit_done: 18,
    board_moved: 21,
    auth_asked: 31,
    all: 37,
};

/// Wide: the whole deck in frame.
const WIDE: Key = key(0.0, 1.0, 0.5, 0.5);

/// A wide key at `at`.
const fn wide(at: f64) -> Key {
    key(at, 1.0, 0.5, 0.5)
}

/// A push-in key at `at`, anchored to the deck's top-left corner.
fn close(at: f64, zoom: f64) -> Key {
    key(at, zoom, left(zoom), top(zoom))
}

/// The film.
///
/// Read it as a story in four acts rather than as a tab tour, because that is
/// what a narrator has to say over it (`docs/demo/stella-deck-script.md`
/// carries the words, cut to these timestamps):
///
/// 1. **A turn happens.** The lead boots, recalls, plans, fans out, reads and
///    edits — and the edit lands in the transcript as a real diff.
/// 2. **The fan-out is a set of sessions.** One keypress puts a sub-agent's own
///    transcript in the tab: its search, the file it created, the question it
///    is waiting on.
/// 3. **The deck is how you read the run.** FILES with the PR-style pane,
///    GRAPH, TRACES, the installed AGENTS, SKILLS, MCP, ISSUES.
/// 4. **And how you steer it.** SETTINGS — the agent editor, the tool surface —
///    then back to the session, where the PR is open and a bigger change is
///    waiting for a human at the gate.
///
/// The camera has one rule: a tab opens **wide**, holds long enough to read,
/// and makes one slow push onto the thing the narration is about. A cut always
/// lands on wide, so no tab change happens mid-move — the cut the previous
/// film made at 1.45x into a different tab is exactly the "zoomed in at
/// random" a viewer remembers. Every push is anchored top-left ([`close`]):
/// the deck is left- and top-anchored, and a centred push slices the first
/// glyph of every row it crosses.
#[rustfmt::skip]
fn shots() -> Vec<Shot> {
    let e = EVENTS;
    vec![
        // ── act 1 · a turn happens ──────────────────────────────────────
        // Cold open: the mark resolves into the deck. 1.7s is the mark's own
        // screen time (ASSEMBLE + UNHELD_HOLD, 1.66s) rounded up by a frame.
        shot("splash", Scene::Splash, 1.7,
             vec![key(0.0, 1.30, 0.5, 0.5), wide(1.7)],
             (0, 0), (0, 0)),

        // The lead boots and plans; the two lanes appear. Wide and still: the
        // transcript is already moving, and it is the one thing on screen
        // that should be.
        shot("session · the lead boots and plans", Scene::SessionLane(0), 3.4,
             vec![WIDE], (0, 0), (0, e.lead_planned)),
        shot("session · the fan-out", Scene::SessionLane(0), 2.6,
             vec![WIDE], (0, 0), (e.lead_planned, e.subs_dispatched)),
        // The read, then the edit. Still wide — the diff arrives at the
        // bottom of the transcript and the eye should be allowed to find it.
        shot("session · the edit lands", Scene::SessionLane(0), 3.5,
             vec![WIDE], (0, 0), (e.subs_dispatched, e.edit_done)),
        // And in on it: the inline diff, GitHub-style, under the call that
        // made it. The push centres on the transcript rows below the band.
        shot("session · the diff, inline", Scene::SessionLane(0), 4.5,
             vec![wide(0.0), wide(0.6), key(4.5, 1.45, left(1.45), 0.62)],
             (0, 0), (e.edit_done, e.edit_done)),

        // ── act 2 · the fan-out is a set of sessions ────────────────────
        // `⏎ focus` on the auth lane: its own transcript fills the tab. It
        // searches, writes the route (a created-file diff), then asks.
        shot("session · a lane's own view", Scene::SessionLane(1), 6.0,
             vec![wide(0.0), wide(1.2), close(6.0, 1.35)],
             (0, 0), (e.board_moved, e.auth_asked)),

        // ── act 3 · the deck is how you read the run ────────────────────
        // FILES: the ledger, two rows, who touched what.
        shot("files · the ledger", Scene::Tab(DeckTab::Files), 2.6,
             vec![WIDE], (0, 0), (e.all, e.all)),
        // `⏎` on a row opens the PR-style pane; the cursor moves to the
        // second file halfway through so both diffs are seen.
        shot("files · the diff pane", Scene::FilesDiff, 5.5,
             vec![wide(0.0), wide(0.8), close(5.5, 1.30)],
             (0, 1), (e.all, e.all)),

        // GRAPH: the surface a screenshot explains worst. Wide, in on the
        // node list, then across to the relations panel beside it.
        shot("graph · the neighborhood", Scene::Tab(DeckTab::Graph), 6.0,
             vec![wide(0.0), wide(1.4), close(3.4, 1.40),
                  key(6.0, 1.40, left(1.40) + 0.28, top(1.40))],
             (0, 4), (e.all, e.all)),

        // TRACES: every event of the run, one line each, in order.
        shot("traces · the event log", Scene::Tab(DeckTab::Traces), 3.6,
             vec![wide(0.0), wide(0.8), close(3.6, 1.30)],
             (0, 0), (e.all, e.all)),

        // AGENTS: the definitions installed on disk, the ones a lead can
        // assume and a sub-agent can be dispatched as.
        shot("agents · installed definitions", Scene::Tab(DeckTab::Agents), 4.2,
             vec![wide(0.0), wide(1.0), close(4.2, 1.35)],
             (0, 2), (e.all, e.all)),

        shot("skills", Scene::Tab(DeckTab::Skills), 3.6,
             vec![wide(0.0), wide(0.8), close(3.6, 1.35)],
             (0, 3), (e.all, e.all)),

        shot("mcp · connected servers", Scene::Tab(DeckTab::Mcp), 3.0,
             vec![wide(0.0), wide(0.6), close(3.0, 1.30)],
             (0, 3), (e.all, e.all)),

        shot("issues", Scene::Tab(DeckTab::Issues), 3.0,
             vec![wide(0.0), wide(0.6), close(3.0, 1.30)],
             (0, 2), (e.all, e.all)),

        // ── act 4 · and how you steer it ────────────────────────────────
        // The agent config: global routing, then the agent's own tab, held
        // close — these rows are the point.
        shot("settings · agents · global", Scene::EngineConfig(EngineTab::Global), 3.2,
             vec![wide(0.0), wide(0.6), close(3.2, 1.40)],
             (0, 2), (e.all, e.all)),
        shot("settings · agents · default", Scene::EngineConfig(EngineTab::Agent(EngineRole::Default)), 3.4,
             vec![close(0.0, 1.40), close(3.4, 1.45)],
             (0, 3), (e.all, e.all)),
        // The tool surface, and out.
        shot("settings · tools", Scene::SettingsTools, 3.4,
             vec![wide(0.0), wide(0.6), close(2.2, 1.30), wide(3.4)],
             (0, 3), (e.all, e.all)),

        // Back where it started: the PR is open, the board has moved, and
        // the lead's next, larger change is waiting at the gate for a human.
        // Wide and still, like the opening — the film is a loop.
        shot("session · shipped, and waiting at the gate", Scene::SessionLane(0), 5.5,
             vec![WIDE], (0, 0), (e.all, e.all)),
    ]
}

// ───────────────────────────── the fixtures ─────────────────────────────

/// The scripted session, folded up to its first `take` events.
///
/// Folding a prefix is what animates the transcript: the model is rebuilt from
/// scratch each frame rather than mutated forward, which costs a few hundred
/// microseconds and buys the guarantee that frame *n* depends on nothing but
/// *n* — the property `--verify` checks.
fn model_at(take: usize) -> WorkspaceModel {
    let inbound = demo_inbound(0, FIXTURE_PID);

    let mut model = WorkspaceModel::new();
    model.now_ms = 312_000;
    for item in inbound.iter().take(take) {
        model.apply_inbound(item);
    }

    // The one thing no event folds: the out-of-band CPU/MEM sample. Written
    // explicitly, or the dashboard reports this machine and the film stops
    // being reproducible.
    let samples = [
        ResourceSample {
            cpu_pct: 42.0,
            mem_bytes: 148 * 1024 * 1024,
        },
        ResourceSample {
            cpu_pct: 7.5,
            mem_bytes: 61 * 1024 * 1024,
        },
        ResourceSample {
            cpu_pct: 0.0,
            mem_bytes: 12 * 1024 * 1024,
        },
    ];
    for (entry, res) in model.agents.iter_mut().zip(samples) {
        entry.res = res;
    }
    model.global_cpu_pct = 49.5;
    model
}

fn fixture_skills() -> SkillsView {
    let skill =
        |scope, name: &str, description: &str, origin: &str, enabled, version, latest| SkillRow {
            scope,
            name: name.to_string(),
            description: description.to_string(),
            body: format!("# {name}\n\n{description}\n"),
            origin: origin.to_string(),
            enabled,
            version,
            latest,
            removable: origin != "workspace",
        };
    SkillsView {
        rows: vec![
            skill(
                SkillScope::Project,
                "release-checklist",
                "Cut a release: version bump, changelog, tag, artifacts.",
                "workspace",
                true,
                3,
                3,
            ),
            skill(
                SkillScope::User,
                "rust-review",
                "Review Rust diffs for ownership, error handling, and API shape.",
                "user",
                true,
                2,
                4,
            ),
            skill(
                SkillScope::User,
                "sql-tuning",
                "Read a query plan and propose indexes.",
                "installed",
                false,
                1,
                1,
            ),
            skill(
                SkillScope::Project,
                "bench-rig-access",
                "Reach the benchmark rig and read a run's reward.",
                "auto",
                true,
                1,
                1,
            ),
        ],
        status: Some("4 skills · 3 enabled".to_string()),
        busy: false,
        created: None,
    }
}

/// The definitions the AGENTS tab lists: what is installed on disk at the
/// project and user levels. A project agent with a scoped toolbelt, a user
/// agent with none (the pane says "all tools" honestly), and a versioned one
/// whose picker has something to pick.
fn fixture_installed_agents() -> Vec<InstalledAgentEntry> {
    let version = |n: u32, label: &str| AgentVersionInfo {
        version: n,
        label: label.to_string(),
    };
    let entry = |name: &str,
                 description: &str,
                 tools: Option<&[&str]>,
                 scope: AgentScope,
                 versions: Vec<AgentVersionInfo>| {
        let dir = match scope {
            AgentScope::Project => ".stella/agents",
            AgentScope::User => "~/.stella/agents",
        };
        let pinned = versions.last().map_or(1, |v| v.version);
        InstalledAgentEntry {
            name: name.to_string(),
            description: description.to_string(),
            tools: tools.map(|t| t.iter().map(|s| s.to_string()).collect()),
            scope,
            source_path: format!("{dir}/{name}.md"),
            version: pinned,
            versions,
            content: format!("---\nname: {name}\ndescription: {description}\n---\n"),
        }
    };
    vec![
        entry(
            "reviewer",
            "Reviews a diff for API shape and errors.",
            Some(&["read_file", "search", "bash"]),
            AgentScope::Project,
            vec![
                version(1, "2026-07-02"),
                version(2, "2026-07-19"),
                version(3, "2026-08-14"),
            ],
        ),
        entry(
            "migrator",
            "Ports a module, behaviour pinned by tests.",
            Some(&["read_file", "write_file", "edit_file", "search", "bash"]),
            AgentScope::Project,
            vec![version(1, "2026-08-01")],
        ),
        entry(
            "release-captain",
            "Cuts a release: bump, changelog, tag, ship.",
            None,
            AgentScope::User,
            vec![version(1, "2026-06-11"), version(2, "2026-08-09")],
        ),
    ]
}

fn fixture_issues() -> Vec<IssueRow> {
    let issue =
        |key: &str, title: &str, state: &str, labels: &[&str], assignee: Option<&str>| IssueRow {
            key: key.to_string(),
            title: title.to_string(),
            state: state.to_string(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            assignee: assignee.map(|s| s.to_string()),
            url: format!("https://github.com/example/repo/issues/{}", &key[1..]),
            updated_at: Some("2026-01-14T09:30:00Z".to_string()),
        };
    vec![
        issue(
            "#874",
            "Command-deck render golden tests",
            "open",
            &["enhancement", "area:tui"],
            Some("dev"),
        ),
        issue(
            "#826",
            "run_deck event-loop pty harness",
            "open",
            &["enhancement"],
            None,
        ),
        issue(
            "#791",
            "Compaction drops the last tool result",
            "open",
            &["bug", "area:core"],
            Some("dev"),
        ),
    ]
}

fn fixture_tool_policy() -> ToolPolicyState {
    ToolPolicyState {
        tools: [
            ("get_environment", "environment"),
            ("get_state", "scratch"),
            ("save_state", "scratch"),
            ("delegate", "task"),
            ("task_assign", "task"),
            ("mcp__github__create_issue", "mcp"),
            ("deploy_to_staging", "custom"),
        ]
        .into_iter()
        .map(|(name, group)| ToolRow {
            name: name.to_string(),
            group: group.to_string(),
            locked: name == "task_assign",
            off: (name == "task_assign").then(|| ToolDenial {
                key: "task_assign".to_string(),
                scope: Some(ToolScope::Managed),
            }),
        })
        .collect(),
        switches: std::collections::BTreeMap::from([("task_assign".to_string(), false)]),
    }
}

fn fixture_mcp_servers() -> Vec<McpServerInfo> {
    vec![
        McpServerInfo {
            name: "stripe".into(),
            title: Some("Stripe".into()),
            description: Some("Payments, refunds, and balance reads.".into()),
            endpoint: "https://mcp.stripe.com/v1".into(),
            kind: "http".into(),
            enabled: true,
            connected: true,
            health: Some("live".into()),
            tool_count: 21,
            oauth: Some(true),
            calls: 14,
            ..Default::default()
        },
        McpServerInfo {
            name: "fs".into(),
            endpoint: "npx -y @modelcontextprotocol/server-filesystem /w".into(),
            kind: "stdio".into(),
            enabled: true,
            connected: true,
            health: Some("live".into()),
            tool_count: 4,
            ..Default::default()
        },
        McpServerInfo {
            name: "linear".into(),
            title: Some("Linear".into()),
            description: Some("Issues, projects, cycles, and documents.".into()),
            endpoint: "https://mcp.linear.app/mcp".into(),
            kind: "http".into(),
            enabled: false,
            oauth: Some(false),
            ..Default::default()
        },
        McpServerInfo {
            name: "github".into(),
            title: Some("GitHub".into()),
            description: Some("Issues, pull requests, checks, and releases.".into()),
            endpoint: "https://api.githubcopilot.com/mcp".into(),
            kind: "http".into(),
            enabled: true,
            connected: true,
            health: Some("live".into()),
            tool_count: 38,
            oauth: Some(true),
            calls: 6,
            ..Default::default()
        },
    ]
}

/// The deck state for one frame of one shot.
///
/// Built fresh per frame from `(scene, sel)` — see [`model_at`] for why nothing
/// here is carried forward.
fn ui_for(scene: Scene, sel: usize, model: &WorkspaceModel, splash_ms: u64) -> DeckUi {
    let mut ui = DeckUi {
        graph: Some(demo_graph()),
        ..DeckUi::default()
    };
    ui.engine.pristine = Some(demo_engine_config());
    ui.engine.state = Some(demo_engine_config());
    ui.tools.state = Some(fixture_tool_policy());

    match scene {
        Scene::Splash => {
            ui.splash = stella_tui::splash::state_backdated_for_preview(splash_ms);
            ui.tab = DeckTab::Session;
            return ui;
        }
        Scene::SessionLane(focused) => {
            ui.splash.skip();
            ui.tab = DeckTab::Session;
            ui.focused = focused.min(model.agents.len().saturating_sub(1));
        }
        Scene::FilesDiff => {
            ui.splash.skip();
            ui.tab = DeckTab::Files;
            ui.files_sel = sel;
            ui.files_diff_open = true;
        }
        Scene::Tab(tab) => {
            ui.splash.skip();
            ui.tab = tab;
            match tab {
                DeckTab::Session => ui.focused = 0,
                DeckTab::Agents => {
                    ui.installed.entries = fixture_installed_agents();
                    ui.installed.loaded = true;
                    ui.installed.sel = sel.min(ui.installed.entries.len().saturating_sub(1));
                }
                DeckTab::Traces => ui.trace_filter = None,
                DeckTab::Graph => ui.graph_cursor = sel,
                DeckTab::Files => ui.files_sel = sel,
                DeckTab::Skills => {
                    ui.skills.view = fixture_skills();
                    ui.skills.sel = sel;
                }
                DeckTab::Mcp => {
                    ui.mcp.servers = fixture_mcp_servers();
                    ui.mcp.selected = sel;
                }
                DeckTab::Issues => {
                    ui.issues.rows = fixture_issues();
                    ui.issues.loaded = true;
                    ui.issues.sel = sel;
                }
                DeckTab::Settings => {}
            }
        }
        Scene::SettingsTools => {
            ui.splash.skip();
            ui.tab = DeckTab::Settings;
            ui.settings_pane = SettingsPane::Tools;
            ui.tools.row = sel;
            ui.tools.focused = true;
        }
        Scene::EngineConfig(tab) => {
            ui.splash.skip();
            ui.tab = DeckTab::Settings;
            ui.settings_pane = SettingsPane::Agents;
            ui.engine.tab = tab;
            ui.engine.focused = true;
            ui.engine.row = sel.min(ui.engine.row_count().saturating_sub(1));
        }
    }
    ui
}

// ───────────────────────────── encoding ─────────────────────────────

/// A colour as the renderer wants it: `"RRGGBB"`, or `None` for the terminal's
/// own default.
///
/// The deck's palette is 24-bit throughout (`palette.rs` is all `Color::Rgb`),
/// and `render_deck` only narrows it for terminals that cannot do truecolor —
/// which a film has no reason to pretend to be. The named and indexed arms
/// exist because `Color` is not ours to make exhaustive-proof: they resolve
/// against the standard xterm ramp so a future palette edit degrades to the
/// right hue instead of to a panic.
fn hex(c: Color) -> Option<String> {
    let (r, g, b) = match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Reset => return None,
        Color::Black => (0, 0, 0),
        Color::Red => (0xCD, 0x31, 0x31),
        Color::Green => (0x0D, 0xBC, 0x79),
        Color::Yellow => (0xE5, 0xE5, 0x10),
        Color::Blue => (0x24, 0x72, 0xC8),
        Color::Magenta => (0xBC, 0x3F, 0xBC),
        Color::Cyan => (0x11, 0xA8, 0xCD),
        Color::Gray => (0xE5, 0xE5, 0xE5),
        Color::DarkGray => (0x66, 0x66, 0x66),
        Color::LightRed => (0xF1, 0x4C, 0x4C),
        Color::LightGreen => (0x23, 0xD1, 0x8B),
        Color::LightYellow => (0xF5, 0xF5, 0x43),
        Color::LightBlue => (0x3B, 0x8E, 0xEA),
        Color::LightMagenta => (0xD6, 0x70, 0xD6),
        Color::LightCyan => (0x29, 0xB8, 0xDB),
        Color::White => (0xFF, 0xFF, 0xFF),
        Color::Indexed(i) => xterm256(i),
    };
    Some(format!("{r:02X}{g:02X}{b:02X}"))
}

/// The standard xterm 256-colour ramp: 16 system colours, a 6x6x6 cube, then a
/// 24-step gray ramp.
fn xterm256(i: u8) -> (u8, u8, u8) {
    const SYSTEM: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xCD, 0x31, 0x31),
        (0x0D, 0xBC, 0x79),
        (0xE5, 0xE5, 0x10),
        (0x24, 0x72, 0xC8),
        (0xBC, 0x3F, 0xBC),
        (0x11, 0xA8, 0xCD),
        (0xE5, 0xE5, 0xE5),
        (0x66, 0x66, 0x66),
        (0xF1, 0x4C, 0x4C),
        (0x23, 0xD1, 0x8B),
        (0xF5, 0xF5, 0x43),
        (0x3B, 0x8E, 0xEA),
        (0xD6, 0x70, 0xD6),
        (0x29, 0xB8, 0xDB),
        (0xFF, 0xFF, 0xFF),
    ];
    const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match i {
        0..=15 => SYSTEM[i as usize],
        16..=231 => {
            let n = i as usize - 16;
            (CUBE[n / 36], CUBE[(n / 6) % 6], CUBE[n % 6])
        }
        _ => {
            let v = 8 + (i as u16 - 232) * 10;
            let v = v as u8;
            (v, v, v)
        }
    }
}

/// The modifier bitmask the renderer reads.
///
/// Only the four that change how a glyph is drawn. `REVERSED` is resolved here
/// rather than passed through: swapping fg/bg is a render decision with exactly
/// one right answer, and doing it at the source means the renderer never has to
/// know what "reversed" means.
fn mod_bits(m: Modifier) -> u8 {
    let mut bits = 0;
    if m.contains(Modifier::BOLD) {
        bits |= 1;
    }
    if m.contains(Modifier::ITALIC) {
        bits |= 2;
    }
    if m.contains(Modifier::DIM) {
        bits |= 4;
    }
    if m.contains(Modifier::UNDERLINED) {
        bits |= 8;
    }
    bits
}

/// Render one deck state and encode it as run-length rows.
///
/// A run breaks whenever the style changes; the text within a run is emitted
/// verbatim, which is what makes the file small enough to keep around.
fn encode_frame(model: &WorkspaceModel, ui: &mut DeckUi) -> String {
    let mut terminal = Terminal::new(TestBackend::new(COLS, ROWS)).expect("TestBackend");
    terminal
        .draw(|f| render_deck(model, ui, f))
        .expect("render_deck");
    let buf = terminal.backend().buffer();

    let mut out = String::from("[");
    for y in 0..ROWS {
        if y > 0 {
            out.push(',');
        }
        out.push('[');
        let mut runs = 0usize;
        // (fg, bg, mods, text) of the run being accumulated.
        let mut open: Option<(Option<String>, Option<String>, u8, String)> = None;

        for x in 0..COLS {
            let Some(cell) = buf.cell((x, y)) else {
                continue;
            };
            let style = cell.style();
            let reversed = style.add_modifier.contains(Modifier::REVERSED);
            let (fg, bg) = if reversed {
                (
                    style.bg.unwrap_or(Color::Reset),
                    style.fg.unwrap_or(Color::Reset),
                )
            } else {
                (
                    style.fg.unwrap_or(Color::Reset),
                    style.bg.unwrap_or(Color::Reset),
                )
            };
            let (fg, bg, bits) = (hex(fg), hex(bg), mod_bits(style.add_modifier));

            match &mut open {
                Some((of, ob, om, text)) if *of == fg && *ob == bg && *om == bits => {
                    text.push_str(cell.symbol());
                }
                _ => {
                    if let Some(run) = open.take() {
                        if runs > 0 {
                            out.push(',');
                        }
                        push_run(&mut out, run);
                        runs += 1;
                    }
                    open = Some((fg, bg, bits, cell.symbol().to_string()));
                }
            }
        }
        if let Some(run) = open.take() {
            if runs > 0 {
                out.push(',');
            }
            push_run(&mut out, run);
        }
        out.push(']');
    }
    out.push(']');
    out
}

fn push_run(out: &mut String, (fg, bg, bits, text): (Option<String>, Option<String>, u8, String)) {
    let q = |c: &Option<String>| match c {
        Some(h) => format!("\"{h}\""),
        None => "null".to_string(),
    };
    let _ = write!(
        out,
        "[{},{},{},{}]",
        q(&fg),
        q(&bg),
        bits,
        json_string(&text)
    );
}

/// JSON-escape a run's text.
///
/// Hand-rolled rather than `serde_json::to_string`: the deck's frames are
/// mostly box-drawing and ASCII, this is called a few hundred thousand times,
/// and the escape set for a terminal cell is small and closed.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ───────────────────────────── the film ─────────────────────────────

/// Every emitted line of the film, in order.
///
/// Returned as a `Vec` rather than streamed so `--verify` can render twice and
/// compare without re-plumbing the writer.
fn film() -> Vec<String> {
    let scripted = demo_inbound(0, FIXTURE_PID).len();
    assert_eq!(
        scripted, EVENTS.all,
        "deck_film: the scripted session has {scripted} events but EVENTS.all says \
         {}; re-cut the beats in EVENTS against scenario::demo_inbound",
        EVENTS.all
    );
    let shots = shots();
    let total: f64 = shots.iter().map(|s| s.secs).sum();
    let frames = (total * f64::from(FPS)).round() as usize;

    let mut out = Vec::with_capacity(frames + 1);
    out.push(format!(
        "{{\"kind\":\"header\",\"fps\":{FPS},\"cols\":{COLS},\"rows\":{ROWS},\
         \"frames\":{frames},\"seconds\":{total:.3}}}"
    ));

    // Content id per distinct grid, so a repeated frame costs one short line.
    let mut ids: HashMap<String, usize> = HashMap::new();

    let mut elapsed = 0.0_f64;
    let mut i = 0usize;
    for s in &shots {
        let shot_frames = (s.secs * f64::from(FPS)).round() as usize;
        for f in 0..shot_frames {
            let t = f as f64 / f64::from(FPS);
            let progress = if shot_frames > 1 {
                f as f64 / (shot_frames - 1) as f64
            } else {
                0.0
            };

            let (from, to) = (s.session.0 as f64, s.session.1 as f64);
            let take = (from + (to - from) * progress).round() as usize;
            let model = model_at(take);
            let sel = s.sel.0
                + (((s.sel.1 as f64 - s.sel.0 as f64) * progress).round() as usize)
                    .min(s.sel.1.saturating_sub(s.sel.0));
            // The splash renders on its own clock, not the shot's camera track:
            // the assemble is ~1.4s and the shot is longer, so it finishes and
            // holds while the camera keeps moving.
            let splash_ms = (t * 1000.0) as u64;

            let mut ui = ui_for(s.scene, sel, &model, splash_ms);
            let grid = encode_frame(&model, &mut ui);

            let next_id = ids.len();
            let (id, fresh) = match ids.get(&grid) {
                Some(id) => (*id, false),
                None => {
                    ids.insert(grid.clone(), next_id);
                    (next_id, true)
                }
            };

            let (zoom, cx, cy) = camera_at(&s.keys, t);
            let now = elapsed + t;
            let fade = fade_at(now, total);

            let mut line = format!(
                "{{\"kind\":\"frame\",\"i\":{i},\"cam\":[{zoom:.4},{cx:.4},{cy:.4}],\
                 \"fade\":{fade:.4},\"grid\":{id}"
            );
            if fresh {
                let _ = write!(line, ",\"rows\":{grid}");
            }
            line.push('}');
            out.push(line);
            i += 1;
        }
        elapsed += s.secs;
    }
    out
}

/// The black level at `t`: 0 at full brightness, 1 at black.
fn fade_at(t: f64, total: f64) -> f64 {
    let in_f = if t < FADE { 1.0 - t / FADE } else { 0.0 };
    let remaining = total - t;
    let out_f = if remaining < FADE {
        1.0 - remaining / FADE
    } else {
        0.0
    };
    in_f.max(out_f).clamp(0.0, 1.0)
}

/// How densely each row of a scene is drawn — one bucket per grid row, as the
/// count of cells carrying a glyph.
///
/// Not a bounding box: the deck paints the app ground as a real frame fill and
/// its chrome reaches every edge, so *every* scene's box is the whole grid and
/// the number says nothing. What a camera actually needs to avoid is a **void**
/// — a band of rows the fixture left empty, which composes as a hole when the
/// shot pulls back. The scripted session is deliberately small (three agents, a
/// fifteen-line transcript) against a 54-row grid, so those bands are large and
/// their position is the thing to point the camera away from.
///
/// `--extents` prints this per shot; every camera track below is set from what
/// it reported rather than from taste.
fn row_density(model: &WorkspaceModel, ui: &mut DeckUi) -> Vec<u16> {
    let (cols, rows) = (probe_cols(), probe_rows());
    let mut terminal = Terminal::new(TestBackend::new(cols, rows)).expect("TestBackend");
    terminal
        .draw(|f| render_deck(model, ui, f))
        .expect("render_deck");
    let buf = terminal.backend().buffer();
    (0..rows)
        .map(|y| {
            (0..cols)
                .filter(|&x| {
                    buf.cell((x, y))
                        .is_some_and(|c| !c.symbol().trim().is_empty())
                })
                .count() as u16
        })
        .collect()
}

fn print_extents() {
    // Eight levels of block, so a 54-row profile reads as a shape in one line.
    const BLOCKS: [char; 9] = [
        ' ', '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}',
        '\u{2588}',
    ];
    println!("row density per shot — one cell per grid row, ' ' is an empty row\n");
    for s in &shots() {
        let model = model_at(s.session.1);
        let mut ui = ui_for(s.scene, s.sel.1, &model, 1_000);
        let density = row_density(&model, &mut ui);
        let peak = density.iter().copied().max().unwrap_or(1).max(1);
        let profile: String = density
            .iter()
            .map(|&d| BLOCKS[(usize::from(d) * 8).div_ceil(usize::from(peak)).min(8)])
            .collect();
        let filled = density.iter().filter(|&&d| d > 0).count();
        let first = density.iter().position(|&d| d > 0).unwrap_or(0);
        let last = density.iter().rposition(|&d| d > 0).unwrap_or(0);
        println!(
            "{:<30} |{profile}|  rows {first}..{last}, {filled}/{} drawn",
            s.label,
            probe_rows()
        );
    }
}

fn print_shots() {
    let shots = shots();
    let mut at = 0.0;
    println!("{:>7}  {:>6}  shot", "start", "len");
    for s in &shots {
        println!("{at:>6.1}s  {:>5.1}s  {}", s.secs, s.label);
        at += s.secs;
    }
    println!(
        "{at:>6.1}s total · {} scripted events",
        demo_inbound(0, FIXTURE_PID).len()
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--shots") {
        print_shots();
        return;
    }

    if args.iter().any(|a| a == "--extents") {
        print_extents();
        return;
    }

    if args.iter().any(|a| a == "--verify") {
        let a = film();
        let b = film();
        if a == b {
            eprintln!(
                "deck_film: reproducible — {} frames, byte-identical",
                a.len() - 1
            );
        } else {
            let first = a
                .iter()
                .zip(&b)
                .position(|(x, y)| x != y)
                .unwrap_or(a.len().min(b.len()));
            eprintln!("deck_film: NOT reproducible — first difference at line {first}");
            std::process::exit(1);
        }
        return;
    }

    let mut stdout = std::io::BufWriter::new(std::io::stdout().lock());
    use std::io::Write as _;
    for line in film() {
        let _ = writeln!(stdout, "{line}");
    }
    let _ = stdout.flush();
}
