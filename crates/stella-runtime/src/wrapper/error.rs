//! Why a wrapper call failed, as a typed answer (invariant 5).
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
