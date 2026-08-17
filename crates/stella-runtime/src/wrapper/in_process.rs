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
//! The handler is **synchronous**, which is the one place this differs from
//! the trait it feeds, and the difference is the honest one: a wrapper whose
//! work is pure computation over the request (a fixture, a policy, a fold over
//! evidence the host already gathered) belongs here, and a wrapper that spawns
//! a benchmark or waits on a socket belongs behind the subprocess transport —
//! which is a process boundary the host can bound with a timeout and kill.
//! Blocking an async runtime worker on a test suite is how one plugin stalls
//! every session in a server.

use async_trait::async_trait;
use stella_plugin::{AfterTurnRequest, AfterTurnResponse, BeforeTurnRequest, BeforeTurnResponse};

use super::{TurnWrapper, WrapperError};

/// A wrapper that answers in this process, over the wire contract's own types.
///
/// Implementors are Rust plugins linked into the host. The bound is `Send +
/// Sync` because a host holds one across `.await` points and may drive several
/// sessions from one runtime.
pub trait WrapperHandler: Send + Sync {
    /// Contribute to the turn about to run. See
    /// [`TurnWrapper::before_turn`] for what may and may not be said.
    ///
    /// # Errors
    ///
    /// [`WrapperError::Handler`] is the variant a handler builds for its own
    /// failures; the transport variants are not reachable from here.
    fn before_turn(&self, request: BeforeTurnRequest) -> Result<BeforeTurnResponse, WrapperError>;

    /// Report evidence about the turn that ran. See
    /// [`TurnWrapper::after_turn`].
    ///
    /// # Errors
    ///
    /// [`WrapperError::Handler`], as above.
    fn after_turn(&self, request: AfterTurnRequest) -> Result<AfterTurnResponse, WrapperError>;
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
}

impl<H: WrapperHandler> InProcessWrapper<H> {
    /// Wrap a handler.
    #[must_use]
    pub fn new(handler: H) -> Self {
        Self { handler }
    }

    /// The handler, for a host that also drives it directly.
    #[must_use]
    pub fn handler(&self) -> &H {
        &self.handler
    }
}

#[async_trait]
impl<H: WrapperHandler> TurnWrapper for InProcessWrapper<H> {
    async fn before_turn(
        &self,
        request: BeforeTurnRequest,
    ) -> Result<BeforeTurnResponse, WrapperError> {
        self.handler.before_turn(request)
    }

    async fn after_turn(
        &self,
        request: AfterTurnRequest,
    ) -> Result<AfterTurnResponse, WrapperError> {
        self.handler.after_turn(request)
    }
}
