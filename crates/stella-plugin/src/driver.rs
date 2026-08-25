//! The driver channel — the wire half of a second dispatch context, for a
//! plugin that **drives** turns rather than sitting inside one.
//!
//! `doc:backlog-self-driving` §3.0 is the design and this module is its wire
//! half. [`crate::host_call`] is the same apparatus for the wrapper socket, and
//! everything essential there is carried over verbatim: a plugin **asks**
//! the host for a capability and never reaches for one, an undeclared ask is
//! refused with a code the plugin branches on, and a refusal is a value rather
//! than a death. Only the *context* differs — the conversation is opened by a
//! driver session, not by a wrapper point.
//!
//! # Why a second context rather than a wider first one
//!
//! [`LoopGrant::permits_call`](crate::LoopGrant::permits_call) requires
//! `participation >= Steering`, and a host call may be made "during
//! `before_turn` or `after_turn` and nowhere else". Both are conditions about
//! standing **inside** a turn. A driver has no turn to stand in — it is what
//! starts them — so no grade on the [`Participation`](crate::Participation)
//! ladder would let it in, and raising its grade would be answering the wrong
//! question. `plugins/stella-selfdriving` declares `participation = "none"`
//! honestly and was, before this module, structurally unable to hold any
//! capability at all (#3599 §2.1).
//!
//! The ladder is therefore **not extended and not renumbered**. Driving is a
//! different axis from in-turn influence, and conflating them would make
//! `arbiter` — already "the strongest grant" over a turn's completion —
//! silently also mean "may merge to `main`". A separate [`DriverGrant`] with
//! its own call list keeps the two consents legible to the human reading them.
//!
//! # The shape
//!
//! ```text
//! host  → { "point": "drive", "body": { "session": "…" } }
//! driver→ { "call": "backlog_next", "id": 1 }
//! host  → { "result": 1, "ok": {} }
//! driver→ { "point": "drive", "body": { "next": { "sleep": { "secs": 900 } } } }
//! ```
//!
//! [`DriverMessage`] is the union of the two things a driver may write, and it
//! is closed for the reason [`PluginMessage`](crate::PluginMessage) is: a
//! message carrying both a `point` and a `call` is not a typo, it is two claims
//! about what the driver is doing, and believing one of them silently is how a
//! session ends in the wrong place (#3500).
//!
//! # What a call carries at B0, and what it does not
//!
//! **No arguments, and no result payload.** Every one of the eighteen verbs in
//! [`DriverCall`] is named by `doc:backlog-self-driving` §3.1–§3.5 and
//! implemented by none of them yet — B0 is the phase that makes a driver able
//! to *hold* a capability, and B1–B6 are the phases that give each one
//! something to do. Inventing eighteen argument tables and eighteen result
//! tables here would be writing a wire contract for behaviour no host code can
//! typecheck against, and every one of them would change at the phase that
//! implemented it. So the argument and result shapes land **with the verb that
//! needs them**, and what B0 pins down is the part consent depends on: which
//! capabilities exist, which of them this driver declared, and what happens
//! when it asks for one it did not.
//!
//! [`DriverOk`] is consequently an empty table today — "the host performed it"
//! — and grows a field per verb that has something to report.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::host_call::HostCallFailure;

/// The capabilities a **driver** may ask the host for.
///
/// Closed, and [`DriverGrant::calls`] declares which of them a given driver may
/// use. The eighteen are exactly the verbs `doc:backlog-self-driving` §3.1–§3.5
/// tabulates, in five families:
///
/// - **`backlog`** (§3.1) — the provider-agnostic issue port that replaces
///   three hardcoded `gh` call sites. Every other family depends on it: `work`
///   needs a unit, `sweep` needs somewhere to file, `deliver` needs something
///   to close.
/// - **`work`** (§3.2) — one unit of backlog through Stella's own staged
///   pipeline, in `candidate_ws.rs`'s shadow worktree, with the pipeline's
///   verdict as the definition of done.
/// - **`deliver`** (§3.3) — branch, PR, CI, review, merge, driven by a pure
///   state machine that buys no model call.
/// - **`sweep`** (§3.4) — where the next question comes from.
/// - **`curate`** (§3.5) — proposals for tools, context records and skills,
///   which the loop may *propose* and never grant itself (§7).
///
/// Two things are deliberately absent, and both are `doc:backlog-self-driving`
/// §3.6's own rules rather than an oversight: there is **no `release` verb** at
/// B0–B6, and there is **no verb that edits a driver's own grant** — a
/// capability that could widen `calls` would make the grant advisory.
///
/// Every verb is its own variant rather than a family with a mode parameter,
/// because invariant 9 governs a capability the same way it governs a tool: a
/// parameter may scope an operation, never select one. `curate accept` and
/// `curate reject` would be two calls, not one with a boolean — which is why
/// the latter is simply not here yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverCall {
    /// The ranked readiness queue.
    BacklogNext,
    /// Take a cooperative lease on an issue, or report who holds it.
    BacklogClaim,
    /// File a finding as an issue, deduplicated by fingerprint.
    BacklogFile,
    /// Close an issue with a receipt naming the evidence.
    BacklogClose,
    /// Bind an `execution_id` to an issue key, so spend is attributable.
    BacklogLink,
    /// Run one unit of backlog through the turn loop the host is driving.
    WorkStart,
    /// The in-flight unit: stage, spend, elapsed, checkpoint.
    WorkStatus,
    /// Release the claim and the worktree, recording why.
    WorkAbandon,
    /// Branch, commit, push, and open the pull request as a draft.
    DeliverOpen,
    /// One read of the forge — CI, reviews, mergeability, base drift.
    DeliverObserve,
    /// The deterministic next action, given the observation.
    DeliverNext,
    /// Merge, when and only when `deliver_next` says so.
    DeliverMerge,
    /// Run the open lens's tooling and emit the findings not yet seen.
    SweepAudit,
    /// Re-run the witnesses named by closed issues' receipts.
    SweepRegress,
    /// Fold the ledger and emit the loop's own pathologies as findings.
    SweepMeta,
    /// Emit a proposal — a tool, a context record, a skill — with evidence.
    CuratePropose,
    /// Pending proposals and their evidence counts.
    CurateList,
    /// Apply a proposal, if the workspace's declared authority permits it.
    CurateAccept,
}

impl DriverCall {
    /// The name this capability is written as on the wire and in
    /// `[driver] calls`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BacklogNext => "backlog_next",
            Self::BacklogClaim => "backlog_claim",
            Self::BacklogFile => "backlog_file",
            Self::BacklogClose => "backlog_close",
            Self::BacklogLink => "backlog_link",
            Self::WorkStart => "work_start",
            Self::WorkStatus => "work_status",
            Self::WorkAbandon => "work_abandon",
            Self::DeliverOpen => "deliver_open",
            Self::DeliverObserve => "deliver_observe",
            Self::DeliverNext => "deliver_next",
            Self::DeliverMerge => "deliver_merge",
            Self::SweepAudit => "sweep_audit",
            Self::SweepRegress => "sweep_regress",
            Self::SweepMeta => "sweep_meta",
            Self::CuratePropose => "curate_propose",
            Self::CurateList => "curate_list",
            Self::CurateAccept => "curate_accept",
        }
    }

    /// Which of the five families this verb belongs to.
    ///
    /// Consent reads by family, because "may read and write your issue tracker"
    /// is the sentence a human weighs and `backlog_file` on its own is not.
    #[must_use]
    pub fn family(self) -> DriverFamily {
        match self {
            Self::BacklogNext
            | Self::BacklogClaim
            | Self::BacklogFile
            | Self::BacklogClose
            | Self::BacklogLink => DriverFamily::Backlog,
            Self::WorkStart | Self::WorkStatus | Self::WorkAbandon => DriverFamily::Work,
            Self::DeliverOpen | Self::DeliverObserve | Self::DeliverNext | Self::DeliverMerge => {
                DriverFamily::Deliver
            }
            Self::SweepAudit | Self::SweepRegress | Self::SweepMeta => DriverFamily::Sweep,
            Self::CuratePropose | Self::CurateList | Self::CurateAccept => DriverFamily::Curate,
        }
    }

    /// Every verb, in declaration order.
    ///
    /// Exhaustive by construction rather than by discipline: the `match` in
    /// [`DriverCall::as_str`] fails to compile if a variant is added and this
    /// list is generated from the same source of truth as the consent
    /// rendering and the wire corpus, so a new capability cannot ship
    /// undocumented.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::BacklogNext,
            Self::BacklogClaim,
            Self::BacklogFile,
            Self::BacklogClose,
            Self::BacklogLink,
            Self::WorkStart,
            Self::WorkStatus,
            Self::WorkAbandon,
            Self::DeliverOpen,
            Self::DeliverObserve,
            Self::DeliverNext,
            Self::DeliverMerge,
            Self::SweepAudit,
            Self::SweepRegress,
            Self::SweepMeta,
            Self::CuratePropose,
            Self::CurateList,
            Self::CurateAccept,
        ]
    }
}

impl std::fmt::Display for DriverCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The five capability families a driver's calls fall into.
///
/// A consent-rendering vocabulary, not a wire tag: nothing on the wire spells a
/// family, because a grant is declared verb by verb and a family-level grant
/// would be a mode selector wearing a consent prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DriverFamily {
    /// The issue port.
    Backlog,
    /// One unit of backlog through the pipeline.
    Work,
    /// Branch, PR, CI, review, merge.
    Deliver,
    /// Where the next question comes from.
    Sweep,
    /// Proposals for tools, context records and skills.
    Curate,
}

impl DriverFamily {
    /// The sentence a human reads at install for this family.
    ///
    /// Stated as what the plugin will do **to their machine and their forge**,
    /// not as the name of a subsystem: `doc:pipeline-as-plugins` §6.1's rule
    /// that a grant a user cannot read before installing is not meaningfully
    /// declared.
    #[must_use]
    pub fn consent_sentence(self) -> &'static str {
        match self {
            Self::Backlog => {
                "reads and writes your issue tracker: ranks the queue, claims issues, \
                              files findings, closes with receipts"
            }
            Self::Work => {
                "runs Stella against an issue in an isolated worktree, spending model \
                           calls and your provider budget"
            }
            Self::Deliver => "pushes branches, opens pull requests, reads CI, and merges",
            Self::Sweep => "runs audit tooling over this workspace and files what it finds",
            Self::Curate => {
                "proposes new tools, context records and skills, and applies the ones \
                             your declared authority permits"
            }
        }
    }

    /// The family's name, as the consent rendering labels it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Backlog => "backlog",
            Self::Work => "work",
            Self::Deliver => "deliver",
            Self::Sweep => "sweep",
            Self::Curate => "curate",
        }
    }
}

impl std::fmt::Display for DriverFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The `[driver]` block — a plugin's declaration that it drives Stella, and of
/// exactly which capabilities it will ask for while doing so.
///
/// **Absent means a plugin is not a driver at all.** That is the outer half of
/// the gate and it is why [`crate::PluginManifest::driver`] is an `Option`
/// rather than a defaulting struct: a manifest with no `[driver]` block opens
/// no driver session, so there is nothing for a call list to be consulted
/// against. The inner half is [`DriverGrant::permits_call`].
///
/// Deliberately carries **no participation grade**. There is no grade to carry:
/// [`Participation`](crate::Participation) is a ladder of in-turn influence and
/// a driver is not on it (§3.0). A plugin may hold both a `[loop]` grant and a
/// `[driver]` grant — a wrapper that also drives is coherent — and neither one
/// grants any part of the other.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriverGrant {
    /// The capabilities this driver may ask for, exhaustively — an undeclared
    /// call is refused, even if the driver's process asks for it.
    ///
    /// Empty is a complete answer and the default: a driver that only reads
    /// what the session handed it declares nothing here. What it may not do is
    /// ask for a capability a human never read at install, which is
    /// [`DriverGrant::permits_call`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<DriverCall>,
    /// The most calls per driver session this plugin asks for.
    ///
    /// **An ask, never an authority** — the
    /// [`LoopGrant::max_calls`](crate::LoopGrant::max_calls) discipline, in the
    /// other dispatch context. The host clamps it against its own ceiling
    /// (`stella_runtime::wrapper::DEFAULT_DRIVER_MAX_CALLS`) and a spent
    /// allowance refuses further calls *to the driver* rather than killing it.
    /// Absent means "the host's ceiling", never "unbounded".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_calls: Option<u32>,
}

impl DriverGrant {
    /// Whether the host may perform `call` when this driver asks for it — the
    /// authoritative filter behind "an undeclared call is refused".
    ///
    /// One condition rather than
    /// [`LoopGrant::permits_call`](crate::LoopGrant::permits_call)'s two, and
    /// the asymmetry is the design rather than a gap: there the first condition
    /// grades the plugin's standing inside a turn, and here the *existence of
    /// this grant* is that standing. A manifest with no `[driver]` block yields
    /// no `DriverGrant` to ask through, which is the same refusal one layer
    /// out.
    #[must_use]
    pub fn permits_call(&self, call: DriverCall) -> bool {
        self.calls.contains(&call)
    }

    /// The families this grant touches, deduplicated and in family order.
    ///
    /// What the consent rendering reads: a human weighs "may push branches and
    /// merge" rather than four verb names.
    #[must_use]
    pub fn families(&self) -> Vec<DriverFamily> {
        let mut families: Vec<DriverFamily> = self.calls.iter().map(|call| call.family()).collect();
        families.sort_unstable();
        families.dedup();
        families
    }
}

/// One message a driver writes while a session is open.
///
/// Closed, and the *session response terminates the conversation* — the framing
/// rule [`PluginMessage`](crate::PluginMessage) states, in the driver's context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverMessage {
    /// A capability ask, mid-session. The conversation continues.
    Call(DriverCallRequest),
    /// The session response. The conversation ends here.
    Response(DriveResponse),
}

impl Serialize for DriverMessage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Call(call) => call.serialize(serializer),
            Self::Response(response) => response.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for DriverMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        DriverEnvelope::deserialize(deserializer)?.into_message()
    }
}

/// Every key either shape of driver message may carry, and no others.
///
/// One envelope for both shapes, for [`crate::host_call`]'s reason: the check
/// that matters is the *cross* one, and `deny_unknown_fields` on two separate
/// envelopes would reject a stray `point` as an unknown field rather than as
/// the contradiction it is.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DriverEnvelope {
    #[serde(default)]
    point: Option<DrivePoint>,
    #[serde(default)]
    body: Option<DriveBody>,
    #[serde(default)]
    call: Option<DriverCall>,
    #[serde(default)]
    id: Option<u32>,
}

impl DriverEnvelope {
    fn into_message<E: serde::de::Error>(self) -> Result<DriverMessage, E> {
        match (self.point, self.call) {
            (Some(_), Some(_)) => Err(E::custom(
                "a driver message carries either `point` (the response that ends the session) or \
                 `call` (a capability ask), never both",
            )),
            (Some(DrivePoint::Drive), None) => {
                if self.id.is_some() {
                    return Err(E::custom(
                        "a session response carries `point` and `body` only; `id` belongs to a \
                         capability ask",
                    ));
                }
                let body = self
                    .body
                    .ok_or_else(|| E::custom("a session response must carry `body`"))?;
                Ok(DriverMessage::Response(DriveResponse { next: body.next }))
            }
            (None, Some(call)) => {
                if self.body.is_some() {
                    return Err(E::custom(
                        "a capability ask carries `call` and `id`; `body` belongs to a session \
                         response",
                    ));
                }
                let id = self
                    .id
                    .ok_or_else(|| E::custom("a capability ask must carry `id`"))?;
                Ok(DriverMessage::Call(DriverCallRequest { id, call }))
            }
            (None, None) => Err(E::custom(
                "a driver message must carry either `point` (a session response) or `call` (a \
                 capability ask)",
            )),
        }
    }
}

/// The one point a driver session dispatches.
///
/// A closed single-variant enum rather than a bare string, so `"point":
/// "before_turn"` written into a driver session is a decode error rather than
/// something a reader shrugs at. A second driver point would be a reviewable
/// addition here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrivePoint {
    /// The driver session itself.
    Drive,
}

/// The host's opening of a driver session: `{"point": "drive", "body": {…}}`.
///
/// The body carries the session identity and nothing else at B0. The loop state
/// `doc:backlog-self-driving` §3.0 sketches beside it is `LoopState`, which is
/// B2's — pure, in `stella-autonomy`, and not yet written. Naming a field for
/// it here would be pinning a wire shape to a type that does not exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriveRequest {
    /// Which point is open. Always [`DrivePoint::Drive`]; present so the
    /// request is self-describing on the wire.
    pub point: DrivePoint,
    /// The session body.
    pub body: DriveSession,
}

impl DriveRequest {
    /// Open a session.
    #[must_use]
    pub fn new(session: impl Into<String>) -> Self {
        Self {
            point: DrivePoint::Drive,
            body: DriveSession {
                session: session.into(),
            },
        }
    }
}

/// What the host tells a driver when it opens the session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriveSession {
    /// The host's identifier for this session, echoed into its telemetry.
    pub session: String,
}

/// The response that ends a driver session.
///
/// A driver does not simply stop: it says what it wants to happen next, and
/// both answers are terminal for *this* session. That is
/// `doc:backlog-self-driving` §3.7's discipline at the wire — **a loop that
/// cannot halt is not autonomous, it is unsupervised**, and a loop that halts
/// without saying why is worse than one that keeps running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveResponse {
    /// What the driver wants to happen after this session.
    pub next: DriveNext,
}

impl<'de> Deserialize<'de> for DriveResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Through the shared envelope, so a response is read by exactly the
        // reader that would have rejected it inside a [`DriverMessage`] — two
        // readers for one shape is how the halves drift.
        match DriverEnvelope::deserialize(deserializer)?.into_message()? {
            DriverMessage::Response(response) => Ok(response),
            DriverMessage::Call(_) => Err(serde::de::Error::custom(
                "expected a session response, found a capability ask",
            )),
        }
    }
}

/// The response body, as the shared envelope reads and writes it.
///
/// Private, because `{"next": …}` is a shape of the envelope rather than a type
/// a driver author handles: what they build is a [`DriveResponse`], and the
/// `point`/`body` framing around it is the channel's.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriveBody {
    next: DriveNext,
}

impl Serialize for DriveResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("DriveResponse", 2)?;
        state.serialize_field("point", &DrivePoint::Drive)?;
        state.serialize_field(
            "body",
            &DriveBody {
                next: self.next.clone(),
            },
        )?;
        state.end()
    }
}

/// What a driver asks to happen after this session.
///
/// Two answers, both of them terminal for the session and neither of them a
/// silence. `Sleep` is `doc:backlog-self-driving` §3.7's `Watch`, reduced to the
/// only wake condition B0 can honestly serve — a duration. The condition-shaped
/// wakes (`WakeCondition`) arrive with the supply model in B4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriveNext {
    /// Re-open a session after this many seconds.
    Sleep {
        /// How long to wait. The host clamps it; a driver cannot buy an
        /// unbounded absence by asking for one.
        secs: u32,
    },
    /// Stop, and say why. Reached for a spent budget, a revoked grant, an
    /// operator stop, or work the driver declines to continue.
    Halt {
        /// The sentence a human reads in the loop's ledger.
        reason: String,
    },
}

/// One capability ask: `{"call": …, "id": …}`.
///
/// The `id` is the driver's own correlation number and the host echoes it back
/// as [`DriverCallResponse::result`], for
/// [`HostCallRequest`](crate::HostCallRequest)'s reason: a driver that
/// pipelines two asks needs to tell the answers apart, and a number the host
/// assigned would arrive too late to be useful.
///
/// There is no `args` key, and its absence is checked rather than tolerated —
/// see this module's header. The verb that needs arguments brings them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DriverCallRequest {
    /// What the driver calls this ask, echoed on the answer.
    pub id: u32,
    /// Which capability.
    pub call: DriverCall,
}

impl<'de> Deserialize<'de> for DriverCallRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match DriverEnvelope::deserialize(deserializer)?.into_message()? {
            DriverMessage::Call(call) => Ok(call),
            DriverMessage::Response(_) => Err(serde::de::Error::custom(
                "expected a capability ask, found a session response",
            )),
        }
    }
}

/// The host's answer to one capability ask: `{"result": …, "ok"|"err": {…}}`.
///
/// `Deserialize` is written out by hand over a denying envelope for the reason
/// [`HostCallResponse`](crate::HostCallResponse) measured rather than assumed:
/// serde's flat-map reader takes the first key it recognises and drops the
/// rest, so a derived reader would decode `{"result":1,"ok":{},"err":{…}}` as a
/// success and the refusal beside it would vanish.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DriverCallResponse {
    /// The [`DriverCallRequest::id`] this answers.
    pub result: u32,
    /// What came of it.
    #[serde(flatten)]
    pub outcome: DriverCallOutcome,
}

/// The three keys an answer may carry, and the only three.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DriverResultEnvelope {
    result: u32,
    #[serde(default)]
    ok: Option<DriverOk>,
    #[serde(default)]
    err: Option<HostCallFailure>,
}

impl<'de> Deserialize<'de> for DriverCallResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let envelope = DriverResultEnvelope::deserialize(deserializer)?;
        let outcome = match (envelope.ok, envelope.err) {
            (Some(ok), None) => DriverCallOutcome::Ok(ok),
            (None, Some(err)) => DriverCallOutcome::Err(err),
            (Some(_), Some(_)) => {
                return Err(serde::de::Error::custom(
                    "a driver-call answer carries either `ok` or `err`, never both",
                ));
            }
            (None, None) => {
                return Err(serde::de::Error::custom(
                    "a driver-call answer must carry either `ok` or `err`",
                ));
            }
        };
        Ok(Self {
            result: envelope.result,
            outcome,
        })
    }
}

impl DriverCallResponse {
    /// An answer carrying what the host did.
    #[must_use]
    pub fn ok(result: u32, ok: DriverOk) -> Self {
        Self {
            result,
            outcome: DriverCallOutcome::Ok(ok),
        }
    }

    /// An answer carrying why the host would not, or could not.
    #[must_use]
    pub fn err(result: u32, failure: HostCallFailure) -> Self {
        Self {
            result,
            outcome: DriverCallOutcome::Err(failure),
        }
    }
}

/// Whether a capability ask succeeded, as the one key that distinguishes them.
///
/// The failure half is [`HostCallFailure`] itself, reused rather than mirrored:
/// the six reasons a capability is not performed — undeclared, forbidden,
/// unsupported, allowance spent, unavailable, failed — are properties of asking
/// a host for something, not of which dispatch context asked. A driver branches
/// on the same closed [`HostCallRefusal`](crate::HostCallRefusal) codes a
/// wrapper does, and a second copy of that enum would be two vocabularies for
/// one concept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverCallOutcome {
    /// The host performed the call.
    Ok(DriverOk),
    /// The host refused it, or tried it and it failed. Either way the driver is
    /// told, and is expected to degrade honestly rather than die.
    Err(HostCallFailure),
}

/// What a successful capability ask returned.
///
/// **Empty today, and that is the honest shape rather than a placeholder.** No
/// verb is implemented at B0 (this module's header says why), so there is
/// nothing for a result table to describe. The phase that implements a verb
/// adds the field it reports — and because this table denies unknown fields, a
/// host answering with a payload the contract does not have is a decode error
/// on the driver's side rather than a value silently dropped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriverOk {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_call::HostCallRefusal;

    #[test]
    fn every_call_has_a_distinct_wire_name_and_a_family() {
        let mut names: Vec<&str> = DriverCall::all().iter().map(|call| call.as_str()).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "two capabilities share a wire name");
        // Eighteen verbs across five families, and every family reachable —
        // a family no verb belongs to would be a consent sentence nothing can
        // ever print.
        let mut families: Vec<DriverFamily> =
            DriverCall::all().iter().map(|call| call.family()).collect();
        families.sort_unstable();
        families.dedup();
        assert_eq!(families.len(), 5);
    }

    #[test]
    fn all_is_exhaustive_over_the_enum() {
        // Round-tripping every wire name back through the deserializer proves
        // `all()` and the enum agree, without a hand-maintained count.
        for call in DriverCall::all() {
            let json = serde_json::to_string(call).expect("a call serializes");
            let back: DriverCall = serde_json::from_str(&json).expect("and reads back");
            assert_eq!(back, *call);
        }
        let missed = serde_json::from_str::<DriverCall>("\"release\"");
        assert!(
            missed.is_err(),
            "`release` is deliberately not a capability"
        );
    }

    #[test]
    fn an_undeclared_call_is_not_permitted_and_a_declared_one_is() {
        let grant = DriverGrant {
            calls: vec![DriverCall::BacklogNext],
            max_calls: None,
        };
        assert!(grant.permits_call(DriverCall::BacklogNext));
        assert!(!grant.permits_call(DriverCall::DeliverMerge));
    }

    #[test]
    fn families_are_deduplicated_and_ordered() {
        let grant = DriverGrant {
            calls: vec![
                DriverCall::DeliverMerge,
                DriverCall::BacklogNext,
                DriverCall::BacklogFile,
            ],
            max_calls: None,
        };
        assert_eq!(
            grant.families(),
            vec![DriverFamily::Backlog, DriverFamily::Deliver]
        );
    }

    #[test]
    fn a_message_carrying_both_a_point_and_a_call_is_refused() {
        let err = serde_json::from_str::<DriverMessage>(
            r#"{"point":"drive","body":{"next":{"halt":{"reason":"x"}}},"call":"backlog_next","id":1}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("never both"), "{err}");
    }

    #[test]
    fn a_capability_ask_may_not_carry_arguments_yet() {
        // `args` is not a key of this contract at B0, and the envelope denies
        // it rather than ignoring it — a driver written against a later phase's
        // arguments must fail loudly against a host that has none.
        let err = serde_json::from_str::<DriverCallRequest>(
            r#"{"call":"backlog_next","id":1,"args":{"limit":5}}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("args"), "{err}");
    }

    #[test]
    fn a_session_round_trips_through_both_message_shapes() {
        let ask = DriverMessage::Call(DriverCallRequest {
            id: 1,
            call: DriverCall::BacklogNext,
        });
        let json = serde_json::to_string(&ask).expect("an ask serializes");
        assert_eq!(json, r#"{"id":1,"call":"backlog_next"}"#);
        assert_eq!(
            serde_json::from_str::<DriverMessage>(&json).expect("and reads back"),
            ask
        );

        let done = DriverMessage::Response(DriveResponse {
            next: DriveNext::Sleep { secs: 900 },
        });
        let json = serde_json::to_string(&done).expect("a response serializes");
        assert_eq!(
            json,
            r#"{"point":"drive","body":{"next":{"sleep":{"secs":900}}}}"#
        );
        assert_eq!(
            serde_json::from_str::<DriverMessage>(&json).expect("and reads back"),
            done
        );
    }

    #[test]
    fn an_answer_carrying_both_ok_and_err_does_not_decode_as_success() {
        let err = serde_json::from_str::<DriverCallResponse>(
            r#"{"result":1,"ok":{},"err":{"refusal":"undeclared","detail":"x"}}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("never both"), "{err}");
    }

    #[test]
    fn a_refusal_round_trips_with_its_code_intact() {
        let refused = DriverCallResponse::err(
            7,
            HostCallFailure::new(HostCallRefusal::Undeclared, "not in [driver] calls"),
        );
        let json = serde_json::to_string(&refused).expect("a refusal serializes");
        let back: DriverCallResponse = serde_json::from_str(&json).expect("and reads back");
        assert_eq!(back, refused);
        match back.outcome {
            DriverCallOutcome::Err(failure) => {
                assert_eq!(failure.refusal, HostCallRefusal::Undeclared);
            }
            DriverCallOutcome::Ok(_) => panic!("a refusal decoded as a success"),
        }
    }
}
