//! The compiled-frame identity event body (Phase 1 installment 5).
//!
//! [`CompiledContextFrameBuilt`] carries a decision worth reading before
//! changing it: the compiled frame is *stella's own artifact*, distinct from the
//! provider-emitted `ContextFrame` it is named after, and the 2026-07-26
//! amendment folded it into the step manifest rather than a parallel aggregate.
//! Both are `doc:adr/0006-contextframe-vs-compiledcontextframe`.
//!
//! (That citation lives here rather than on the struct on purpose. The schema
//! exporters carry *type* doc comments into `docs/wire/` verbatim, so a
//! documentation-only edit up there would restate the wire contract and redden
//! `wire-schema` until the generated artifacts were regenerated and committed.
//! Module docs are not exported, so this is the free place to say it.)
//!
//! This module used to also hold `LifecycleEventEnvelope` — a versioned
//! envelope for nine further adaptive-context lifecycle bodies, landed
//! schema-first so the wire shape would be pinned before the first emitter.
//! No emitter ever landed: nothing in the workspace constructed or read one,
//! and #3135 (the tracking issue for the wiring) resolved the deferral by
//! deleting the unwired channel instead. A future lifecycle channel gets
//! designed together with its first producer; this crate does not stockpile
//! contracts ahead of one. What survives is the one type production
//! constructs: `stella-core::receipts` builds a [`CompiledContextFrameBuilt`]
//! and it rides the step manifest on [`crate::event::AgentEvent`].
//!
//! The golden JCS vector below is the reason the body is pinned here: those
//! canonical bytes are the preimage `stella-core::context_record::hash` builds
//! `record_hash` values from, so renaming, retyping, or reordering a field
//! must break a test rather than silently change a hash across replays.

use serde::{Deserialize, Serialize};

/// Stable-ID payload of `compiled_context_frame_built`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CompiledContextFrameBuilt {
    /// The compiled frame id.
    pub compiled_frame_id: String,
    /// Its byte-stable frame hash (`sha256:<hex>`).
    pub frame_hash: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The RFC 8785 (JCS) canonical bytes of a value — the same canonicalizer
    /// crate + version `stella-core::context_record::hash` builds `record_hash`
    /// preimages with, so an event body's bytes here and there agree.
    fn jcs<T: Serialize>(value: &T) -> String {
        serde_json_canonicalizer::to_string(value).expect("canonicalizes")
    }

    #[test]
    fn golden_jcs_vector_for_compiled_context_frame_built() {
        // The exact canonical bytes of the event body. Keys are sorted and
        // whitespace minimal, so the vector is hand-verifiable. Renaming,
        // retyping, adding, or reordering a field breaks this line — which is
        // the point: the body is a wire contract across replays.
        assert_eq!(
            jcs(&CompiledContextFrameBuilt {
                compiled_frame_id: "cf_1".into(),
                frame_hash: "sha256:aa".into(),
            }),
            r#"{"compiled_frame_id":"cf_1","frame_hash":"sha256:aa"}"#
        );
    }
}
