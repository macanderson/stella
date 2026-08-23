//! The event vocabulary — plain enum variants flowing from `stella-core` to
//! whichever renderer (TUI or the JSON serializer) is listening.
//! `--output-format stream-json` is a `serde_json` serialization of this
//! exact enum, one line per event: a stable, versioned machine interface.
//!
//! The vocabulary is additive-only: later variants are appended as the
//! context/media/fleet crates land, never a breaking rename.
//!
//! "Additive" is directional, but both directions are now survivable. New
//! *fields* ride `serde(default)`, so a newer reader parses every older
//! stream. New *variants* travel backwards through
//! [`AgentEvent::Unknown`]: an unrecognized `"type"` deserializes into that
//! variant with the original JSON object preserved whole in `payload`, and
//! re-serializes without losing a key or a value. An older binary therefore
//! *skips* an event from the future instead of failing the whole stream.
//!
//! Round-tripping an unknown event preserves its *content*, not its exact
//! bytes: [`serde_json::Value`] holds object keys in a `BTreeMap`, so they
//! come back sorted rather than in their original order. JSON object key
//! order carries no meaning (RFC 8259 §4), so nothing downstream may depend
//! on it — but a consumer diffing raw lines should compare parsed values.
//!
//! The backwards direction is variant-shaped, not field-shaped, and the
//! difference is easy to over-read. A *known* tag carrying a field this build
//! has never heard of parses fine — serde ignores unrecognized keys — but that
//! field is gone the moment the event is re-serialized, because nothing
//! captured it. A proxy or `replay::to_jsonl` relaying a newer stream
//! therefore passes new *events* through whole and silently narrows new
//! *fields* on events it already knows. Only [`AgentEvent::Unknown`] preserves
//! an object verbatim; capturing stray keys on the typed variants would take a
//! `serde(flatten)` overflow map on each one, and no variant carries one.
//!
//! The tolerance is deliberately narrow, and the line matters: the fallback
//! fires **only for an unrecognized tag**. A *recognized* tag whose body does
//! not match its variant is still a hard error, because that is a real
//! encoder bug or a corrupt record — silently degrading it to `Unknown` would
//! turn data corruption into a shrug. See [`KNOWN_TYPE_TAGS`].
//!
//! Note this makes `AgentEvent`'s `Deserialize` impl specific to
//! self-describing formats (it buffers through [`serde_json::Value`] to read
//! the tag before dispatching). That is the format this type has always been
//! defined against — `--output-format stream-json` *is* `serde_json` — so the
//! constraint costs nothing in portability. It does cost work: every decode
//! materializes the whole event as a [`serde_json::Value`] first, so a body's
//! strings are allocated twice on the way in. That is paid on the journal
//! replay path, not on the live emit path (serialization is unaffected), and
//! it is the price of reading the tag before dispatching without hand-writing
//! a visitor per variant.
//!
//! ## Money is `f64`, and that carries one hard edge
//!
//! Every dollar field on this stream — `cost_usd`, `spent_usd`, `limit_usd`,
//! `estimated_cost_usd` — is an `f64`, because JSON has no decimal type and a
//! string-encoded decimal would be a breaking wire change for every existing
//! consumer. At the magnitudes involved (fractions of a cent, summed over
//! thousands of steps) binary rounding is far below a billable unit, so the
//! representation is not the problem.
//!
//! The **non-finite** case is. `serde_json` writes `NaN` and `±Infinity` as
//! JSON `null`, so an emitter that lets a division by a zero estimate reach one
//! of these fields — `calibration_factor` is the realistic path, see
//! [`AgentEvent::Compaction`] — produces a line that this crate then refuses to
//! read back: `null` is not an `f64`, the tag is known, and by the rule above
//! that is a hard error rather than an [`AgentEvent::Unknown`]. For a bare
//! `f64` the whole event is lost, and for an `Option<f64>` it is worse — `null`
//! parses as `None`, so an infinite `limit_usd` comes back as "no limit set".
//! Emitters must keep these fields finite; the wire cannot express the
//! difference.
//!
//! The tolerance stops at `AgentEvent`'s own tag. The enums *nested* inside a
//! variant — [`ModelCallRole`], [`StageKind`], [`PolicyKind`], [`CiStatus`],
//! and their peers — are closed, so a value one of them does not know is a
//! body that does not fit a known tag, i.e. a hard error. Only [`BlockKind`]
//! and [`CacheZone`] carry a `serde(other)` catch-all today. Adding a variant
//! to a nested vocabulary is therefore still a one-directional change, and
//! the reader that meets it drops the whole event, not just the field.
//!
//! One field is deliberately outside that rule: [`AgentEvent::Stage`]'s `name`
//! is a [`crate::StageName`], an open vocabulary, because a stage a plugin
//! contributed has a name this crate cannot enumerate ahead of time
//! (`doc:roleless-core`). It is not a `serde(other)` catch-all — a catch-all
//! variant is unit-only and *discards* the name, which for a stage is the one
//! piece of information a reader needs. It carries the word instead.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::compaction_rewrite::CompactionRewrite;
use crate::context_event::CompiledContextFrameBuilt;
use crate::delivery_event::DeliveryOutcome;
// The ladder's own wire vocabulary lives in `crate::ladder`; a verdict event
// carries it, and `ProofStep::Oracle` names the tree an observation ran
// against. Re-exported rather than imported so `event::LadderSnapshot` — the
// path these types had before the move — still resolves for every reader.
pub use crate::ladder::{LadderRung, LadderSnapshot, OracleObservation, ProofTree};
// The proof-step vocabulary moved to `crate::proof` the same way (#1787), with
// the same contract: `event::ProofStep` still resolves for every reader.
pub use crate::proof::ProofStep;
use crate::subagent_event::SubAgentPhase;
use crate::tool::{ToolCall, ToolOutput};

// The context-receipts vocabulary lives in `crate::receipt` and is re-exported
// here unchanged: these types are part of this module's public surface (they
// only ever appear as `AgentEvent` payloads), and every consumer that spells
// `stella_protocol::event::BlockKind` must keep resolving after the split.
pub use crate::receipt::{
    BlockKind, BlockOrigin, CacheZone, ContextFrameRef, ContextProviderUsage, ContextUsage,
    ManifestEntry, ProviderShare,
};

// The leaf payload types a variant carries, split to a sibling file (the same
// #1857 reason the tests were) and re-exported unchanged: they are part of
// this module's public surface, and every consumer that spells
// `stella_protocol::event::TaskItem` must keep resolving after the split.
pub use payload::{
    CiStatus, FileChangeKind, HunkProposal, MediaArtifactRef, MediaJobState, MediaKind, PrStatus,
    ProposedHunk, ScopeProposal, TaskItem, TaskStatus, VerdictEvidence,
};

mod call_role;
pub mod consumers;
mod payload;
mod scopes;
mod steer_cause;
mod tags;

pub use call_role::ModelCallRole;
pub use steer_cause::SteerCause;
// Re-exported so `KNOWN_TYPE_TAGS` keeps its path through this module and
// `crate::KNOWN_TYPE_TAGS`: the table moved to a submodule (#2730), the
// public name did not.
pub use tags::KNOWN_TYPE_TAGS;

// The small closed-set enums several `AgentEvent` variants carry as fields,
// split to a sibling file (the same reason the payload types were) and
// re-exported unchanged: every consumer that spells
// `stella_protocol::event::StageKind` must keep resolving after the split.
pub use scopes::{
    BudgetMode, BudgetScope, PolicyKind, StageKind, StageScope, UsageIncompleteReason, Withholder,
};

/// The one spelling of "this usage envelope could not name its model", for
/// [`AgentEvent::UsageIncomplete::model`].
///
/// A named constant rather than a literal at each emit site because it is a
/// value consumers filter on, and because the sites that used to write it
/// unconditionally are exactly the defect #2831 records: a field that reads as
/// attribution while carrying a placeholder. Every caller that can name its
/// model now does, and this is what remains for an adapter that genuinely
/// cannot ([`crate::Provider::model`] returning `None`).
pub const UNKNOWN_MODEL: &str = "unknown";

/// The prefix the engine puts on a `StepOutcome::Aborted` reason when the
/// abort was a model call failing, so that reason reads as a sentence on its
/// own: `"model call failed: terminal provider error: …"`.
///
/// It lives here, in the crate both the emitter and every renderer already
/// depend on, because **two surfaces have to agree on it and they cannot see
/// each other**. `stella-core` writes it onto the abort reason; the host then
/// re-emits that reason as a second [`AgentEvent::Error`], because some abort
/// paths (budget, loop detection, cancel) emit no `Error` of their own and
/// would otherwise show no row at all. For a *model call* failure the engine
/// has already emitted one, so the host's row is the same failure wearing this
/// prefix — and a reader saw two red rows and counted two failures (#4146).
///
/// The renderer that collapses them is `stella-tui`, which deliberately does
/// not depend on `stella-core` and must not: it folds `AgentEvent`s and has no
/// business knowing the engine. Hardcoding the string on that side instead
/// would put it in two crates with nothing comparing them, so the day the
/// wording changed the de-duplication would silently stop working and no test
/// anywhere would go red. One definition, both sides.
pub const MODEL_CALL_FAILED_PREFIX: &str = "model call failed: ";

/// `serde(default)` value for [`AgentEvent::RetriesExhausted::retryable`]:
/// event logs recorded before the field existed (#926) replay as if every
/// failure were a genuine, potentially-retryable exhaustion — the reading
/// that was already implied by the event's name before this distinction
/// existed — rather than being silently reclassified as terminal.
fn retries_exhausted_retryable_default() -> bool {
    true
}

mod kind;
pub use kind::AgentEvent;

impl AgentEvent {
    /// Whether this event arrived with a `"type"` this build does not know —
    /// i.e. it was emitted by a newer stella. Consumers that want to surface
    /// "there is something here I cannot render" ask this rather than matching
    /// the variant directly.
    #[must_use]
    pub fn is_unknown(&self) -> bool {
        matches!(self, AgentEvent::Unknown { .. })
    }
}

impl Serialize for AgentEvent {
    /// Every known variant delegates to the derived codec, so the wire format
    /// is unchanged. [`AgentEvent::Unknown`] bypasses it and re-emits the
    /// object it was parsed from — a future event therefore survives a
    /// decode/encode round-trip with every key and value intact (though
    /// possibly reordered), which is what lets a recorder, a proxy, or
    /// `replay::to_jsonl` handle a stream it only partly understands without
    /// corrupting the part it does not.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            AgentEvent::Unknown { payload, .. } => payload.serialize(serializer),
            known => AgentEvent::serialize(known, serializer),
        }
    }
}

impl<'de> Deserialize<'de> for AgentEvent {
    /// Reads the `"type"` tag, then dispatches.
    ///
    /// An unrecognized tag becomes [`AgentEvent::Unknown`] with the whole
    /// original object preserved. Everything else — including an object with
    /// no `"type"` at all, or a non-string one — goes to the derived codec,
    /// which still rejects a body that does not fit its variant.
    ///
    /// The asymmetry is the point. A tag we have never seen is a newer stella
    /// talking to an older reader, which is normal and must not break the
    /// stream. A tag we *do* know carrying a body that does not fit is an
    /// encoder bug or a corrupt record, and laundering that into `Unknown`
    /// would convert a loud failure into silent data loss.
    ///
    /// Two things the [`Value`] hop costs, both invisible until you look for
    /// them (#672):
    ///
    ///  - **Duplicate keys stop being an error.** Deserializing a struct
    ///    directly rejects `{"type":"text","delta":"a","delta":"b"}` as a
    ///    duplicate field; a [`Value`] is a map, so the second key overwrites
    ///    the first and the derived codec only ever sees the last one. A
    ///    duplicated field is therefore last-wins rather than corruption
    ///    the reader refuses — narrower than the "a known tag with a bad body
    ///    is loud" doctrine above claims. Producers must not rely on it either
    ///    way: a stream with duplicate keys is malformed input this decoder
    ///    happens to survive, not a supported shape.
    ///  - **Errors lose their position.** The inner failure is a
    ///    `serde_json::Error` re-wrapped through `D::Error::custom`, so
    ///    "missing field `delta` at line 1 column 15" arrives as
    ///    "missing field `delta`". The *reason* survives; the offset into the
    ///    offending journal line does not.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error as _;

        let value = Value::deserialize(deserializer)?;
        match value.get("type").and_then(Value::as_str) {
            Some(tag) if !KNOWN_TYPE_TAGS.contains(&tag) => Ok(AgentEvent::Unknown {
                event_type: tag.to_owned(),
                payload: value,
            }),
            _ => AgentEvent::deserialize(value).map_err(D::Error::custom),
        }
    }
}

// The event surface's tests, split to a sibling file (the `anthropic/tests.rs`
// pattern) so the wire contract itself stays under the file-size ratchet
// (#1857).
#[cfg(test)]
mod tests;
