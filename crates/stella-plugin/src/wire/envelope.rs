//! The two-key envelope every wrapper message arrives in, and the reader that
//! refuses a third key.
//!
//! Split out of [`crate::wire`] rather than grown inside it: that file is the
//! wire *vocabulary* — the requests, responses, grants and evidence a plugin
//! speaks — and this is the framing those types arrive in. Two subjects, and
//! the file-size ratchet is what made the distinction required rather than
//! tidy (AGENTS.md, "God files — plan around them, never into them").

use serde::{Deserialize, Deserializer};

use super::WrapperPoint;

/// The two keys an envelope may carry, and the only two.
///
/// **This is why the envelope is not simply
/// `#[derive(Deserialize)] #[serde(tag, content)]`** (#3500). Serde's reader
/// for an adjacently tagged enum ignores every key beside the tag and the
/// content, so `{"point": …, "body": …, "extra": 1}` decoded cleanly and the
/// `extra` vanished — while this crate's stated rule, enforced by
/// `deny_unknown_fields` on every manifest table, is that an unknown key is a
/// load error. The wire has the stronger claim on that rule than the manifest
/// does: a manifest is written by a human who can be told, and the wire is
/// spoken by a program that will not notice. Track C found the host accepting
/// what all three of its reference plugins refused.
///
/// Serde offers no `deny_unknown_fields` for an adjacently tagged enum, so the
/// strictness is hand-written here.
#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum EnvelopeField {
    Point,
    Body,
}

/// The two keys, in the order serde reports a missing one.
const ENVELOPE_FIELDS: &[&str] = &["point", "body"];

/// One envelope's two shapes, as the two ways its body can be read.
///
/// # Why a trait rather than one concrete `Envelope` struct
///
/// The first version of this held `body: serde_json::Value` until `point` had
/// been read, then re-deserialized out of that buffer. It cost two things,
/// both small and both real (#3518): a body decode error reached the author
/// through `serde::de::Error::custom`, which drops the `at line N column M` a
/// direct `from_slice` carries — the half a plugin author navigates by — and
/// every message paid an extra allocation and parse pass on a path carrying
/// `changed_files` lists and answer text.
///
/// The buffer is what made the two keys **order-free**, though, and that is
/// worth keeping: "point must come first" is a JSON contract nobody could obey
/// by accident, since a map is unordered in most languages' serializers. So
/// both paths exist and the visitor picks: [`Self::seed_body`] when `point`
/// arrived first (the order the host itself always writes, and the common
/// case), which deserializes straight from the input and keeps its position;
/// [`Self::from_value`] otherwise, out of the buffer, exactly as before.
pub(crate) trait FromEnvelope: Sized {
    /// Read the body directly from the map, now that `point` is known.
    fn seed_body<'de, A>(point: WrapperPoint, map: &mut A) -> Result<Self, A::Error>
    where
        A: serde::de::MapAccess<'de>;

    /// Read the body out of the buffer held while `point` was still unknown.
    fn from_value<E: serde::de::Error>(
        point: WrapperPoint,
        body: serde_json::Value,
    ) -> Result<Self, E>;
}

/// Decode one envelope body into the type its point names.
///
/// Reported through the calling deserializer's own error type, so a plugin
/// author sees one failure vocabulary rather than "a serde_json error appeared
/// inside your serde error". The named field survives that trip — an unknown
/// key in the body still reads as `unknown field \`…\`` — but the position does
/// not, which is why this is now the fallback path rather than the only one.
pub(crate) fn decode_body<T, E>(body: serde_json::Value) -> Result<T, E>
where
    T: serde::de::DeserializeOwned,
    E: serde::de::Error,
{
    serde_json::from_value(body).map_err(E::custom)
}

/// Read a two-key envelope, denying unknown keys and accepting either order.
pub(crate) fn deserialize_envelope<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: FromEnvelope,
{
    struct EnvelopeVisitor<T>(std::marker::PhantomData<T>);

    impl<'de, T: FromEnvelope> serde::de::Visitor<'de> for EnvelopeVisitor<T> {
        type Value = T;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a wrapper envelope with a `point` and a `body`")
        }

        fn visit_map<A>(self, mut map: A) -> Result<T, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            use serde::de::Error as _;

            let mut point: Option<WrapperPoint> = None;
            let mut seeded: Option<T> = None;
            let mut buffered: Option<serde_json::Value> = None;

            while let Some(key) = map.next_key::<EnvelopeField>()? {
                match key {
                    EnvelopeField::Point => {
                        if point.is_some() {
                            return Err(A::Error::duplicate_field("point"));
                        }
                        point = Some(map.next_value()?);
                    }
                    EnvelopeField::Body => {
                        if seeded.is_some() || buffered.is_some() {
                            return Err(A::Error::duplicate_field("body"));
                        }
                        match point {
                            // The common path, and the whole point of the
                            // visitor: the body is deserialized from the input
                            // itself, so `serde_json` reports its own position.
                            Some(point) => seeded = Some(T::seed_body(point, &mut map)?),
                            None => buffered = Some(map.next_value()?),
                        }
                    }
                }
            }

            match (seeded, buffered, point) {
                (Some(value), _, _) => Ok(value),
                (None, Some(body), Some(point)) => T::from_value(point, body),
                (None, Some(_), None) => Err(A::Error::missing_field("point")),
                (None, None, _) => Err(A::Error::missing_field("body")),
            }
        }
    }

    deserializer.deserialize_struct(
        "Envelope",
        ENVELOPE_FIELDS,
        EnvelopeVisitor(std::marker::PhantomData),
    )
}
