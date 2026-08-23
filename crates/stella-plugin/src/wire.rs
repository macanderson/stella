//! The wrapper socket's wire contract — the two points a plugin answers, and
//! the vocabulary the host decides on.
//!
//! `doc:wrapper-socket` §2 puts this half of the socket here and the trait one
//! crate up, and §3 says why this half is the *primary* one: a loop driven over
//! HTTP and a plugin spoken to over a pipe need the identical thing — the
//! participation points expressed as data rather than as Rust borrows. So the
//! ordering rule is mechanical: **a point that cannot be expressed as
//! serialized request/response is not a point**, and that has to be discovered
//! while the trait is still editable (`doc:pipeline-as-plugins` §5 commitment
//! 1). This module is authored in the same change as
//! `stella_runtime::wrapper`'s trait for exactly that reason.
//!
//! # What a plugin may say, and what it may not
//!
//! Two request/response pairs and nothing else: [`BeforeTurnRequest`] /
//! [`BeforeTurnResponse`], and [`AfterTurnRequest`] / [`AfterTurnResponse`].
//! [`WrapperPoint`] is the closed set, and **`judge` and `again` are absent
//! from it on purpose**. They are host functions over the plugin's declared
//! [`VerdictRule`] and the [`EvidenceSet`] its `after_turn` returned
//! (`stella_runtime::wrapper::judge` / `::again`), which is what keeps "a
//! verification plugin quietly calls a model to decide done" impossible by
//! construction rather than by policy (`doc:pipeline-as-plugins` §6,
//! `doc:turn-loop-wrappers` §9.2). A plugin cannot implement either one in any
//! language, because neither is a message it can be sent.
//!
//! # The four rules this module is written against
//!
//! - **No borrows, no lifetimes, no trait objects.** An out-of-process plugin
//!   cannot be handed a `&dyn` anything (`doc:wrapper-socket` §6). Every type
//!   here is owned plain data; the candidate worktree crosses as a
//!   [`CandidateGrant`] — the host's [`CandidateHandle`] plus the two facts a
//!   plugin cannot act without, its root and the test to run — never as a
//!   port. The grant is what makes "every capability arrives in the request"
//!   true rather than aspirational: before it, the only implementation of the
//!   handle was in-process async Rust over a `&dyn` port, so three reference
//!   plugins had to take their test command out of `[runtime] env` instead
//!   (#3498).
//! - **No terminal, no git, no cwd, no credential in any signature.** A
//!   wrapper that only works when a TTY or a git checkout is present is a CLI
//!   feature, not a socket. A wrapper names a *role intent*
//!   ([`BeforeTurnResponse::role`]); the host resolves it against the user's
//!   BYOK providers and makes every model call itself (invariant 3).
//! - **[`PROTOCOL_VERSION`] rides on every message and the contract is
//!   additive-only.** Every table also denies unknown fields, which is not in
//!   tension with that: a field the host does not know, at a version the host
//!   accepts, is a typo — and the crate's whole posture is that a manifest
//!   which quietly does nothing is worse than one that refuses to load.
//! - **Invariant 4.** Every type round-trips through `serde_json`
//!   byte-for-byte; `tests/wire_contract.rs` asserts it rather than promising
//!   it.
//!
//! # Invariant 7 is encoded, not remembered
//!
//! Contributed context is a [`VolatileContext`], whose body is a private field
//! and whose only exit is [`VolatileContext::into_message`], returning a
//! **user** message. Encoded is the operative word: the type carried this
//! claim as a doc comment over two public `String`s until #3524, which is a
//! rule a reviewer has to remember rather than one the compiler holds. See
//! that type for the `compile_fail` doctest that now pins it.

mod disclosure;
mod envelope;

pub use self::disclosure::{HOOK_FIELDS, HookField, WIRE_FIELDS, WireField, hook_disclosures_for};

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};
use stella_protocol::candidate::CandidateHandle;
use stella_protocol::completion::CompletionMessage;

use self::envelope::deserialize_envelope;
pub(crate) use self::envelope::{FromEnvelope, PendingBody, decode_body};
use crate::manifest::PluginManifest;
use crate::observed::ObservedEvidence;
use crate::oracle::Oracle;
use crate::wrapper::{Signal, SignalKind, StageName};

/// The version every message on this wire carries.
///
/// Additive-only: a new optional field, a new enum variant with a default, a
/// new `[roles]` intent all keep this number. It moves only for a change a
/// version-1 reader could misread, and a host refuses a message whose version
/// is *above* the one it supports rather than silently dropping the half it
/// cannot see.
pub const PROTOCOL_VERSION: u32 = 1;

/// The points a plugin answers.
///
/// Closed, and deliberately two: `judge` and `again` are host functions, not
/// messages (module docs). A third point is a design change, not a new value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum WrapperPoint {
    /// Before the loop is asked for a turn.
    BeforeTurn,
    /// Once the turn's completion lands.
    AfterTurn,
}

impl WrapperPoint {
    /// The name this point is written as on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BeforeTurn => "before_turn",
            Self::AfterTurn => "after_turn",
        }
    }
}

impl std::fmt::Display for WrapperPoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One request on the wire: `{"point": …, "body": {…}}`.
///
/// Adjacently framed rather than internally tagged so the body keeps
/// `deny_unknown_fields` — an internally tagged enum hands the tag field down
/// into the variant, where a denying struct rejects it. `Serialize` is derived
/// from that framing; `Deserialize` is written out by hand over a two-key
/// envelope that denies unknown fields, because the derived reader for the
/// same framing accepts any number of keys beside `point` and `body` and drops
/// them silently (#3500 — see this module's `Envelope`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "point", content = "body", rename_all = "snake_case")]
// The reader denies unknown envelope keys by hand (`Envelope`, below) rather
// than by attribute, so the derived schema would otherwise claim the socket
// accepts keys it refuses. Spelled here so the published contract describes
// the reader that actually runs (#3532).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
pub enum WrapperRequest {
    /// Ask the wrapper to contribute to a turn that has not run yet.
    BeforeTurn(BeforeTurnRequest),
    /// Tell the wrapper a turn completed, and ask for evidence.
    AfterTurn(AfterTurnRequest),
}

impl FromEnvelope for WrapperRequest {
    fn seed_body<'de, A>(point: WrapperPoint, map: &mut A) -> Result<Self, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        Ok(match point {
            WrapperPoint::BeforeTurn => Self::BeforeTurn(map.next_value()?),
            WrapperPoint::AfterTurn => Self::AfterTurn(map.next_value()?),
        })
    }

    fn from_value<E: serde::de::Error>(
        point: WrapperPoint,
        body: serde_json::Value,
    ) -> Result<Self, E> {
        match point {
            WrapperPoint::BeforeTurn => decode_body(body).map(Self::BeforeTurn),
            WrapperPoint::AfterTurn => decode_body(body).map(Self::AfterTurn),
        }
    }
}

impl<'de> Deserialize<'de> for WrapperRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_envelope(deserializer)
    }
}

impl WrapperRequest {
    /// Which point this request addresses.
    #[must_use]
    pub fn point(&self) -> WrapperPoint {
        match self {
            Self::BeforeTurn(_) => WrapperPoint::BeforeTurn,
            Self::AfterTurn(_) => WrapperPoint::AfterTurn,
        }
    }

    /// The version the sender wrote this message at.
    #[must_use]
    pub fn protocol_version(&self) -> u32 {
        match self {
            Self::BeforeTurn(request) => request.protocol_version,
            Self::AfterTurn(request) => request.protocol_version,
        }
    }
}

/// One response on the wire, in the same framing as [`WrapperRequest`] — and
/// refused the same way when it carries a key this host does not know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "point", content = "body", rename_all = "snake_case")]
// [`WrapperRequest`]'s reason, unchanged: a hand-written reader that refuses
// unknown envelope keys needs the attribute for the schema to say so.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
pub enum WrapperResponse {
    /// What the wrapper contributes to the turn about to run.
    BeforeTurn(BeforeTurnResponse),
    /// The evidence the wrapper gathered about the turn that ran.
    AfterTurn(AfterTurnResponse),
}

impl FromEnvelope for WrapperResponse {
    fn seed_body<'de, A>(point: WrapperPoint, map: &mut A) -> Result<Self, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        Ok(match point {
            WrapperPoint::BeforeTurn => Self::BeforeTurn(map.next_value()?),
            WrapperPoint::AfterTurn => Self::AfterTurn(map.next_value()?),
        })
    }

    fn from_value<E: serde::de::Error>(
        point: WrapperPoint,
        body: serde_json::Value,
    ) -> Result<Self, E> {
        Self::from_parts(point, body)
    }
}

impl<'de> Deserialize<'de> for WrapperResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_envelope(deserializer)
    }
}

impl WrapperResponse {
    /// Decode an already-split envelope into the response its point names.
    ///
    /// Split out of [`Deserialize`] because the host-call channel classifies a
    /// plugin's message before it knows which shape it has
    /// ([`PluginMessage`](crate::PluginMessage)): a message carrying both a
    /// `point` and a `call` is a contradiction rather than a response, and that
    /// judgement has to be made over the whole envelope. Both readers then reach
    /// the identical body decoder, so a point response means one thing whether
    /// it arrived alone or ended a conversation.
    pub(crate) fn from_parts<E: serde::de::Error>(
        point: WrapperPoint,
        body: serde_json::Value,
    ) -> Result<Self, E> {
        match point {
            WrapperPoint::BeforeTurn => decode_body(body).map(Self::BeforeTurn),
            WrapperPoint::AfterTurn => decode_body(body).map(Self::AfterTurn),
        }
    }

    /// Which point this response answers — compared against the request's, so
    /// a plugin answering the wrong question is a named error rather than a
    /// verdict decided from the wrong evidence.
    #[must_use]
    pub fn point(&self) -> WrapperPoint {
        match self {
            Self::BeforeTurn(_) => WrapperPoint::BeforeTurn,
            Self::AfterTurn(_) => WrapperPoint::AfterTurn,
        }
    }

    /// The version the plugin wrote this message at.
    #[must_use]
    pub fn protocol_version(&self) -> u32 {
        match self {
            Self::BeforeTurn(response) => response.protocol_version,
            Self::AfterTurn(response) => response.protocol_version,
        }
    }
}

/// The candidate workspace, as a plugin receives it.
///
/// **A capability the host resolves and bounds, never a path a plugin is
/// trusted to stay inside.** The distinction is the whole design and it is
/// worth being exact about, because the grant does hand out an absolute path:
///
/// - [`Self::root`] is where the plugin's *own* reads and its *own* test run
///   happen. A plugin that runs a test suite needs a directory, and pretending
///   otherwise is what pushed three reference plugins onto `[runtime] env` for
///   their test command (#3498).
/// - [`Self::handle`] is the capability. Every path the plugin names on the
///   way *back* — a scope, a withheld adoption path, a witness artifact — is
///   resolved against this handle's root by the host and refused if it lands
///   anywhere else, after symlinks, on the host's own filesystem
///   (`CandidateDenial`, and this crate's `candidate_grant::fence` for the
///   implementation — it was the staged pipeline's
///   `ports::CandidateHandles` until #3865 deleted that crate). The refusal is
///   the host's; nothing here is a promise
///   the plugin was asked to keep.
///
/// So a plugin that ignores the root and lies about where it went has told the
/// host nothing the host will act on, which is the property that lets the same
/// grant cross to a process in any language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CandidateGrant {
    /// The name the host minted for this workspace, and the only thing that
    /// re-addresses it. Opaque: see [`CandidateHandle`].
    pub handle: CandidateHandle,
    /// The workspace's absolute root on the host's filesystem, canonical — the
    /// host resolves symlinks *before* minting the grant, so the path a plugin
    /// is told and the path the host fences against are the same one.
    pub root: String,
    /// The test the host would run in this workspace, when it has one to
    /// give. `None` is "the host has no test invocation", never "run whatever
    /// you like": a plugin with no plan reports
    /// [`FlipObservation::Unobservable`] rather than guessing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<TestPlan>,
}

impl CandidateGrant {
    /// A grant with no test invocation — the shape a host mints for a
    /// workspace whose test command it does not know.
    #[must_use]
    pub fn new(handle: CandidateHandle, root: impl Into<String>) -> Self {
        Self {
            handle,
            root: root.into(),
            test: None,
        }
    }

    /// The same grant, carrying the test the host would run.
    #[must_use]
    pub fn with_test(mut self, test: TestPlan) -> Self {
        self.test = Some(test);
        self
    }
}

/// One test invocation, as the host already parsed it.
///
/// **argv, never a shell string** — the #1400 rule every spawned thing in this
/// workspace follows, and the reason the host's own strict test-command parser
/// runs before a grant is minted rather than inside the plugin. A plugin
/// receives a program and its arguments; it never receives a line to hand to a
/// shell, and a command the host's parser refuses produces no grant to carry
/// one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TestPlan {
    /// The test runner executable, from the host's closed runner vocabulary.
    pub program: String,
    /// Its exact argument vector.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// What this same invocation reported *before* the turn ran — the red half
    /// of a fail→pass flip, handed over so a plugin does not have to
    /// reconstruct it (or take it from an environment variable, which is what
    /// Track C had to do).
    #[serde(default)]
    pub baseline: TestBaseline,
}

impl TestPlan {
    /// A plan with no baseline observation.
    #[must_use]
    pub fn new(program: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
            baseline: TestBaseline::NotRun,
        }
    }

    /// The same plan, carrying what the invocation reported before the turn.
    #[must_use]
    pub fn with_baseline(mut self, baseline: TestBaseline) -> Self {
        self.baseline = baseline;
        self
    }
}

/// What a [`TestPlan`]'s invocation reported before the turn ran.
///
/// Four answers rather than an exit code, and the fourth is why: a run that
/// timed out or could not find its toolchain never observed an assertion, and
/// scoring its non-zero exit as "red" is exactly the bug `CmdOutcome`'s
/// `assertion_result` exists to close (#860) — an infra failure would satisfy
/// a flip's precondition and the next clean run would be credited as a fix.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum TestBaseline {
    /// The host did not run the invocation before the turn, so nothing is
    /// claimed about it.
    #[default]
    NotRun,
    /// It ran and its assertions passed.
    Passed,
    /// It ran and its assertions genuinely failed — the red a flip needs.
    Failed,
    /// It was attempted and did not complete, so it says nothing about
    /// assertions either way.
    Unobserved,
}

/// Everything a wrapper is given before a turn runs.
///
/// Every capability arrives *in the request* — there is no ambient authority
/// to reach for, which is the property that lets the same plugin run under
/// `stella-cli`, `stella-serve` and an embedded host without change
/// (`doc:wrapper-socket` §6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BeforeTurnRequest {
    /// The version this message is written at.
    pub protocol_version: u32,
    /// The variant id of the wrapper being asked — `StageProgram::variant`,
    /// and the join key of every per-variant comparison (#3388).
    pub wrapper: String,
    /// Which declared stage this call is for. `before_turn` runs once per
    /// stage the resolved [`StageProgram`](crate::StageProgram) says runs.
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Which declared stage this call is for. before_turn runs once per \
                           stage the host resolved this wrapper's declared order down to."
        )
    )]
    pub stage: StageName,
    /// Which round of the wrapper's loop this is; `0` for the first turn.
    pub round: u32,
    /// The goal, as the user stated it.
    pub goal: String,
    /// The candidate workspace this turn will run against, when the host has
    /// created one — see [`CandidateGrant`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<CandidateGrant>,
    /// What earlier stages of this same turn published, in publication order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub published: Vec<PublishedSignal>,
}

/// What a wrapper contributes to the turn about to run.
///
/// Everything here is an *offer*: the host applies it, bounded by what the
/// manifest declared and what the user consented to. A wrapper cannot run the
/// loop itself, and nothing in this type is a channel into it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BeforeTurnResponse {
    /// The version this message is written at.
    pub protocol_version: u32,
    /// Context to put in front of the model — as volatile messages *after* the
    /// byte-stable prefix, which [`VolatileContext`] makes the only option.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<VolatileContext>,
    /// A role *intent* for this turn: the name of a `[roles.<name>]` entry the
    /// same manifest declares. Never a model id, a provider, a URL or a
    /// credential — the host resolves the intent against the user's BYOK
    /// providers, and refuses an intent the manifest never declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Workspace-relative paths the wrapper believes the turn should stay
    /// within. Advisory input to the host's own scoping — a plugin-supplied
    /// path is never itself a permission (`stella_protocol::candidate`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    /// Signals this stage publishes for later stages of the same turn to read.
    ///
    /// The host validates each value ([`PublishedSignal::is_well_typed`]) and
    /// carries it forward into the next stage's
    /// [`BeforeTurnRequest::published`], so a later stage's *plugin* reads it.
    ///
    /// **A declared gap, not a silence** (invariant 10's discipline), and it is
    /// narrower than it was: a published value still cannot reach a
    /// *condition*, because [`Wrapper::resolve`](crate::Wrapper::resolve) takes
    /// one up-front [`SignalValues`](crate::SignalValues) snapshot and decides
    /// which stages run before any of them has published anything. Resolving
    /// progressively is #3491.
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Signals this stage publishes for later stages of the same turn to \
                           read. The host validates each value's type and carries it forward \
                           into the next stage's request. It does not change which stages run: \
                           that is resolved once, before the first stage."
        )
    )]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publish: Vec<PublishedSignal>,
}

impl BeforeTurnResponse {
    /// An empty contribution at the current [`PROTOCOL_VERSION`] — the shape a
    /// wrapper returns for a stage it has nothing to say at.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            ..Self::default()
        }
    }
}

/// Context a wrapper contributes to a turn.
///
/// **The invariant-7 constraint, encoded in the type.** Contributed context
/// rides as a volatile message *after* the byte-stable system-prompt prefix,
/// never inside it: prompt-cache hits are a feature, and a wrapper that could
/// inject into the stable prefix would make every installed plugin a per-turn
/// cost regression for every user who installed it. That is the same
/// discipline `crates/stella-cli/src/agent/prompt.rs::build_system_prompt` and
/// `crates/stella-cli/src/memory.rs` already hold for recalled context.
///
/// So this type carries no placement field. There is no `Placement::System`
/// to pick, no `stable: bool` to set, and **the body is not reachable as a
/// field**: the one way out of the value is
/// [`VolatileContext::into_message`], which builds a
/// [`CompletionMessage::user`] — a message that by construction is not index
/// 0's system message.
///
/// The privacy is the enforcement, and it was prose until #3524. While `text`
/// was public, a host could write `prompt.push_str(&ctx.text)` into the stable
/// prefix, change no type, and stay green — the unit test below constructs a
/// value and calls `into_message`, so it can say nothing about a caller that
/// never does. Every installed plugin then became a per-turn prompt-cache
/// miss, which is exactly the cost regression this type exists to make
/// unrepresentable. Now that line does not compile out of crate, which the
/// `compile_fail` doctest below pins:
///
/// ```compile_fail,E0616
/// let ctx = stella_plugin::VolatileContext::new("recall", "the last run failed on I/O");
/// let mut prompt = String::from("<system prefix>");
/// prompt.push_str(&ctx.text);
/// ```
///
/// The sanctioned reads, and all of them: the provenance label a journal
/// prints, and spending the value as the message it is.
///
/// ```
/// use stella_plugin::VolatileContext;
/// let ctx = VolatileContext::new("recall", "the last run failed on I/O");
/// assert_eq!(ctx.label(), "recall");
/// assert_eq!(ctx.into_message().content, "the last run failed on I/O");
/// ```
///
/// What this does **not** claim: that a host cannot splice the body in
/// anywhere at all. A caller that spends the contribution and then reaches
/// into the returned [`CompletionMessage`] has unwrapped a *user* message on
/// purpose, in a line that says so; no type can stop that, and pretending
/// otherwise would be the same overclaim the field made. What is closed is the
/// cheap path — the one a contributor takes without noticing they took it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct VolatileContext {
    /// A short human-readable name for where this came from, for the journal
    /// and the deck. Not shown to the model as a header — the host decides
    /// presentation. Read through [`VolatileContext::label`].
    label: String,
    /// The context itself. Private on purpose — see the type docs; the only
    /// exit is [`VolatileContext::into_message`].
    text: String,
}

impl VolatileContext {
    /// Build a contribution.
    ///
    /// The only constructor, because the fields are private: a value on the
    /// wire still decodes through `serde`, and a value in Rust is built here.
    #[must_use]
    pub fn new(label: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            text: text.into(),
        }
    }

    /// Where this contribution came from, for a journal or the deck.
    ///
    /// A borrow rather than a move, and deliberately the *only* borrowing
    /// accessor: a label is provenance a surface prints beside the
    /// contribution, and it is not the body. There is no `text(&self)` beside
    /// it — that would hand back the one string the type exists to keep out of
    /// the byte-stable prefix, and reopen the hole privacy just closed.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Spend this contribution as the volatile message it is.
    ///
    /// The only exit from the type, and it is a `User` message on purpose —
    /// see the type docs. Consuming `self` keeps a caller from spending the
    /// same contribution twice into one turn.
    #[must_use]
    pub fn into_message(self) -> CompletionMessage {
        CompletionMessage::user(self.text)
    }
}

/// One signal value a stage published.
///
/// The signal set is the manifest's closed [`Signal`] vocabulary, so a stage
/// cannot invent a fact for a later condition to read — the load-time rule
/// ("a condition naming a signal the host does not publish is a load error")
/// stated for the socket that actually dispatches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PublishedSignal {
    /// Which signal.
    pub signal: Signal,
    /// Its value this turn.
    pub value: SignalValue,
}

impl PublishedSignal {
    /// Whether the value's shape matches the signal's declared kind.
    ///
    /// A count published as a boolean is a mistake, never a coercion — the
    /// [`ManifestError::ConditionTypeMismatch`](crate::ManifestError) rule on
    /// the publishing side. The host checks this before believing a
    /// contribution; the wire cannot enforce it, because JSON has both shapes.
    #[must_use]
    pub fn is_well_typed(self) -> bool {
        matches!(
            (self.signal.kind(), self.value),
            (SignalKind::Boolean, SignalValue::Boolean(_))
                | (SignalKind::Count, SignalValue::Count(_))
        )
    }
}

/// A published signal's value — the two shapes [`SignalKind`] enumerates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum SignalValue {
    /// A truth value.
    Boolean(bool),
    /// A non-negative count.
    Count(u64),
}

/// Everything a wrapper is given once the turn's completion lands.
///
/// It receives the outcome; it does **not** hold a channel into the turn. That
/// is #3379's one-directional connection stated as a socket rule — the
/// pipeline no longer edits the engine's stream, and no plugin ever gets to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AfterTurnRequest {
    /// The version this message is written at.
    pub protocol_version: u32,
    /// The variant id of the wrapper being asked.
    pub wrapper: String,
    /// Which declared stage this evidence is about, mirroring
    /// [`BeforeTurnRequest::stage`] on the other side of the same round.
    ///
    /// **Optional, and the two cases are different answers.** A host that
    /// resolved a [`StageProgram`](crate::StageProgram) names the same stage
    /// its `before_turn` named, which is what lets a plugin declaring several
    /// stages per round know which one it is reporting on, and what lets a
    /// host correlate two evidence sets from one round. A host that runs no
    /// stage program **omits the field** rather than naming a stand-in
    /// boundary: there is no stage, and writing
    /// [`StageName::Execute`](crate::StageName) here would be the host
    /// inventing a declaration nobody made. `None` is therefore "this host
    /// has no stages", never "some default stage".
    ///
    /// Additive (`PROTOCOL_VERSION` unchanged): a plugin written before this
    /// field existed sends and reads messages without it, and a host that
    /// omits it produces the identical bytes it always did.
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Which declared stage this evidence is about, mirroring the stage the \
                           same round's before_turn named. Absent means this host runs no stage \
                           program at all — never a default stage."
        )
    )]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<StageName>,
    /// Which round of the wrapper's loop just ran.
    pub round: u32,
    /// The goal, as the user stated it.
    pub goal: String,
    /// The candidate workspace the turn ran against, when there was one —
    /// see [`CandidateGrant`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<CandidateGrant>,
    /// What the turn did.
    pub turn: TurnOutcome,
}

/// The read-only report of a turn that finished.
///
/// The engine always finishes its own turn and always says so; `completed` is
/// that statement, and a wrapper's "the whole job is over" is a separate,
/// separately named thing ([`Continuation`]) that cannot fake it.
///
/// # Why two of the four fields are optional
///
/// `tools` and `changed_files` are facts a host **may not have**, and the two
/// answers a plugin needs to tell apart are "the turn dispatched no tools /
/// changed no files" and "this host does not report them". They were plain
/// `Vec`s until #3552, so every host that could not measure them sent `[]` —
/// which reads as the first answer and *is* the second. A wrapper that gates
/// its evidence on "did the turn touch anything" then graded every run as
/// untouched, and nothing in the message let it notice.
///
/// So the absent case is spelled `None` (the key is omitted on the wire) and
/// the empty case is spelled `Some(vec![])` (`[]`). Additive:
/// [`PROTOCOL_VERSION`] is unchanged, a plugin written against the old shape
/// reads `[]` exactly where it always did, and a host that omits the key sends
/// bytes the old readers already accepted as "no entries" — the difference is
/// that a reader who *cares* can now ask.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TurnOutcome {
    /// Whether the engine reported the turn complete, as opposed to aborted.
    pub completed: bool,
    /// The final assistant text.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub answer: String,
    /// The tools the turn dispatched, in call order, by name — or `None` when
    /// this host does not observe them. See the type docs: `Some(vec![])` is
    /// "the turn dispatched none", which is a different claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    /// Workspace-relative paths the turn changed, or `None` when this host does
    /// not measure them. `Some(vec![])` is "the turn changed nothing".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_files: Option<Vec<String>>,
}

/// The evidence a wrapper gathered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AfterTurnResponse {
    /// The version this message is written at.
    pub protocol_version: u32,
    /// What the wrapper observed — its own half of the evidence, never the
    /// host's ([`ObservedEvidence`], and #3499 for why the difference is a
    /// type).
    pub evidence: ObservedEvidence,
}

/// The whole evidence vocabulary a verdict is decided from: what the wrapper
/// observed, merged with what the host checked.
///
/// A struct of closed fields rather than an open list of facts, and that is
/// the design: `judge` must be **total** over this type, and totality over a
/// fixed set of fields whose values are closed enums is the compiler's job
/// rather than an argument. Nothing here is a verdict — a wrapper reports what
/// it saw, and the rule that turns observations into "done" is the one its
/// manifest declared and a human consented to.
///
/// A model call a wrapper spends in `after_turn` (a goal-mode verifier, say)
/// arrives here the same way everything else does: as a number under a
/// declared measurement name, decided against a budget the manifest states.
/// The spend is then visible on the receipt against a declared role instead of
/// hiding inside a `judge` (`doc:turn-loop-wrappers` §9.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSet {
    /// Whose observation this set is. Host-owned like [`Self::tamper`] — not a
    /// word a plugin can say, and set by [`EvidenceSet::from_observed`] from
    /// the fact that its input *is* a plugin's `after_turn` body (#3513).
    pub provenance: EvidenceProvenance,
    /// What the wrapper saw of the fail→pass flip.
    pub flip: FlipObservation,
    /// What the **host's** tamper check found. Snapshotting artifact identity
    /// is host-side (`doc:pipeline-as-plugins` §4 A10), so this field is not on
    /// the wire a plugin speaks at all and is merged in by
    /// [`EvidenceSet::from_observed`] (#3499).
    pub tamper: TamperFinding,
    /// The numbers the oracle reported, by declared measurement name. A name
    /// absent here is *missing*, never a satisfied budget — see
    /// `stella_runtime::wrapper::judge`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub measurements: BTreeMap<String, u64>,
}

impl EvidenceSet {
    /// The set a host reports when the wrapper could not gather anything —
    /// its process failed, timed out, or answered unreadably.
    ///
    /// Deliberately *not* an empty set that reads as "nothing was wrong":
    /// [`FlipObservation::Unobservable`] makes `judge` abstain rather than
    /// blame the worker for evidence nobody collected.
    ///
    /// Its provenance is [`EvidenceProvenance::HostObserved`] because the
    /// `Unobservable` flip here is the *host's* own conclusion about a plugin
    /// that did not answer, not a claim the plugin made. It can never credit
    /// anything, so this is about honesty in the report, not the verdict.
    #[must_use]
    pub fn unobserved() -> Self {
        Self {
            provenance: EvidenceProvenance::HostObserved,
            flip: FlipObservation::Unobservable,
            tamper: TamperFinding::NotChecked,
            measurements: BTreeMap::new(),
        }
    }
}

/// What a wrapper saw of the fail→pass flip.
///
/// Closed, for the [`FlipPolicy`](crate::FlipPolicy) reason: an unknown value
/// must be a refusal rather than a silently weaker contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(
    feature = "schema",
    schemars(
        description = "What a wrapper saw of the fail→pass flip. Closed: a host refuses a value \
                       it does not know rather than reading it as a weaker one."
    )
)]
pub enum FlipObservation {
    /// No flip was attempted — this wrapper's evidence is not a transition
    /// (`flip = "not-applicable"`), or no witness was authored this turn.
    NotAttempted,
    /// Red before the work, green after it: the flip the witness contract
    /// asks for.
    Achieved,
    /// The witness ran on both sides and did not flip.
    NotAchieved,
    /// The witness failed **the same way** before and after, so its red says
    /// nothing about the change (#2540). A claim about the instrument, not
    /// about the work — which is why it makes `judge` abstain rather than
    /// blame a worker who cannot repair an instrument.
    Unsatisfiable,
    /// The witness could not be run at all.
    Unobservable,
}

/// What a wrapper's tamper check found.
///
/// The policy that asks for it is [`TamperPolicy`](crate::TamperPolicy), and
/// the snapshotting stays host-side — a plugin reports what the host's
/// comparison said, it does not vouch for its own witness.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TamperFinding {
    /// Identity was snapshotted at authoring time and still matches.
    Clean,
    /// An artifact changed between authoring and verification.
    Tampered {
        /// The first artifact whose identity changed, for the report.
        artifact: String,
    },
    /// No snapshot was taken, so there is nothing to compare. Under a policy
    /// that demands one this is *not* a pass — see
    /// `stella_runtime::wrapper::judge`.
    NotChecked,
}

/// Who observed the evidence a verdict was decided from.
///
/// #3511's Option 2 settled that a verification plugin reports its own
/// evidence and the host does not re-run it. What that settlement left behind
/// is this: a `Met` reached on a plugin's word was byte-identical to one
/// reached on a host's observation, at the exact seam the oracle is moving to
/// (#3513). This does not decide anything — `judge` reads it never — it
/// travels with the evidence so the verdict can say whose claim it is.
///
/// Closed, for [`FlipObservation`]'s reason: an unknown provenance must be a
/// refusal, never a silently weaker claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceProvenance {
    /// The flip and the measurements are the plugin's own report of its own
    /// work. Honest, consented to at install, and *not* a host observation.
    PluginReported,
    /// The host itself established this evidence.
    ///
    /// Today the only producer is [`EvidenceSet::unobserved`]: no host in this
    /// tree can observe a flip for itself, because the one host call that
    /// would run a check answers `Unsupported` (#3580).
    HostObserved,
}

/// The rule that decides done, as data.
///
/// Assembled from what the manifest already declares — `[requirements]` and
/// the `[oracle]` block's flip/tamper policy and checks — so a Python author
/// and a Rust author write the identical artifact and neither writes a verdict
/// as code (`doc:pipeline-as-plugins` §6). It travels on the wire because a
/// remote host has to be able to evaluate it, but a plugin never *sends* one:
/// the rule is read from the manifest a human consented to at install.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerdictRule {
    /// The enumerable definition of done: requirement name → the statement a
    /// hold cites. A `BTreeMap`, so the order a verdict reports failures in is
    /// deterministic (invariant 7's discipline).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub requirements: BTreeMap<String, String>,
    /// The plugin's oracle — its flip/tamper policy and its checks, when the
    /// manifest declared one. The oracle runs in the plugin's own process and
    /// reports what it saw (#3511); what travels here is the *rule*, which the
    /// host reads off the manifest and evaluates itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oracle: Option<Oracle>,
}

impl VerdictRule {
    /// Read the rule a manifest declares.
    ///
    /// Total: a manifest with no requirements yields a rule with none, which
    /// `judge` answers [`Verdict::Met`] for — a steering-grade wrapper that
    /// contributes context and gathers nothing has nothing to hold open.
    #[must_use]
    pub fn from_manifest(manifest: &PluginManifest) -> Self {
        Self {
            requirements: manifest.requirements.clone().unwrap_or_default(),
            oracle: manifest.oracle.clone(),
        }
    }
}

/// What the host decided from the evidence.
///
/// Three answers, and the third one is why this is not a `bool`: "nothing
/// available proved it either way" is a real outcome, and reporting it as a
/// failure blames a worker for the instrument while reporting it as a pass is
/// the false claim this project exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Every declared requirement is met by the evidence — and on whose
    /// observation. The provenance is carried, never consulted: `judge`
    /// reaches this arm on the evidence alone (#3513).
    Met {
        /// Whose observation the requirements were met on.
        evidence: EvidenceProvenance,
    },
    /// At least one requirement is determinately unmet.
    Unmet {
        /// The failures, in requirement order.
        unmet: Vec<UnmetRequirement>,
    },
    /// The evidence cannot decide. Scored as unverified: not a pass, and
    /// explicitly not a failure.
    Undecided {
        /// Why, in the words a report prints.
        reason: UndecidedReason,
    },
}

impl Verdict {
    /// Attach the wrapper's own advisory note to every clause this verdict
    /// found unmet.
    ///
    /// **After the decision, never before it.** `judge` reads [`EvidenceSet`],
    /// whose fields are closed by construction so that totality is the
    /// compiler's job; a free-text field on that type would make it an
    /// argument instead. So the host decides first and enriches second, and
    /// nothing downstream of this can change a verdict — the only reader is
    /// the correction text a held-open round renders.
    ///
    /// One note across every clause because that is what the plugin reported:
    /// [`ObservedEvidence::detail`](crate::ObservedEvidence::detail) is one
    /// observation about the turn, not a claim about a particular requirement,
    /// and splitting it per clause would invent an attribution the plugin never
    /// made. The renderer prints it once.
    ///
    /// `None` — a plugin with nothing to add, or a verdict with nothing unmet —
    /// leaves everything exactly as `judge` left it.
    #[must_use]
    pub fn with_detail(mut self, detail: Option<String>) -> Self {
        if let (Some(detail), Self::Unmet { unmet }) = (detail, &mut self) {
            for clause in unmet.iter_mut() {
                clause.detail = Some(detail.clone());
            }
        }
        self
    }
}

/// One clause of the definition of done that the evidence did not meet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnmetRequirement {
    /// The `[requirements]` key.
    pub requirement: String,
    /// Its human-readable statement, carried so a hold message is
    /// attributable without a second lookup.
    pub statement: String,
    /// What the evidence said.
    pub because: UnmetBecause,
    /// What the wrapper's own process wanted the next round told, in its own
    /// words — [`ObservedEvidence::detail`](crate::ObservedEvidence::detail),
    /// carried here and nowhere else (#3840).
    ///
    /// **Never an input to a verdict.** It is attached *after* `judge` has
    /// decided, by `Verdict::with_detail`, so the decision is still made over
    /// [`EvidenceSet`]'s closed fields alone and `judge` is still total over
    /// them. The only thing that reads this is the correction text a held-open
    /// round renders, which is exactly the fidelity the built-in goal loop had
    /// and a plugin did not: `stella_core::goal`'s `verifier_feedback_text`
    /// hands the worker the verifier's own sentence every round, where a
    /// plugin's correction was the same static `[requirements]` statement
    /// however the assessment differed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl std::fmt::Display for UnmetRequirement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} — {}",
            self.requirement, self.statement, self.because
        )
    }
}

/// Why a requirement is unmet. Closed: each variant is one thing the evidence
/// vocabulary can determinately say went wrong.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnmetBecause {
    /// The declared flip was not observed.
    NoFlip {
        /// What was observed instead.
        observed: FlipObservation,
    },
    /// A declared budget was exceeded.
    Budget {
        /// The check as the manifest wrote it.
        check: String,
        /// What the oracle reported for the measurement it reads.
        reported: u64,
    },
    /// The witness artifacts changed between authoring and verification, so
    /// the flip is not credited however it landed.
    Tampered {
        /// The artifact whose identity changed.
        artifact: String,
    },
}

impl std::fmt::Display for UnmetBecause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoFlip { observed } => write!(f, "no fail→pass flip ({observed:?})"),
            Self::Budget { check, reported } => {
                write!(f, "budget \"{check}\" was reported as {reported}")
            }
            Self::Tampered { artifact } => {
                write!(f, "witness artifact \"{artifact}\" was modified")
            }
        }
    }
}

/// Why the evidence could not decide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UndecidedReason {
    /// Requirements were declared but no `[oracle]` exists to establish them.
    NoOracle,
    /// A requirement no check decides, under a policy with no flip to decide
    /// it either. Rejected at load
    /// ([`ManifestError::UndecidableRequirement`](crate::ManifestError)), so
    /// this is reachable only for a rule that did not come from a validated
    /// manifest.
    Undecidable {
        /// The requirement nothing could establish.
        requirement: String,
    },
    /// The oracle reported no value for a measurement one of its checks reads.
    MeasurementMissing {
        /// The requirement the check decides.
        requirement: String,
        /// The measurement that was absent from the reported set.
        measurement: String,
    },
    /// A declared check could not be read. Rejected at load, so reachable only
    /// for a hand-built rule.
    UnreadableCheck {
        /// The requirement the check decides.
        requirement: String,
        /// Why it could not be read.
        reason: String,
    },
    /// The flip evidence itself was unavailable: the witness could not be run,
    /// or the wrapper never gathered anything.
    FlipUnobservable,
    /// The witness failed identically before and after, so it discriminates
    /// nothing.
    WitnessUnsatisfiable,
    /// The declared tamper policy demands a snapshot and none was taken, so
    /// the flip cannot be credited or refused.
    TamperUnchecked,
}

/// Where the wrapper's loop stands when `again` is asked.
///
/// The host's state, not the plugin's: `holds_spent` counts what has already
/// been spent and `host_max_holds` is the ceiling the host enforces. A plugin
/// cannot write either one — [`LoopGrant::max_holds`](crate::LoopGrant) is its
/// *ask*, and the clamp against this ceiling is what keeps a manifest from
/// buying an unbounded loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoundState {
    /// Completion holds already spent by this wrapper in this run.
    pub holds_spent: u32,
    /// The host's own ceiling on holds, whatever the manifest asked for.
    pub host_max_holds: u32,
}

/// What happens after a verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Continuation {
    /// Another turn, carrying the correction the verdict produced.
    Again {
        /// What the next turn is told.
        correction: Correction,
    },
    /// The wrapper's job is over.
    ///
    /// Note what this is *not*: the engine's turn ending. The engine always
    /// finishes its own turn and always says so; this is the wrapper's
    /// separate statement, and both appear in the journal
    /// (`doc:wrapper-socket` §5).
    Stop {
        /// How it ended.
        outcome: Outcome,
    },
}

/// What a held-open turn is told next round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Correction {
    /// The requirements still unmet, so the next turn's instruction is
    /// attributable to the declaration a human consented to.
    pub unmet: Vec<UnmetRequirement>,
    /// The correction text, which rides as a volatile message like every other
    /// contribution — see [`VolatileContext`].
    pub guidance: VolatileContext,
}

/// How a wrapper's loop ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Every declared requirement is met — and on whose observation (#3513).
    Met {
        /// Whose observation the requirements were met on.
        evidence: EvidenceProvenance,
    },
    /// Requirements remain unmet and the loop will not try again.
    Unmet {
        /// What is still unmet — reported, never silently dropped.
        unmet: Vec<UnmetRequirement>,
        /// Why the loop stopped rather than holding again.
        stopped: StopReason,
    },
    /// Nothing decided it either way.
    Undecided {
        /// Why.
        reason: UndecidedReason,
    },
}

/// Why a loop with unmet requirements is not asking for another turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The hold allowance is spent. The effective allowance is the manifest's
    /// ask clamped to the host's ceiling.
    AllowanceSpent {
        /// Holds already spent.
        spent: u32,
        /// The effective allowance after the host's clamp.
        allowed: u32,
    },
    /// The wrapper's declared grade does not permit holding a completion open
    /// — only an `arbiter` may (`doc:pipeline-as-plugins` §4, the `[loop]`
    /// ladder).
    NotAnArbiter,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wrapper::HostStage;

    /// The one exit from [`VolatileContext`] is a non-system message, which is
    /// invariant 7 held structurally rather than by review. If a future edit
    /// adds a placement field or changes the role, this fails.
    #[test]
    fn contributed_context_can_only_become_a_message_after_the_stable_prefix() {
        let message = VolatileContext::new("recall", "the last run failed on I/O").into_message();
        assert_eq!(message.role, stella_protocol::completion::MessageRole::User);
        assert_eq!(message.content, "the last run failed on I/O");
    }

    #[test]
    fn a_published_signal_must_match_its_signals_declared_kind() {
        assert!(
            PublishedSignal {
                signal: Signal::Questions,
                value: SignalValue::Count(3),
            }
            .is_well_typed()
        );
        assert!(
            !PublishedSignal {
                signal: Signal::Questions,
                value: SignalValue::Boolean(true),
            }
            .is_well_typed()
        );
        assert!(
            !PublishedSignal {
                signal: Signal::Conversational,
                value: SignalValue::Count(1),
            }
            .is_well_typed()
        );
    }

    /// Names one destructured field, consuming its binding.
    ///
    /// The consumption is the point: a field added to a pattern below and then
    /// left out of the list beside it is an unused binding, which the
    /// workspace's clippy step (`--all-targets -D warnings`) refuses.
    fn named<T>(path: &'static str, _value: T) -> &'static str {
        path
    }

    /// How many rows [`WIRE_FIELDS`] holds for this field. One is the contract.
    fn rows_for(point: WrapperPoint, path: &str) -> usize {
        WIRE_FIELDS
            .iter()
            .filter(|field| field.point == point && field.path == path)
            .count()
    }

    /// **The anti-drift guard for #3514, made transitive by #4310.** The
    /// consent prompt's data disclosure is only as complete as
    /// [`WIRE_FIELDS`], and a list of sentences maintained by hand is complete
    /// exactly until the next field lands. So the table is checked against the
    /// types themselves: adding a field to either request — or to any type
    /// reachable from one — stops the patterns below compiling (`E0027`), and
    /// a row naming a field no request has fails here.
    #[test]
    fn every_wire_field_is_in_the_table() {
        let BeforeTurnRequest {
            protocol_version,
            wrapper,
            stage,
            round,
            goal,
            candidate,
            published,
        } = BeforeTurnRequest {
            protocol_version: PROTOCOL_VERSION,
            wrapper: "lite-v1".to_string(),
            stage: StageName::Host(HostStage::Execute),
            round: 0,
            goal: "make the failing test pass".to_string(),
            candidate: Some(
                CandidateGrant::new(CandidateHandle::new("candidate-1"), "/tmp/candidate-1")
                    .with_test(TestPlan::new("cargo", vec!["test".to_string()])),
            ),
            published: vec![PublishedSignal {
                signal: Signal::DiffLines,
                value: SignalValue::Count(0),
            }],
        };
        // Neither `candidate` nor `published` is a leaf: every field of both
        // crosses the same pipe, so each is named on its own and the container
        // is not (#4310).
        let CandidateGrant { handle, root, test } =
            candidate.expect("the literal above carries a grant");
        let TestPlan {
            program,
            args,
            baseline,
        } = test.expect("the literal above carries a test plan");
        let PublishedSignal { signal, value } = published
            .into_iter()
            .next()
            .expect("the literal above carries a published signal");
        let before_paths = [
            named("protocol_version", protocol_version),
            named("wrapper", wrapper),
            named("stage", stage),
            named("round", round),
            named("goal", goal),
            named("candidate.handle", handle),
            named("candidate.root", root),
            named("candidate.test.program", program),
            named("candidate.test.args", args),
            named("candidate.test.baseline", baseline),
            named("published.signal", signal),
            named("published.value", value),
        ];

        let AfterTurnRequest {
            protocol_version,
            wrapper,
            stage,
            round,
            goal,
            candidate,
            turn,
        } = AfterTurnRequest {
            protocol_version: PROTOCOL_VERSION,
            wrapper: "lite-v1".to_string(),
            stage: None,
            round: 0,
            goal: "make the failing test pass".to_string(),
            candidate: Some(
                CandidateGrant::new(CandidateHandle::new("candidate-1"), "/tmp/candidate-1")
                    .with_test(TestPlan::new("cargo", vec!["test".to_string()])),
            ),
            turn: TurnOutcome::default(),
        };
        // `turn` is not a leaf — its own fields cross the same pipe, so each is
        // named on its own and the struct itself is not.
        let TurnOutcome {
            completed,
            answer,
            tools,
            changed_files,
        } = turn;
        let CandidateGrant { handle, root, test } =
            candidate.expect("the literal above carries a grant");
        let TestPlan {
            program,
            args,
            baseline,
        } = test.expect("the literal above carries a test plan");
        let after_paths = [
            named("protocol_version", protocol_version),
            named("wrapper", wrapper),
            named("stage", stage),
            named("round", round),
            named("goal", goal),
            named("candidate.handle", handle),
            named("candidate.root", root),
            named("candidate.test.program", program),
            named("candidate.test.args", args),
            named("candidate.test.baseline", baseline),
            named("turn.completed", completed),
            named("turn.answer", answer),
            named("turn.tools", tools),
            named("turn.changed_files", changed_files),
        ];

        let wire: Vec<(WrapperPoint, &str)> = before_paths
            .iter()
            .map(|path| (WrapperPoint::BeforeTurn, *path))
            .chain(
                after_paths
                    .iter()
                    .map(|path| (WrapperPoint::AfterTurn, *path)),
            )
            .collect();

        for (point, path) in &wire {
            assert_eq!(
                rows_for(*point, path),
                1,
                "`{point}.{path}` crosses to a plugin's process and has no single row in \
                 WIRE_FIELDS: give it one, disclosing it or saying `None` on purpose"
            );
        }
        for field in WIRE_FIELDS {
            assert!(
                wire.contains(&(field.point, field.path)),
                "WIRE_FIELDS names `{}.{}`, which is not a field of that request",
                field.point,
                field.path
            );
            assert!(
                field
                    .disclosure
                    .is_none_or(|sentence| !sentence.trim().is_empty()),
                "`{}.{}` is disclosed with no sentence to disclose it in",
                field.point,
                field.path
            );
        }
    }

    /// A host that could not gather evidence must not hand `judge` a set that
    /// reads as "nothing was wrong".
    #[test]
    fn the_unobserved_set_is_unobservable_not_empty() {
        let set = EvidenceSet::unobserved();
        assert_eq!(set.flip, FlipObservation::Unobservable);
        assert_eq!(set.tamper, TamperFinding::NotChecked);
        assert!(set.measurements.is_empty());
    }
}
