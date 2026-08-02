// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Errors, which are the real leak — `docs/design/diagnostics.md` §5.3.
//!
//! §5.2 stops a `String` reaching a field. That closes the obvious door and
//! leaves the one people actually walk through: **error messages**.
//! `format!("{e}")` on an `io::Error` carries the filename. A provider error's
//! `Display` carries the response body. A tool error's carries the command and
//! its stderr. Every one of those is a `String` that looks like diagnostics and
//! is really content.
//!
//! Invariant 5 already solved this without anyone noticing: **every library
//! error in this workspace is a typed enum**. So an error can be recorded as
//! *which variant it was*, plus fields explicitly marked safe — and the
//! `Display` string is simply never produced.
//!
//! ```
//! # use stella_diag::{log_error, Loggable};
//! # #[derive(Debug)]
//! # enum ModelError {
//! #     HttpStatus { status: u16, body: String },
//! #     Decode { detail: String },
//! #     Io(std::io::Error),
//! # }
//! log_error! {
//!     ModelError {
//!         // `status` is a number and rides along; `body` is not named, so it
//!         // cannot reach the record even though the variant carries it.
//!         HttpStatus { status } => "http_status",
//!         // No fields at all: the variant name is the whole diagnosis.
//!         Decode { .. } => "decode",
//!         // A newtype whose payload is mapped to something safe.
//!         Io(source) => "io" { kind = source.kind() },
//!     }
//! }
//!
//! let error = ModelError::HttpStatus { status: 429, body: String::from("rate limited: key sk-…") };
//! let json = serde_json::to_string(&error.to_field()).unwrap();
//! assert_eq!(json, r#"{"code":"http_status","status":429}"#);
//! ```
//!
//! In a codebase where errors were `String`s this would be a large piece of
//! work. Here invariant 5 paid for it years ago, and the macro is mostly
//! bookkeeping.
//!
//! ## What the shape buys
//!
//! A recorded error is `{"code": "...", ...fields}` — a **stable code** plus
//! scalars. Operators alert on the code (§11), and it survives every rewording
//! of the message a human sees, because the message and the record no longer
//! share a source.

use crate::field::{FieldValue, StaticStr};

/// Build the `{"code": …}` map an error records as.
///
/// Exposed for [`log_error!`] to call; use the macro rather than this.
#[doc(hidden)]
#[must_use]
pub fn error_field(code: &'static str, fields: Vec<(StaticStr, FieldValue)>) -> FieldValue {
    let mut entries = Vec::with_capacity(fields.len() + 1);
    entries.push((
        StaticStr::new("code"),
        FieldValue::Str(StaticStr::new(code)),
    ));
    entries.extend(fields);
    FieldValue::Map(entries)
}

/// Make a typed error [`Loggable`](crate::Loggable) by naming its variants and
/// the fields of each that are safe to record.
///
/// Every variant must be listed — the generated `match` is exhaustive, so
/// adding a variant to the error is a compile error here until someone decides
/// what it records as. That is the point: a new error variant should not
/// silently become invisible in the logs, and it should not silently start
/// leaking either.
///
/// Three variant forms:
///
/// | Form | Records |
/// |---|---|
/// | `Variant { a, b } => "code"` | `{"code":"code","a":…,"b":…}` — the named bindings must be [`Loggable`](crate::Loggable) |
/// | `Variant { .. } => "code"` | `{"code":"code"}` — the variant name alone |
/// | `Variant(binding) => "code" { k = expr }` | `{"code":"code","k":…}` — for newtypes and computed fields |
///
/// The third form is where an `io::Error` becomes its
/// [`ErrorKind`](std::io::ErrorKind) and nothing more. Note that whatever
/// `expr` evaluates to must still implement [`Loggable`](crate::Loggable), so
/// the escape from a typed error into a `String` is closed here too.
///
/// A unit variant is written `Variant {} => "code"`.
#[macro_export]
macro_rules! log_error {
    (
        $ty:ty {
            $(
                $variant:ident
                // `$(..)?` is matched and never transcribed, which is what lets
                // `{ .. }`, `{ status }`, `{ status, .. }` and `{}` all be
                // written at a call site while the expansion below always emits
                // the one pattern `{ bindings.., .. }`. A `$(..)?` in the
                // *expansion* would be a repetition over no metavariable, which
                // is an error — the reason this is shaped the way it is.
                $( { $($binding:ident),* $(,)? $(..)? } )?
                $( ( $($tuple:ident),* $(,)? ) )?
                => $code:literal
                $( { $($extra:ident = $value:expr),* $(,)? } )?
            ),* $(,)?
        }
    ) => {
        impl $crate::Loggable for $ty {
            // The allows sit on the GENERATED function, not at the call site:
            // a public macro that makes its user's `-D warnings` build fail is
            // a macro nobody uses. `vec_init_then_push` is unavoidable here —
            // the field list is assembled from two independent repetitions, so
            // it cannot be one `vec!` literal.
            #[allow(clippy::vec_init_then_push)]
            fn to_field(&self) -> $crate::FieldValue {
                #[allow(unused_mut, unused_variables)]
                match self {
                    $(
                        Self::$variant
                            $( { $($binding,)* .. } )?
                            $( ( $($tuple),* ) )?
                        => {
                            let mut fields: ::std::vec::Vec<(
                                $crate::StaticStr,
                                $crate::FieldValue,
                            )> = ::std::vec::Vec::new();
                            $($(fields.push((
                                $crate::StaticStr::new(::core::stringify!($binding)),
                                $crate::Loggable::to_field($binding),
                            ));)*)?
                            $($(fields.push((
                                $crate::StaticStr::new(::core::stringify!($extra)),
                                $crate::Loggable::to_field(&$value),
                            ));)*)?
                            $crate::error_field($code, fields)
                        }
                    ),*
                }
            }
        }
    };
}

/// `std::io::ErrorKind` records as its name and never as its message.
///
/// This is the single most valuable impl in the crate for §5.3's purposes: an
/// `io::Error`'s `Display` names the file it failed on, and its `ErrorKind`
/// answers the operational question — was it missing, was it a permission
/// problem, did it time out — without naming anything.
impl crate::Loggable for std::io::ErrorKind {
    fn to_field(&self) -> FieldValue {
        // Through an explicit table rather than `Debug` or `Display`. `Display`
        // renders prose ("entity not found") that can change between Rust
        // releases, and `Debug` renders a variant name this crate would then be
        // promising to keep stable on the standard library's behalf. A code is
        // only worth alerting on if we own its spelling (§11).
        FieldValue::Str(StaticStr::new(io_kind_name(*self)))
    }
}

/// The stable spelling of an [`std::io::ErrorKind`].
///
/// The list is the kinds a coding agent actually distinguishes; anything else
/// records as `other`, which keeps the vocabulary closed even as the standard
/// library adds kinds (it is a `#[non_exhaustive]` enum, so this cannot be an
/// exhaustive match and must not pretend to be).
fn io_kind_name(kind: std::io::ErrorKind) -> &'static str {
    use std::io::ErrorKind as K;
    match kind {
        K::NotFound => "not_found",
        K::PermissionDenied => "permission_denied",
        K::ConnectionRefused => "connection_refused",
        K::ConnectionReset => "connection_reset",
        K::ConnectionAborted => "connection_aborted",
        K::NotConnected => "not_connected",
        K::AddrInUse => "addr_in_use",
        K::AddrNotAvailable => "addr_not_available",
        K::BrokenPipe => "broken_pipe",
        K::AlreadyExists => "already_exists",
        K::WouldBlock => "would_block",
        K::InvalidInput => "invalid_input",
        K::InvalidData => "invalid_data",
        K::TimedOut => "timed_out",
        K::WriteZero => "write_zero",
        K::Interrupted => "interrupted",
        K::Unsupported => "unsupported",
        K::UnexpectedEof => "unexpected_eof",
        K::OutOfMemory => "out_of_memory",
        K::IsADirectory => "is_a_directory",
        K::NotADirectory => "not_a_directory",
        K::StorageFull => "storage_full",
        K::ReadOnlyFilesystem => "read_only_filesystem",
        K::FileTooLarge => "file_too_large",
        K::ResourceBusy => "resource_busy",
        K::Deadlock => "deadlock",
        K::CrossesDevices => "crosses_devices",
        K::TooManyLinks => "too_many_links",
        K::ArgumentListTooLong => "argument_list_too_long",
        _ => "other",
    }
}

/// An `io::Error` records as its kind. The message — which names the file — is
/// never produced.
impl crate::Loggable for std::io::Error {
    fn to_field(&self) -> FieldValue {
        error_field(
            "io",
            vec![(
                StaticStr::new("kind"),
                crate::Loggable::to_field(&self.kind()),
            )],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Fields, Loggable};

    /// The unread fields are the whole point: they exist on the value, and
    /// the tests below prove they cannot reach a record. Reading them would
    /// defeat the demonstration.
    #[derive(Debug)]
    #[allow(dead_code, reason = "unread fields are the property under test")]
    enum ModelError {
        HttpStatus { status: u16, body: String },
        RateLimited { status: u16, retry_after_s: u64 },
        Decode { detail: String },
        Io(std::io::Error),
        Cancelled,
    }

    log_error! {
        ModelError {
            HttpStatus { status } => "http_status",
            RateLimited { status, retry_after_s } => "rate_limited",
            Decode { .. } => "decode",
            Io(source) => "io" { kind = source.kind() },
            Cancelled {} => "cancelled",
        }
    }

    /// The headline of §5.3: the variant's own message-bearing field exists on
    /// the value and cannot reach the record, because it was never named.
    #[test]
    fn an_unnamed_field_cannot_reach_the_record() {
        let error = ModelError::HttpStatus {
            status: 429,
            body: String::from("rate limited for key sk-live-DEADBEEF"),
        };
        let json = serde_json::to_string(&error.to_field()).expect("serialize");
        assert_eq!(json, r#"{"code":"http_status","status":429}"#);
        assert!(!json.contains("sk-live"), "{json}");
    }

    /// A variant with nothing safe to say still says which variant it was,
    /// which is usually the whole diagnosis.
    #[test]
    fn a_variant_with_no_safe_fields_records_its_name() {
        let json = serde_json::to_string(
            &ModelError::Decode {
                detail: String::from("expected `,` at line 4 of the user's file"),
            }
            .to_field(),
        )
        .expect("serialize");
        assert_eq!(json, r#"{"code":"decode"}"#);
    }

    #[test]
    fn a_unit_variant_records_its_name() {
        assert_eq!(
            serde_json::to_string(&ModelError::Cancelled.to_field()).expect("serialize"),
            r#"{"code":"cancelled"}"#
        );
    }

    /// §5.3's own example: an `io::Error` becomes its kind, never its message,
    /// because the message is where the filename lives.
    #[test]
    fn an_io_error_records_its_kind_and_not_the_file_it_names() {
        let io = std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "/home/ada/.ssh/id_ed25519: permission denied",
        );
        let json = serde_json::to_string(&ModelError::Io(io).to_field()).expect("serialize");
        assert_eq!(json, r#"{"code":"io","kind":"permission_denied"}"#);
        assert!(!json.contains("ada"), "{json}");
    }

    #[test]
    fn several_safe_fields_all_ride_along() {
        let json = serde_json::to_string(
            &ModelError::RateLimited {
                status: 429,
                retry_after_s: 30,
            }
            .to_field(),
        )
        .expect("serialize");
        assert_eq!(
            json,
            r#"{"code":"rate_limited","status":429,"retry_after_s":30}"#
        );
    }

    #[test]
    fn a_recorded_error_nests_cleanly_inside_a_records_fields() {
        let fields = Fields::new().with("attempt", 2_u32).with(
            "error",
            ModelError::RateLimited {
                status: 429,
                retry_after_s: 30,
            },
        );
        let json = serde_json::to_string(&fields).expect("serialize");
        assert_eq!(
            json,
            r#"{"attempt":2,"error":{"code":"rate_limited","status":429,"retry_after_s":30}}"#
        );
        // And it survives the round trip a crash file depends on.
        let back: Fields = serde_json::from_str(&json).expect("parse");
        assert_eq!(back, fields);
    }

    /// `ErrorKind` is `#[non_exhaustive]`, so the mapping must stay total
    /// rather than pretend to be exhaustive.
    #[test]
    fn every_common_io_kind_has_a_stable_spelling() {
        use std::io::ErrorKind as K;
        for (kind, expected) in [
            (K::NotFound, "not_found"),
            (K::PermissionDenied, "permission_denied"),
            (K::TimedOut, "timed_out"),
            (K::BrokenPipe, "broken_pipe"),
            (K::UnexpectedEof, "unexpected_eof"),
        ] {
            assert_eq!(io_kind_name(kind), expected);
        }
        // A kind outside the reviewed list still records, as `other`.
        assert_eq!(io_kind_name(K::Other), "other");
    }

    /// A bare `io::Error` is loggable directly, not only inside a facet.
    #[test]
    fn an_io_error_is_loggable_on_its_own() {
        let error = std::io::Error::from(std::io::ErrorKind::TimedOut);
        assert_eq!(
            serde_json::to_string(&error.to_field()).expect("serialize"),
            r#"{"code":"io","kind":"timed_out"}"#
        );
    }
}
