// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The name of a stage boundary — **an open vocabulary** (`doc:roleless-core`).
//!
//! # What was closed, and what that cost
//!
//! [`StageKind`] is the twelve boundaries the host itself emits, and until now
//! it was also the only thing [`AgentEvent::Stage`] could say. That made the
//! stage vocabulary a closed set compiled into the host, which is the exact
//! shape `doc:roleless-core` exists to remove: the moment core enumerates the
//! stages, the set of turn shapes a plugin can express is capped at the set
//! core anticipated, and "a plugin can fundamentally change how a turn runs"
//! degrades into "a plugin can rearrange the stages we already shipped".
//!
//! It failed in the direction that misleads, too. A stage a plugin contributed
//! could not be spelled on the wire at all, so it did not render as an unknown
//! stage — it could not be emitted, and the surface whose whole job is to say
//! what the turn is doing would show the turn skipping from one host stage to
//! the next with the contributed work invisible between them.
//!
//! # What is open now, and what stays closed
//!
//! [`StageName`] is either one of [`StageKind`]'s twelve or a contributed name
//! carried as the plugin's own word. On the wire it is a plain string in both
//! cases, so every one of the twelve encodes exactly the byte it always did
//! and every recorded session still reads.
//!
//! [`StageKind`] itself stays closed and is deliberately untouched. It is not
//! the stage vocabulary any more; it is the set of boundaries *this host*
//! emits, which is a smaller and honest claim — and the one the exhaustive
//! matches that must stay exhaustive (the diagnostics bridge, the wire corpus)
//! are actually about.
//!
//! # Normalization is what makes the round-trip exact
//!
//! [`StageName::new`] resolves a name that [`StageKind`] knows into
//! [`StageName::Known`], so [`StageName::Contributed`] can never hold a string
//! that a known kind would also answer to. Without that, `Contributed("judge")`
//! would decode as [`StageKind::Verdict`] and re-encode as `"verdict"` — a
//! value that does not survive its own round trip, which invariant 4 forbids.
//!
//! # This module does not learn what a plugin is
//!
//! Nothing here names one. A contributed name arrives as text and leaves as
//! text; whether it exists because of a plugin, a wrapper manifest or a host
//! that grew a thirteenth boundary is a question this type cannot ask and does
//! not need to.
//!
//! [`AgentEvent::Stage`]: crate::AgentEvent::Stage

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::event::StageKind;

impl StageKind {
    /// Every boundary this host emits, in the turn order they occur in.
    ///
    /// Ordered rather than declaration-ordered by accident: a renderer that
    /// needs a stable reading order for the host's own stages takes it from
    /// here instead of writing a second list that can drift from this one.
    pub const ALL: [Self; 12] = [
        Self::Triage,
        Self::ContextRecall,
        Self::Research,
        Self::Plan,
        Self::ScopeReview,
        Self::Witness,
        Self::Execute,
        Self::Verify,
        Self::Verdict,
        Self::Reflect,
        Self::ContextWrite,
        Self::Complete,
    ];

    /// The exact string this kind encodes as on the wire.
    ///
    /// Hand-written rather than derived because [`StageName`] has to compare
    /// an arbitrary string against the vocabulary without routing every
    /// comparison through `serde_json`. `the_wire_table_matches_serde` pins
    /// the two together, so the table cannot drift from the derive that is
    /// actually authoritative.
    #[must_use]
    pub const fn as_wire_str(self) -> &'static str {
        match self {
            Self::Triage => "triage",
            Self::ContextRecall => "context_recall",
            Self::Research => "research",
            Self::Plan => "plan",
            Self::ScopeReview => "scope_review",
            Self::Witness => "witness",
            Self::Execute => "execute",
            Self::Verify => "verify",
            Self::Verdict => "verdict",
            Self::Reflect => "reflect",
            Self::ContextWrite => "context_write",
            Self::Complete => "complete",
        }
    }

    /// The kind a wire string names, including the historical aliases.
    ///
    /// `judge` and `verifier` are [`StageKind::Verdict`]'s previous spellings
    /// and are accepted here for the same reason the `serde` derive accepts
    /// them: replay, the observatory and the golden fixtures all parse stored
    /// streams, and a stream recorded against those builds must still read.
    #[must_use]
    pub fn from_wire_str(text: &str) -> Option<Self> {
        match text {
            "judge" | "verifier" => Some(Self::Verdict),
            other => Self::ALL
                .into_iter()
                .find(|kind| kind.as_wire_str() == other),
        }
    }
}

/// The name of a stage boundary: one of the host's own, or a contributed word.
///
/// Encodes as a plain string either way — see the module docs for why the
/// contributed arm cannot hold a name the host already answers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageName {
    /// A boundary this host emits.
    Known(StageKind),
    /// A stage something outside the host contributed, under its own word.
    ///
    /// Never a name [`StageKind::from_wire_str`] resolves — [`StageName::new`]
    /// is the only constructor that can produce this arm, and it normalizes.
    Contributed(String),
}

impl StageName {
    /// The stage a name refers to, resolving the host's own vocabulary first.
    #[must_use]
    pub fn new(name: &str) -> Self {
        StageKind::from_wire_str(name)
            .map_or_else(|| Self::Contributed(name.to_owned()), Self::Known)
    }

    /// The host boundary this is, or `None` for a contributed stage.
    ///
    /// The accessor the exhaustive matches use: a consumer that genuinely
    /// needs the closed vocabulary (the diagnostics bridge, whose field values
    /// cannot hold a runtime string at all) asks for it here and handles the
    /// `None` explicitly, rather than being handed a guess.
    #[must_use]
    pub const fn kind(&self) -> Option<StageKind> {
        match self {
            Self::Known(kind) => Some(*kind),
            Self::Contributed(_) => None,
        }
    }

    /// The name as it appears on the wire and on screen.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Known(kind) => kind.as_wire_str(),
            Self::Contributed(name) => name.as_str(),
        }
    }

    /// Whether this stage came from outside the host.
    #[must_use]
    pub const fn is_contributed(&self) -> bool {
        matches!(self, Self::Contributed(_))
    }
}

impl From<StageKind> for StageName {
    fn from(kind: StageKind) -> Self {
        Self::Known(kind)
    }
}

impl fmt::Display for StageName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for StageName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for StageName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Ok(Self::new(&text))
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for StageName {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "StageName".into()
    }

    /// A string, **not** an enum of the twelve.
    ///
    /// The schema is the contract a non-Rust reader validates against, and a
    /// closed enum there would make a contributed stage a schema violation —
    /// re-closing on the wire exactly the vocabulary this type opened.
    ///
    /// The twelve still ride along as `examples`, which documents without
    /// constraining. That is the half a consumer actually loses when a closed
    /// enum becomes a string: not validation (it never wanted to reject a
    /// stage), but the list of names worth writing a branch for. A reader
    /// should switch on these and keep a default arm — which is the shape it
    /// needed anyway, since a stage it has never heard of is now reachable.
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = String::json_schema(generator);
        schema.insert(
            "description".to_owned(),
            serde_json::Value::String(
                "The name of a stage boundary — an OPEN vocabulary. The host's own boundaries \
                 are listed in `examples`; an installed plugin may contribute a stage under any \
                 other name, so a consumer must branch on the names it knows and keep a default \
                 arm rather than treating an unlisted value as invalid."
                    .to_owned(),
            ),
        );
        schema.insert(
            "examples".to_owned(),
            serde_json::Value::Array(
                StageKind::ALL
                    .iter()
                    .map(|kind| serde_json::Value::String(kind.as_wire_str().to_owned()))
                    .collect(),
            ),
        );
        schema
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ALL` is a hand-written array, and it became load-bearing the moment
    /// the schema stopped enumerating the twelve: the wire-contract corpus
    /// samples stage kinds *from it*, so a kind missing here is a kind nothing
    /// proves against the committed schema.
    ///
    /// The `slot` match is exhaustive, so a thirteenth variant does not compile
    /// until it is given a slot — and then this test fails on an out-of-range
    /// index until `ALL` grows too. That is the pair of failures that makes the
    /// omission impossible to land quietly.
    #[test]
    fn all_lists_every_kind_exactly_once() {
        fn slot(kind: StageKind) -> usize {
            match kind {
                StageKind::Triage => 0,
                StageKind::ContextRecall => 1,
                StageKind::Research => 2,
                StageKind::Plan => 3,
                StageKind::ScopeReview => 4,
                StageKind::Witness => 5,
                StageKind::Execute => 6,
                StageKind::Verify => 7,
                StageKind::Verdict => 8,
                StageKind::Reflect => 9,
                StageKind::ContextWrite => 10,
                StageKind::Complete => 11,
            }
        }

        let mut seen = vec![false; StageKind::ALL.len()];
        for kind in StageKind::ALL {
            let slot = slot(kind);
            assert!(
                !std::mem::replace(&mut seen[slot], true),
                "{kind:?} appears in StageKind::ALL more than once"
            );
        }
        assert!(
            seen.iter().all(|hit| *hit),
            "StageKind::ALL is missing a kind the vocabulary declares"
        );
    }

    /// The hand-written wire table is only safe because this test pins it to
    /// the `serde` derive that actually governs the bytes. A variant renamed
    /// in `event.rs` and not here fails right here.
    #[test]
    fn the_wire_table_matches_serde() {
        for kind in StageKind::ALL {
            let encoded = serde_json::to_string(&kind).unwrap();
            assert_eq!(
                encoded,
                format!("\"{}\"", kind.as_wire_str()),
                "{kind:?} encodes differently than as_wire_str claims"
            );
            assert_eq!(StageKind::from_wire_str(kind.as_wire_str()), Some(kind));
        }
    }

    /// **The witness.** A stage the host has never heard of survives the wire
    /// under its own word — the thing that could not be expressed at all
    /// before, and the whole reason a renderer can now show it.
    #[test]
    fn a_contributed_stage_round_trips_under_its_own_name() {
        let name = StageName::new("triage-lite");
        assert_eq!(name, StageName::Contributed("triage-lite".to_owned()));
        assert_eq!(name.kind(), None);
        assert!(name.is_contributed());

        let json = serde_json::to_string(&name).unwrap();
        assert_eq!(json, "\"triage-lite\"");
        assert_eq!(serde_json::from_str::<StageName>(&json).unwrap(), name);
    }

    /// Every host boundary encodes as the byte it always did, so a recorded
    /// session written before this type existed still reads.
    #[test]
    fn a_host_stage_encodes_exactly_as_it_always_did() {
        for kind in StageKind::ALL {
            let was = serde_json::to_string(&kind).unwrap();
            let now = serde_json::to_string(&StageName::from(kind)).unwrap();
            assert_eq!(was, now, "{kind:?} changed its encoding");
            assert_eq!(
                serde_json::from_str::<StageName>(&was).unwrap(),
                StageName::Known(kind)
            );
        }
    }

    /// Normalization, from both directions: the constructor resolves a known
    /// name rather than wrapping it, and the historical aliases resolve too.
    ///
    /// Load-bearing because `Contributed` holding a name the host answers to
    /// would be a value that does not survive its own round trip — it would
    /// decode as `Verdict` and re-encode as `"verdict"`.
    #[test]
    fn a_known_name_never_becomes_a_contributed_one() {
        assert_eq!(
            StageName::new("execute"),
            StageName::Known(StageKind::Execute)
        );
        for alias in ["judge", "verifier"] {
            let name = StageName::new(alias);
            assert_eq!(name, StageName::Known(StageKind::Verdict));
            assert_eq!(
                serde_json::to_string(&name).unwrap(),
                "\"verdict\"",
                "an alias must re-encode as the canonical spelling"
            );
        }
    }

    /// A contributed name is compared, never parsed: case and separators carry
    /// no meaning to the host, so two spellings are two stages.
    #[test]
    fn a_contributed_name_is_opaque() {
        assert_ne!(StageName::new("Execute"), StageName::new("execute"));
        assert_eq!(StageName::new("Execute").as_str(), "Execute");
    }
}
