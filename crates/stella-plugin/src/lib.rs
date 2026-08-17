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
//! aspirational, and [`Wrapper::resolve`] for the reader that turns a
//! declaration into an answer: the host fills in [`SignalValues`] — total by
//! construction, so an unpublished signal is not representable, let alone
//! silently `false` — and gets back the ordered [`StageProgram`] this turn
//! will run.
//!
//! `[[capabilities]]` (`doc:pipeline-as-plugins` §A1) is the other half of a
//! consent document: what the plugin asks to reach *outside* the turn,
//! graded in [`stella_protocol::RiskLevel`] — the gate's own vocabulary, so
//! the grade a user is shown at install and the grade an `AuthzGate` refuses
//! on cannot disagree. [`consent_text`] renders both halves into the words an
//! install prompt shows, purely and deterministically.
//!
//! `[oracle]`'s evidence half — `measurements` and `[[oracle.checks]]` — is
//! what lets a definition of done that is **not** a witness test be stated as
//! data: the oracle declares the numbers it reports, and each check compares
//! one of them against a budget in the same closed grammar. See
//! `evidence.rs` for the falsifier that established the need and
//! [`Oracle::unmet`] for the pure evaluator that decides it.
//!
//! `[runtime]` (`doc:pipeline-as-plugins` §A5) is the process half: the argv
//! the host starts, the timeout it enforces, and the **exact** environment
//! slice the child inherits. See [`Runtime`] for why there is deliberately no
//! `language` field — `argv` already distinguishes a Python plugin from a
//! Node one without Stella learning what a language is — and why the
//! environment is an allowlist rather than a scrub.
//!
//! `wire.rs` (#3380, `doc:wrapper-socket` §2) is the socket itself, and it is
//! the reason every block above is worth declaring: the two points a plugin
//! answers ([`WrapperRequest`] / [`WrapperResponse`]), the closed
//! [`EvidenceSet`] it may report, and the [`VerdictRule`] read off this
//! manifest that the **host** — never the plugin — evaluates. `judge` and
//! `again` are deliberately absent from that vocabulary: they are free
//! functions in `stella-runtime`, so a plugin cannot implement either one in
//! any language, and "a verification plugin quietly calls a model to decide
//! done" stays impossible by construction rather than by policy.
//!
//! `observed.rs` (#3499) is the line between the two halves of that evidence:
//! [`ObservedEvidence`] is what a plugin returns, and it has **no tamper
//! field** — snapshotting artifact identity is host-side, so a plugin asked to
//! report it could only ever answer "not checked" and leave every verdict
//! undecided. The host merges its own finding through
//! [`EvidenceSet::from_observed`], which makes "the host owns tamper" a fact of
//! the types rather than a rule someone has to remember.
//!
//! `host_call.rs` (#3540, `doc:wrapper-socket` §6b) corrects a design error in
//! the socket above: every point there is the host asking and the plugin
//! answering, which forecloses a plugin that needs something only the host has
//! — recall, a bounded child turn, a test re-run. A plugin may now **ask**
//! ([`PluginMessage::Call`]) and read the answer before returning the response
//! that ends the point. It still may never *reach*: the host performs the call,
//! applies [`LoopGrant::permits_call`], and returns only what the declared grant
//! permits, so an ask is the handing of a capability made explicit rather than
//! ambient authority.
//!
//! `package.rs` (#3565) is the other half of what a plugin *is*: `[[tools]]`,
//! `[[skills]]` and `[[records]]` declare what the package **ships** into the
//! three surfaces a workspace already steers itself with. Those contributions
//! are discovered by directory convention (#3380), which is what keeps
//! provenance unforgeable — and is why nothing declared them, so
//! [`consent_text`] could not name them and an embedding host showed a document
//! that omitted executable code entering the agent's tool surface. The
//! declaration closes that, and [`PluginManifest::reconcile`] keeps the two
//! halves honest: the host hands over its own read as a [`PackageListing`] and
//! any disagreement in either direction is a refusal, so a package can neither
//! ship a tool it did not declare nor declare one it does not ship. See
//! [`RecordEnforcement`] for why a package may ship a record that steers and
//! never one that denies.
//!
//! The three functions a host must not bypass are [`LoopGrant::permits_hook`],
//! [`LoopGrant::permits_point`] and [`LoopGrant::permits_call`]: they are the
//! authoritative filters behind the epic's rule that an undeclared hook is never
//! invoked, an undeclared wrapper point is never dispatched, and an undeclared
//! capability is never performed — however eagerly the plugin's own process
//! would answer or ask for one.

mod consent;
mod error;
mod evidence;
mod host_call;
mod manifest;
mod observed;
mod package;
mod program;
mod runtime;
mod wire;
/// The generated description of the wrapper socket's wire contract, published
/// under `docs/wire/` and gate-checked by `scripts/check-wire-schema.sh`.
///
/// Behind the `schema` feature for the reason `stella-protocol`'s
/// `schema_export` is: describing the wire format must cost the shipping binary
/// nothing, so a default build never compiles it.
#[cfg(feature = "schema")]
pub mod wire_corpus;
mod wrapper;

pub use consent::{Capability, RiskLevel, consent_text, highest_risk};
pub use error::ManifestError;
pub use evidence::{MeasurementRule, OracleCheck, UnmetCheck};
pub use host_call::{
    ChildTurnArgs, ChildTurnResult, HostCall, HostCallArgs, HostCallFailure, HostCallOk,
    HostCallOutcome, HostCallRefusal, HostCallRequest, HostCallResponse, PluginMessage, RecallArgs,
    RecallFrame, RecallResult, RunTestArgs,
};
pub use manifest::{
    FlipPolicy, HookEvent, LoopGrant, Oracle, OracleCommand, OracleProcess, OracleProcessSource,
    Participation, PluginManifest, Role, Subloop, TamperPolicy,
};
pub use observed::ObservedEvidence;
pub use package::{
    ContributionKind, KindMismatch, PackageListing, PackageMismatch, RecordContribution,
    RecordEnforcement, SkillContribution, ToolContribution,
};
pub use program::{SignalValues, StageProgram};
pub use runtime::Runtime;
pub use wire::{
    AfterTurnRequest, AfterTurnResponse, BeforeTurnRequest, BeforeTurnResponse, CandidateGrant,
    Continuation, Correction, EvidenceSet, FlipObservation, Outcome, PROTOCOL_VERSION,
    PublishedSignal, RoundState, SignalValue, StopReason, TamperFinding, TestBaseline, TestPlan,
    TurnOutcome, UndecidedReason, UnmetBecause, UnmetRequirement, Verdict, VerdictRule,
    VolatileContext, WrapperPoint, WrapperRequest, WrapperResponse,
};
pub use wrapper::{CompareOp, Condition, Signal, SignalKind, StageName, Wrapper, WrapperStage};
