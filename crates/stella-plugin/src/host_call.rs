//! The host-call channel — a plugin may **ask** the host for a capability, and
//! may never reach for one.
//!
//! `doc:wrapper-socket` §6b is the design and this module is its wire half. §5
//! defines four points and every one of them is the host asking and the plugin
//! answering; that is one exchange in one direction, and it forecloses an
//! entire class of plugin — one that needs something only the host has.
//! `stella-research` shipped its research half and **declined recall** because
//! there was nothing on the wire it could use, and `stella-plan` reads the same
//! frames, so the second extraction was blocked by the identical gap (#3540).
//!
//! # The shape
//!
//! While handling a point, a plugin may emit host calls and read their
//! responses before returning the response that ends the exchange. One
//! request/response becomes a bounded **conversation** that *ends* in the point
//! response:
//!
//! ```text
//! host  → { "point": "before_turn", "body": { … } }
//! plugin→ { "call": "recall", "id": 1, "args": { "goal": "…", "limit": 8 } }
//! host  → { "result": 1, "ok": { "frames": [ … ] } }
//! plugin→ { "point": "before_turn", "body": { "context": [ … ] } }     ← ends it
//! ```
//!
//! [`PluginMessage`] is the union of the two things a plugin may write, and it
//! is a **closed** union: a message is a host call or the point response, never
//! both and never a third thing.
//!
//! # What this deliberately is not
//!
//! **Not an RPC surface.** [`HostCall`] is a closed enum of three capabilities,
//! and a plugin may only make a call its manifest declared — refused the way an
//! undeclared hook is refused ([`LoopGrant::permits_hook`] is the precedent,
//! [`LoopGrant::permits_call`] is the filter). A new capability is a reviewable
//! addition to that enum, never a string a plugin invents.
//!
//! **Not ambient authority.** The plugin does not retrieve; it asks. The host
//! performs the retrieval, applies the gate, and returns only what the declared
//! grant permits. An ask is the *handing* of a capability, made explicit —
//! `doc:pipeline-as-plugins` §0.3 intact.
//!
//! **Not available to `judge` or `again`.** A host call happens during
//! `before_turn` or `after_turn` and nowhere else, so both host functions stay
//! synchronous, I/O-free and total, and "a plugin cannot grade its own work with
//! a model" survives untouched.
//!
//! # A refusal is a value, not a death
//!
//! Every failure a call can have — undeclared, unsupported, over the allowance,
//! no such plane configured, tried and failed — arrives back at the plugin as a
//! [`HostCallFailure`] with a closed [`HostCallRefusal`] code it can branch on.
//! That is the fail-open direction the Stop gate already argues for
//! (`crates/stella-core/src/user_hooks.rs:55-59`): a plugin that asked for
//! something it may not have should degrade honestly, not be killed mid-point.
//! The host's own duty is the other half — the refusal is **reported**, never
//! silent (`stella_runtime::wrapper::HostCallGate::refusals`).
//!
//! [`LoopGrant::permits_hook`]: crate::LoopGrant::permits_hook
//! [`LoopGrant::permits_call`]: crate::LoopGrant::permits_call

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use stella_protocol::candidate::CandidateHandle;

use crate::wire::{WrapperPoint, WrapperResponse, decode_body};

/// The capabilities a plugin may ask the host for.
///
/// **Closed, and the manifest declares which of them this plugin may use**
/// (`[loop] calls`). The three are the ones `doc:wrapper-socket` §6b names, and
/// each exists because a real extraction is blocked without it:
///
/// - `recall` — the context plane, read-only. `stella-research` is defined as
///   replacing "research sub-agents, **recall**", and recall is
///   `ContextRecallPort::recall(goal) -> Recall { frames, .. }` fanned out over
///   `context.db` and `codegraph.db`. A plugin gets none of it without this.
/// - `child_turn` — a bounded turn at a declared role intent, which
///   `doc:turn-loop-wrappers` §9.3 named: "a wrapper is handed a `ChildTurn`
///   port… it names a role intent; the host resolves it, carves the budget,
///   runs the turn and settles once." Performed by
///   `stella_runtime::wrapper::ChildTurns` over the host's own sub-agent
///   dispatcher (#3564), so the plugin holds no provider and no credential and
///   every model call is the host's.
/// - `run_test` — the candidate's test invocation, for the re-runs a
///   verification plugin needs. #3498 solved the *first* run narrowly by putting
///   the plan in the request; it stays there, and this call is the re-run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostCall {
    /// Ask the context plane for material relevant to a goal.
    Recall,
    /// Ask for one bounded turn at a declared role intent.
    ChildTurn,
    /// Ask for the candidate workspace's test invocation to be run again.
    RunTest,
}

impl HostCall {
    /// The name this capability is written as on the wire and in `[loop] calls`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recall => "recall",
            Self::ChildTurn => "child_turn",
            Self::RunTest => "run_test",
        }
    }
}

impl std::fmt::Display for HostCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One message a plugin writes while a point is open.
///
/// The union is closed and the *point response terminates the conversation* —
/// that is the whole framing rule. A plugin that writes a second message after
/// its point response is writing to a host that has stopped listening.
///
/// `Deserialize` is written out by hand over a denying envelope, for the reason
/// `stella_plugin::wire`'s own envelope is: serde's readers for tagged enums
/// ignore every key beside the ones they name, so a mixed or misspelled message
/// would decode cleanly and the extra half would vanish (#3500). Here the
/// hazard is sharper than there — a message carrying *both* a `point` and a
/// `call` is not a typo, it is two claims about what the plugin is doing, and
/// silently believing one of them is how a conversation ends in the wrong place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginMessage {
    /// A host call, mid-point. The conversation continues.
    Call(HostCallRequest),
    /// The point response. The conversation ends here.
    Response(WrapperResponse),
}

impl Serialize for PluginMessage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Delegated rather than tagged: the two arms are already distinguished
        // on the wire by their own keys, and wrapping them would invent a
        // framing §6b does not have.
        match self {
            Self::Call(call) => call.serialize(serializer),
            Self::Response(response) => response.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for PluginMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        PluginEnvelope::deserialize(deserializer)?.into_message()
    }
}

/// Every key either shape of plugin message may carry, and no others.
///
/// One envelope for both shapes rather than two, because the check that matters
/// is the *cross* one: `deny_unknown_fields` on a three-key call envelope would
/// reject a stray `point`, but it would reject it as an unknown field rather
/// than as the contradiction it is, and a reader of two separate envelopes has
/// to remember to try them in the right order.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginEnvelope {
    #[serde(default)]
    point: Option<WrapperPoint>,
    #[serde(default)]
    body: Option<serde_json::Value>,
    #[serde(default)]
    call: Option<HostCall>,
    #[serde(default)]
    id: Option<u32>,
    #[serde(default)]
    args: Option<serde_json::Value>,
}

impl PluginEnvelope {
    /// Classify one envelope, or say why it is neither shape.
    fn into_message<E: serde::de::Error>(self) -> Result<PluginMessage, E> {
        match (self.point, self.call) {
            (Some(_), Some(_)) => Err(E::custom(
                "a plugin message carries either `point` (the response that ends the point) or \
                 `call` (a host call), never both",
            )),
            (Some(point), None) => {
                if self.id.is_some() || self.args.is_some() {
                    return Err(E::custom(
                        "a point response carries `point` and `body` only; `id` and `args` belong \
                         to a host call",
                    ));
                }
                let body = self
                    .body
                    .ok_or_else(|| E::custom("a point response must carry `body`"))?;
                WrapperResponse::from_parts(point, body).map(PluginMessage::Response)
            }
            (None, Some(call)) => {
                if self.body.is_some() {
                    return Err(E::custom(
                        "a host call carries `call`, `id` and `args`; `body` belongs to a point \
                         response",
                    ));
                }
                let id = self
                    .id
                    .ok_or_else(|| E::custom("a host call must carry `id`"))?;
                let args = self
                    .args
                    .ok_or_else(|| E::custom("a host call must carry `args`"))?;
                let args = match call {
                    HostCall::Recall => decode_body(args).map(HostCallArgs::Recall)?,
                    HostCall::ChildTurn => decode_body(args).map(HostCallArgs::ChildTurn)?,
                    HostCall::RunTest => decode_body(args).map(HostCallArgs::RunTest)?,
                };
                Ok(PluginMessage::Call(HostCallRequest { id, args }))
            }
            (None, None) => Err(E::custom(
                "a plugin message must carry either `point` (a point response) or `call` (a host \
                 call)",
            )),
        }
    }
}

/// One host call: `{"call": …, "id": …, "args": {…}}`.
///
/// The `id` is the plugin's own correlation number and the host echoes it back
/// as [`HostCallResponse::result`]. It is deliberately the *plugin's* to choose:
/// a plugin that pipelines two calls needs to tell the answers apart, and a
/// number the host assigned would arrive too late to be useful.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostCallRequest {
    /// What the plugin calls this call, echoed on the answer.
    pub id: u32,
    /// Which capability, and the arguments for it.
    ///
    /// Flattened, so the wire shape is the flat `{"call", "id", "args"}` §6b
    /// specifies rather than a nested object.
    #[serde(flatten)]
    pub args: HostCallArgs,
}

impl<'de> Deserialize<'de> for HostCallRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match PluginEnvelope::deserialize(deserializer)?.into_message()? {
            PluginMessage::Call(call) => Ok(call),
            PluginMessage::Response(_) => Err(serde::de::Error::custom(
                "expected a host call, found a point response",
            )),
        }
    }
}

impl HostCallRequest {
    /// Which capability this call asks for.
    #[must_use]
    pub fn call(&self) -> HostCall {
        self.args.call()
    }
}

/// The arguments of each capability, tagged by the capability's own name.
///
/// Adjacently tagged (`call` / `args`) for the reason
/// [`WrapperRequest`](crate::WrapperRequest) is: an internally tagged enum hands
/// the tag down into the variant, where a `deny_unknown_fields` struct rejects
/// it, and every argument table here denies unknown fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "call", content = "args", rename_all = "snake_case")]
pub enum HostCallArgs {
    /// Ask the context plane for material relevant to a goal.
    Recall(RecallArgs),
    /// Ask for one bounded turn at a declared role intent.
    ChildTurn(ChildTurnArgs),
    /// Ask for the candidate workspace's test invocation to be run again.
    RunTest(RunTestArgs),
}

impl HostCallArgs {
    /// Which capability these arguments are for.
    #[must_use]
    pub fn call(&self) -> HostCall {
        match self {
            Self::Recall(_) => HostCall::Recall,
            Self::ChildTurn(_) => HostCall::ChildTurn,
            Self::RunTest(_) => HostCall::RunTest,
        }
    }
}

/// `recall` — what the plugin wants recalled, and how much of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallArgs {
    /// What to recall material about. Ordinarily the goal the point's request
    /// carried, but a plugin narrowing to a sub-question is the point of asking
    /// rather than being handed frames.
    pub goal: String,
    /// The most frames the plugin wants back.
    ///
    /// **An ask, never an authority** — the `max_holds` discipline, one layer
    /// down. The host clamps it against its own ceiling, and `None` means "the
    /// host's own default", not "unbounded".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// `child_turn` — a bounded turn at a declared role intent.
///
/// The role is an *intent* — the name of a `[roles.<name>]` entry the same
/// manifest declares — never a model id, a provider, a URL or a credential. The
/// host resolves it against the user's BYOK providers, carves the budget, runs
/// the turn and settles once, so invariant 3's "every model call is made by the
/// host" survives a plugin asking for one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildTurnArgs {
    /// The declared role intent to run the turn at.
    pub role: String,
    /// What the child turn is asked to do.
    pub instruction: String,
}

/// `run_test` — re-run the candidate workspace's declared test invocation.
///
/// The *plan* is not a parameter: it arrived in the point's request as
/// [`CandidateGrant::test`](crate::CandidateGrant), which is where #3498 put it
/// and where it stays. What crosses here is only which workspace to run it in,
/// as the opaque handle the host minted — a plugin cannot name a directory and
/// have the host run a test in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunTestArgs {
    /// The candidate workspace, as the grant named it.
    pub candidate: CandidateHandle,
}

/// The host's answer to one host call: `{"result": …, "ok"|"err": {…}}`.
///
/// `Serialize` is derived over a flattened [`HostCallOutcome`], which produces
/// exactly the two-key shape §6b writes down. `Deserialize` is written out by
/// hand, and the reason is measured rather than assumed: this type first
/// carried a derived reader with the note that a flattened externally tagged
/// enum makes "exactly one of `ok` and `err`" structural. **It does not** —
/// serde's flat-map reader takes the first key it recognises and drops the
/// rest, so `{"result":1,"ok":{},"err":{…}}` decoded as a success and the
/// refusal beside it vanished. That is #3500's shape a third time, in the one
/// direction where the reader is a plugin's own SDK. The envelope below denies
/// unknown fields and refuses both-or-neither, so the claim is now true because
/// something checks it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostCallResponse {
    /// The [`HostCallRequest::id`] this answers.
    pub result: u32,
    /// What came of it.
    #[serde(flatten)]
    pub outcome: HostCallOutcome,
}

/// The three keys an answer may carry, and the only three.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultEnvelope {
    result: u32,
    #[serde(default)]
    ok: Option<HostCallOk>,
    #[serde(default)]
    err: Option<HostCallFailure>,
}

impl<'de> Deserialize<'de> for HostCallResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let envelope = ResultEnvelope::deserialize(deserializer)?;
        let outcome = match (envelope.ok, envelope.err) {
            (Some(ok), None) => HostCallOutcome::Ok(ok),
            (None, Some(err)) => HostCallOutcome::Err(err),
            (Some(_), Some(_)) => {
                return Err(serde::de::Error::custom(
                    "a host-call answer carries either `ok` or `err`, never both",
                ));
            }
            (None, None) => {
                return Err(serde::de::Error::custom(
                    "a host-call answer must carry either `ok` or `err`",
                ));
            }
        };
        Ok(Self {
            result: envelope.result,
            outcome,
        })
    }
}

impl HostCallResponse {
    /// An answer carrying what the host retrieved.
    #[must_use]
    pub fn ok(result: u32, ok: HostCallOk) -> Self {
        Self {
            result,
            outcome: HostCallOutcome::Ok(ok),
        }
    }

    /// An answer carrying why the host would not, or could not.
    #[must_use]
    pub fn err(result: u32, failure: HostCallFailure) -> Self {
        Self {
            result,
            outcome: HostCallOutcome::Err(failure),
        }
    }
}

/// Whether a host call succeeded, as the one key that distinguishes them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostCallOutcome {
    /// The host performed the call.
    Ok(HostCallOk),
    /// The host refused it, or tried it and it failed. Either way the plugin
    /// is told, and is expected to degrade honestly rather than die.
    Err(HostCallFailure),
}

/// What a successful host call returned.
///
/// **Untagged**, because §6b's shape puts the result directly under `ok` —
/// `{"result": 1, "ok": {"frames": [ … ]}}` — and the plugin already knows which
/// capability the id it chose belongs to. The cost of untagged is that variants
/// must stay discriminable by their required keys alone: a second variant whose
/// table could be read as a `recall` result would make decoding order-dependent.
/// So the rule for adding one, stated where it will be read: **every variant's
/// required key set must be disjoint from every other's**, and
/// `wire_corpus.rs` publishes each so a violation is a diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HostCallOk {
    /// `recall` — the frames the host retrieved and the gate permitted.
    Recall(RecallResult),
    /// `child_turn` — what the bounded turn the host ran reported back.
    ChildTurn(ChildTurnResult),
}

/// What one `child_turn` produced.
///
/// Deliberately the child's *report* and nothing else. A plugin does not get
/// the transcript, the tool calls, the token counts or the dollars: those are
/// the host's accounting, and the whole reason a child turn is worth spending
/// is that its context stays on the host's side of the seam
/// (`stella_core::subagent`'s context-economy guarantee). What crosses is the
/// answer, plus enough for the plugin to say honestly where it came from.
///
/// Its required keys are `role`, `seat`, `report` and `completed`, and every
/// one of them is disjoint from [`RecallResult`]'s `frames` — the rule
/// [`HostCallOk`] states for adding an untagged variant, satisfied here by
/// construction rather than by inspection, since both tables deny unknown
/// fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildTurnResult {
    /// The role intent the plugin named, echoed back — the plugin's own
    /// `[roles.<name>]` key, never a model, a provider or an endpoint.
    pub role: String,
    /// The responsibility the host resolved that intent to, and therefore the
    /// one this call's spend is booked against on the receipt
    /// (`ModelCallRole`'s wire token — `triage`, `research`, `plan`, …).
    ///
    /// It is here so a plugin can *report* what it spent rather than only that
    /// it spent: `doc:turn-loop-wrappers` §9.2's whole argument for preferring
    /// a declared role to a `judge` is that the spend becomes visible, and a
    /// plugin that cannot name the seat cannot make it visible downstream.
    pub seat: String,
    /// The child's answer, already clamped by the host's report ceiling.
    pub report: String,
    /// Whether the child reached a final answer. `false` is an ordinary
    /// outcome the plugin weighs — its carve ran out, its step cap hit — and
    /// `report` then carries whatever text it had produced, which may be
    /// empty.
    pub completed: bool,
}

/// The frames one `recall` returned.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallResult {
    /// The frames, in the host's canonical render order. Empty is an ordinary
    /// answer — nothing was relevant — never an error the plugin must handle
    /// specially (the `ContextRecallPort` discipline, L-C6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<RecallFrame>,
}

/// One recalled frame, as a plugin receives it.
///
/// A deliberate *view* of the host's own frame and not a copy of it: the
/// record id, the token cost, the provenance method and the content digest are
/// the host's accounting, and a plugin that cannot act on them has no business
/// holding them. What is here is what a plugin needs to build context a human
/// can trace — the citation label to attribute it, the kind and source to weigh
/// it, the uri to point at it, and the text itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallFrame {
    /// The human-readable citation, never a raw id (L-C4).
    pub label: String,
    /// The frame kind — `symbol`, `memory`, `graph`, ….
    pub kind: String,
    /// The record's original source from its provenance chain.
    pub source: String,
    /// The canonical source uri, when the record declared one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// The frame's text.
    pub content: String,
}

/// Why a host call did not return a result.
///
/// The code is closed so a plugin can branch on it; the detail is the sentence a
/// human reads when the plugin reports what it could not get.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostCallFailure {
    /// Which refusal, as the plugin's own `match` reads it.
    pub refusal: HostCallRefusal,
    /// Why, in words — for the plugin's log and the host's report. Never the
    /// thing a plugin branches on.
    pub detail: String,
}

impl HostCallFailure {
    /// Build a failure.
    #[must_use]
    pub fn new(refusal: HostCallRefusal, detail: impl Into<String>) -> Self {
        Self {
            refusal,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for HostCallFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.refusal, self.detail)
    }
}

/// The closed set of reasons a host call comes back empty.
///
/// Each one is a different thing for a plugin to do next, which is why they are
/// not one code with a message: an [`Undeclared`](Self::Undeclared) call is the
/// plugin author's manifest to fix, a [`Forbidden`](Self::Forbidden) one is an
/// ask no manifest can buy, an [`Unsupported`](Self::Unsupported) one is
/// this host's gap, an [`AllowanceSpent`](Self::AllowanceSpent) one means stop
/// asking and answer, [`Unavailable`](Self::Unavailable) means the capability
/// exists but this host has no plane behind it, and [`Failed`](Self::Failed)
/// means the host tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostCallRefusal {
    /// The plugin's manifest does not declare the thing that was asked for —
    /// this capability in `[loop] calls`, or, for `child_turn`, the role
    /// intent in `[roles]`. Refused the way an undeclared hook is never
    /// invoked, and the same remedy either way: declare it, and let a human
    /// read it at install.
    Undeclared,
    /// The host will not perform this ask **whatever the manifest says**, and
    /// declaring harder will not change the answer.
    ///
    /// Distinct from [`Undeclared`](Self::Undeclared) because the remedy is
    /// different: there the plugin author edits the manifest, here they must
    /// ask for something else. The standing case is verifier independence — a
    /// `child_turn` whose role intent resolves to the **worker's** seat would
    /// let a plugin grade the work with the model that did it — the
    /// self-grading the built-in staged pipeline's roster used to report for
    /// an operator's own configuration (`Roster::independence_losses`, deleted
    /// with that crate in #3865). An operator could choose it; a plugin may
    /// not buy it at all.
    Forbidden,
    /// This host does not implement the capability at all. A declared gap on
    /// the host's side, not a fault of the plugin's.
    Unsupported,
    /// The per-point host-call allowance is spent. The plugin's declared
    /// `max_calls` is an ask; this is the clamp answering.
    AllowanceSpent,
    /// The capability is implemented but this host has nothing behind it — no
    /// context plane configured, no candidate workspace for this run.
    Unavailable,
    /// The host performed the call and it failed.
    Failed,
}

impl HostCallRefusal {
    /// The name this refusal is written as on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Undeclared => "undeclared",
            Self::Forbidden => "forbidden",
            Self::Unsupported => "unsupported",
            Self::AllowanceSpent => "allowance-spent",
            Self::Unavailable => "unavailable",
            Self::Failed => "failed",
        }
    }
}

impl std::fmt::Display for HostCallRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{BeforeTurnResponse, PROTOCOL_VERSION};

    fn decode(text: &str) -> Result<PluginMessage, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// The frame shape `doc:wrapper-socket` §6b writes down, decoded as the
    /// call it is.
    #[test]
    fn a_host_call_is_read_off_the_wire_shape_the_design_specifies() {
        let message = decode(r#"{"call":"recall","id":1,"args":{"goal":"the parser","limit":8}}"#)
            .expect("a host call decodes");
        assert_eq!(
            message,
            PluginMessage::Call(HostCallRequest {
                id: 1,
                args: HostCallArgs::Recall(RecallArgs {
                    goal: "the parser".to_string(),
                    limit: Some(8),
                }),
            })
        );
    }

    /// The point response is still the message that ends the conversation, and
    /// it decodes through the same union.
    #[test]
    fn a_point_response_still_decodes_through_the_union_that_now_carries_calls() {
        let message =
            decode(r#"{"point":"before_turn","body":{"protocol_version":1}}"#).expect("decodes");
        assert_eq!(
            message,
            PluginMessage::Response(WrapperResponse::BeforeTurn(BeforeTurnResponse::empty()))
        );
        assert_eq!(
            BeforeTurnResponse::empty().protocol_version,
            PROTOCOL_VERSION
        );
    }

    /// A message claiming to be both is two claims about what the plugin is
    /// doing; believing either one silently is how a conversation ends in the
    /// wrong place.
    #[test]
    fn a_message_that_is_both_a_call_and_a_response_is_refused() {
        let error = decode(r#"{"point":"before_turn","body":{},"call":"recall","id":1,"args":{}}"#)
            .expect_err("a mixed message is refused");
        assert!(error.to_string().contains("never both"), "{error}");
    }

    /// The #3500 rule, on the union: an unknown key is a typo, and a typo that
    /// decodes cleanly is a plugin author debugging a silence.
    #[test]
    fn an_unknown_key_beside_a_host_call_is_refused() {
        let error = decode(r#"{"call":"recall","id":1,"args":{"goal":"x"},"extra":1}"#)
            .expect_err("an unknown key is refused");
        assert!(error.to_string().contains("extra"), "{error}");
    }

    /// A capability is a value from a closed enum, never a string a plugin
    /// invents.
    #[test]
    fn a_capability_the_enum_does_not_name_is_not_a_call() {
        let error = decode(r#"{"call":"read_file","id":1,"args":{}}"#)
            .expect_err("an invented capability is refused");
        assert!(error.to_string().contains("read_file"), "{error}");
    }

    /// Exactly one of `ok` and `err`, structurally.
    #[test]
    fn a_host_call_answer_carries_exactly_one_outcome() {
        let ok = serde_json::to_string(&HostCallResponse::ok(
            1,
            HostCallOk::Recall(RecallResult::default()),
        ))
        .expect("encodes");
        assert_eq!(ok, r#"{"result":1,"ok":{}}"#);

        let err = serde_json::to_string(&HostCallResponse::err(
            2,
            HostCallFailure::new(HostCallRefusal::Undeclared, "not declared"),
        ))
        .expect("encodes");
        assert_eq!(
            err,
            r#"{"result":2,"err":{"refusal":"undeclared","detail":"not declared"}}"#
        );

        let both: Result<HostCallResponse, _> =
            serde_json::from_str(r#"{"result":1,"ok":{},"err":{"refusal":"failed","detail":"x"}}"#);
        assert!(both.is_err(), "two outcomes must not decode");
    }

    /// The untagged rule [`HostCallOk`] states, checked rather than asserted:
    /// a `child_turn` result must not be readable as a `recall` result, and
    /// vice versa. Both tables deny unknown fields, which is what makes the
    /// decode order irrelevant — and the only thing that does.
    #[test]
    fn a_child_turn_result_is_never_mistaken_for_a_recall_result() {
        let answer = HostCallResponse::ok(
            4,
            HostCallOk::ChildTurn(ChildTurnResult {
                role: "reviewer".to_string(),
                seat: "research".to_string(),
                report: "the diff drops the retry".to_string(),
                completed: true,
            }),
        );
        let text = serde_json::to_string(&answer).expect("encodes");
        assert_eq!(
            text,
            r#"{"result":4,"ok":{"role":"reviewer","seat":"research","report":"the diff drops the retry","completed":true}}"#
        );
        assert_eq!(
            serde_json::from_str::<HostCallResponse>(&text).expect("decodes"),
            answer,
            "the child-turn arm survives a round trip through the untagged union"
        );

        // And the other direction: frames still read as frames, though `recall`
        // is the variant tried first and would happily swallow a table it
        // recognised.
        let frames = HostCallResponse::ok(5, HostCallOk::Recall(RecallResult::default()));
        let text = serde_json::to_string(&frames).expect("encodes");
        assert_eq!(
            serde_json::from_str::<HostCallResponse>(&text).expect("decodes"),
            frames
        );
    }

    /// `forbidden` is a code a plugin branches on, so it has to survive the
    /// wire under the spelling the host writes.
    #[test]
    fn the_forbidden_refusal_crosses_the_wire_as_its_own_code() {
        let text = serde_json::to_string(&HostCallResponse::err(
            6,
            HostCallFailure::new(HostCallRefusal::Forbidden, "that is the worker's seat"),
        ))
        .expect("encodes");
        assert_eq!(
            text,
            r#"{"result":6,"err":{"refusal":"forbidden","detail":"that is the worker's seat"}}"#
        );
        assert_eq!(HostCallRefusal::Forbidden.as_str(), "forbidden");
    }
}
