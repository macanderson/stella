//! The in-process transport — the Rust fast path, and nothing more than that.
//!
//! `doc:pipeline-as-plugins` §5 commitment 2 is the constraint this module is
//! written against: **Rust may additionally have an in-process path, but the
//! wire path must be the one CI exercises.** If Rust could reach a capability
//! Python cannot ask for, the wire contract becomes second-class and rots. So
//! this transport is deliberately not a richer seam — a [`WrapperHandler`]
//! takes and returns the exact same owned request/response values a
//! [`SubprocessWrapper`](super::SubprocessWrapper) carries over stdio, and
//! there is nothing a handler can say that a JSON message could not.
//!
//! The handler was **synchronous** until the host-call channel landed, and the
//! argument for that is worth recording because it did not survive its own
//! premise: a wrapper whose work is pure computation over the request (a
//! fixture, a policy, a fold over evidence the host already gathered) has no
//! reason to await, and a wrapper that spawns a benchmark belongs behind the
//! process boundary a host can bound with a timeout and kill. That is still the
//! guidance. What changed is that asking the host for recall **is** I/O by
//! construction (`doc:wrapper-socket` §6b), so a synchronous handler could not
//! reach a capability a Python plugin could — and a wire contract that Rust
//! cannot fully speak rots exactly as fast as the other direction
//! (`doc:pipeline-as-plugins` §5 commitment 2). The handler is now `async` and
//! takes the same [`HostCallChannel`] the subprocess transport relays through,
//! so both paths reach the identical capability set.
//!
//! Blocking an async runtime worker on a test suite is still how one plugin
//! stalls every session in a server. That is now guidance rather than a
//! signature, which is a real loss — noted rather than papered over.

use std::sync::Arc;

use async_trait::async_trait;
use stella_plugin::{AfterTurnRequest, AfterTurnResponse, BeforeTurnRequest, BeforeTurnResponse};

use super::host_call::{HostCallChannel, HostCallGate, NoHostCalls};
use super::{TurnWrapper, WrapperError};

/// A wrapper that answers in this process, over the wire contract's own types.
///
/// Implementors are Rust plugins linked into the host. The bound is `Send +
/// Sync` because a host holds one across `.await` points and may drive several
/// sessions from one runtime.
#[async_trait]
pub trait WrapperHandler: Send + Sync {
    /// Contribute to the turn about to run. See
    /// [`TurnWrapper::before_turn`] for what may and may not be said.
    ///
    /// `host` is the same bounded conversation a subprocess plugin gets: ask for
    /// a declared capability, read the answer, and answer the point. A refusal
    /// comes back as a value to degrade on, never as an error.
    ///
    /// # Errors
    ///
    /// [`WrapperError::Handler`] is the variant a handler builds for its own
    /// failures; the transport variants are not reachable from here.
    async fn before_turn(
        &self,
        request: BeforeTurnRequest,
        host: &dyn HostCallChannel,
    ) -> Result<BeforeTurnResponse, WrapperError>;

    /// Report evidence about the turn that ran. See
    /// [`TurnWrapper::after_turn`].
    ///
    /// # Errors
    ///
    /// [`WrapperError::Handler`], as above.
    async fn after_turn(
        &self,
        request: AfterTurnRequest,
        host: &dyn HostCallChannel,
    ) -> Result<AfterTurnResponse, WrapperError>;
}

/// The in-process transport: a [`WrapperHandler`] driven through
/// [`TurnWrapper`].
///
/// Generic rather than boxed so a host pays no dispatch for the fast path it
/// chose the fast path for; a host that needs one type for both transports
/// holds `Arc<dyn TurnWrapper>`, which both implementations satisfy.
#[derive(Debug, Clone)]
pub struct InProcessWrapper<H> {
    handler: H,
    gate: Option<Arc<HostCallGate>>,
}

impl<H: WrapperHandler> InProcessWrapper<H> {
    /// Wrap a handler.
    #[must_use]
    pub fn new(handler: H) -> Self {
        Self {
            handler,
            gate: None,
        }
    }

    /// Serve this handler's declared host calls from `gate` — the same gate,
    /// the same clamp and the same report the subprocess transport uses
    /// ([`SubprocessWrapper::serving`](super::SubprocessWrapper::serving)).
    #[must_use]
    pub fn serving(mut self, gate: Arc<HostCallGate>) -> Self {
        self.gate = Some(gate);
        self
    }

    /// The handler, for a host that also drives it directly.
    #[must_use]
    pub fn handler(&self) -> &H {
        &self.handler
    }

    /// The conversation for one point.
    ///
    /// A handler with no gate behind it gets [`NoHostCalls`], which refuses
    /// every ask with a reason rather than offering a channel that answers
    /// nothing.
    fn open(&self) -> Box<dyn HostCallChannel + '_> {
        match &self.gate {
            Some(gate) => Box::new(gate.open()),
            None => Box::new(NoHostCalls),
        }
    }
}

#[async_trait]
impl<H: WrapperHandler> TurnWrapper for InProcessWrapper<H> {
    async fn before_turn(
        &self,
        request: BeforeTurnRequest,
    ) -> Result<BeforeTurnResponse, WrapperError> {
        let channel = self.open();
        self.handler.before_turn(request, channel.as_ref()).await
    }

    async fn after_turn(
        &self,
        request: AfterTurnRequest,
    ) -> Result<AfterTurnResponse, WrapperError> {
        let channel = self.open();
        self.handler.after_turn(request, channel.as_ref()).await
    }
}
