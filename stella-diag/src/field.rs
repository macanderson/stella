// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The headline of `docs/design/diagnostics.md` §5.2: **content in a log does
//! not compile.**
//!
//! A field value is not `serde_json::Value` and not `impl Display`. It is
//! [`FieldValue`], a closed algebra whose only string variant holds a
//! [`StaticStr`] — a string that provably lives in the binary. [`Loggable`] is
//! implemented for the things that cannot carry runtime content (integers,
//! `bool`, `f64`, `Duration`, `&'static str`, closed enums declared through
//! [`log_enum!`](crate::log_enum), and the two reviewed escape hatches
//! [`PathClass`](crate::PathClass) and [`Redacted`](crate::Redacted)), and
//! deliberately **not** for `String`, a non-`'static` `&str`, `Path`,
//! `PathBuf`, `serde_json::Value`, or any `Display` blanket.
//!
//! So this is a type error rather than a privacy incident:
//!
//! ```compile_fail
//! # use stella_diag::{Dx, Cx, diag};
//! # let dx = Dx::disabled();
//! let user_path = String::from("/home/ada/.ssh/id_ed25519");
//! diag!(&dx, warn, "tools.write.denied", path = user_path);
//! ```
//!
//! `tracing` would happily log that with `%user_path`. Invariant 3's most
//! dangerous failure mode stops being a thing review has to catch and starts
//! being a thing the compiler catches. The witness lives in
//! `tests/compile_fail.rs` (property 1 of §12) because a runtime test cannot
//! observe a compile error.
//!
//! **Stated honestly, because overclaiming is worse than the gap:**
//! `Box::leak` turns a `String` into a `&'static str` and would defeat this.
//! That is a deliberate, single-token, greppable act, banned by the
//! `disallowed-methods` entry in the workspace `clippy.toml` that landed with
//! this module. The claim is not "impossible"; it is *"impossible by accident,
//! and loud on purpose"*.

use std::borrow::Cow;
use std::fmt;
use std::time::Duration;

use serde::de::{self, MapAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::cx::ShortId;

/// A string that provably lives in the binary.
///
/// The inner `Cow` is **private on purpose**, and it is the seal on §5.2:
/// [`StaticStr::new`] accepts only a `&'static str`, so no emit site can put a
/// runtime `String` into a record. The owned half of the `Cow` exists solely
/// so a record can be *read back* — `Deserialize` produces `Cow::Owned`,
/// because parsing a crash file is the one direction where the bytes really
/// are runtime data. Reading is not emitting.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StaticStr(Cow<'static, str>);

impl StaticStr {
    /// Wrap a string that lives in the binary.
    #[must_use]
    pub const fn new(text: &'static str) -> Self {
        Self(Cow::Borrowed(text))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for StaticStr {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, formatter)
    }
}

impl fmt::Display for StaticStr {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<&'static str> for StaticStr {
    fn from(text: &'static str) -> Self {
        Self::new(text)
    }
}

impl Serialize for StaticStr {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for StaticStr {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(|owned| Self(Cow::Owned(owned)))
    }
}

/// The reviewed escape hatch's payload — see [`Redacted`](crate::Redacted).
///
/// Both fields are private, which is what makes this the *only* runtime string
/// that can reach a record, reachable only through
/// [`Redacted::reviewed`](crate::Redacted::reviewed) and therefore only with a
/// justification written in the source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewedValue {
    reviewed: String,
    note: StaticStr,
}

impl ReviewedValue {
    pub(crate) fn new(reviewed: String, note: StaticStr) -> Self {
        Self { reviewed, note }
    }

    /// The reviewed content itself.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.reviewed
    }

    /// Why its author judged it safe to record.
    #[must_use]
    pub fn note(&self) -> &str {
        self.note.as_str()
    }
}

/// What a diagnostic field may hold. A closed algebra, and the reason a record
/// is content-free by construction.
///
/// The only variants that can carry a string are [`FieldValue::Str`], whose
/// payload is a [`StaticStr`], and [`FieldValue::Reviewed`], whose payload can
/// only be built by [`Redacted::reviewed`](crate::Redacted::reviewed). There is
/// no `Value`, no `Display`, and no `Debug` escape.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    Null,
    Bool(bool),
    /// A **negative** integer. Build with [`FieldValue::int`], which routes
    /// non-negative values to [`FieldValue::Uint`] so the Rust value and the
    /// JSON value agree in both directions.
    Int(i64),
    Uint(u64),
    /// Non-finite values are normalised to [`FieldValue::Null`] by the
    /// [`Loggable`] impls, so a record always round-trips (`NaN` has no JSON
    /// spelling and serde renders it as `null` on the way out but cannot read
    /// it back as a float).
    Float(f64),
    Str(StaticStr),
    /// A truncated correlation handle. Its own variant rather than a string
    /// because [`ShortId`] is the one runtime-derived value whose *width* is
    /// the safety argument (§7.2), and eight bytes is a type here, not a
    /// convention.
    Short(ShortId),
    List(Vec<FieldValue>),
    /// A nested object. Keys are [`StaticStr`], so the *shape* of a record is
    /// as content-free as its values. Insertion order is preserved.
    Map(Vec<(StaticStr, FieldValue)>),
    Reviewed(ReviewedValue),
}

impl Serialize for FieldValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_unit(),
            Self::Bool(value) => serializer.serialize_bool(*value),
            Self::Int(value) => serializer.serialize_i64(*value),
            Self::Uint(value) => serializer.serialize_u64(*value),
            Self::Float(value) => serializer.serialize_f64(*value),
            Self::Str(value) => value.serialize(serializer),
            Self::Short(value) => value.serialize(serializer),
            Self::List(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Self::Map(entries) => {
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                for (key, value) in entries {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
            Self::Reviewed(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for FieldValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(FieldValueVisitor)
    }
}

struct FieldValueVisitor;

/// The text of a value that was written as a JSON string, whichever variant it
/// read back as. Eight-hex-character strings land in [`FieldValue::Short`], so
/// matching on [`FieldValue::Str`] alone would miss them.
fn as_text(value: &FieldValue) -> Option<String> {
    match value {
        FieldValue::Str(text) => Some(text.as_str().to_owned()),
        FieldValue::Short(handle) => Some(handle.as_str().to_owned()),
        _ => None,
    }
}

impl<'de> Visitor<'de> for FieldValueVisitor {
    type Value = FieldValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a diagnostic field value")
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(FieldValue::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(FieldValue::Null)
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(FieldValue::Bool(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(FieldValue::Int(value))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(FieldValue::Uint(value))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Ok(FieldValue::Float(value))
    }

    /// A string of exactly eight lowercase hex characters reads back as a
    /// [`ShortId`], which is the only shape [`FieldValue::Short`] can have
    /// written. A `&'static str` field whose literal value happens to be eight
    /// lowercase hex digits therefore reads back as a `Short` — it
    /// re-serializes to identical bytes, so invariant 4 holds either way, and
    /// no consumer of a record distinguishes the two.
    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(ShortId::from_exact(value).map_or_else(
            || FieldValue::Str(StaticStr(Cow::Owned(value.to_owned()))),
            FieldValue::Short,
        ))
    }

    fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(FieldValue::List(items))
    }

    /// Objects are [`FieldValue::Map`], except for the exact two-key shape
    /// [`ReviewedValue`] writes. The discrimination is by key set rather than
    /// by a tag because the *serialized* form is the public surface (§11) and a
    /// tag would be noise in every operator's `jq`. A hand-built `Map` with
    /// precisely the keys `reviewed` and `note` would read back as a
    /// `Reviewed` — it re-serializes to identical bytes, so invariant 4 holds
    /// byte-for-byte either way; only the Rust-level variant differs, and no
    /// emit site in the workspace builds that key pair by hand.
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut entries: Vec<(StaticStr, FieldValue)> = Vec::new();
        while let Some((key, value)) = map.next_entry::<String, FieldValue>()? {
            entries.push((StaticStr(Cow::Owned(key)), value));
        }
        if entries.len() == 2
            && (entries[0].0.as_str(), entries[1].0.as_str()) == ("reviewed", "note")
            && let (Some(reviewed), Some(note)) = (as_text(&entries[0].1), as_text(&entries[1].1))
        {
            return Ok(FieldValue::Reviewed(ReviewedValue {
                reviewed,
                note: StaticStr(Cow::Owned(note)),
            }));
        }
        Ok(FieldValue::Map(entries))
    }
}

/// The types a diagnostic field may be built from.
///
/// Implemented only for values that cannot carry runtime content. The absence
/// of an impl is the feature: see the module docs, and resist the blanket
/// `impl<T: Display> Loggable for T` — that single impl is the entire hole §5.2
/// exists to close, and §14 records that it will be proposed by someone every
/// six months.
pub trait Loggable {
    fn to_field(&self) -> FieldValue;
}

impl FieldValue {
    /// A signed integer, in the crate's canonical spelling.
    ///
    /// JSON has one number type, so `-1` and `1` differ only in sign and a
    /// parser cannot tell `Int(1)` from `Uint(1)` on the way back —
    /// `serde_json` reports any non-negative integer as `u64`. Normalising
    /// *here*, at construction, is what makes [`FieldValue::Int`] mean
    /// "negative" and lets a record round-trip to an equal value rather than
    /// merely to equal bytes. Property 8 of §12 is what caught the difference.
    #[must_use]
    pub fn int(value: i64) -> Self {
        u64::try_from(value).map_or(Self::Int(value), Self::Uint)
    }
}

macro_rules! loggable_int {
    ($($ty:ty),* $(,)?) => {
        $(impl Loggable for $ty {
            fn to_field(&self) -> FieldValue {
                FieldValue::int(i64::from(*self))
            }
        })*
    };
}

macro_rules! loggable_uint {
    ($($ty:ty),* $(,)?) => {
        $(impl Loggable for $ty {
            fn to_field(&self) -> FieldValue {
                FieldValue::Uint(u64::from(*self))
            }
        })*
    };
}

loggable_int!(i8, i16, i32, i64);
loggable_uint!(u8, u16, u32, u64);

impl Loggable for usize {
    fn to_field(&self) -> FieldValue {
        // `usize` is 64-bit on every target this workspace ships to, but the
        // cast is saturating rather than `as` so a 128-bit future cannot make
        // a count silently wrap into a negative-looking number.
        FieldValue::Uint(u64::try_from(*self).unwrap_or(u64::MAX))
    }
}

impl Loggable for isize {
    fn to_field(&self) -> FieldValue {
        FieldValue::int(i64::try_from(*self).unwrap_or(i64::MAX))
    }
}

impl Loggable for bool {
    fn to_field(&self) -> FieldValue {
        FieldValue::Bool(*self)
    }
}

impl Loggable for f32 {
    fn to_field(&self) -> FieldValue {
        f64::from(*self).to_field()
    }
}

impl Loggable for f64 {
    /// Exact across a round trip for every finite `f64`, including
    /// full-precision ones — but only because this crate's manifest enables
    /// serde_json's `float_roundtrip`. Its default parser is lossy by one ULP
    /// for a large fraction of doubles, which would make invariant 4 false for
    /// any record carrying a float. See `Cargo.toml`.
    fn to_field(&self) -> FieldValue {
        if self.is_finite() {
            FieldValue::Float(*self)
        } else {
            // JSON has no spelling for NaN or ±∞. Recording `null` keeps the
            // field present (its absence would read as "never measured") and
            // keeps the record round-trippable, which infinity would not be.
            FieldValue::Null
        }
    }
}

impl Loggable for Duration {
    /// Milliseconds, matching the workspace's existing `ts`/`duration_ms`
    /// convention (`stella-serve::observe::event::millis`).
    fn to_field(&self) -> FieldValue {
        FieldValue::Uint(u64::try_from(self.as_millis()).unwrap_or(u64::MAX))
    }
}

impl Loggable for &'static str {
    fn to_field(&self) -> FieldValue {
        FieldValue::Str(StaticStr::new(self))
    }
}

impl Loggable for StaticStr {
    fn to_field(&self) -> FieldValue {
        FieldValue::Str(self.clone())
    }
}

impl<T: Loggable> Loggable for Option<T> {
    /// `None` records as `null` rather than omitting the field: "we looked and
    /// there was nothing" and "we never looked" are different diagnoses, and a
    /// missing key cannot tell them apart.
    fn to_field(&self) -> FieldValue {
        self.as_ref().map_or(FieldValue::Null, Loggable::to_field)
    }
}

impl<T: Loggable> Loggable for &T {
    fn to_field(&self) -> FieldValue {
        (*self).to_field()
    }
}

impl<T: Loggable> Loggable for Vec<T> {
    fn to_field(&self) -> FieldValue {
        FieldValue::List(self.iter().map(Loggable::to_field).collect())
    }
}

impl<T: Loggable> Loggable for [T] {
    fn to_field(&self) -> FieldValue {
        FieldValue::List(self.iter().map(Loggable::to_field).collect())
    }
}

/// The `fields` object of one record: ordered `(key, value)` pairs.
///
/// Keys are [`StaticStr`], so a record's shape is as content-free as its
/// values. Order is insertion order — the order the emit site wrote its
/// fields — which is what makes rendered records diffable across runs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fields(Vec<(StaticStr, FieldValue)>);

impl Fields {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a field. Duplicate keys are kept rather than replaced: a record
    /// that says the same thing twice is a bug worth seeing, not one worth
    /// hiding.
    #[must_use]
    pub fn with(mut self, key: &'static str, value: impl Loggable) -> Self {
        self.0.push((StaticStr::new(key), value.to_field()));
        self
    }

    /// Append an already-built value. Used by [`diag!`](crate::diag), which
    /// has called [`Loggable::to_field`] itself.
    pub fn push(&mut self, key: &'static str, value: FieldValue) {
        self.0.push((StaticStr::new(key), value));
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&FieldValue> {
        self.0
            .iter()
            .find(|(name, _)| name.as_str() == key)
            .map(|(_, value)| value)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &FieldValue)> {
        self.0.iter().map(|(key, value)| (key.as_str(), value))
    }
}

impl Serialize for Fields {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Fields {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match FieldValue::deserialize(deserializer)? {
            FieldValue::Map(entries) => Ok(Self(entries)),
            // The `reviewed`/`note` shape is a legal *field* object, so a
            // `fields` map that happens to contain exactly those two keys
            // parses as `Reviewed` above. Unfold it back rather than rejecting
            // the record: dropping a crash file on a key-set coincidence would
            // be the worst possible failure for the one artifact §7.4 exists to
            // make askable.
            FieldValue::Reviewed(value) => Ok(Self(vec![
                (
                    StaticStr::new("reviewed"),
                    FieldValue::Str(StaticStr(Cow::Owned(value.value().to_owned()))),
                ),
                (
                    StaticStr::new("note"),
                    FieldValue::Str(StaticStr(Cow::Owned(value.note().to_owned()))),
                ),
            ])),
            other => Err(de::Error::custom(format!(
                "diagnostic fields must be an object, got {other:?}"
            ))),
        }
    }
}

/// Declare a closed enum that may be logged.
///
/// A closed variant set is a finite, reviewed vocabulary — §5.2's third row —
/// so it is safe where a `String` is not. The generated type renders to its
/// `&'static str` spelling, which is the value that reaches the record and the
/// value operators match on.
///
/// ```
/// # use stella_diag::{log_enum, Loggable};
/// log_enum! {
///     /// Why a store migration retried.
///     pub enum RetryReason {
///         Locked => "locked",
///         Busy => "busy",
///     }
/// }
/// assert_eq!(RetryReason::Busy.as_static(), "busy");
/// ```
#[macro_export]
macro_rules! log_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident => $spelling:literal
            ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        $vis enum $name {
            $(
                $(#[$variant_meta])*
                $variant,
            )*
        }

        impl $name {
            /// This variant's stable spelling, as it appears in a record.
            #[must_use]
            pub const fn as_static(self) -> &'static str {
                match self {
                    $(Self::$variant => $spelling,)*
                }
            }
        }

        impl $crate::Loggable for $name {
            fn to_field(&self) -> $crate::FieldValue {
                $crate::FieldValue::Str($crate::StaticStr::new(self.as_static()))
            }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                formatter.write_str(self.as_static())
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(
                &self,
                serializer: S,
            ) -> ::core::result::Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_static())
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    log_enum! {
        /// A test vocabulary.
        pub enum Outcome {
            Hit => "hit",
            Miss => "miss",
        }
    }

    #[test]
    fn a_log_enum_records_its_stable_spelling() {
        assert_eq!(
            Outcome::Hit.to_field(),
            FieldValue::Str(StaticStr::new("hit"))
        );
        assert_eq!(Outcome::Miss.to_string(), "miss");
    }

    #[test]
    fn a_static_str_deserializes_owned_and_still_compares_by_content() {
        let json = "\"store.migration.retry\"";
        let parsed: StaticStr = serde_json::from_str(json).expect("parse");
        assert_eq!(parsed, StaticStr::new("store.migration.retry"));
    }

    /// Non-finite floats have no JSON spelling. Recording `null` keeps the
    /// field present *and* keeps the record readable; `NaN` would serialize to
    /// `null` anyway and then fail to parse back as a float.
    #[test]
    fn a_non_finite_float_records_as_null() {
        assert_eq!(f64::NAN.to_field(), FieldValue::Null);
        assert_eq!(f64::INFINITY.to_field(), FieldValue::Null);
        assert_eq!((1.5_f64).to_field(), FieldValue::Float(1.5));
    }

    #[test]
    fn fields_keep_insertion_order() {
        let fields = Fields::new()
            .with("attempt", 2_u64)
            .with("backoff_ms", 250_u64);
        let json = serde_json::to_string(&fields).expect("serialize");
        assert_eq!(json, r#"{"attempt":2,"backoff_ms":250}"#);
    }

    #[test]
    fn a_nested_map_round_trips() {
        let fields = Fields::new().with("attempt", 2_u64);
        let mut with_error = fields;
        with_error.push(
            "error",
            FieldValue::Map(vec![
                (
                    StaticStr::new("code"),
                    FieldValue::Str(StaticStr::new("Io")),
                ),
                (
                    StaticStr::new("kind"),
                    FieldValue::Str(StaticStr::new("PermissionDenied")),
                ),
            ]),
        );
        let json = serde_json::to_string(&with_error).expect("serialize");
        let back: Fields = serde_json::from_str(&json).expect("parse");
        assert_eq!(back, with_error);
    }

    /// `None` is a measurement, not an omission — the key stays.
    #[test]
    fn an_absent_option_records_as_null_not_as_a_missing_key() {
        let fields = Fields::new().with("status", None::<u16>);
        assert_eq!(
            serde_json::to_string(&fields).expect("serialize"),
            r#"{"status":null}"#
        );
    }

    #[test]
    fn a_duration_records_as_milliseconds() {
        assert_eq!(Duration::from_millis(250).to_field(), FieldValue::Uint(250));
    }
}
