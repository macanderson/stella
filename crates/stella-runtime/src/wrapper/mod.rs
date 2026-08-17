//! The wrapper socket — four points, one wire contract, no host assumed.
//!
//! `doc:wrapper-socket` is the design; this module is it. A wrapper is **two
//! async calls it makes, plus two pure functions the host runs on its behalf**,
//! and that asymmetry is the whole thing:
//!
//! | Point | Shape | Where |
//! |---|---|---|
//! | `before_turn` | async, may be remoted | [`TurnWrapper::before_turn`] |
//! | `after_turn` | async, may be remoted | [`TurnWrapper::after_turn`] |
//! | `judge` | synchronous, host-run, total | [`judge`] |
//! | `again?` | synchronous, host-run, total | [`again`] |
//!
//! # Why the trait is here and the types are one crate down
//!
//! `before_turn` performs recall and `after_turn` spawns processes. That is
//! I/O, and invariant 2 forbids I/O in the engine, so the socket cannot live in
//! `stella-core` (`doc:turn-loop-wrappers` §9.1). It lives in the crate that
//! already owns engine assembly and reads no ambient environment by contract
//! (`tests/no_ambient_reads.rs`) — including here: nothing in this module
//! consults the process environment or the current directory, which is exactly
//! what lets the same wrapper run under N sessions with N workspace roots.
//!
//! The request and response *types* live in `stella-plugin`, because a non-Rust
//! plugin author needs that crate's JSON shapes and nothing else. The Rust
//! trait is a typed view of the same shapes, never a second design
//! (`doc:pipeline-as-plugins` §5 commitment 1) — which is why this module and
//! `stella_plugin::wire` were authored in one change.
//!
//! # Two transports, and the wire one is the one CI exercises
//!
//! [`SubprocessWrapper`] speaks the wire contract over stdio, in whatever
//! language the plugin is written in. [`InProcessWrapper`] is the in-process
//! fast path §3 explicitly permits for Rust — and it is *only* a permit: both
//! implementations take and return the identical owned request/response types,
//! so nothing is reachable through one that is not reachable through the other.
//! If Rust ever gets a capability Python cannot ask for, the wire contract has
//! become second-class and will rot.
//!
//! # What a host must not skip
//!
//! [`admissible`] is the check between a plugin's answer and the turn: a role
//! intent the manifest never declared is refused, and a signal published at the
//! wrong type is refused. Both are load-time rules `stella-plugin` already
//! enforces on *declarations*; a socket that dispatches has to enforce the same
//! rules on *values*, or the manifest's guarantees stop at the process
//! boundary.
//!
//! It is also **on** the path rather than beside it: it consumes the response
//! and answers with an [`AdmittedContribution`], which is the only value
//! [`WrapperDispatch`] will apply. When the check merely returned `Ok(())` and
//! the caller went on to read the response it already had, "the host checked
//! first" was a habit with no non-test caller to practise it (#3494).
//!
//! # Who calls the four points
//!
//! [`WrapperDispatch`] — the host sequence, and the reason it is here rather than in
//! `stella-cli`: `doc:wrapper-socket` §6 requires the same plugin to run under
//! three drivers, and a sequence living in the binary is one the other two
//! cannot reach.
//!
//! # The conversation inside a point
//!
//! Every point above is the host asking and the plugin answering, and that was
//! the whole socket until #3540: strictly one-directional, which forecloses any
//! plugin that needs something only the host has. `doc:wrapper-socket` §6b
//! corrects it — **a plugin may ask the host for a capability; it may never
//! reach for one** — and [`HostCallGate`] is the host half. A point becomes a
//! bounded conversation that *ends* in the point response, and both transports
//! drive the identical [`HostCallChannel`], so a Rust plugin can ask for exactly
//! what a Python plugin can ask for.
//!
//! Nothing about the four points changes. [`judge`] and [`again`] gain nothing:
//! a host call is available during `before_turn` and `after_turn` only, so both
//! stay synchronous, I/O-free and total, and a plugin still cannot grade its own
//! work with a model. The host performs every call itself, behind
//! [`LoopGrant::permits_call`](stella_plugin::LoopGrant::permits_call) and a
//! clamped per-point allowance, so what the plugin gets is what a human
//! consented to at install and nothing more.

mod dispatch;
mod error;
mod host_call;
mod in_process;
mod subprocess;
mod verdict;

use async_trait::async_trait;
use stella_plugin::{
    AfterTurnRequest, AfterTurnResponse, BeforeTurnRequest, BeforeTurnResponse, PluginManifest,
    PublishedSignal, SignalValue, VolatileContext,
};
use stella_protocol::completion::CompletionMessage;

pub use dispatch::{
    DEFAULT_HOST_MAX_HOLDS, DispatchReport, DrivenTurn, RoundInput, TurnDriver, TurnPrelude,
    WrapperDispatch,
};
pub use error::WrapperError;
pub use host_call::{
    DEFAULT_HOST_MAX_CALLS, DEFAULT_RECALL_FRAMES, HostCallChannel, HostCallGate, HostCapabilities,
    NoHostCalls, PointChannel, RecallHost, RecallOnly, RefusedCall,
};
pub use in_process::{InProcessWrapper, WrapperHandler};
pub use subprocess::{
    AdmittedWrapper, DEFAULT_WRAPPER_TIMEOUT, MAX_WRAPPER_TIMEOUT, SubprocessWrapper,
    refuses_env_name,
};
pub use verdict::{again, judge};

/// The two points a wrapper answers.
///
/// Both are `async` because either may be remoted — to a child process, to an
/// HTTP host, to anything that can carry JSON. Both take an owned request and
/// return an owned response: an out-of-process plugin cannot be handed a `&dyn`
/// anything, so there are no borrows, no lifetimes and no trait objects in
/// either signature (`doc:wrapper-socket` §6).
///
/// Note what is **not** here. There is no `judge` and no `again`: they are free
/// functions in this module ([`judge`], [`again`]), over the rule the plugin's
/// manifest declared and the evidence its `after_turn` returned. A plugin
/// cannot implement either one, which is what keeps a verdict from quietly
/// becoming a model call (`doc:pipeline-as-plugins` §6). There is also no
/// `Engine`, no provider and no credential: a wrapper names a role *intent* and
/// the host makes every model call itself (invariant 3).
#[async_trait]
pub trait TurnWrapper: Send + Sync {
    /// Contribute to a turn that has not run yet.
    ///
    /// May add context, narrow scope, name a role intent, publish signals for
    /// later stages. May **not** run the loop itself or reach for ambient
    /// authority — every capability arrives in the request.
    ///
    /// # Errors
    ///
    /// [`WrapperError`] when the wrapper could not be reached, exceeded its
    /// budget, or answered unusably. A host that catches one runs the turn
    /// without the contribution rather than failing the turn: a wrapper that
    /// cannot speak has nothing to say, and that is not the user's fault.
    async fn before_turn(
        &self,
        request: BeforeTurnRequest,
    ) -> Result<BeforeTurnResponse, WrapperError>;

    /// Gather evidence about the turn that just completed.
    ///
    /// May run a test, read a diff, author a witness, or spend a declared
    /// role's model call and report the parsed assessment as a measurement.
    /// May **not** change the turn that ran: it receives the outcome and holds
    /// no channel into it (#3379's one-directional connection, as a socket
    /// rule).
    ///
    /// # Errors
    ///
    /// [`WrapperError`], as above. A host that catches one passes
    /// [`EvidenceSet::unobserved`](stella_plugin::EvidenceSet::unobserved) to
    /// [`judge`] so the verdict abstains, rather than reading silence as a
    /// failure of the work.
    async fn after_turn(
        &self,
        request: AfterTurnRequest,
    ) -> Result<AfterTurnResponse, WrapperError>;
}

/// A `before_turn` contribution that has passed [`admissible`].
///
/// **The check's output, not a receipt for it.** There is no constructor and no
/// public field: the only way to hold one is to have run the check that
/// produces it, and the only way to spend the context inside it is
/// [`Self::into_messages`]. That is the difference between a rule and a
/// property — while [`admissible`] answered `Ok(())` and left the caller to
/// read the `BeforeTurnResponse` it already had, nothing stopped a host
/// applying a contribution it never checked, and for as long as the socket had
/// no host at all nothing noticed (#3494).
///
/// What this does **not** claim: that a host cannot reach into a
/// [`BeforeTurnResponse`]'s public fields itself. Those fields are public
/// because a plugin author in another language needs the shape, and no type can
/// close that. What is closed is the cheap path — the one a contributor takes
/// without noticing they took it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedContribution {
    role: Option<String>,
    context: Vec<VolatileContext>,
    scope: Vec<String>,
    publish: Vec<PublishedSignal>,
}

impl AdmittedContribution {
    /// The role intent, already checked against the `[roles]` the manifest
    /// declares.
    #[must_use]
    pub fn role(&self) -> Option<&str> {
        self.role.as_deref()
    }

    /// Workspace-relative paths the wrapper believes the turn should stay
    /// within. Advisory: a plugin-supplied path is never itself a permission.
    #[must_use]
    pub fn scope(&self) -> &[String] {
        &self.scope
    }

    /// The signals this stage published, already checked against the kind each
    /// one declares.
    #[must_use]
    pub fn published(&self) -> &[PublishedSignal] {
        &self.publish
    }

    /// Spend the contributed context as the volatile messages it is.
    ///
    /// Invariant 7, held at the type rather than at the call site: every
    /// message here is a user message built by
    /// [`VolatileContext::into_message`], so a host has nothing in hand that
    /// could reach the byte-stable system prefix.
    #[must_use]
    pub fn into_messages(self) -> Vec<CompletionMessage> {
        self.context
            .into_iter()
            .map(VolatileContext::into_message)
            .collect()
    }
}

/// Whether a `before_turn` contribution may be applied to the turn.
///
/// Two checks, both of which restate a load-time rule at the value level:
///
/// - **A role intent must be declared.** `[roles.<name>]` is what a human read
///   at install; an intent invented at run time is a routing decision nobody
///   consented to. Refused, not defaulted — a soft-fail here would make the
///   consent text a description of the common case.
/// - **A published signal must match its declared kind.** A count published as
///   a boolean is the same mistake
///   `ManifestError::ConditionTypeMismatch` rejects in a condition, arriving
///   from the other side. JSON has both shapes, so only the host can catch it.
///
/// The contributed *context* needs no check here, and that is the point of
/// `VolatileContext`: there is no field on it that could ask for the stable
/// prefix, so "a plugin cannot make every turn a cache miss" is a property of
/// the type rather than a rule this function has to remember (invariant 7).
///
/// It takes the response **by value** and hands back the checked contribution,
/// so applying something unchecked means writing a line that says so rather
/// than forgetting a line.
///
/// # Errors
///
/// [`WrapperError::UndeclaredRole`] or [`WrapperError::MistypedSignal`],
/// naming the offending value.
pub fn admissible(
    manifest: &PluginManifest,
    response: BeforeTurnResponse,
) -> Result<AdmittedContribution, WrapperError> {
    let wrapper = manifest
        .wrapper
        .as_ref()
        .map_or_else(|| manifest.name.clone(), |w| w.id.clone());

    if let Some(role) = &response.role {
        let declared = manifest
            .roles
            .as_ref()
            .is_some_and(|roles| roles.contains_key(role));
        if !declared {
            return Err(WrapperError::UndeclaredRole {
                wrapper,
                role: role.clone(),
            });
        }
    }

    for published in &response.publish {
        if !published.is_well_typed() {
            return Err(WrapperError::MistypedSignal {
                wrapper,
                signal: published.signal,
                published: match published.value {
                    SignalValue::Boolean(_) => "boolean",
                    SignalValue::Count(_) => "count",
                },
                declared: published.signal.kind(),
            });
        }
    }

    Ok(AdmittedContribution {
        role: response.role,
        context: response.context,
        scope: response.scope,
        publish: response.publish,
    })
}
