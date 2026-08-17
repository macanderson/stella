// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a gate says when it says no — the payload of
//! `stella_core::bus::HookDecision::Deny` (#3380, `doc:pipeline-as-plugins`
//! §4 A6).
//!
//! # Why a `String` was not enough
//!
//! A denial used to be one prose field. That is adequate for a `PreToolUse`
//! block ("this path is off limits") and inadequate for the gate's primary
//! tenant, which is verification: an arbiter that refuses a completing turn
//! knows *which witness* it ran, *which command* produced the observation,
//! whether the fail→pass flip was achieved, and the digest of the artifact it
//! decided over. Flattened into prose, every one of those facts is
//! unrecoverable by anything but a regex — which is invariant #5's objection
//! to `Result<_, String>` restated one layer out: a consumer that has to
//! branch cannot branch on a sentence.
//!
//! # Why it lives here
//!
//! `stella-core` owns the decision enum, `stella-cli`'s trace fold and a
//! future host both read it, and `crates/stella-core/src/bus.rs` is a
//! grandfathered god file closed to growth. A type crossing a crate boundary
//! belongs in this crate by invariant #1, and by invariant #4 it round-trips
//! through `serde_json` byte-for-byte — see this module's tests.
//!
//! # Wire shape
//!
//! [`Denial`] is the newtype payload of an internally-tagged variant, so its
//! fields sit beside the tag:
//!
//! ```json
//! {"action":"deny","reason":"the witness is still red",
//!  "evidence":{"witness":"tests/flip.rs","command":["cargo","test"],
//!              "flip":"not_achieved","digest":"sha256:…"}}
//! ```
//!
//! The `evidence` object is optional and every field inside it has a default,
//! so a hook that has spoken `{"action":"deny","reason":"…"}` since #2684
//! parses unchanged. Growth here is additive-only, exactly like the rest of
//! this crate.

use serde::{Deserialize, Serialize};

use crate::ladder::FlipOutcome;

/// A gate's refusal: the prose every consumer can render, plus the structured
/// evidence a verifying gate decided from.
///
/// Construct the prose-only case through [`From`] — `"nope".into()` — which is
/// what keeps the many non-verifying denial sites (a rules engine, an authz
/// gate, a shell hook that printed no evidence) as short as they were when the
/// payload was a bare `String`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Denial {
    /// Why the call or the completion was refused, in prose. Always present:
    /// a denial a human cannot read is not a denial, whatever else it carries.
    pub reason: String,
    /// The deterministic evidence behind the refusal, when the denier had
    /// any.
    ///
    /// `None` means **this gate does not verify** — never "verification ran
    /// and found nothing". The distinction is the same one [`FlipOutcome`]
    /// exists to preserve one level down, and it is why this is an `Option`
    /// around the struct rather than a struct of all-empty fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<DenialEvidence>,
}

impl Denial {
    /// A prose-only denial — the shape every pre-#3380 caller had.
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            evidence: None,
        }
    }

    /// Attach the evidence a verifying gate decided from.
    #[must_use]
    pub fn with_evidence(mut self, evidence: DenialEvidence) -> Self {
        self.evidence = Some(evidence);
        self
    }
}

impl From<String> for Denial {
    fn from(reason: String) -> Self {
        Self::new(reason)
    }
}

impl From<&str> for Denial {
    fn from(reason: &str) -> Self {
        Self::new(reason)
    }
}

impl std::fmt::Display for Denial {
    /// The prose alone. The evidence is structured precisely so that a
    /// consumer renders it deliberately (or branches on it) rather than
    /// inheriting some default English from this crate, which has no view.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

/// What a verifying gate observed, structured so a consumer can branch on it.
///
/// Every field defaults, so a denier supplies exactly what it measured and
/// nothing more — an absent field is "not measured", which for the flip is
/// spelled explicitly by [`FlipOutcome::Unobserved`] rather than left to a
/// missing key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DenialEvidence {
    /// The witness the verdict rests on — a path, or whatever id the runner
    /// names it by. `None` when the gate tracked no witness at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness: Option<String>,
    /// The command that produced the observation, as argv.
    ///
    /// **argv, never a shell string** — the same rule the oracle's
    /// `OracleCommand` already holds, for the same reason: a string here is a
    /// shell injection the host has no way to fence, and a reader cannot tell
    /// an argument containing a space from two arguments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// What the flip oracle found. Tri-state and **always serialized**,
    /// because the whole point of [`FlipOutcome`] is that "not measured" and
    /// "measured and came back short" must not collapse into one token — and
    /// a field omitted when it is `unobserved` re-introduces exactly that
    /// collapse for a reader who does not know the default.
    #[serde(default)]
    pub flip: FlipOutcome,
    /// Digest of the artifact the verdict was computed over, so a later
    /// reader can tell whether the thing it is looking at is the thing that
    /// was judged. Opaque to this crate — the denier chooses the algorithm
    /// and says so in the string (`"sha256:…"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Invariant #4: every type crossing a crate boundary round-trips through
    /// `serde_json` byte-for-byte.
    #[test]
    fn a_denial_round_trips_through_serde_json() {
        for denial in [
            Denial::new("plain prose"),
            Denial::new("the witness is still red").with_evidence(DenialEvidence {
                witness: Some("tests/flip.rs".into()),
                command: vec!["cargo".into(), "test".into(), "-p".into(), "x".into()],
                flip: FlipOutcome::NotAchieved,
                digest: Some("sha256:deadbeef".into()),
            }),
            Denial::new("nothing measured").with_evidence(DenialEvidence::default()),
        ] {
            let json = serde_json::to_string(&denial).unwrap();
            let back: Denial = serde_json::from_str(&json).unwrap();
            assert_eq!(denial, back);
            assert_eq!(json, serde_json::to_string(&back).unwrap());
        }
    }

    /// The evidence is optional on the wire and the flip is not, so a denial
    /// written before #3380 still parses and a denial with evidence always
    /// says what the oracle saw.
    #[test]
    fn the_wire_shape_is_additive_and_the_flip_is_never_omitted() {
        let legacy: Denial = serde_json::from_str(r#"{"reason":"nope"}"#).unwrap();
        assert_eq!(legacy, Denial::new("nope"));
        assert_eq!(
            serde_json::to_string(&legacy).unwrap(),
            r#"{"reason":"nope"}"#,
            "a prose-only denial must not grow a null `evidence` key"
        );

        let measured = Denial::new("r").with_evidence(DenialEvidence::default());
        assert_eq!(
            serde_json::to_string(&measured).unwrap(),
            r#"{"reason":"r","evidence":{"flip":"unobserved"}}"#,
            "`unobserved` is stated, never left to a missing key"
        );
    }

    #[test]
    fn a_string_converts_and_displays_as_itself() {
        let denial: Denial = "blocked path".to_string().into();
        assert_eq!(denial.reason, "blocked path");
        assert_eq!(denial.evidence, None);
        assert_eq!(denial.to_string(), "blocked path");
    }
}
