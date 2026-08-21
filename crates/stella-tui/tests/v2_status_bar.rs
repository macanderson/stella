//! Golden frames for the v2 status bar (SPEC 5), plus the two assertions the
//! whole v2 design leans on: red scarcity and no hex literals in render code.
//!
//! ## Why these goldens carry colour and the v1 goldens do not
//!
//! `tests/deck_render_snapshots.rs` strips styling on purpose, and its
//! reasoning is sound for v1: the deck remaps its palette for light terminals
//! and degrades by colour depth, so a style-bearing golden would be a golden
//! about the environment. It states the cost honestly — a regression that only
//! stops *highlighting* a row is invisible there.
//!
//! v2 cannot pay that cost. The two-metal rule (SPEC 2) is a claim about
//! colour and nothing else: gold means stella acting, silver means the world
//! coming in, and a frame that renders the right glyphs in the wrong metal has
//! broken the design while passing a character-grid golden. Red scarcity is
//! the same claim sharpened — red never appears in a healthy frame, which is
//! what makes a red gate an alarm with no blinking and no bell, and it is
//! unassertable without reading cell colours.
//!
//! So each golden is two blocks: the character grid, then a **metal map** —
//! one letter per cell naming the palette token that cell's foreground came
//! from. The map is the reviewable half. A cell that drifts from `G` to `w` is
//! a sentence in the diff, where an RGB dump would be noise.
//!
//! The environment objection does not apply because these frames are rendered
//! through `TestBackend` at fixed size from a fixture `Status`, never through
//! a real terminal's capability detection. Degradation is tested where it
//! lives, in `stella-tui-theme`'s `every_token_has_a_fallback`.
//!
//! ## Regenerating
//!
//! ```text
//! INSTA_UPDATE=always cargo test -p stella-tui --test v2_status_bar
//! ```
//!
//! Then read the diff, and read the metal map against
//! `renderings/png/01-session-turn-lifecycle`. A golden blessed without
//! looking is a changelog, not a test.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::Widget;

use stella_tui::v2::status_bar::{Status, StatusBar, cells};
use stella_tui_theme::token;

// ── frame capture ───────────────────────────────────────────────────────────

/// The letter a palette token takes in a metal map.
///
/// Chosen so the two metals are the loudest thing in the map: gold is `G`,
/// its lift `B`, silver `s` and its type tier `S`. Prose is `w`, chrome is
/// lowercase, and an unrecognised colour is `?` — which is itself a finding,
/// since every colour on a v2 surface is supposed to be a token.
fn metal(color: Color) -> char {
    match color {
        // The terminal's own default — a cell nothing claimed, which on a
        // painted ground means padding.
        Color::Reset => '-',
        token::GOLD => 'G',
        token::GOLD_BRIGHT => 'B',
        token::SILVER => 's',
        token::SILVER_TYPE => 'S',
        token::TEXT => 'w',
        token::MUTED => 'm',
        token::DIM => 'd',
        token::COMMENT => 'c',
        token::GREEN => 'n',
        token::RED => 'R',
        token::BORDER => 'b',
        token::RULE => 'u',
        token::BG => '.',
        token::PANEL => 'p',
        token::HL => 'h',
        _ => '?',
    }
}

/// Render a widget into a fixed viewport and dump it as a reviewable golden:
/// the character grid, a rule, then the metal map.
fn frame<W: Widget>(widget: W, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    widget.render(area, &mut buf);

    let mut out = String::new();
    for y in 0..height {
        for x in 0..width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out.push_str(&"─".repeat(width as usize));
    out.push_str("\nmetal map: G gold · B gold_bright · s silver · w text · m muted · d dim · b border · R red · n green\n");
    for y in 0..height {
        for x in 0..width {
            out.push(metal(buf[(x, y)].fg));
        }
        out.push('\n');
    }
    out
}

/// Every cell whose foreground or background is [`token::RED`].
///
/// SPEC 2: red is the rarest colour on screen, and it never appears in a
/// healthy frame. This is the count that makes that a test rather than an
/// intention. Every healthy-frame snapshot carries it.
fn red_cells<W: Widget>(widget: W, width: u16, height: u16) -> usize {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    widget.render(area, &mut buf);
    (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let cell = &buf[(x, y)];
            cell.fg == token::RED || cell.bg == token::RED
        })
        .count()
}

/// The fixture from `renderings/01-session-turn-lifecycle`, verbatim.
fn demo() -> Status<'static> {
    Status {
        worker: "kimi-k3",
        stage: "execute",
        ctx_used: 0.35,
        spend_usd: 0.45,
        saved_usd: 0.69,
        inbox: 21,
        // The rendering shows no deadline, because nothing was timing that
        // session. `None` is that fact, and it is not the same fact as `0`.
        deadline_remaining_ms: None,
    }
}

// ── goldens ─────────────────────────────────────────────────────────────────

#[test]
fn v2_status_bar_at_deck_width() {
    insta::assert_snapshot!(frame(StatusBar(demo()), 100, 1));
}

/// The row narrows before it wraps: cells drop from the right, worker and
/// stage never do, and the help affordance goes with the room for it.
#[test]
fn v2_status_bar_narrow() {
    insta::assert_snapshot!(frame(StatusBar(demo()), 46, 1));
}

#[test]
fn v2_status_bar_at_the_extremes() {
    insta::assert_snapshot!(frame(
        StatusBar(Status {
            ctx_used: 0.0,
            spend_usd: 0.0,
            saved_usd: 0.0,
            inbox: 0,
            ..demo()
        }),
        100,
        1
    ));
}

// ── the standing assertions ─────────────────────────────────────────────────

#[test]
fn a_healthy_status_bar_has_no_red_cells() {
    // SPEC 2. Not test hygiene — this is the mechanism by which a red gate
    // reads as an alarm without animation or sound. If this ever fails, the
    // question is what put red on a healthy frame, never whether to relax it.
    assert_eq!(red_cells(StatusBar(demo()), 100, 1), 0);
    assert_eq!(red_cells(StatusBar(demo()), 46, 1), 0);
    assert_eq!(
        red_cells(
            StatusBar(Status {
                ctx_used: 1.0,
                spend_usd: 999.99,
                ..demo()
            }),
            100,
            1
        ),
        0,
        "a full context meter and a large spend are still healthy — neither \
         is a failure, and neither may borrow the failure colour"
    );
}

/// Every colour on a v2 surface comes from the theme crate.
///
/// The plan's P0 acceptance asks for a sweep that finds no warm hex outside
/// the theme crate; this is that sweep, scoped to the render code that exists.
/// It grows with each phase — add the phase's module directory to `V2_ROOTS`.
///
/// Deliberately not a whole-workspace grep: v1 render code is *supposed* to
/// carry the v1 palette until the surface that replaces it ships, and a check
/// that fails on the tree it inherited teaches nothing.
#[test]
fn no_hex_literals_in_v2_render_code() {
    const V2_ROOTS: [&str; 2] = ["src/v2.rs", "src/v2"];

    let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut checked = 0usize;
    let mut queue: Vec<std::path::PathBuf> =
        V2_ROOTS.iter().map(|root| crate_dir.join(root)).collect();

    while let Some(path) = queue.pop() {
        if path.is_dir() {
            queue.extend(
                std::fs::read_dir(&path)
                    .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
                    .map(|entry| entry.expect("dir entry").path()),
            );
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read source");
        checked += 1;
        for (n, line) in source.lines().enumerate() {
            // Doc comments quote hexes when they explain a token; only code
            // may not carry one.
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            assert!(
                !code.contains("Color::Rgb("),
                "{}:{}: a v2 render module built a colour from channels. Every \
                 colour comes from `stella_tui_theme::token`.",
                path.display(),
                n + 1
            );
        }
    }
    assert!(
        checked >= 2,
        "the sweep found {checked} v2 source files, which means V2_ROOTS has \
         gone stale — a sweep over nothing passes"
    );
}

// ── the decision function ───────────────────────────────────────────────────

#[test]
fn the_bar_says_what_spec_5_says_it_says() {
    let flat: Vec<String> = cells(&demo())
        .iter()
        .map(|cell| cell.iter().map(|s| s.content.to_string()).collect())
        .collect();
    // SPEC 5's order, exactly: worker · stage · ctx [bar] % · $spend ·
    // saved $x · ✉ n. (`? help` is pinned right by the renderer, not a cell,
    // so it is asserted in the golden instead.)
    assert_eq!(flat.len(), 6);
    assert_eq!(flat[0], "kimi-k3");
    assert_eq!(flat[1], "execute");
    assert!(flat[2].starts_with("ctx "), "{}", flat[2]);
    assert!(flat[2].ends_with(" 35%"), "{}", flat[2]);
    assert_eq!(flat[3], "$0.45");
    assert_eq!(flat[4], "saved $0.69");
    assert_eq!(flat[5], "✉ 21");
}

#[test]
fn money_is_gold_and_the_meter_is_gold_on_border_gray() {
    let cells = cells(&demo());
    // SPEC 5: "Money renders gold." Both amounts, not just the spend.
    for (i, name) in [(3usize, "spend"), (4, "saved")] {
        let amount = cells[i].last().expect("an amount span");
        assert_eq!(
            amount.style.fg,
            Some(token::GOLD),
            "{name} is money and money is gold"
        );
    }
    // SPEC 5: "Meters render gold fill on `border` gray. No pink, no green
    // meters." The v1 statline's CPU meter shades by load through
    // `theme::gauge_color`, which is exactly the meter that goes away.
    let ctx = &cells[2];
    let fills: Vec<_> = ctx
        .iter()
        .filter(|s| s.style.fg == Some(token::GOLD))
        .collect();
    assert!(!fills.is_empty(), "the meter has no gold fill");
    let track: Vec<_> = ctx
        .iter()
        .filter(|s| s.style.fg == Some(token::BORDER))
        .collect();
    assert!(!track.is_empty(), "the meter has no border-gray track");
    for span in ctx {
        assert!(
            span.style.fg != Some(token::GREEN) && span.style.fg != Some(token::RED),
            "the context meter took a verdict colour"
        );
    }
}

#[test]
fn the_meter_tracks_the_number_printed_beside_it() {
    for (frac, want_pct) in [(0.0, "0%"), (0.35, "35%"), (0.999, "100%"), (1.0, "100%")] {
        let bar = cells(&Status {
            ctx_used: frac,
            ..demo()
        });
        let ctx: String = bar[2].iter().map(|s| s.content.to_string()).collect();
        assert!(
            ctx.ends_with(&format!(" {want_pct}")),
            "ctx at {frac} printed `{ctx}`"
        );
        // Solid cells, not gold cells: the head of the fill is gold too, and
        // the property under test is where the bar *ends*, not how much of it
        // is gold. Rounding may move the head; it may not move either end.
        let solid = ctx.chars().filter(|c| *c == '█').count();
        let gold_cells = bar[2]
            .iter()
            .filter(|s| s.style.fg == Some(token::GOLD))
            .map(|s| s.content.chars().count())
            .sum::<usize>();
        match frac {
            f if f <= 0.0 => assert_eq!(gold_cells, 0, "an empty meter drew fill"),
            f if f >= 1.0 => assert_eq!(solid, 12, "a full meter left track showing"),
            _ => {
                assert!(solid < 12, "{frac} rendered as a full bar");
                assert!(gold_cells >= 1, "{frac} rendered as an empty bar");
            }
        }
    }
}

// ── the conditional cell: an armed task deadline (#4126) ────────────────────

/// The text of every cell, in order.
fn flat(status: &Status<'_>) -> Vec<String> {
    cells(status)
        .iter()
        .map(|cell| cell.iter().map(|s| s.content.to_string()).collect())
        .collect()
}

/// #4126 witness. An armed task deadline draws a cell; an unarmed run does not.
///
/// The two halves are one test because the pair is the property: the v1 cell
/// was built around `None` and `Some(0)` being different facts, and a test that
/// only checked the armed half would pass on a bar that printed `deadline 0s`
/// for a run nobody was timing.
#[test]
fn an_armed_deadline_earns_a_cell_and_an_unarmed_run_does_not() {
    let armed = flat(&Status {
        deadline_remaining_ms: Some(754_000),
        ..demo()
    });
    assert_eq!(armed.len(), 7, "the armed bar: {armed:?}");
    assert_eq!(
        armed[2], "deadline 12m 34s",
        "SPEC 5 puts the countdown third, right after the stage"
    );

    let unarmed = flat(&demo());
    assert_eq!(unarmed.len(), 6, "the unarmed bar grew a cell: {unarmed:?}");
    assert!(
        !unarmed.iter().any(|c| c.contains("deadline")),
        "an unarmed run paid columns for a deadline nobody set: {unarmed:?}"
    );
}

/// A crossed deadline reads as a word, never as a quantity.
///
/// `0s` invites "no deadline", which is the one thing `Some(0)` does not mean —
/// it means the `SIGKILL` has already been earned.
#[test]
fn a_crossed_deadline_reads_expired_not_zero() {
    let bar = flat(&Status {
        deadline_remaining_ms: Some(0),
        ..demo()
    });
    assert_eq!(bar[2], "deadline expired");
}

/// The countdown escalates to red inside the last minute, and only there.
///
/// SPEC 2 keeps red for failure and destructive events. A kill under a minute
/// away is a destructive event in progress; four minutes out is not, and must
/// not spend the scarcity that makes red an alarm.
#[test]
fn the_countdown_takes_red_only_inside_the_last_minute() {
    let value_fg = |ms: u64| {
        cells(&Status {
            deadline_remaining_ms: Some(ms),
            ..demo()
        })[2]
            .last()
            .expect("the countdown's value span")
            .style
            .fg
    };
    for calm in [61_000, 240_000, 7_200_000] {
        assert_eq!(value_fg(calm), Some(token::TEXT), "{calm}ms took an alarm");
    }
    for alarm in [0, 1_000, 59_000, 60_000] {
        assert_eq!(value_fg(alarm), Some(token::RED), "{alarm}ms drew no alarm");
    }
    // And the alarm is not the only carrier: the word survives `NO_COLOR` and
    // red-blindness, which is SPEC 13's floor.
    assert!(
        flat(&Status {
            deadline_remaining_ms: Some(0),
            ..demo()
        })[2]
            .starts_with("deadline ")
    );
}

/// The cell that says the run is about to stop is the last one the drop rule
/// gives up.
///
/// v1 made this a priority number on a table; v2 makes it a position, so this
/// asserts the consequence rather than the mechanism.
///
/// 46 columns is the width `v2_status_bar_narrow` pins: tight enough that the
/// unarmed bar is down to worker and stage alone. Every other cell is already
/// gone there, so a countdown that still renders has outranked all of them —
/// including the context meter, which is what the bar drops last when nothing
/// is timing the run.
#[test]
fn a_narrow_row_drops_everything_before_it_drops_the_deadline() {
    let rendered = frame(
        StatusBar(Status {
            deadline_remaining_ms: Some(30_000),
            ..demo()
        }),
        46,
        1,
    );
    assert!(
        rendered.contains("deadline 30s"),
        "a 46-column row dropped the countdown:\n{rendered}"
    );
    assert!(
        !rendered.contains("ctx "),
        "46 columns fit more than the floor, so this proves nothing:\n{rendered}"
    );
}

/// An armed deadline is allowed to put red on the frame; nothing else is.
///
/// The healthy-frame assertion above is the other half of this pair, and both
/// have to hold: red that never appears is not a signal, and red that appears
/// on a healthy frame is not an alarm.
#[test]
fn the_alarm_is_the_only_red_the_bar_can_draw() {
    let armed = Status {
        deadline_remaining_ms: Some(30_000),
        ..demo()
    };
    assert!(red_cells(StatusBar(armed), 100, 1) > 0);
    assert_eq!(
        red_cells(
            StatusBar(Status {
                deadline_remaining_ms: Some(3_600_000),
                ..demo()
            }),
            100,
            1
        ),
        0,
        "an hour of headroom is not an alarm"
    );
}

/// Out-of-range input is clamped, not trusted. `ctx_used` is a ratio computed
/// upstream from a token count and a window size, and both have been wrong
/// before; a bar that panics or overdraws on 1.4 is a crash in the one widget
/// that must never take the screen down.
#[test]
fn the_meter_survives_a_ratio_upstream_got_wrong() {
    for frac in [-1.0, 1.5, f64::NAN, f64::INFINITY] {
        let bar = cells(&Status {
            ctx_used: frac,
            ..demo()
        });
        let width: usize = bar[2].iter().map(|s| s.content.chars().count()).sum();
        // "ctx " (4) + 12 meter cells + " N%".." NNN%" (3..=5).
        assert!(
            (19..=21).contains(&width),
            "ctx at {frac} was {width} cells"
        );
    }
}
