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

use crate::wire::{FromEnvelope, PendingBody, WrapperPoint, WrapperResponse, decode_body};

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
/// - `candidate_fanout` — N isolated writable workspaces, each running one
///   full worker turn, reported back with the evidence a plugin scores them
///   on. `plugins/stella-candidates` (`doc:pipeline-as-plugins` §3/§7 item 4)
///   is best-of-N and could not be written at all without it: every other
///   capability here is read-only or single-tracked, so the strongest thing
///   the socket could express was "retry with correction over the one real
///   tree", which is a different operation and should not ship under that
///   name (#3844).
/// - `adopt_candidate` — apply one fan-out candidate's changes to the real
///   tree and discard its siblings. Its own capability rather than a field on
///   the fan-out result, because a plugin that only scores candidates and one
///   that also lands one are different grants for a human to read at install,
///   and invariant 9 says a parameter may scope an operation and never select
///   one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostCall {
    /// Ask the context plane for material relevant to a goal.
    Recall,
    /// Ask for one bounded turn at a declared role intent.
    ChildTurn,
    /// Ask for the candidate workspace's test invocation to be run again.
    RunTest,
    /// Ask for N isolated workspaces, each running one full worker turn.
    CandidateFanout,
    /// Ask for one fan-out candidate to be applied to the real tree.
    AdoptCandidate,
}

impl HostCall {
    /// The name this capability is written as on the wire and in `[loop] calls`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recall => "recall",
            Self::ChildTurn => "child_turn",
            Self::RunTest => "run_test",
            Self::CandidateFanout => "candidate_fanout",
            Self::AdoptCandidate => "adopt_candidate",
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
        deserialize_plugin_message(deserializer)
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
#[serde(field_identifier, rename_all = "snake_case")]
enum PluginField {
    Point,
    Body,
    Call,
    Id,
    Args,
}

/// The five keys, in the order serde reports a missing one.
const PLUGIN_FIELDS: &[&str] = &["point", "body", "call", "id", "args"];

/// Read one plugin message: two shapes over five keys, no unknown key, and the
/// position of a malformed body kept.
///
/// # Why this is hand-written twice over
///
/// The first version buffered `body` and `args` as `serde_json::Value` and
/// re-parsed them once the envelope had been classified. That cost the same
/// thing [`crate::wire`]'s envelope used to cost (#3518, #4436): a body decode
/// error reached the plugin author through `serde::de::Error::custom`, which
/// drops the `at line N column M` a direct parse carries — and this channel is
/// the one an author debugs *from the plugin's side*, with no host stack to read
/// instead.
///
/// So both tagged bodies are seeded straight from the input when their tag
/// arrived first — `body` behind `point`, `args` behind `call`, which is the
/// order every message in §6b's own transcript is written in — and buffered
/// otherwise, exactly as the two-key envelope does. The shared
/// [`PendingBody`] is what makes that the same mechanism rather than a second
/// one that looks like it.
///
/// # Why the seeded error is held rather than returned
///
/// A five-key envelope cannot classify early: whether this is a point response
/// or a host call is not known until `point` and `call` have both been ruled on,
/// and the *contradiction* — both present — is the more useful thing to tell an
/// author than "your `args` are malformed". [`PendingBody::Seeded`] therefore
/// carries a `Result`, [`classify`] runs first, and the held error is what it
/// falls back to.
///
/// The read does stop at that failure, because a seeded body that failed partway
/// through leaves the parser inside it ([`PendingBody::stop`]). So the
/// contradiction wins only when *both* halves arrived before the malformed body;
/// a `point` written after it is never read, and the author is told about the
/// body instead. Either way the message is refused with a position.
fn deserialize_plugin_message<'de, D>(deserializer: D) -> Result<PluginMessage, D::Error>
where
    D: Deserializer<'de>,
{
    struct PluginMessageVisitor;

    impl<'de> serde::de::Visitor<'de> for PluginMessageVisitor {
        type Value = PluginMessage;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(
                "a plugin message: a point response (`point`, `body`) or a host call (`call`, \
                 `id`, `args`)",
            )
        }

        fn visit_map<A>(self, mut map: A) -> Result<PluginMessage, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            use serde::de::Error as _;

            let mut point: Option<WrapperPoint> = None;
            let mut call: Option<HostCall> = None;
            let mut id: Option<u32> = None;
            let mut body: Option<PendingBody<WrapperResponse, A::Error>> = None;
            let mut args: Option<PendingBody<HostCallArgs, A::Error>> = None;

            while let Some(key) = map.next_key::<PluginField>()? {
                match key {
                    PluginField::Point => {
                        if point.is_some() {
                            return Err(A::Error::duplicate_field("point"));
                        }
                        point = Some(map.next_value()?);
                    }
                    PluginField::Call => {
                        if call.is_some() {
                            return Err(A::Error::duplicate_field("call"));
                        }
                        call = Some(map.next_value()?);
                    }
                    PluginField::Id => {
                        if id.is_some() {
                            return Err(A::Error::duplicate_field("id"));
                        }
                        id = Some(map.next_value()?);
                    }
                    PluginField::Body => {
                        if body.is_some() {
                            return Err(A::Error::duplicate_field("body"));
                        }
                        let pending = match point {
                            Some(point) => {
                                PendingBody::Seeded(WrapperResponse::seed_body(point, &mut map))
                            }
                            None => PendingBody::Buffered(map.next_value()?),
                        };
                        let stop = pending.stop();
                        body = Some(pending);
                        if stop {
                            break;
                        }
                    }
                    PluginField::Args => {
                        if args.is_some() {
                            return Err(A::Error::duplicate_field("args"));
                        }
                        let pending = match call {
                            Some(call) => PendingBody::Seeded(seed_args(call, &mut map)),
                            None => PendingBody::Buffered(map.next_value()?),
                        };
                        let stop = pending.stop();
                        args = Some(pending);
                        if stop {
                            break;
                        }
                    }
                }
            }

            classify(point, body, call, id, args)
        }
    }

    deserializer.deserialize_struct("PluginMessage", PLUGIN_FIELDS, PluginMessageVisitor)
}

/// Read a host call's `args` straight from the input, now that `call` is known.
///
/// The mirror of [`crate::wire::FromEnvelope::seed_body`], for the tag this
/// envelope's second body hangs off.
fn seed_args<'de, A>(call: HostCall, map: &mut A) -> Result<HostCallArgs, A::Error>
where
    A: serde::de::MapAccess<'de>,
{
    Ok(match call {
        HostCall::Recall => HostCallArgs::Recall(map.next_value()?),
        HostCall::ChildTurn => HostCallArgs::ChildTurn(map.next_value()?),
        HostCall::RunTest => HostCallArgs::RunTest(map.next_value()?),
        HostCall::CandidateFanout => HostCallArgs::CandidateFanout(map.next_value()?),
        HostCall::AdoptCandidate => HostCallArgs::AdoptCandidate(map.next_value()?),
    })
}

/// Decide which of the two shapes a read envelope is, or say why it is neither.
fn classify<E: serde::de::Error>(
    point: Option<WrapperPoint>,
    body: Option<PendingBody<WrapperResponse, E>>,
    call: Option<HostCall>,
    id: Option<u32>,
    args: Option<PendingBody<HostCallArgs, E>>,
) -> Result<PluginMessage, E> {
    match (point, call) {
        (Some(_), Some(_)) => Err(E::custom(
            "a plugin message carries either `point` (the response that ends the point) or `call` \
             (a host call), never both",
        )),
        (Some(point), None) => {
            if id.is_some() || args.is_some() {
                return Err(E::custom(
                    "a point response carries `point` and `body` only; `id` and `args` belong to \
                     a host call",
                ));
            }
            match body {
                Some(PendingBody::Seeded(response)) => response.map(PluginMessage::Response),
                Some(PendingBody::Buffered(body)) => {
                    WrapperResponse::from_parts(point, body).map(PluginMessage::Response)
                }
                None => Err(E::custom("a point response must carry `body`")),
            }
        }
        (None, Some(call)) => {
            if body.is_some() {
                return Err(E::custom(
                    "a host call carries `call`, `id` and `args`; `body` belongs to a point \
                     response",
                ));
            }
            let id = id.ok_or_else(|| E::custom("a host call must carry `id`"))?;
            let args = match args {
                Some(PendingBody::Seeded(args)) => args?,
                Some(PendingBody::Buffered(args)) => match call {
                    HostCall::Recall => decode_body(args).map(HostCallArgs::Recall)?,
                    HostCall::ChildTurn => decode_body(args).map(HostCallArgs::ChildTurn)?,
                    HostCall::RunTest => decode_body(args).map(HostCallArgs::RunTest)?,
                    HostCall::CandidateFanout => {
                        decode_body(args).map(HostCallArgs::CandidateFanout)?
                    }
                    HostCall::AdoptCandidate => {
                        decode_body(args).map(HostCallArgs::AdoptCandidate)?
                    }
                },
                None => return Err(E::custom("a host call must carry `args`")),
            };
            Ok(PluginMessage::Call(HostCallRequest { id, args }))
        }
        (None, None) => Err(E::custom(
            "a plugin message must carry either `point` (a point response) or `call` (a host call)",
        )),
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
        match deserialize_plugin_message(deserializer)? {
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
    /// Ask for N isolated workspaces, each running one full worker turn.
    CandidateFanout(CandidateFanoutArgs),
    /// Ask for one fan-out candidate to be applied to the real tree.
    AdoptCandidate(AdoptCandidateArgs),
}

impl HostCallArgs {
    /// Which capability these arguments are for.
    #[must_use]
    pub fn call(&self) -> HostCall {
        match self {
            Self::Recall(_) => HostCall::Recall,
            Self::ChildTurn(_) => HostCall::ChildTurn,
            Self::RunTest(_) => HostCall::RunTest,
            Self::CandidateFanout(_) => HostCall::CandidateFanout,
            Self::AdoptCandidate(_) => HostCall::AdoptCandidate,
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

/// `candidate_fanout` — N isolated workspaces, each running one worker turn.
///
/// The three fields are the whole ask, and what is *absent* is the design:
/// there is no field for a path, a branch, a base revision, a concurrency
/// level, a tool grant or a dollar amount, so none of them is something the
/// host has to remember to ignore ([`ChildTurnArgs`]'s argument, at the one
/// capability where the turns write).
///
/// `width` is the sharpest of the three. It is **an ask, never an authority**
/// — the [`RecallArgs::limit`] discipline, at the capability where getting it
/// wrong costs N times a worker turn rather than N frames of prompt. The host
/// clamps it against `[loop] max_fanout_width`'s own clamp
/// (`stella_runtime::wrapper::DEFAULT_HOST_MAX_FANOUT_WIDTH`), and the answer
/// reports back both what was asked and what ran, so a plugin can say honestly
/// that it scored three candidates rather than the eight it wanted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateFanoutArgs {
    /// The declared role intent every candidate turn runs at.
    ///
    /// Resolved exactly as [`ChildTurnArgs::role`] is — intent → declared
    /// `tier` → seat — and then judged by the *opposite* rule, which is the
    /// one thing about this capability a plugin author must read before
    /// writing a manifest. A child turn may not resolve to the worker's seat,
    /// because a plugin must not grade work with the model that did it. A
    /// fan-out candidate is not evidence about the work, it **is** the work,
    /// so it must resolve to the worker's seat and nothing else: booking a
    /// writing turn against `triage` would put spend on the receipt under a
    /// responsibility that did no writing, which is the same misattribution
    /// read from the other end.
    pub role: String,
    /// What every candidate turn is asked to do.
    pub instruction: String,
    /// How many candidates the plugin wants. An ask; see the type docs.
    pub width: u32,
}

/// `adopt_candidate` — land one candidate's changes on the real tree.
///
/// The handle is the only parameter, and it is the host's own minted name
/// rather than a path, for the reason [`CandidateGrant`](crate::CandidateGrant)
/// states: a plugin cannot name a directory and have the host write to it. The
/// host resolves the name against the fan-out table *it* minted and refuses
/// anything else — which is also what keeps
/// [`HOST_TREE_HANDLE`](crate::HOST_TREE_HANDLE) un-adoptable, since it names
/// no entry in any table and never has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdoptCandidateArgs {
    /// The candidate to adopt, as the fan-out answer named it.
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
    /// `candidate_fanout` — the candidates the host built, and what each did.
    CandidateFanout(CandidateFanoutResult),
    /// `adopt_candidate` — which candidate landed, and which were discarded.
    AdoptCandidate(AdoptCandidateResult),
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

/// What one `candidate_fanout` produced.
///
/// Both fields are **required** on the wire, and that is load-bearing rather
/// than stylistic: [`HostCallOk`] is untagged and its rule is that every
/// variant's required key set must be disjoint from every other's.
/// [`RecallResult`]'s only field defaults, so `{}` reads as a recall answer —
/// a fan-out result whose `candidates` defaulted would be a table that decodes
/// as the wrong variant the moment a host had nothing to report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateFanoutResult {
    /// The width the plugin asked for, echoed back so it can see the clamp.
    ///
    /// Reported rather than assumed, because "I scored 3 of the 8 I asked
    /// for" and "I scored the 3 I asked for" are different sentences and only
    /// the host knows which one is true.
    pub requested: u32,
    /// One entry per candidate the host actually built and ran, in the order
    /// it minted them. Shorter than [`Self::requested`] whenever the clamp
    /// bit, or a candidate's workspace could not be created — never longer.
    pub candidates: Vec<FanoutCandidate>,
}

/// One candidate of a fan-out, as the plugin receives it.
///
/// The evidence half is deliberately small and deliberately *mechanical*:
/// a handle to re-address it, a root to read and test it in, the turn's own
/// report, whether it finished, and how big the diff is. What is absent is a
/// score — scoring is the plugin's whole job, and a host that shipped one
/// would be the thing the plugin was extracted to replace.
///
/// [`Self::root`] is a real absolute path for the same reason
/// [`CandidateGrant::root`](crate::CandidateGrant) is one: a plugin that runs
/// a test suite needs a directory. It is not thereby a capability — every path
/// the plugin names on the way *back* is resolved against the handle by the
/// host, and [`Self::candidate`] is the only thing that re-addresses this
/// workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FanoutCandidate {
    /// The name the host minted for this workspace — what
    /// [`AdoptCandidateArgs::candidate`] and
    /// [`RunTestArgs::candidate`] are spelled with.
    pub candidate: CandidateHandle,
    /// The workspace's absolute root on the host's filesystem, canonical.
    pub root: String,
    /// The candidate turn's own answer, clamped by the host's report ceiling.
    pub report: String,
    /// Whether the candidate turn reached a final answer. `false` is an
    /// ordinary outcome the plugin weighs — its carve ran out, its step cap
    /// hit — and the workspace is still there to be read and tested.
    pub completed: bool,
    /// Files the candidate turn changed in its workspace.
    pub files_changed: u32,
    /// Lines it added and removed, summed — the crude size signal
    /// `CandidateSummary` carried before the staged pipeline was deleted
    /// (#3865), kept crude on purpose: a plugin that wants a better one reads
    /// [`Self::root`].
    pub lines_changed: u32,
}

/// What one `adopt_candidate` did.
///
/// The host's decision reported as an outcome, never as a promise: by the time
/// this crosses, the winner's changes are on the real tree and every workspace
/// of the fan-out is gone — the winner's copy included, since its bytes now
/// live where they were wanted. A plugin that named a handle the host does not
/// hold gets a [`HostCallFailure`] instead and nothing moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdoptCandidateResult {
    /// The candidate whose changes are now on the real tree.
    pub adopted: CandidateHandle,
    /// The siblings the host threw away without landing, in the order it
    /// minted them. The winner is not among them: its workspace is cleaned up
    /// too, but "landed, and its scratch copy removed" is a different fact
    /// from "discarded", and collapsing the two would make a width-1 fan-out
    /// read as having discarded its only candidate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discarded: Vec<CandidateHandle>,
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
    /// let a plugin grade the work with the model that did it, which is the
    /// self-grading the staged pipeline's roster reported on
    /// (`crates/stella-pipeline`, deleted in #3865) and which a plugin may not
    /// buy at all.
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
    ///
    /// The `body` and the `args` are well-formed on purpose, so what is refused
    /// is the framing and nothing else. A mixed message whose body is *also*
    /// malformed is
    /// `the_contradiction_outranks_a_malformed_body_underneath_it`.
    #[test]
    fn a_message_that_is_both_a_call_and_a_response_is_refused() {
        let error = decode(
            r#"{"point":"before_turn","body":{"protocol_version":1},"call":"recall","id":1,"args":{"goal":"x"}}"#,
        )
        .expect_err("a mixed message is refused");
        assert!(error.to_string().contains("never both"), "{error}");
    }

    /// **The #4436 witness.** A malformed `args` body reports *where* it is
    /// malformed, the way #3518 made the two-key envelope report it.
    ///
    /// The two orders assert different halves. `call` first is the order §6b's
    /// own transcript is written in and the one that keeps the position, because
    /// the args are deserialized straight from the input. `args` first must
    /// still decode and still name the field: a JSON library that writes its map
    /// in the other order cannot be made wrong by this, and it has no position to
    /// offer because it reached the buffer — the same deliberate trade
    /// `a_malformed_body_reports_the_position_the_author_navigates_by` records
    /// for the wrapper envelope.
    #[test]
    fn a_malformed_host_call_body_reports_the_position() {
        // Pretty-printed, so a line number is a real answer and not always 1.
        let call_first = "{\n  \"call\": \"recall\",\n  \"id\": 1,\n  \"args\": {\n    \
                          \"goal\": \"the parser\",\n    \"limits\": 8\n  }\n}";
        let error = decode(call_first).expect_err("an unknown key inside the args is refused");
        let text = error.to_string();
        assert!(
            text.contains("limits"),
            "the field is what decides it, got {text}"
        );
        assert!(
            text.contains("at line 6 column"),
            "the position is the half an author navigates by, got {text}"
        );
        assert_eq!(error.line(), 6, "got {text}");

        let args_first = "{\n  \"args\": {\n    \"goal\": \"the parser\",\n    \
                          \"limits\": 8\n  },\n  \"id\": 1,\n  \"call\": \"recall\"\n}";
        let error = decode(args_first)
            .expect_err("the args are held to the same shape whichever key came first");
        assert!(error.to_string().contains("limits"), "{error}");

        // And a well-formed call decodes in either order, which is what proves
        // the two assertions above failed on the args rather than on the order.
        for text in [
            r#"{"call":"recall","id":1,"args":{"goal":"the parser"}}"#,
            r#"{"args":{"goal":"the parser"},"id":1,"call":"recall"}"#,
        ] {
            let message = decode(text).expect("either order decodes");
            let PluginMessage::Call(call) = message else {
                panic!("a call, not a response");
            };
            assert_eq!(call.call(), HostCall::Recall);
        }
    }

    /// A malformed body does not get to answer a question the envelope has not
    /// asked yet: a message carrying both a `point` and a `call` is a
    /// contradiction about what the plugin is doing, and that is more useful to
    /// its author than which key inside the args is misspelled.
    ///
    /// True while both halves arrived before the malformed body, which is the
    /// limit `deserialize_plugin_message` documents — the read stops there, so a
    /// `point` written *after* the bad args is never seen and the author is told
    /// about the args instead.
    #[test]
    fn the_contradiction_outranks_a_malformed_body_underneath_it() {
        let error = decode(
            r#"{"point":"before_turn","body":{"protocol_version":1},"call":"recall","args":{"limits":8}}"#,
        )
        .expect_err("a mixed message is refused");
        assert!(error.to_string().contains("never both"), "{error}");
    }

    /// A repeated envelope key is a malformed message, not a preference — the
    /// derived reader used to let the last one win.
    #[test]
    fn a_repeated_host_call_key_is_named_rather_than_resolved() {
        for (text, key) in [
            (
                r#"{"call":"recall","call":"child_turn","id":1,"args":{"goal":"x"}}"#,
                "call",
            ),
            (
                r#"{"call":"recall","id":1,"id":2,"args":{"goal":"x"}}"#,
                "id",
            ),
            (
                r#"{"call":"recall","id":1,"args":{"goal":"x"},"args":{"goal":"y"}}"#,
                "args",
            ),
        ] {
            let error = decode(text).expect_err("a repeated key is refused");
            assert!(error.to_string().contains(key), "{key}: {error}");
        }
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
