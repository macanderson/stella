//! Why a child conversation failed, as a typed answer (invariant 5) — once for
//! the wrapper socket ([`WrapperError`]) and once for the driver channel
//! ([`DriverError`]).
//!
//! The two are siblings rather than one enum with a mode, for
//! [`driver_call`](super::driver_call)'s reason one layer up: a wrapper answers
//! a point inside a turn and a driver opens a session that starts them, so half
//! of [`WrapperError`]'s variants name things a driver cannot reach (a role it
//! did not declare, a mistyped signal, a stage order that would not resolve,
//! the point it was asked versus the point it answered). Handing a driver's
//! caller those arms would be handing it a `match` it can never complete
//! honestly.
//!
//! The distinctions here are the ones a host has to act on, and each one was
//! chosen because collapsing it loses a decision:
//!
//! - **The plugin never ran** ([`WrapperError::Spawn`]) versus **it ran and
//!   failed** ([`WrapperError::Exit`]) — the first is an installation problem
//!   the user must fix, the second is the plugin's own bug. This is the same
//!   split `stella_core::hooks::HookExecError` draws, for the same reason.
//! - **It answered unreadably** ([`WrapperError::Decode`]) versus **it
//!   answered the wrong question** ([`WrapperError::PointMismatch`]) — the
//!   second is the one that would otherwise decide a verdict from the wrong
//!   evidence, silently.
//! - **It spoke a contract this host does not have**
//!   ([`WrapperError::ProtocolVersion`]) — refused rather than parsed
//!   leniently, because an additive-only contract means a *newer* message may
//!   carry a field whose absence changes the answer.
//!
//! A host that catches any of these must not read the failure as "the wrapper
//! had nothing to say": `stella_plugin::EvidenceSet::unobserved` is the value
//! to hand [`judge`](super::judge) instead, so the verdict abstains rather
//! than blaming the worker for evidence nobody collected.

use std::time::Duration;

/// A wrapper point could not be dispatched, or its answer could not be used.
#[derive(Debug, thiserror::Error)]
pub enum WrapperError {
    /// A transport was constructed with no program to run.
    #[error("a wrapper's argv must name a program: it is empty")]
    EmptyArgv,

    /// A transport was constructed with a zero timeout, which would kill the
    /// plugin before it ran — the `[oracle] command.timeout_secs` rule.
    #[error("a wrapper's timeout must be at least one millisecond")]
    ZeroTimeout,

    /// The process could not be started at all.
    #[error("cannot start wrapper `{program}`: {source}")]
    Spawn {
        /// `argv[0]`, as declared.
        program: String,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },

    /// The request could not be written to the child, or its answer could not
    /// be read back.
    #[error("cannot exchange with wrapper `{program}`: {source}")]
    Transport {
        /// `argv[0]`, as declared.
        program: String,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },

    /// The plugin outlived its budget and was killed.
    #[error("wrapper `{program}` did not answer within {}s", .timeout.as_secs_f64())]
    Timeout {
        /// `argv[0]`, as declared.
        program: String,
        /// The budget it exceeded.
        timeout: Duration,
    },

    /// The plugin wrote more than the transport will read, and the read was
    /// stopped there.
    ///
    /// Distinct from [`WrapperError::Decode`] on purpose, even though the
    /// truncated bytes would also fail to parse: the plugin's answer was never
    /// received, so there is nothing to show its author and nothing about the
    /// JSON to fix. The fix is the volume — and the host's own reason for
    /// caring is that it stopped holding the rest of it.
    #[error(
        "wrapper `{program}` wrote more to {stream} than the {cap}-byte ceiling this host will \
         read, so the call was refused"
    )]
    OutputCap {
        /// `argv[0]`, as declared.
        program: String,
        /// Which stream crossed the ceiling: `"stdout"` or `"stderr"`.
        stream: &'static str,
        /// The ceiling it crossed.
        cap: usize,
    },

    /// The plugin ran and exited non-zero.
    #[error("wrapper `{program}` exited with {status}: {stderr}")]
    Exit {
        /// `argv[0]`, as declared.
        program: String,
        /// The exit status, rendered — a code, or "a signal" where the
        /// platform reports one.
        status: String,
        /// What it wrote to stderr, bounded.
        stderr: String,
    },

    /// The request could not be serialized. Reachable only for a value that
    /// cannot be represented as JSON at all.
    #[error("cannot encode a wrapper request: {0}")]
    Encode(#[source] serde_json::Error),

    /// The plugin's stdout ended without the response that ends the point.
    ///
    /// Distinct from [`WrapperError::Exit`] because the child exited *cleanly*:
    /// it said nothing and reported success, which is the one shape that would
    /// otherwise read as "the wrapper had nothing to contribute". A plugin that
    /// has nothing to say returns an empty response and says so
    /// (`BeforeTurnResponse::empty`); saying nothing at all is a bug in the
    /// plugin, and one its author cannot see without being told.
    #[error("wrapper `{program}` exited without answering the point it was asked: {stderr}")]
    NoResponse {
        /// `argv[0]`, as declared.
        program: String,
        /// What it wrote to stderr, bounded — usually where the reason is.
        stderr: String,
    },

    /// The plugin made a host call at a host that closed its stdin, because no
    /// callable capability was declared to this transport
    /// (`doc:wrapper-socket` §6b).
    ///
    /// The refusal a declared-but-ungranted call gets is delivered *to the
    /// plugin*, which is the fail-open direction the channel is built around.
    /// This variant is the one case where that is impossible: with no gate
    /// offering a call there is no conversation, so stdin was closed after the
    /// request and there is no way back to the plugin. Reported to the host
    /// instead of waiting for the deadline, because "my plugin hangs" is a much
    /// worse diagnosis than the two this names.
    ///
    /// **Two causes, and the second is a host's bug rather than a plugin's.**
    /// Either the manifest declares no `[loop] calls` — the plugin author's
    /// one-line fix — or the host never attached a
    /// [`HostCallGate`](super::HostCallGate) for a plugin that did. Stdin's
    /// lifetime is decided from the gate, because a plugin that declares no call
    /// reads its request with `cat` and would hang on a pipe held open for a
    /// conversation it can never have. #3561 is the shipping instance.
    #[error(
        "wrapper `{program}` asked the host for \"{call}\", but no channel was open to answer on: \
         its manifest declares no [loop] calls, or this host attached no gate"
    )]
    UnannouncedCall {
        /// `argv[0]`, as declared.
        program: String,
        /// The capability it asked for.
        call: stella_plugin::HostCall,
    },

    /// The plugin asked for a host call after the channel that answers it had
    /// already died, mid-conversation.
    ///
    /// **Distinct from [`WrapperError::UnannouncedCall`] on purpose**, even
    /// though both leave a call unanswered: that variant means the manifest
    /// declares no `[loop] calls`, or no [`HostCallGate`](super::HostCallGate)
    /// was ever attached — a manifest or host misconfiguration, fixed by
    /// editing `[loop] calls` or attaching a gate. Neither is true here — this
    /// fires only after at least one call was already answered through this
    /// exact gate, which proves both were fine. The fault is transport-level:
    /// the write that carried a previous call's answer back to the child's
    /// stdin failed (a closed stdin, a broken pipe), the writer task exited
    /// because of it, and the plugin then pipelined another call before
    /// noticing — a pipelining shape the framing explicitly allows. `source`
    /// is that write failure, recovered by joining the writer task rather than
    /// discarded: aborting it without awaiting, as the caller once did, throws
    /// the one clue that explains what actually happened.
    #[error(
        "wrapper `{program}` asked the host for \"{call}\", but the channel answering an earlier \
         call in this conversation had already failed: {source}"
    )]
    AnswerChannelFailed {
        /// `argv[0]`, as declared.
        program: String,
        /// The capability the plugin asked for after the channel had died.
        call: stella_plugin::HostCall,
        /// The write failure that killed the channel.
        #[source]
        source: std::io::Error,
    },

    /// The plugin's last message was a host call and then its stdout ended, so
    /// it could not have read the answer it asked for.
    ///
    /// A plugin that asks and stops listening has abandoned the point, and
    /// crediting the round to it would mean judging a turn on evidence it never
    /// finished gathering.
    #[error(
        "wrapper `{program}` asked the host for \"{call}\" and then closed its output without \
         reading the answer"
    )]
    UnansweredCall {
        /// `argv[0]`, as declared.
        program: String,
        /// The capability it asked for and abandoned.
        call: stella_plugin::HostCall,
    },

    /// The plugin's answer was not a readable response.
    #[error("wrapper `{program}` answered unreadably: {source} (in: {answer})")]
    Decode {
        /// `argv[0]`, as declared.
        program: String,
        /// What it wrote, bounded, so the fix does not need a second run.
        answer: String,
        /// The parse failure.
        #[source]
        source: serde_json::Error,
    },

    /// The plugin answered a different point than the one it was asked.
    #[error("wrapper `{program}` was asked for {expected} and answered {actual}")]
    PointMismatch {
        /// `argv[0]`, as declared.
        program: String,
        /// The point the host addressed.
        expected: stella_plugin::WrapperPoint,
        /// The point the plugin answered.
        actual: stella_plugin::WrapperPoint,
    },

    /// The plugin spoke a protocol version above this host's.
    #[error(
        "wrapper `{program}` speaks protocol version {declared}, above this host's {supported}; \
         a newer message may carry a field whose absence changes the answer"
    )]
    ProtocolVersion {
        /// `argv[0]`, as declared.
        program: String,
        /// The version the plugin wrote.
        declared: u32,
        /// The version this host supports.
        supported: u32,
    },

    /// A `before_turn` response named a role intent the manifest never
    /// declared. Refused rather than resolved: the roles a human consented to
    /// at install are the roles a wrapper may spend.
    #[error(
        "wrapper `{wrapper}` asked for role \"{role}\", which its manifest does not declare in \
         [roles]"
    )]
    UndeclaredRole {
        /// The wrapper's variant id.
        wrapper: String,
        /// The undeclared intent.
        role: String,
    },

    /// A `before_turn` response published a signal at the wrong type — a count
    /// as a boolean, or the reverse. The load-time
    /// `ManifestError::ConditionTypeMismatch` rule, applied to the value
    /// rather than to the condition that reads it.
    #[error(
        "wrapper `{wrapper}` published \"{signal}\" as a {published} value, but it is a {declared} \
         signal"
    )]
    MistypedSignal {
        /// The wrapper's variant id.
        wrapper: String,
        /// The signal published at the wrong type.
        signal: stella_plugin::Signal,
        /// The shape the value took.
        published: &'static str,
        /// The kind the signal declares.
        declared: stella_plugin::SignalKind,
    },

    /// A manifest was bound to the dispatcher without declaring `[wrapper]`.
    ///
    /// Not a defect in the plugin — a manifest may declare hooks, an oracle and
    /// nothing else — but there is no stage order to resolve and no variant id
    /// to record for it, so there is nothing to drive.
    #[error("plugin `{plugin}` declares no [wrapper] block, so it has no stage order to run")]
    NotAWrapper {
        /// The manifest name.
        plugin: String,
    },

    /// The declared stage order could not be resolved against this host's
    /// published signals.
    ///
    /// Unreachable for a manifest that came from `PluginManifest::from_toml_str`
    /// — a validated wrapper resolves for every possible `SignalValues`, which
    /// `stella-plugin`'s `tests/wrapper_program.rs` asserts as a property — so
    /// this names a hand-built manifest. It is an error rather than an empty
    /// program because "which stages run" is not a question a host may answer by
    /// guessing.
    ///
    /// The source is boxed because `ManifestError` is the larger of the two
    /// types by some margin, and every other variant here would pay for it.
    #[error("wrapper `{wrapper}` could not resolve its stage order: {source}")]
    Unresolvable {
        /// The wrapper's variant id.
        wrapper: String,
        /// Why the resolution failed.
        #[source]
        source: Box<stella_plugin::ManifestError>,
    },

    /// An in-process wrapper's own logic failed. The detail is a leaf
    /// explanation for a human, not something a caller branches on — the
    /// branch a caller needs is "which point failed", which the call site
    /// already knows.
    #[error("wrapper `{wrapper}` failed at {point}: {detail}")]
    Handler {
        /// The wrapper's variant id.
        wrapper: String,
        /// Which point it failed at.
        point: stella_plugin::WrapperPoint,
        /// Why.
        detail: String,
    },
}

/// A driver session could not be opened, or its answer could not be used.
///
/// [`WrapperError`]'s shape, minus every variant that is about standing inside
/// a turn and plus nothing: a driver session has one point, carries no protocol
/// version on the wire (`stella_plugin::DriveRequest`), and contributes nothing
/// to a turn that would need admitting. What is left is what can go wrong
/// between a host and a child process it is talking to.
#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    /// A transport was constructed with no program to run.
    #[error("a driver's argv must name a program: it is empty")]
    EmptyArgv,

    /// A transport was constructed with a zero budget, which would kill the
    /// driver before it ran.
    #[error("a driver session's budget must be at least one millisecond")]
    ZeroTimeout,

    /// The process could not be started at all.
    #[error("cannot start driver `{program}`: {source}")]
    Spawn {
        /// `argv[0]`, as declared.
        program: String,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },

    /// The session could not be written to the child, or its answer could not
    /// be read back.
    #[error("cannot hold a session with driver `{program}`: {source}")]
    Transport {
        /// `argv[0]`, as declared.
        program: String,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },

    /// The driver outlived its session budget and was killed.
    ///
    /// The budget covers the **whole** session, not each message of it: a
    /// driver that spends its session asking is killed at the same deadline as
    /// one that spends it thinking, so talking buys no time
    /// (`doc:wrapper-socket` §6b).
    #[error("driver `{program}` did not end its session within {}s", .timeout.as_secs_f64())]
    Timeout {
        /// `argv[0]`, as declared.
        program: String,
        /// The budget it exceeded.
        timeout: Duration,
    },

    /// The driver wrote more than the transport will read, and the read was
    /// stopped there.
    #[error(
        "driver `{program}` wrote more to {stream} than the {cap}-byte ceiling this host will \
         read, so the session was ended"
    )]
    OutputCap {
        /// `argv[0]`, as declared.
        program: String,
        /// Which stream crossed the ceiling: `"stdout"` or `"stderr"`.
        stream: &'static str,
        /// The ceiling it crossed.
        cap: usize,
    },

    /// The driver ran and exited non-zero.
    #[error("driver `{program}` exited with {status}: {stderr}")]
    Exit {
        /// `argv[0]`, as declared.
        program: String,
        /// The exit status, rendered — a code, or "a signal" where the
        /// platform reports one.
        status: String,
        /// What it wrote to stderr, bounded.
        stderr: String,
    },

    /// The session request could not be serialized. Reachable only for a value
    /// that cannot be represented as JSON at all.
    #[error("cannot encode a driver session request: {0}")]
    Encode(#[source] serde_json::Error),

    /// The driver's stdout ended without the response that ends the session.
    ///
    /// **The one failure a self-driving loop must never read as "nothing to
    /// do".** Both terminal answers say something — `sleep` asks to be woken,
    /// `halt` says why it stopped — and a driver that says neither has not
    /// decided anything. A host that treated silence as a halt would retire a
    /// loop on a crash; one that treated it as a sleep would restart a broken
    /// driver forever.
    #[error("driver `{program}` exited without ending its session: {stderr}")]
    NoResponse {
        /// `argv[0]`, as declared.
        program: String,
        /// What it wrote to stderr, bounded — usually where the reason is.
        stderr: String,
    },

    /// The driver's answer was not a readable message.
    #[error("driver `{program}` answered unreadably: {source} (in: {answer})")]
    Decode {
        /// `argv[0]`, as declared.
        program: String,
        /// What it wrote, bounded, so the fix does not need a second run.
        answer: String,
        /// The parse failure.
        #[source]
        source: serde_json::Error,
    },

    /// The driver asked for a capability at a host that closed its stdin,
    /// because no callable capability was on offer.
    ///
    /// [`WrapperError::UnannouncedCall`]'s shape and its two causes: either the
    /// manifest declares no `[driver] calls`, or this host attached no
    /// [`DriverCallGate`](super::DriverCallGate). Reported to the host rather
    /// than waited out, because the refusal a *declared* call gets is delivered
    /// to the driver and this one cannot be — there is no channel left to
    /// deliver it on.
    #[error(
        "driver `{program}` asked the host for \"{call}\", but no channel was open to answer on: \
         its manifest declares no [driver] calls, or this host attached no gate"
    )]
    UnannouncedCall {
        /// `argv[0]`, as declared.
        program: String,
        /// The capability it asked for.
        call: stella_plugin::DriverCall,
    },

    /// The driver asked for a capability after the channel that answers it had
    /// already died, mid-session.
    ///
    /// Distinct from [`DriverError::UnannouncedCall`] for
    /// [`WrapperError::AnswerChannelFailed`]'s reason: this one fires only
    /// after an answer already went back through this exact gate, which proves
    /// the manifest and the gate were both fine and the fault is the pipe.
    /// `source` is that write failure, recovered by joining the writer task
    /// rather than discarded.
    #[error(
        "driver `{program}` asked the host for \"{call}\", but the channel answering an earlier \
         ask in this session had already failed: {source}"
    )]
    AnswerChannelFailed {
        /// `argv[0]`, as declared.
        program: String,
        /// The capability the driver asked for after the channel had died.
        call: stella_plugin::DriverCall,
        /// The write failure that killed the channel.
        #[source]
        source: std::io::Error,
    },

    /// The driver's last message was a capability ask and then its stdout
    /// ended, so it could not have read the answer.
    ///
    /// A driver that asks and stops listening has abandoned the session, and
    /// crediting it with a `next` it never wrote would be inventing the one
    /// decision this channel exists to carry.
    #[error(
        "driver `{program}` asked the host for \"{call}\" and then closed its output without \
         reading the answer"
    )]
    UnansweredCall {
        /// `argv[0]`, as declared.
        program: String,
        /// The capability it asked for and abandoned.
        call: stella_plugin::DriverCall,
    },
}

/// Bound a child's own output before it lands in an error message.
///
/// A plugin is third-party code and its stderr is whatever it felt like
/// writing; an unbounded copy of it in an error is how a runaway process
/// becomes a runaway log line.
pub(crate) fn bounded(text: &[u8], limit: usize) -> String {
    let text = String::from_utf8_lossy(text);
    let trimmed = text.trim();
    match trimmed.char_indices().nth(limit) {
        None => trimmed.to_string(),
        Some((cut, _)) => format!("{}… ({} bytes total)", &trimmed[..cut], text.len()),
    }
}
