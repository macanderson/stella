//! Tests for [`super`], the panel wire contract.
//!
//! Split out of `panel.rs` at the gate's line ceiling. Only the tests moved.
//! The types they exercise stayed put, so the move changes no behavior.

use super::*;

fn text(glyphs: &str) -> PanelText {
    PanelText::new(glyphs).expect("plain glyphs")
}

#[test]
fn every_denial_has_a_distinct_wire_name_and_a_sentence() {
    let mut names: Vec<&str> = PanelDenial::all()
        .iter()
        .map(|denial| denial.as_str())
        .collect();
    let count = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), count, "two denials share a wire name");
    for denial in PanelDenial::all() {
        assert!(!denial.consent_sentence().trim().is_empty(), "{denial}");
    }
}

#[test]
fn all_is_exhaustive_over_the_denial_set() {
    // Round-trip every wire name through the deserializer. That shows
    // `all()` and the enum agree, with no count to keep by hand.
    for denial in PanelDenial::all() {
        let json = serde_json::to_string(denial).expect("a denial serializes");
        let back: PanelDenial = serde_json::from_str(&json).expect("and reads back");
        assert_eq!(back, *denial);
    }
    assert!(
        serde_json::from_str::<PanelDenial>("\"read-anything\"").is_err(),
        "the denial set is Stella's, so an unknown limit is a refusal"
    );
}

/// A grant that passes every rule, for a test that is about one of them.
fn grant(surfaces: Vec<PanelSurface>) -> PanelGrant {
    PanelGrant {
        surfaces,
        command: None,
        denies: PanelDenial::all().to_vec(),
        process: None,
    }
}

#[test]
fn a_grant_reports_the_first_denial_it_fails_to_name() {
    let partial = PanelGrant {
        denies: vec![PanelDenial::WriteOutsideSandbox],
        ..grant(vec![PanelSurface::Overlay])
    };
    assert_eq!(partial.missing_denial(), Some(PanelDenial::Network));
    let complete = grant(vec![PanelSurface::Overlay]);
    assert_eq!(complete.missing_denial(), None);
    assert!(complete.denies(PanelDenial::Network));
}

#[test]
fn every_surface_has_a_distinct_wire_name_and_a_sentence() {
    let mut names: Vec<&str> = PanelSurface::all()
        .iter()
        .map(|surface| surface.as_str())
        .collect();
    let count = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), count, "two surfaces share a wire name");
    for surface in PanelSurface::all() {
        assert!(!surface.consent_sentence().trim().is_empty(), "{surface}");
        let json = serde_json::to_string(surface).expect("a surface serializes");
        let back: PanelSurface = serde_json::from_str(&json).expect("and reads back");
        assert_eq!(back, *surface);
    }
    assert!(
        serde_json::from_str::<PanelSurface>("\"status_bar\"").is_err(),
        "the placements are Stella's, so an unknown one is a refusal"
    );
}

#[test]
fn a_panel_that_draws_nowhere_is_refused() {
    assert!(matches!(
        grant(Vec::new()).validate("gates"),
        Err(ManifestError::PanelNoSurface)
    ));
    assert!(matches!(
        grant(vec![PanelSurface::Settings, PanelSurface::Settings]).validate("gates"),
        Err(ManifestError::PanelDuplicateSurface {
            surface: PanelSurface::Settings
        })
    ));
    assert!(
        grant(vec![PanelSurface::Settings])
            .validate("gates")
            .is_ok()
    );
}

/// The name a panel registers is the one `command_or` resolves. So the
/// derived name is held to the same shape rules as a declared one.
///
/// A plugin with no `command` field registers its own name instead. A past
/// bug let `name = "vera:admin"` buy the namespace-shaped slash command the
/// explicit path refuses on its own. The consent text then told a reader
/// the panel answers to `/vera:admin` and to `/vera:admin:vera:admin`. Spaces
/// and capitals got through too, and registered a command nobody can type.
#[test]
fn a_derived_slash_name_is_held_to_the_same_shape_as_a_declared_one() {
    let derived = |name: &str| grant(vec![PanelSurface::Command]).validate(name);

    assert!(
        matches!(
            derived("vera:admin"),
            Err(ManifestError::PanelCommandCarriesNamespace { .. })
        ),
        "a plugin cannot buy the alias namespace by spelling it in its name"
    );
    assert!(matches!(
        derived("Vera Admin"),
        Err(ManifestError::PanelCommandNotASlug { .. })
    ));
    assert!(
        derived("vera").is_ok(),
        "an ordinary slug name still passes"
    );
    assert!(
        grant(vec![PanelSurface::Settings])
            .validate("vera:admin")
            .is_ok(),
        "a panel with no popup registers no name, so its plugin's name is \
         not a slash command and is not judged as one"
    );
}

#[test]
fn a_slash_name_resolves_only_for_a_panel_that_has_a_popup() {
    let popup = PanelGrant {
        command: Some("hello".to_string()),
        ..grant(vec![PanelSurface::Command])
    };
    assert_eq!(popup.command_or("gates"), Some("hello"));
    // Absent means the plugin's id, which is the product rule.
    assert_eq!(
        grant(vec![PanelSurface::Command]).command_or("gates"),
        Some("gates")
    );
    // And a panel with no popup registers no name, declared or defaulted.
    assert_eq!(
        grant(vec![PanelSurface::Settings]).command_or("gates"),
        None
    );
}

#[test]
fn a_run_of_glyphs_refuses_every_control_character() {
    for hazard in ["\u{1b}[2J", "a\nb", "a\rb", "a\tb", "a\u{9b}c"] {
        assert!(
            PanelText::new(hazard).is_err(),
            "{hazard:?} decoded as drawable glyphs"
        );
    }
    assert_eq!(
        PanelText::new("\u{1b}").unwrap_err(),
        PanelTextError::ControlCharacter { index: 0, code: 27 }
    );
    // The index counts `char`s. A multi-byte glyph before the hazard does
    // not turn it into a byte offset nobody can find.
    assert_eq!(
        PanelText::new("✦\u{1b}").unwrap_err(),
        PanelTextError::ControlCharacter { index: 1, code: 27 }
    );
}

#[test]
fn a_frame_of_lines_that_overruns_its_lease_is_refused() {
    let rect = PanelRect::new(8, 2);
    let one = || PanelLine::new(vec![PanelSpan::new(text("gates"), PanelStyle::plain())]);
    let fits = PanelPaint::Lines(vec![one(), one()]);
    assert_eq!(fits.fits(rect), Ok(()));

    let too_tall = PanelPaint::Lines(vec![one(), one(), one()]);
    assert_eq!(
        too_tall.fits(rect),
        Err(PanelOverflow::Rows { lines: 3, rows: 2 })
    );

    let too_wide = PanelPaint::Lines(vec![PanelLine::new(vec![PanelSpan::new(
        text("nine cells"),
        PanelStyle::plain(),
    )])]);
    assert_eq!(
        too_wide.fits(rect),
        Err(PanelOverflow::Line {
            line: 0,
            cells: 10,
            cols: 8,
        })
    );
}

#[test]
fn a_cell_diff_outside_the_lease_is_refused() {
    let rect = PanelRect::new(8, 2);
    let patch = |row, col, glyphs| PanelPatch::new(row, col, text(glyphs), PanelStyle::plain());

    assert_eq!(PanelPaint::Diff(vec![patch(1, 7, "x")]).fits(rect), Ok(()));
    assert_eq!(
        PanelPaint::Diff(vec![patch(2, 0, "x")]).fits(rect),
        Err(PanelOverflow::Row { row: 2, rows: 2 })
    );
    assert_eq!(
        PanelPaint::Diff(vec![patch(0, 6, "abc")]).fits(rect),
        Err(PanelOverflow::Patch {
            row: 0,
            col: 6,
            cells: 3,
            cols: 8,
        })
    );
}

#[test]
fn a_column_plus_a_long_run_cannot_wrap_back_into_the_lease() {
    // `u16::MAX` cells past the right edge is the addition that wraps in
    // `u16` and lands back inside a small rectangle. The check runs in
    // `usize`, so it refuses.
    let rect = PanelRect::new(4, 1);
    let long = text(&"x".repeat(usize::from(u16::MAX) + 8));
    let paint = PanelPaint::Diff(vec![PanelPatch::new(0, 2, long, PanelStyle::plain())]);
    assert!(matches!(
        paint.fits(rect),
        Err(PanelOverflow::Patch {
            col: 2,
            cols: 4,
            ..
        })
    ));
}

#[test]
fn an_empty_frame_fits_every_lease_including_an_empty_one() {
    let empty = PanelRect::new(0, 0);
    assert_eq!(PanelPaint::Lines(Vec::new()).fits(empty), Ok(()));
    assert_eq!(PanelPaint::Diff(Vec::new()).fits(empty), Ok(()));
    // And a lease with no cells admits no patch at all.
    let paint = PanelPaint::Diff(vec![PanelPatch::new(0, 0, text("x"), PanelStyle::plain())]);
    assert_eq!(
        paint.fits(empty),
        Err(PanelOverflow::Row { row: 0, rows: 0 })
    );
}

#[test]
fn a_patch_of_no_glyphs_is_still_anchored_inside_the_lease() {
    // A run of nothing writes nothing. So the *extent* check has no opinion
    // about where it starts: `col + 0` is inside every lease. But a host
    // reads the column first. Give it one its buffer lacks, and `fits` is
    // the only guard standing in the way.
    let rect = PanelRect::new(8, 2);
    let empty_run = |col| PanelPatch::new(0, col, text(""), PanelStyle::plain());

    assert_eq!(PanelPaint::Diff(vec![empty_run(7)]).fits(rect), Ok(()));
    assert_eq!(
        PanelPaint::Diff(vec![empty_run(8)]).fits(rect),
        Err(PanelOverflow::Patch {
            row: 0,
            col: 8,
            cells: 0,
            cols: 8,
        })
    );
    assert_eq!(
        PanelPaint::Diff(vec![empty_run(u16::MAX)]).fits(rect),
        Err(PanelOverflow::Patch {
            row: 0,
            col: u16::MAX,
            cells: 0,
            cols: 8,
        })
    );

    // The same rule read from the other end: a lease with rows but no
    // columns is a lease with no cells, so it admits no patch either.
    let columnless = PanelRect::new(0, 2);
    assert_eq!(
        PanelPaint::Diff(vec![empty_run(0)]).fits(columnless),
        Err(PanelOverflow::Patch {
            row: 0,
            col: 0,
            cells: 0,
            cols: 0,
        })
    );
}

#[test]
fn a_lease_admits_the_frame_that_answers_it() {
    let lease = PanelLease::new("gates", PanelSurface::Overlay, 7, PanelRect::new(12, 1), 33);
    let frame = PanelFrame::new(
        PanelSurface::Overlay,
        7,
        PanelPaint::Lines(vec![PanelLine::new(vec![PanelSpan::new(
            text("3 green"),
            PanelStyle::ink(PanelInk::Green),
        )])]),
    );
    assert_eq!(lease.admits(&frame), Ok(()));
    assert_eq!(frame.protocol_version, PROTOCOL_VERSION);
}
