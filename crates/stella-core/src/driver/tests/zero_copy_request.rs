//! Witnesses that the step path never deep-copies what it sends (#921).
//!
//! These assert on *addresses*, not on allocation counts. An allocator probe
//! would measure the whole test binary — every other task on the runtime, the
//! event channel, the tool registry — and would need the suite to run
//! single-threaded to mean anything. A slice address is exact, deterministic,
//! and says precisely the thing the issue asks for: the bytes the adapter
//! serializes are the caller's own bytes, not a copy of them.

use super::*;

/// Records the address (and length) of the transcript and tool-schema slices
/// each attempt hands the adapter, and fails its first call with a
/// **retryable** error so the engine drives a second attempt through the same
/// `FnMut` closure — the exact place the per-attempt copy used to happen.
struct SliceAddressProvider {
    /// `(messages.as_ptr(), messages.len(), tools.as_ptr(), tools.len())` per
    /// attempt, in order.
    seen: std::sync::Mutex<Vec<(usize, usize, usize, usize)>>,
    attempts: AtomicU32,
}

#[async_trait]
impl Provider for SliceAddressProvider {
    fn id(&self) -> &str {
        "slice-address"
    }

    // Deliberately implements only the borrowed method: the engine reaches the
    // adapter through `complete_observed_ref`, whose default forwards the view
    // untouched. If that default ever materialized an owned request, these
    // addresses would stop matching.
    async fn complete_ref(
        &self,
        req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResultAlias, ProviderError> {
        self.seen.lock().unwrap_or_else(|p| p.into_inner()).push((
            req.messages.as_ptr() as usize,
            req.messages.len(),
            req.tools.as_ptr() as usize,
            req.tools.len(),
        ));
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(ProviderError::Transport("blip".into()));
        }
        Ok(text_result("done"))
    }
}

/// The heart of #921: a retried call must not re-copy anything.
///
/// Before the fix, `CompletionRequest` owned its `messages` and `tools` and the
/// retry `attempt_fn` was `FnMut`, so every attempt paid `messages.to_vec()` +
/// `tools_schema.clone()` — a deep copy of every message string, every tool
/// call's `serde_json::Value`, every tool result payload, and every tool
/// schema's full JSON parameter document. Two attempts meant two copies.
///
/// Identical addresses across both attempts prove there are now zero.
#[tokio::test]
async fn a_retried_attempt_re_sends_the_same_slices_it_did_the_first_time() {
    let provider = SliceAddressProvider {
        seen: std::sync::Mutex::new(Vec::new()),
        attempts: AtomicU32::new(0),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut messages = vec![
        CompletionMessage::system("You are Stella."),
        CompletionMessage::user("do the work"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );

    let seen = provider
        .seen
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    assert_eq!(
        seen.len(),
        2,
        "fixture sanity: the first attempt must fail retryably so a second one \
         runs — without two attempts this proves nothing, got {seen:?}"
    );
    assert!(
        seen[0].3 > 0,
        "fixture sanity: the tool list must be non-empty, or its address is the \
         dangling one every empty slice shares and the assertion below is vacuous"
    );
    assert_eq!(
        seen[0], seen[1],
        "the retried attempt was handed different memory than the first — the \
         request is being deep-copied per attempt again (#921)"
    );
}

/// The other half of the acceptance criterion: not merely "no copy per
/// attempt" but no copy *at all*. The adapter must serialize straight off the
/// caller's transcript.
///
/// This is also what makes the prompt-cache stability contract (L-E8 / #372)
/// structural rather than merely observed — see
/// `the_system_prefix_stays_byte_stable_across_a_compacting_turn`, which pins
/// the *bytes*. There is no intermediate buffer in which the system prefix
/// could drift, because there is no intermediate buffer.
#[tokio::test]
async fn the_adapter_serializes_off_the_callers_own_transcript() {
    let provider = SliceAddressProvider {
        seen: std::sync::Mutex::new(Vec::new()),
        attempts: AtomicU32::new(0),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let (tx, _rx) = mpsc::unbounded_channel();
    // Reserved up front so appending the turn's answer cannot reallocate the
    // buffer out from under the address recorded during the call — the test
    // would then be measuring `Vec` growth, not copying.
    let mut messages = Vec::with_capacity(64);
    messages.push(CompletionMessage::system("You are Stella."));
    messages.push(CompletionMessage::user("do the work"));
    let caller_transcript = messages.as_ptr() as usize;
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        messages.as_ptr() as usize,
        caller_transcript,
        "fixture sanity: the transcript reallocated during the turn, so the \
         address compared below is not the one the call actually saw"
    );

    let seen = provider
        .seen
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let (sent, _, _, _) = seen[0];
    assert_eq!(
        sent, caller_transcript,
        "the adapter was handed a copy of the transcript rather than the \
         caller's own slice (#921)"
    );
}
