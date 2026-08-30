//! The stylesheet's corners: overridable, not squared outright.
//!
//! A repo-wide rule squared every other web surface stella renders; this one never
//! took the rule, and its ten `border-radius` declarations were plain
//! literals — no host could change them without editing the file. Squaring
//! them outright was the other option and was rejected here: it would change
//! what the Observatory renders today, which this file's own header already
//! promises stays byte-for-byte identical to the instrument palette it
//! shares. Reading each corner from a `--radius-*` custom property with the
//! literal as its fallback gets both — nothing changes until a host sets one.

use crate::html::STYLE;

/// Every corner in the base sheet reads from a `--radius-*` property, and its
/// fallback is the exact value the page has always rendered — so a host that
/// sets nothing sees nothing different.
#[test]
fn every_corner_is_a_custom_property_with_its_old_value_as_fallback() {
    for (property, fallback) in [
        ("--radius-panel", "10px"),
        ("--radius-control", "6px"),
        ("--radius-pill", "99px"),
        ("--radius-tag", "4px"),
        ("--radius-dot", "50%"),
        ("--radius-inset", "8px"),
        ("--radius-chip", "2px"),
    ] {
        let declaration = format!("border-radius: var({property}, {fallback})");
        assert!(
            STYLE.contains(&declaration),
            "`{declaration}` is missing — a corner lost its override or its \
             original value"
        );
    }
}

/// No corner is still a bare literal. This is the exact shape that blocked
/// embedding this sheet into a page asserting square corners elsewhere in
/// the tree: a literal cannot be overridden, only edited.
#[test]
fn no_corner_is_a_bare_literal_any_more() {
    for bare in [
        "border-radius: 10px",
        "border-radius: 8px",
        "border-radius: 6px",
        "border-radius: 4px",
        "border-radius: 2px",
        "border-radius: 99px",
        "border-radius: 50%",
    ] {
        assert!(
            !STYLE.contains(bare),
            "`{bare}` still ships as a literal a host cannot override"
        );
    }
}

/// The three sites that already shared one pixel value before this change
/// share one property now too, rather than three independent ones that
/// happen to agree — the whole point of naming the value once.
#[test]
fn sites_that_shared_a_value_now_share_one_property() {
    let chip_sites = STYLE
        .matches("border-radius: var(--radius-chip, 2px)")
        .count();
    assert_eq!(
        chip_sites, 3,
        "`.fstat`, `.dw` and `.aw` were all `2px`; they should read the same property"
    );
    let control_sites = STYLE
        .matches("border-radius: var(--radius-control, 6px)")
        .count();
    assert_eq!(
        control_sites, 2,
        "`.zoom` and `.inspect` were both `6px`; they should read the same property"
    );
}
