//! The panel channel's wire contract — `design/tui-v2/SPEC.md` §12.
//!
//! Split from `wire_contract.rs` when that file met the 1500-line ceiling
//! (AGENTS.md § "God files"). The seam is the channel, not the line count: a
//! panel is its own dispatch context with its own point, grant and vocabulary,
//! so its tests read as a set whether or not they share a file with the
//! wrapper socket's.
//!
//! Held to the identical contract as every other channel here: byte-for-byte
//! in both directions (AGENTS.md #4), every closed vocabulary pinned on both
//! sides, and every table refusing a key it does not know. Three rules are
//! this channel's own and are witnessed rather than asserted in prose — a
//! frame cannot address a cell outside its lease, cannot carry an escape
//! sequence in any language, and cannot reorder its own glyphs against the
//! bytes it sent.

use serde::Serialize;
use serde::de::DeserializeOwned;
use stella_plugin::{
    PROTOCOL_VERSION, PanelDenial, PanelEmphasis, PanelFrame, PanelInk, PanelLease, PanelLine,
    PanelOverflow, PanelPaint, PanelPatch, PanelPoint, PanelRect, PanelRefusal, PanelRequest,
    PanelResponse, PanelSpan, PanelStyle, PanelSurface, PanelText, PluginManifest,
};

/// Serialize, parse back, serialize again — the same bytes, and the same value.
///
/// A copy of `wire_contract.rs`'s helper rather than a shared module, because
/// an integration test is its own crate: sharing it would mean a `mod common`
/// both files include, which costs more than these ten lines are worth.
fn round_trip<T>(value: &T) -> String
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_string(value).expect("value serializes");
    let parsed: T = serde_json::from_str(&json).expect("value parses back");
    assert_eq!(&parsed, value, "value changed across the round trip");
    let again = serde_json::to_string(&parsed).expect("parsed value serializes");
    assert_eq!(json, again, "bytes changed across the round trip");
    json
}

/// The rectangle a host leases an eight-by-two panel for one tick.
fn lease() -> PanelLease {
    PanelLease::new("gates", PanelSurface::Overlay, 42, PanelRect::new(8, 2), 33)
}

/// Glyphs the contract accepts, for a test that is about something else.
fn glyphs(text: &str) -> PanelText {
    PanelText::new(text).expect("plain glyphs")
}

/// One row of one style.
fn row(text: &str) -> PanelLine {
    PanelLine::new(vec![PanelSpan::new(glyphs(text), PanelStyle::plain())])
}

/// Both frame shapes, the styled span and the unstyled one, the populated row
/// and the empty one — every optional member of the panel wire in both of its
/// states, in one pass.
#[test]
fn every_panel_message_round_trips_byte_for_byte() {
    let request = round_trip(&PanelRequest::new(lease()));
    let value: serde_json::Value = serde_json::from_str(&request).unwrap();
    assert_eq!(value["point"], "frame");
    assert_eq!(value["body"]["protocol_version"], PROTOCOL_VERSION);
    assert_eq!(value["body"]["rect"]["cols"], 8);
    assert_eq!(value["body"]["rect"]["rows"], 2);

    let styled = PanelPaint::Lines(vec![
        PanelLine::new(vec![
            PanelSpan::new(
                glyphs("3 "),
                PanelStyle {
                    fg: Some(PanelInk::Green),
                    bg: Some(PanelInk::Panel),
                    emphasis: vec![PanelEmphasis::Bold, PanelEmphasis::Underline],
                },
            ),
            PanelSpan::new(glyphs("green"), PanelStyle::plain()),
        ]),
        PanelLine::default(),
    ]);
    let lines = round_trip(&PanelResponse::new(PanelFrame::new(
        PanelSurface::Overlay,
        42,
        styled,
    )));
    let value: serde_json::Value = serde_json::from_str(&lines).unwrap();
    assert_eq!(value["point"], "frame");
    // An unstyled span omits the key and an empty row omits its spans, so the
    // emptiest legal frame is smaller than a reader of the type would guess.
    assert!(
        value["body"]["paint"]["lines"][0]["spans"][1]
            .get("style")
            .is_none()
    );
    assert!(value["body"]["paint"]["lines"][1].get("spans").is_none());

    let diff = PanelPaint::Diff(vec![PanelPatch::new(
        1,
        6,
        glyphs("ok"),
        PanelStyle::ink(PanelInk::Silver),
    )]);
    let patched = round_trip(&PanelResponse::new(PanelFrame::new(
        PanelSurface::Overlay,
        42,
        diff,
    )));
    let value: serde_json::Value = serde_json::from_str(&patched).unwrap();
    assert_eq!(value["body"]["paint"]["diff"][0]["row"], 1);
    assert_eq!(value["body"]["paint"]["diff"][0]["col"], 6);
    assert_eq!(value["body"]["paint"]["diff"][0]["text"], "ok");

    round_trip(&PanelRect::new(0, 0));
    round_trip(&PanelStyle::plain());
    round_trip(&glyphs("stella*"));
}

/// **The witness for #5054.** A frame may address the cells it was leased and
/// no others, and the refusal names the cell that ran past the edge instead of
/// reporting that something, somewhere, did not fit.
///
/// Four directions, because a rectangle has four ways out and a check that
/// counted only rows would pass a row of the right count and the wrong width.
#[test]
fn a_panel_frame_addressing_a_cell_outside_its_lease_is_refused() {
    let lease = lease();

    // Exactly filling the lease is inside it: the edge is addressable, and a
    // panel that could not use its last column would be leased a lie.
    let full = PanelFrame::new(
        PanelSurface::Overlay,
        42,
        PanelPaint::Lines(vec![row("12345678"), row("12345678")]),
    );
    assert_eq!(lease.admits(&full), Ok(()));
    let corner = PanelFrame::new(
        PanelSurface::Overlay,
        42,
        PanelPaint::Diff(vec![PanelPatch::new(
            1,
            7,
            glyphs("x"),
            PanelStyle::plain(),
        )]),
    );
    assert_eq!(lease.admits(&corner), Ok(()));

    let tall = PanelFrame::new(
        PanelSurface::Overlay,
        42,
        PanelPaint::Lines(vec![row("a"), row("b"), row("c")]),
    );
    assert_eq!(
        lease.admits(&tall),
        Err(PanelRefusal::Overflow(PanelOverflow::Rows {
            lines: 3,
            rows: 2
        }))
    );

    let wide = PanelFrame::new(
        PanelSurface::Overlay,
        42,
        PanelPaint::Lines(vec![row("123456789")]),
    );
    assert_eq!(
        lease.admits(&wide),
        Err(PanelRefusal::Overflow(PanelOverflow::Line {
            line: 0,
            cells: 9,
            cols: 8,
        }))
    );

    let below = PanelFrame::new(
        PanelSurface::Overlay,
        42,
        PanelPaint::Diff(vec![PanelPatch::new(
            2,
            0,
            glyphs("x"),
            PanelStyle::plain(),
        )]),
    );
    assert_eq!(
        lease.admits(&below),
        Err(PanelRefusal::Overflow(PanelOverflow::Row {
            row: 2,
            rows: 2
        }))
    );

    let past = PanelFrame::new(
        PanelSurface::Overlay,
        42,
        PanelPaint::Diff(vec![PanelPatch::new(
            0,
            7,
            glyphs("no"),
            PanelStyle::plain(),
        )]),
    );
    assert_eq!(
        lease.admits(&past),
        Err(PanelRefusal::Overflow(PanelOverflow::Patch {
            row: 0,
            col: 7,
            cells: 2,
            cols: 8,
        }))
    );

    // A run of no glyphs is anchored too. `col + 0` is inside every lease, so
    // an extent check on its own would admit a patch naming a column the host's
    // buffer does not have — nothing to blit, and exactly the coordinate a host
    // that reads the column before it measures the run would index with.
    let anchored_out = PanelFrame::new(
        PanelSurface::Overlay,
        42,
        PanelPaint::Diff(vec![PanelPatch::new(0, 8, glyphs(""), PanelStyle::plain())]),
    );
    assert_eq!(
        lease.admits(&anchored_out),
        Err(PanelRefusal::Overflow(PanelOverflow::Patch {
            row: 0,
            col: 8,
            cells: 0,
            cols: 8,
        }))
    );

    // And the refusal is readable: a host printing it names the coordinates a
    // plugin author has to change.
    let printed = lease
        .admits(&past)
        .expect_err("the patch runs past the edge")
        .to_string();
    assert!(printed.contains("column 7"), "got {printed}");
}

/// A panel writes glyphs and never an escape sequence, in Rust and on the wire.
///
/// The Rust half is the type: [`PanelText`]'s body is private, so a value can
/// only come from the constructor that refuses control characters. The wire
/// half is this — a frame carrying `ESC [ 2 J` is a **decode error**, so a host
/// never holds one to inspect and no amount of care about how it blits could
/// have mattered.
#[test]
fn a_panel_frame_carrying_an_escape_sequence_does_not_decode() {
    // Written as JSON escapes, so the hazard is what a plugin's own encoder
    // would put on the pipe and not something only a Rust literal can say.
    for hazard in [
        "\\u001b[2J",
        "\\u001b]0;stella\\u0007",
        "\\u001b[1;1H",
        "a\\nb",
        "col\\u0009umn",
    ] {
        let json = format!(
            "{{\"point\":\"frame\",\"body\":{{\"protocol_version\":1,\"tick\":1,\
             \"paint\":{{\"diff\":[{{\"row\":0,\"col\":0,\"text\":\"{hazard}\"}}]}}}}}}"
        );
        let err = serde_json::from_str::<PanelResponse>(&json)
            .expect_err("an escape sequence must not decode as drawable glyphs");
        assert!(
            err.to_string().contains("control character"),
            "the refusal must say why, got {err}"
        );
    }

    // The same refusal on the other frame shape, so neither of them is the
    // soft one.
    let json = "{\"point\":\"frame\",\"body\":{\"protocol_version\":1,\"tick\":1,\
                \"paint\":{\"lines\":[{\"spans\":[{\"text\":\"\\u001b[31mred\"}]}]}}}";
    assert!(serde_json::from_str::<PanelResponse>(json).is_err());
}

/// A panel's glyphs read in the order its bytes are in, in Rust and on the wire.
///
/// The test above covers the escape-sequence class. This covers the second
/// one: `U+202E` RIGHT-TO-LEFT OVERRIDE and its siblings reorder the glyphs
/// after them, so a frame can render text whose visual reading is not its
/// content — the Trojan Source shape (CVE-2021-42574). Nothing escapes the
/// leased rectangle, because the host clips every blit; the harm is that a
/// panel is chrome a person agreed to trust, and this is text that means one
/// thing and reads as another inside it.
///
/// Either half alone is a hole. [`PanelText::new`] is the only door in Rust,
/// and `Deserialize` routes through it, so a host never holds a reordered
/// frame to inspect.
#[test]
fn a_panel_frame_carrying_a_bidi_override_does_not_decode() {
    // The whole refused set, written as the JSON escapes a plugin's own encoder
    // would put on the pipe rather than as Rust literals.
    for hazard in [
        "\\u061c", "\\u200e", "\\u200f", "\\u202a", "\\u202b", "\\u202c", "\\u202d", "\\u202e",
        "\\u2066", "\\u2067", "\\u2068", "\\u2069",
    ] {
        let json = format!(
            "{{\"point\":\"frame\",\"body\":{{\"protocol_version\":1,\"tick\":1,\
             \"paint\":{{\"diff\":[{{\"row\":0,\"col\":0,\"text\":\"gates {hazard}neerg\"}}]}}}}}}"
        );
        let err = serde_json::from_str::<PanelResponse>(&json)
            .expect_err("a bidi override must not decode as drawable glyphs");
        assert!(
            err.to_string().contains("bidi formatting character"),
            "the refusal must say why, got {err}"
        );
    }

    // The same refusal on the other frame shape, so neither of them is the
    // soft one.
    let lines = "{\"point\":\"frame\",\"body\":{\"protocol_version\":1,\"tick\":1,\
                 \"paint\":{\"lines\":[{\"spans\":[{\"text\":\"\\u202egates\"}]}]}}}";
    assert!(serde_json::from_str::<PanelResponse>(lines).is_err());

    // The Rust half: the constructor is the door, and it names the character
    // and its position in `char`s rather than bytes.
    assert!(PanelText::new("gates: 3 \u{202e}neerg").is_err());
    let err = PanelText::new("\u{2726}\u{202e}").expect_err("a bidi override is not drawable");
    assert!(err.to_string().contains("U+202E"), "got {err}");
    assert!(err.to_string().contains("position 1"), "got {err}");

    // What the rule keeps, and why it keeps it: `U+200D` ZWJ builds the emoji
    // sequences and `U+200C` ZWNJ is ordinary Persian and Indic text.
    for kept in [
        "\u{200b}",
        "\u{200c}",
        "\u{200d}",
        "\u{1f469}\u{200d}\u{1f4bb}",
    ] {
        assert!(
            PanelText::new(kept).is_ok(),
            "{kept:?} is allowed by the rule this test pins"
        );
    }
}

/// Every closed vocabulary the panel channel adds, on both sides.
#[test]
fn every_panel_vocabulary_is_pinned_on_both_sides() {
    assert_eq!(serde_json::to_value(PanelPoint::Frame).unwrap(), "frame");
    assert_eq!(PanelPoint::Frame.to_string(), "frame");
    round_trip(&PanelPoint::Frame);

    for (denial, wire) in [
        (PanelDenial::Network, "network"),
        (PanelDenial::WriteOutsideSandbox, "write-outside-sandbox"),
    ] {
        assert_eq!(serde_json::to_value(denial).unwrap(), wire);
        assert_eq!(denial.to_string(), wire);
        round_trip(&denial);
    }
    assert_eq!(
        PanelDenial::all(),
        &[PanelDenial::Network, PanelDenial::WriteOutsideSandbox],
        "the denial set is closed, and its order is the one a prompt prints"
    );

    for (surface, wire) in [
        (PanelSurface::Settings, "settings"),
        (PanelSurface::Overlay, "overlay"),
        (PanelSurface::Command, "command"),
    ] {
        assert_eq!(serde_json::to_value(surface).unwrap(), wire);
        assert_eq!(surface.to_string(), wire);
        round_trip(&surface);
    }
    assert_eq!(
        PanelSurface::all(),
        &[
            PanelSurface::Settings,
            PanelSurface::Overlay,
            PanelSurface::Command
        ],
        "the placements are closed, and their order is the one a prompt prints"
    );
    assert!(
        serde_json::from_str::<PanelSurface>("\"status_bar\"").is_err(),
        "where a panel may draw is Stella's to decide, so an unknown placement is a refusal"
    );

    // The spellings are `design/tui-v2/SPEC.md` §3.1's own token names, so a
    // panel asks for the same colour the palette table publishes and a reader
    // crosses between the two documents without a mapping.
    for (ink, wire) in [
        (PanelInk::Bg, "bg"),
        (PanelInk::Panel, "panel"),
        (PanelInk::Hl, "hl"),
        (PanelInk::Border, "border"),
        (PanelInk::Rule, "rule"),
        (PanelInk::Gold, "gold"),
        (PanelInk::GoldBright, "gold_bright"),
        (PanelInk::Silver, "silver"),
        (PanelInk::SilverType, "silver_type"),
        (PanelInk::Text, "text"),
        (PanelInk::Muted, "muted"),
        (PanelInk::Dim, "dim"),
        (PanelInk::Green, "green"),
        (PanelInk::Red, "red"),
        (PanelInk::DiffAddBg, "diff_add_bg"),
        (PanelInk::DiffDelBg, "diff_del_bg"),
    ] {
        assert_eq!(serde_json::to_value(ink).unwrap(), wire);
        round_trip(&ink);
    }
    assert!(
        serde_json::from_str::<PanelInk>("\"#EFC53F\"").is_err(),
        "a panel names a token and never a colour of its own, so the hue clamp \
         holds over plugin pixels too"
    );

    for (emphasis, wire) in [
        (PanelEmphasis::Bold, "bold"),
        (PanelEmphasis::Dim, "dim"),
        (PanelEmphasis::Italic, "italic"),
        (PanelEmphasis::Underline, "underline"),
    ] {
        assert_eq!(serde_json::to_value(emphasis).unwrap(), wire);
        round_trip(&emphasis);
    }
    for absent in ["reverse", "blink"] {
        assert!(
            serde_json::from_str::<PanelEmphasis>(&format!("\"{absent}\"")).is_err(),
            "\"{absent}\" is not a panel's to ask for"
        );
    }
}

/// The panel tables refuse a key they do not know, as every other table on this
/// wire does.
///
/// A **frame** still may not caption itself: the caption is a manifest
/// declaration a human consented to at install, so a per-tick title would let a
/// panel relabel itself after the fact, which is the spoof the block's caption
/// rule exists to prevent (#5203).
#[test]
fn a_panel_may_not_name_itself_or_carry_a_key_the_contract_lacks() {
    let err = serde_json::from_str::<PanelResponse>(
        "{\"point\":\"frame\",\"body\":{\"protocol_version\":1,\"tick\":1,\
         \"title\":\"GATES\",\"paint\":{\"lines\":[]}}}",
    )
    .expect_err("a frame does not name the panel");
    assert!(err.to_string().contains("title"), "got {err}");

    let err = PluginManifest::from_toml_str(
        "name = \"gates\"\n[panel]\nsurfaces = [\"overlay\"]\nborder = \"double\"\n\
         denies = [\"network\", \"write-outside-sandbox\"]",
    )
    .expect_err("the chrome is the host's, so a [panel] block does not style it");
    assert!(err.to_string().contains("border"), "got {err}");

    let err = serde_json::from_str::<PanelRequest>(
        "{\"point\":\"frame\",\"body\":{\"protocol_version\":1,\"panel\":\"gates\",\
         \"tick\":1,\"rect\":{\"cols\":4,\"rows\":1,\"x\":9},\"budget_ms\":33}}",
    )
    .expect_err("a leased rectangle carries an extent and no origin");
    assert!(err.to_string().contains("`x`"), "got {err}");
}

/// The `[panel]` block is a consent document, so it names every limit a panel
/// accepts before it loads — a block naming fewer is refused by the denial it
/// left out instead of being read as a narrower panel.
#[test]
fn a_panel_block_must_name_every_denial_it_accepts() {
    let manifest = |denies: &str| {
        PluginManifest::from_toml_str(&format!(
            "name = \"gates\"\n[panel]\nsurfaces = [\"overlay\"]\ndenies = [{denies}]"
        ))
    };

    let loaded = manifest("\"network\", \"write-outside-sandbox\"").expect("loads");
    let panel = loaded.panel.expect("the block parsed");
    assert!(panel.denies(PanelDenial::Network));
    assert!(panel.denies(PanelDenial::WriteOutsideSandbox));
    assert_eq!(panel.missing_denial(), None);
    // Declaring a panel buys no say in a turn, and needs none.
    assert_eq!(
        loaded.loop_grant.participation,
        stella_plugin::Participation::None
    );

    assert!(matches!(
        manifest("\"network\"").expect_err("a panel that keeps its filesystem"),
        stella_plugin::ManifestError::PanelDenialMissing {
            denial: PanelDenial::WriteOutsideSandbox
        }
    ));
    assert!(matches!(
        manifest("").expect_err("a panel that accepts nothing"),
        stella_plugin::ManifestError::PanelDenialMissing {
            denial: PanelDenial::Network
        }
    ));
    assert!(matches!(
        manifest("\"network\", \"network\", \"write-outside-sandbox\"")
            .expect_err("a repeated limit is an editing mistake"),
        stella_plugin::ManifestError::DuplicatePanelDenial {
            denial: PanelDenial::Network
        }
    ));
    let err = manifest("\"read-anything\"").expect_err("the denial set is Stella's");
    assert!(err.to_string().contains("read-anything"), "got {err}");

    assert!(
        PluginManifest::from_toml_str("name = \"bundle\"")
            .expect("loads")
            .panel
            .is_none(),
        "no [panel] block is no panel, and never an empty one"
    );
}

/// **The witness for #5203.** A `[panel]` block says where it draws, and a
/// block that names nowhere, names one place twice, or promises a `/name` it
/// has no popup for is refused by the rule it broke.
#[test]
fn a_panel_block_says_where_it_draws() {
    let manifest = |block: &str| {
        PluginManifest::from_toml_str(&format!(
            "name = \"gates\"\n[panel]\n{block}\ndenies = [\"network\", \
             \"write-outside-sandbox\"]"
        ))
    };

    let loaded = manifest("surfaces = [\"settings\", \"command\"]").expect("loads");
    let panel = loaded.panel.expect("the block parsed");
    assert!(panel.draws(PanelSurface::Settings));
    assert!(panel.draws(PanelSurface::Command));
    assert!(!panel.draws(PanelSurface::Overlay));
    // Undeclared, so the name falls back to the identity a human consented to.
    assert_eq!(panel.command_or(&loaded.name), Some("gates"));

    assert!(matches!(
        manifest("surfaces = []").expect_err("a panel that draws nowhere"),
        stella_plugin::ManifestError::PanelNoSurface
    ));
    assert!(matches!(
        manifest("").expect_err("an absent list is an empty one"),
        stella_plugin::ManifestError::PanelNoSurface
    ));
    assert!(matches!(
        manifest("surfaces = [\"overlay\", \"overlay\"]")
            .expect_err("a repeated placement is an editing mistake"),
        stella_plugin::ManifestError::PanelDuplicateSurface {
            surface: PanelSurface::Overlay
        }
    ));
    let err = manifest("surfaces = [\"status_bar\"]").expect_err("the placements are Stella's");
    assert!(err.to_string().contains("status_bar"), "got {err}");

    // A slash name with no popup to open is a promise the interface will never
    // keep, so it is refused rather than left as a key that quietly does
    // nothing.
    assert!(matches!(
        manifest("surfaces = [\"settings\"]\ncommand = \"hello\"")
            .expect_err("a name with nothing to open"),
        stella_plugin::ManifestError::PanelCommandWithoutSurface { command } if command == "hello"
    ));
}

/// **The witness for #5210.** A plugin drawing several surfaces gets several
/// leases a tick, and a frame says which one it answers — so a host with three
/// in flight routes the answer instead of guessing, and cannot blit a settings
/// pane into a command popup that happens to be the same size.
#[test]
fn a_frame_for_one_surface_does_not_answer_another_surfaces_lease() {
    let leased = PanelLease::new("gates", PanelSurface::Settings, 7, PanelRect::new(8, 2), 33);
    let answer = |surface, tick| PanelFrame::new(surface, tick, PanelPaint::Lines(vec![row("ok")]));

    assert_eq!(leased.admits(&answer(PanelSurface::Settings, 7)), Ok(()));

    // The same frame, the same size, the wrong panel. Every cell of it is
    // inside the lease, so geometry alone would have drawn it.
    let wrong = answer(PanelSurface::Command, 7);
    assert_eq!(
        wrong.fits(leased.rect),
        Ok(()),
        "it fits — that is the point"
    );
    assert_eq!(
        leased.admits(&wrong),
        Err(PanelRefusal::Surface {
            leased: PanelSurface::Settings,
            answered: PanelSurface::Command,
        })
    );

    // And a frame that answers a tick the host has moved on from is refused
    // rather than drawn late.
    assert_eq!(
        leased.admits(&answer(PanelSurface::Settings, 6)),
        Err(PanelRefusal::Tick {
            leased: 7,
            answered: 6,
        })
    );

    // The lease names the plugin and the surface, and only the pair
    // disambiguates: one plugin, three leases, alike in the `panel` field.
    let other = PanelLease::new("gates", PanelSurface::Command, 7, PanelRect::new(8, 2), 33);
    assert_eq!(other.panel, leased.panel);
    assert_ne!(other.surface, leased.surface);
    assert_eq!(other.admits(&wrong), Ok(()));
}

/// **The witness for the name half of #5203.** The plugin's name is what Stella
/// composes into its own chrome, so a name that could carry an escape sequence
/// there would make [`PanelText`]'s guarantee worthless — the label around a
/// panel's glyphs would be the way in.
#[test]
fn a_plugin_name_that_is_not_drawable_does_not_load() {
    for hazard in [
        "\\u001b[2J",
        "gates\\u001b[31m",
        "two\\nlines",
        "tab\\there",
    ] {
        let err = PluginManifest::from_toml_str(&format!("name = \"{hazard}\""))
            .expect_err("a name Stella cannot print into its own chrome");
        assert!(
            matches!(err, stella_plugin::ManifestError::NameNotDrawable { .. }),
            "{hazard:?} loaded, or was refused by the wrong rule: {err}"
        );
    }

    // The refusal names the position in `char`s, so a multi-byte glyph before
    // the hazard does not shift it into a byte offset nobody can navigate by —
    // `PanelText`'s rule, because it is literally the same predicate.
    assert!(matches!(
        PluginManifest::from_toml_str("name = \"✦\\u001b\"").expect_err("refused"),
        stella_plugin::ManifestError::NameNotDrawable { index: 1, code: 27 }
    ));

    // An ordinary name still loads, including one with punctuation and
    // non-ASCII letters — the rule is drawability, not an allowlist.
    for fine in ["gates", "gate-status", "Gate Status", "ゲート", "stella*"] {
        assert!(
            PluginManifest::from_toml_str(&format!("name = \"{fine}\"")).is_ok(),
            "{fine:?} is drawable and should load"
        );
    }
}

/// A slash name is typed by a person and registered by a host, so the block is
/// held to the shape both need — and to nothing beyond it, because the
/// reserved-name check is `stella-cli`'s (#5055).
#[test]
fn a_panel_slash_name_is_held_to_its_shape() {
    let manifest = |block: &str| {
        PluginManifest::from_toml_str(&format!(
            "name = \"gates\"\n[panel]\nsurfaces = [\"command\"]\n{block}\ndenies = \
             [\"network\", \"write-outside-sandbox\"]"
        ))
    };

    let loaded = manifest("command = \"gate-status\"").expect("loads");
    let panel = loaded.panel.expect("the block parsed");
    assert_eq!(panel.command_or(&loaded.name), Some("gate-status"));

    // The block carries no caption at all, and that absence is the
    // anti-spoofing rule: the label is the plugin's own name, so a panel
    // cannot call itself `GATES`.
    let err = manifest("title = \"GATES\"").expect_err("a panel does not name itself");
    assert!(err.to_string().contains("title"), "got {err}");

    // The namespaced form is real, and it is derived rather than declared —
    // its own refusal, because it is the mistake an author makes on purpose.
    assert!(matches!(
        manifest("command = \"gates:status\"").expect_err("the namespace is Stella's to add"),
        stella_plugin::ManifestError::PanelCommandCarriesNamespace { command }
            if command == "gates:status"
    ));
    for (name, found, index) in [
        ("Gates", 'G', 0usize),
        ("-gates", '-', 0),
        ("2gates", '2', 0),
        ("gate status", ' ', 4),
        ("gate_status", '_', 4),
        ("/gates", '/', 0),
    ] {
        assert!(
            matches!(
                manifest(&format!("command = \"{name}\"")).expect_err("not a typable slug"),
                stella_plugin::ManifestError::PanelCommandNotASlug { found: f, index: i }
                    if f == found && i == index
            ),
            "{name:?} loaded, or was refused by the wrong rule"
        );
    }
    assert!(matches!(
        manifest("command = \"\"").expect_err("a popup with no name"),
        stella_plugin::ManifestError::PanelCommandBlank
    ));

    // And the shape check stops at the shape: `stella-plugin` is a near-leaf
    // that cannot see `DECK_BUILTINS`, so a name colliding with a built-in
    // loads here and is the host's to refuse out loud.
    assert!(
        manifest("command = \"model\"").is_ok(),
        "the reserved-name check belongs to the host, not to the contract crate"
    );
}
