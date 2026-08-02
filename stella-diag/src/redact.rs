// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The escape hatch, made expensive on purpose — `docs/design/diagnostics.md`
//! §5.5.
//!
//! §5.2 makes runtime content a compile error. Some runtime strings are
//! genuinely safe to record and genuinely useful (a git ref name, a provider
//! id an operator chose, a filter clause they typed), so there has to be a way
//! through. The design question is not *whether* to have one — it is how to
//! stop it becoming the default.
//!
//! ```
//! # use stella_diag::{note, Redacted};
//! # let branch_name = String::from("main");
//! Redacted::reviewed(
//!     branch_name,
//!     note!("git ref names are user-chosen but are not model content"),
//! );
//! ```
//!
//! Three properties make this a hatch rather than a loophole:
//!
//! 1. [`note!`] accepts **only a string literal**, so the justification lives
//!    in the source, is greppable, and shows up in the diff that introduced it.
//!    A `format!` or a variable does not compile.
//! 2. The justification travels *into the record*, so a maintainer reading a
//!    crash file can see why a string is in it without opening the source.
//! 3. `Redacted::reviewed` is a fixed, greppable token. §5.5 calls for a gate
//!    script counting the sites against a per-crate ceiling that can only fall,
//!    the same ratchet idiom `check-file-size.sh` already uses; that script is
//!    slice 5 and this type is what it will count.

use crate::field::{FieldValue, Loggable, ReviewedValue, StaticStr};

/// Why an author judged a runtime string safe to record.
///
/// Constructible only through [`note!`](crate::note), and therefore only from
/// a literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Note(&'static str);

impl Note {
    /// Not for direct use — call [`note!`](crate::note), which is what constrains the
    /// argument to a literal. Calling this with a `Box::leak`ed string would
    /// defeat the constraint, which is exactly why the workspace `clippy.toml`
    /// disallows that method.
    #[must_use]
    pub const fn new(text: &'static str) -> Self {
        Self(text)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// Write the justification for recording a runtime string.
///
/// Accepts a string literal and nothing else:
///
/// ```compile_fail
/// # use stella_diag::note;
/// # let reason = String::from("because");
/// note!(reason);
/// ```
#[macro_export]
macro_rules! note {
    ($justification:literal) => {
        $crate::Note::new($justification)
    };
}

/// A runtime string that a human reviewed and judged safe to record.
///
/// The name is deliberately uncomfortable. It reads as "this needed
/// redacting", because the question every one of these should provoke in
/// review is *did it?*
pub struct Redacted<T> {
    value: T,
    note: Note,
}

impl<T: std::fmt::Display> Redacted<T> {
    /// Record `value` verbatim, justified by `note`.
    ///
    /// The `Display` bound is the one place in this crate where `Display`
    /// reaches a record, and it is bounded by construction: reaching it
    /// requires naming this function and writing a literal justification next
    /// to it.
    #[must_use]
    pub const fn reviewed(value: T, note: Note) -> Self {
        Self { value, note }
    }
}

impl<T: std::fmt::Display> Loggable for Redacted<T> {
    fn to_field(&self) -> FieldValue {
        FieldValue::Reviewed(ReviewedValue::new(
            self.value.to_string(),
            StaticStr::new(self.note.as_str()),
        ))
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for Redacted<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Redacted")
            .field("value", &self.value)
            .field("note", &self.note.as_str())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Fields;

    #[test]
    fn a_reviewed_value_records_itself_and_its_justification() {
        let fields = Fields::new().with(
            "branch",
            Redacted::reviewed(
                String::from("feature/logging"),
                note!("git ref names are user-chosen but are not model content"),
            ),
        );
        let json = serde_json::to_string(&fields).expect("serialize");
        assert_eq!(
            json,
            r#"{"branch":{"reviewed":"feature/logging","note":"git ref names are user-chosen but are not model content"}}"#
        );
    }

    /// The justification must survive a round trip, or a crash file loses the
    /// one thing that explains why a string is in it.
    #[test]
    fn a_reviewed_value_round_trips_with_its_note() {
        let fields = Fields::new().with(
            "provider",
            Redacted::reviewed("openrouter", note!("a provider id the operator configured")),
        );
        let json = serde_json::to_string(&fields).expect("serialize");
        let back: Fields = serde_json::from_str(&json).expect("parse");
        assert_eq!(back, fields);
        match back.get("provider") {
            Some(FieldValue::Reviewed(value)) => {
                assert_eq!(value.value(), "openrouter");
                assert_eq!(value.note(), "a provider id the operator configured");
            }
            other => panic!("expected a reviewed value, got {other:?}"),
        }
    }
}
