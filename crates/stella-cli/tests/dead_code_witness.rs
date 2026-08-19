//! Witness for the dead-code removal in `memory/replay/cc_adapter.rs`.
//!
//! The adapter used to carry a `derived_message` helper with zero call sites,
//! kept alive only by an `#[allow(dead_code)]` suppression. That suppression
//! is the defect this test guards: if dead code is reintroduced here behind
//! an `allow`, this test fails. A green run means the module compiles on its
//! own merits — every item reachable, no lint overrides hiding drift.
//!
//! # Kept, though `make dead-code-allows` now covers the whole tree
//!
//! Deliberately **not** folded into that guard (#3949), because it is stricter
//! rather than narrower, in two ways the general one cannot express:
//!
//! * The gate permits a suppression that **says why the lint is wrong here**,
//!   and counts it against a per-crate ratchet. This file permits none at all,
//!   at any justification, because the justification is what went wrong here
//!   the first time: the helper was kept, plausibly argued for, and had zero
//!   callers the whole while.
//! * The second assertion is about one *identifier*, not about suppressions.
//!   No tree-wide guard can know that `derived_message` specifically must not
//!   come back; that is knowledge about this module, and it belongs beside it.
//!
//! The general guard is the floor. This is the one file the floor is not
//! considered enough for.

use std::path::PathBuf;

fn cc_adapter_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/memory/replay/cc_adapter.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

#[test]
fn cc_adapter_has_no_dead_code_suppressions() {
    let source = cc_adapter_source();
    assert!(
        !source.contains("#[allow(dead_code)]"),
        "cc_adapter.rs carries an #[allow(dead_code)] suppression — \
         dead code must be deleted, not silenced"
    );
}

#[test]
fn cc_adapter_has_no_derived_message_helper() {
    let source = cc_adapter_source();
    assert!(
        !source.contains("derived_message"),
        "derived_message was removed as dead code (zero call sites); \
         do not reintroduce it without a caller"
    );
}
