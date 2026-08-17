//! The plugin manifest crate — a plugin's declared say in the turn loop.
//!
//! Slice A of #3245 (plugins as turn-loop participants): parse and validate
//! the manifest blocks that grade a plugin's participation — `[loop]` with
//! its monotone ladder (`none` < `observer` < `steering` < `arbiter`),
//! `[requirements]`, `[oracle]`, `[subloop]`, and `[roles]` — as pure
//! functions over borrowed text. No I/O, no environment, no workspace-crate
//! dependencies: the engine never learns plugins exist, and the host that
//! binds these grants to the engine's gates (the Stop gate, the hook runner,
//! the sub-agent primitive) lives elsewhere and merely consumes this crate's
//! answers.
//!
//! `[wrapper]` (#3381) joins them: the stage order a turn-loop wrapper runs,
//! and the condition under which each stage runs, declared instead of
//! hardcoded. See [`Condition`] for the closed condition grammar and the
//! load-time stage-graph check that make "a condition naming a signal the
//! host does not publish is a load error" mechanical rather than
//! aspirational.
//!
//! `[[capabilities]]` (`doc:pipeline-as-plugins` §A1) is the other half of a
//! consent document: what the plugin asks to reach *outside* the turn,
//! graded in [`stella_protocol::RiskLevel`] — the gate's own vocabulary, so
//! the grade a user is shown at install and the grade an `AuthzGate` refuses
//! on cannot disagree. [`consent_text`] renders both halves into the words an
//! install prompt shows, purely and deterministically.
//!
//! The one function a host must not bypass is [`LoopGrant::permits_hook`]:
//! it is the authoritative filter behind the epic's rule that an undeclared
//! hook is never invoked, even if the plugin's process registers for it.

mod consent;
mod error;
mod manifest;
mod wrapper;

pub use consent::{Capability, RiskLevel, consent_text, highest_risk};
pub use error::ManifestError;
pub use manifest::{
    FlipPolicy, HookEvent, LoopGrant, Oracle, OracleCommand, Participation, PluginManifest, Role,
    Subloop, TamperPolicy,
};
pub use wrapper::{CompareOp, Condition, Signal, SignalKind, StageName, Wrapper, WrapperStage};
