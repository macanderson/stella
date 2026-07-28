//! The in-flight reverse-request registry.
//!
//! When the engine raises a reverse-RPC request (a tool call or a model
//! completion), it registers a one-shot reply channel here keyed by a fresh
//! `request_id`, then emits the request frame and awaits the receiver. When the
//! host POSTs the matching result, the serve layer looks the id up here and
//! fires the sender — unblocking the parked engine step.
//!
//! The registry is shared (`Arc<Mutex<..>>`) between the engine-driving thread
//! (which registers) and the inbound HTTP handlers (which resolve). The one-shot
//! senders bridge the two Tokio runtimes: the receiver is awaited on the session
//! thread's current-thread runtime, the sender fired from the server runtime —
//! `tokio::sync::oneshot` is runtime-agnostic, so the wake crosses cleanly. This
//! is the fleet's `!Send`-future bridge pattern (`stella-cli::fleet_cmd`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use stella_protocol::{CompletionResult, ProviderError, ToolOutput};
use tokio::sync::oneshot;

use crate::error::ServeError;

/// The reply channel a parked provider step awaits: either the host's completed
/// model call, or the failure class it wants the engine's retry logic to see.
type ProviderReply = oneshot::Sender<Result<CompletionResult, ProviderError>>;

/// A registered reply channel, discriminated by the kind of request it answers.
enum PendingReply {
    Tool(oneshot::Sender<ToolOutput>),
    Provider(ProviderReply),
}

/// A cloneable handle to the shared registry. Cloning shares the same map.
#[derive(Clone, Default)]
pub struct Pending {
    inner: Arc<Mutex<HashMap<String, PendingReply>>>,
    /// Latched by [`Pending::cancel`]. The registry is the natural home for the
    /// turn's cancel flag: it is already the shared, `Send`, cloneable handle
    /// that both the HTTP side and the engine thread hold, and cancellation
    /// means exactly "stop waiting on reverse requests".
    cancelled: Arc<AtomicBool>,
}

impl Pending {
    /// Register a tool reply channel under `id`. Called by the engine thread
    /// before it emits the request frame, so the entry always exists by the
    /// time the host can answer.
    ///
    /// Returns `false` — registering nothing, dropping `reply` — when the turn
    /// is already cancelled; the caller must not park. The cancel check shares
    /// the insert's lock on purpose: [`Pending::cancel`] latches its flag
    /// *before* it takes this lock to clear the map, so a cancel whose clear
    /// has already run is guaranteed visible here, and one that has not yet
    /// cleared will drop this entry (waking the parked step) when it does.
    /// Checking outside the lock leaves a window — cancel landing between a
    /// port's `is_cancelled` check and its registration — where the clear
    /// misses the entry and the step re-parks until its full deadline.
    #[must_use]
    pub(crate) fn register_tool(&self, id: String, reply: oneshot::Sender<ToolOutput>) -> bool {
        let mut map = self.lock();
        if self.is_cancelled() {
            return false;
        }
        map.insert(id, PendingReply::Tool(reply));
        true
    }

    /// Register a provider reply channel under `id`. Same contract — and the
    /// same locked cancellation check — as [`Pending::register_tool`].
    #[must_use]
    pub(crate) fn register_provider(&self, id: String, reply: ProviderReply) -> bool {
        let mut map = self.lock();
        if self.is_cancelled() {
            return false;
        }
        map.insert(id, PendingReply::Provider(reply));
        true
    }

    /// Resolve a tool request with the host-supplied output. Errors if `id` is
    /// unknown or names a provider request.
    pub fn resolve_tool(&self, id: &str, output: ToolOutput) -> Result<(), ServeError> {
        // A receive-side drop (the engine step was cancelled) is not an error to
        // the host: the request is simply gone.
        let _ = self.take_tool(id)?.send(output);
        Ok(())
    }

    /// Resolve a provider request with the host-supplied result. Errors if `id`
    /// is unknown or names a tool request.
    pub fn resolve_provider(
        &self,
        id: &str,
        result: Result<CompletionResult, ProviderError>,
    ) -> Result<(), ServeError> {
        let _ = self.take_provider(id)?.send(result);
        Ok(())
    }

    /// Drop every in-flight request. Their receivers observe a channel close,
    /// which each port impl maps to a clean error for the engine — used by the
    /// session thread once its turn has settled, so no reverse-RPC one-shot
    /// outlives it. Teardown mid-turn goes through [`Pending::cancel`] instead:
    /// a bare clear wakes a parked step with a *retryable* error.
    pub(crate) fn clear(&self) {
        self.lock().clear();
    }

    /// Cancel the turn: latch the flag, then drop every in-flight request so any
    /// parked engine step wakes at once.
    ///
    /// The latch is what makes cancellation stick. Dropping the senders alone
    /// would only unblock the *current* step; the engine would then take the
    /// resulting error as an ordinary failure and, for a tool, simply call the
    /// model again — registering a fresh request that parks all over again. With
    /// the flag set, [`Pending::is_cancelled`] lets each port refuse to park
    /// again, so the turn unwinds to `ProviderError::Cancelled` (not retryable)
    /// within one engine step.
    ///
    /// Ordering matters: latch *before* clearing, so a step woken by the clear
    /// always observes the flag already set.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.clear();
    }

    /// Whether [`Pending::cancel`] has been called for this turn.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Drop the reply channel registered under `id` without resolving it, used
    /// when a port gives up on a request (its deadline expired).
    ///
    /// Leaving the entry behind would let a host that answers late resolve a
    /// one-shot nobody is listening to — a silent no-op the host reads as
    /// success. Removing it means that late POST gets an honest
    /// [`ServeError::UnknownRequest`] 409 instead.
    pub(crate) fn abandon(&self, id: &str) {
        self.lock().remove(id);
    }

    /// Take the tool reply channel registered under `id`, leaving a
    /// wrong-kinded entry untouched.
    ///
    /// The kind check and the removal happen under a *single* lock on purpose.
    /// Removing first and re-inserting on a mismatch looks equivalent but is
    /// not: during that window the id is absent from the map, so a correctly
    /// kinded resolve racing a mis-kinded one is rejected as unknown — the host
    /// sees a 409 for a result it will not send twice and the engine step stays
    /// parked forever with nobody left to answer it.
    fn take_tool(&self, id: &str) -> Result<oneshot::Sender<ToolOutput>, ServeError> {
        let mut map = self.lock();
        match map.remove(id) {
            Some(PendingReply::Tool(tx)) => Ok(tx),
            Some(other) => {
                map.insert(id.to_string(), other);
                Err(ServeError::RequestKindMismatch(id.to_string(), "tool"))
            }
            None => Err(ServeError::UnknownRequest(id.to_string())),
        }
    }

    /// Take the provider reply channel registered under `id`. Same
    /// single-lock discipline as [`Pending::take_tool`].
    fn take_provider(&self, id: &str) -> Result<ProviderReply, ServeError> {
        let mut map = self.lock();
        match map.remove(id) {
            Some(PendingReply::Provider(tx)) => Ok(tx),
            Some(other) => {
                map.insert(id.to_string(), other);
                Err(ServeError::RequestKindMismatch(id.to_string(), "provider"))
            }
            None => Err(ServeError::UnknownRequest(id.to_string())),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, PendingReply>> {
        // The registry guards only map insert/remove — no await is held across
        // the lock, so poisoning can only follow a panic while mutating the map
        // itself; recover the guard rather than propagate the poison.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cancel that has fully run before a port registers must refuse the
    /// registration: its wake-everyone clear has already happened, so an entry
    /// inserted now would park its step until the full reverse-request
    /// deadline with nobody left to wake it.
    #[test]
    fn registration_is_refused_once_cancelled() {
        let pending = Pending::default();
        pending.cancel();

        let (tx, rx) = oneshot::channel();
        assert!(!pending.register_provider("prov-0".to_string(), tx));
        // The refused sender is dropped, so a caller that parked anyway would
        // still wake immediately rather than wait for a resolve that can
        // never come.
        assert!(rx.blocking_recv().is_err());

        let (tx, rx) = oneshot::channel();
        assert!(!pending.register_tool("tool-0".to_string(), tx));
        assert!(rx.blocking_recv().is_err());
    }

    /// The other interleaving: a cancel landing *after* registration must
    /// observe the entry and drop its sender — and the flag must already be
    /// latched by then, so the woken step reads the wake as cancellation
    /// (non-retryable), not as an ordinary transport failure.
    #[test]
    fn cancel_after_registration_wakes_the_parked_sender() {
        let pending = Pending::default();
        let (tx, rx) = oneshot::channel();
        assert!(pending.register_provider("prov-0".to_string(), tx));

        pending.cancel();

        assert!(pending.is_cancelled(), "the flag is latched before the clear");
        assert!(rx.blocking_recv().is_err(), "the clear woke the parked step");
    }
}
