//! Witness for the dead-code removal in `memory/replay/cc_adapter.rs`.
//!
//! The adapter used to carry a `derived_message` helper with zero call sites,
//! kept alive only by an `#[allow(dead_code)]` suppression. That suppression
//! is the defect this test guards: if dead code is reintroduced here behind
//! an `allow`, this test fails. A green run means the module compiles on its
//! own merits — every item reachable, no lint overrides hiding drift.

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
