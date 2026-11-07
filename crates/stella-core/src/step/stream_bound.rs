//! The bounds a provider dispatch runs under, and the signal they read.
//!
//! [`StreamProgress`] is what a dispatch writes as it goes, and the three
//! bounds around it are what read it. The idle bound asks whether anything
//! arrived recently, the deadline bound asks whether the task has time left,
//! and the guard in [`super::degenerate`] asks whether what arrived says
//! anything. Each answers a question the other two cannot.
//!
//! They live here rather than in [`super`] because they are one subject with
//! one shared signal, and the step module around them is about a turn's
//! state.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use stella_protocol::{CompletionResult, PartialUsage, ProviderError};

use super::degenerate;

/// Monotonic count of stream fragments a provider dispatch has delivered.
///
/// One signal separates a wedged call from a working one. A provider still
/// sending fragments is answering, however slowly. The gate's delta path
/// clones this, and [`bounded_generation`] reads it.
/// [`crate::accounted_call::run_accounted_call`]'s per-call deadline reads it
/// too. Some callers dispatch through `AccountedCall` instead of a step:
/// pipeline triage, the verifier, plan, and the overflow summarizer. They
/// need the same signal. `count` is `pub(crate)` for that second reader. The ordering is `Relaxed`. The only
/// question asked of it is whether it changed since the last look, and no
/// ordering with other memory affects that.
///
/// `fragments` answers the idle bound's question. `watch` answers the one it
/// cannot (see [`degenerate`]). The two ride together. The same observer
/// calls feed both, on the same fragment. Splitting them would thread two
/// handles down every delta path to ask two halves of one question.
#[derive(Debug, Default)]
struct ProgressState {
    fragments: AtomicU64,
    watch: degenerate::DegenerateWatch,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct StreamProgress(Arc<ProgressState>);

impl StreamProgress {
    /// Record a fragment with no text to read. That means a tool call, or
    /// one piece of a tool call's part-built JSON. It marks life, no more.
    pub(crate) fn record(&self) {
        self.0.fragments.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a fragment of streamed text: an answer, or thinking.
    ///
    /// It marks life just as [`Self::record`] does. It also feeds the text to
    /// the watch. This is its own method for a reason. The other callers have
    /// no text to give. Tool arguments are part-built JSON, and the observer
    /// port does not carry those bytes. Making up a `&str` for them would put
    /// raw JSON in front of a check tuned on plain text.
    pub(crate) fn record_text(&self, delta: &str) {
        self.record();
        self.0.watch.feed(delta);
    }

    pub(crate) fn count(&self) -> u64 {
        self.0.fragments.load(Ordering::Relaxed)
    }

    /// Waits for the watch to mark this stream broken. Returns at once if it
    /// already has.
    pub(crate) async fn degenerated(&self) {
        self.0.watch.tripped().await;
    }

    /// Decide what a finished dispatch hands back.
    ///
    /// Two readings the race beside the dispatch cannot make, and one place
    /// to make them, because [`bounded_generation`] and
    /// [`crate::accounted_call::run_accounted_call`] both end a dispatch this
    /// way and two copies of one rule drift apart.
    ///
    /// The first is a trip that landed in the same poll the call returned in.
    /// `select` polls the call first and never polls the watch again, so
    /// [`Self::degenerated`] is never given the chance to report it.
    ///
    /// The second is an answer the watch did not see in full. The observer
    /// is advisory, and the returned result is the definitive answer. An
    /// adapter whose stream recovery has latched sends a unary request,
    /// returns the whole answer, and never calls the observer. Another may
    /// announce a benign opening and leave the rest unannounced. Either way
    /// the worst answer of all reads as clean to the watch. So the returned
    /// text is always read, on its own and from the start, by
    /// [`degenerate::answer_degenerates`]. That covers an adapter nobody has
    /// written yet, too.
    ///
    /// Reading it apart from the watch is what keeps a streamed answer from
    /// counting twice. The watch saw it fragment by fragment, and the same
    /// text comes back on the result. Feeding it to the watch again would
    /// join the two copies of a long rule into one run.
    ///
    /// A failed dispatch beside a tripped watch is replaced too. A retryable
    /// spelling would buy the broken host the same call again.
    ///
    /// A rejection carries the answer's accounting out with it. The host
    /// served this call and will bill it. The tokens and the cost are a real
    /// charge whatever the characters were. The error is the only thing left
    /// to carry them, because the result they were attached to is being
    /// thrown away. A failed dispatch keeps whatever accounting its own error
    /// already held.
    ///
    /// The carrier is a [`PartialUsage`], and that type promises its inner
    /// `reported` is always `false`, so no partial can pass
    /// [`stella_protocol::CompletionUsage::is_complete`]. The provider's
    /// attestation moves to `input_reported`, the field the type keeps for
    /// it, and the counts travel unchanged.
    pub(crate) fn settle(
        &self,
        result: Result<CompletionResult, ProviderError>,
    ) -> Result<CompletionResult, ProviderError> {
        let answer_tripped = matches!(
            &result,
            Ok(completed) if degenerate::answer_degenerates(&completed.text)
        );
        if answer_tripped || self.0.watch.is_tripped() {
            let spent = match &result {
                Ok(completed) => Some(PartialUsage {
                    usage: stella_protocol::CompletionUsage {
                        reported: false,
                        ..completed.usage
                    },
                    cost_usd: completed.cost_usd,
                    input_reported: completed.usage.reported,
                }),
                Err(failed) => failed.partial_usage().copied(),
            };
            return Err(degenerate::degenerate_error(spent));
        }
        result
    }

    /// Open a new stream on this clock: call it at the top of every attempt.
    ///
    /// The fragment count is left alone. It is shared across attempts on
    /// purpose, so a fresh attempt is not charged for a sibling's silence.
    /// The run of one character is the opposite. It belongs to one stream,
    /// and a retry often opens on a different host, so it starts at zero.
    pub(crate) fn begin_stream(&self) {
        self.0.watch.reset();
    }
}

/// Bound one provider dispatch by [`crate::EngineConfig::model_timeout`]. The bound
/// is **idle time**, the time since the last streamed fragment, not the total
/// length of the call.
///
/// The deadline exists to close an unbounded wait on a provider that is not
/// answering. Total duration cannot say that. It also fires on a provider
/// that is answering the whole time, just slowly. Reasoning models made that
/// difference real. One hard call at high effort can stream for well past ten
/// minutes. A wall-clock bound kills it mid-answer and reports it as a
/// provider fault. Idle time asks what the deadline means: has anything
/// arrived recently?
///
/// A provider that streams nothing at all is bounded as before. With no
/// fragments, the idle clock and the wall clock are one clock.
///
/// The guard in [`degenerate`] rides along with it. That guard closes the
/// mirror of this blind spot. A stream stuck on one character never goes
/// idle. It meets this deadline in every window. Nothing else would stop it
/// short of a person.
///
/// The idle trip is [`ProviderError::Terminal`] on purpose, where the
/// degeneracy guard beside it ends on [`ProviderError::Degenerate`].
/// `Transport` is retryable. Spelling it that way would hand a provider that
/// is not answering the same unbounded wait again, once per attempt. That
/// multiplies the window this deadline exists to close. `stella-serve`'s
/// reverse-RPC deadline made the same call for the same reason.
///
/// This wraps the dispatch, not the whole retry future, so the bound is per
/// generation. Backoff sleeps between attempts are not charged against it. A
/// slow but progressing provider is never cut off by time another attempt
/// spent.
pub(crate) async fn bounded_generation<F>(
    sleeper: &dyn crate::retry::Sleeper,
    limit: Option<Duration>,
    progress: &StreamProgress,
    call: F,
) -> Result<CompletionResult, ProviderError>
where
    F: Future<Output = Result<CompletionResult, ProviderError>>,
{
    let idle = std::pin::pin!(idle_bounded_generation(sleeper, limit, progress, call));
    let degenerate = std::pin::pin!(progress.degenerated());
    match futures_util::future::select(idle, degenerate).await {
        // A dispatch that finished gets the second look
        // `StreamProgress::settle` describes. That look reads the latch, for
        // a trip that landed in the poll the call returned in. It also reads
        // the returned text, which the observer may have announced only in
        // part or not at all.
        futures_util::future::Either::Left((result, _)) => progress.settle(result),
        futures_util::future::Either::Right(((), _)) => Err(degenerate::degenerate_error(None)),
    }
}

/// The idle half of [`bounded_generation`], split out for one reason. The
/// race against the watch then sits in one `select` over the whole call, not
/// inside the loop that re-arms the clock. Inside that loop, a window still
/// running would hold the trip back until it ended.
async fn idle_bounded_generation<F>(
    sleeper: &dyn crate::retry::Sleeper,
    limit: Option<Duration>,
    progress: &StreamProgress,
    call: F,
) -> Result<CompletionResult, ProviderError>
where
    F: Future<Output = Result<CompletionResult, ProviderError>>,
{
    let Some(limit) = limit else {
        return call.await;
    };
    let mut call = std::pin::pin!(call);
    let mut seen = progress.count();
    loop {
        match crate::retry::bounded(sleeper, limit, &mut call).await {
            Some(result) => return result,
            None => {
                // The window elapsed. Whether that is a fault depends on what
                // arrived during it: any fragment at all means the provider is
                // answering, so re-arm and keep waiting. Only a window that
                // passed in complete silence is the wedge this bound is for.
                let now = progress.count();
                if now == seen {
                    return Err(ProviderError::Terminal(format!(
                        "generation stalled: no stream fragment for {}s (model deadline)",
                        limit.as_secs()
                    )));
                }
                seen = now;
            }
        }
    }
}

/// Bound one provider dispatch by what is left of the TASK's own wall-clock
/// deadline ([`crate::BudgetGuard::task_deadline`]). It composes around
/// [`bounded_generation`]'s idle bound rather than replacing it.
///
/// This is **not** the flat wall-clock ceiling `#1467` removed from
/// [`crate::accounted_call::run_accounted_call`]. That one fired the instant
/// a fixed duration elapsed, even while the provider was streaming. It cut
/// live generations off with a ceiling sized to catch silence, and it lost
/// OpenRouter's trailing usage frame with them. This bound fires only when
/// the task is out of time. `task_deadline` is `None` when no task deadline
/// is armed, which is the interactive CLI with no bench harness. Nothing
/// changes for that shape. A call answering well inside the task's remaining
/// budget is never touched.
///
/// What it closes (`#2021`): the between-step deadline check
/// (`driver::settlement::check_budget`) looks at the last step's pace before
/// opening a new one. It cannot see a call that is about to outrun the clock
/// once dispatched. A TB2.1 `fix-git` trial showed that on 2026-08-09. The
/// call went out with 120.19s of task deadline left. It ran 120.43s and
/// produced nothing. The run died 15.9s past the deadline, with
/// otherwise-complete work sitting uncommitted. The idle bound cannot see
/// this either. The provider was never silent. The connection never finished.
///
/// The deadline comes from [`crate::BudgetGuard::task_deadline`], not from a
/// standalone `EngineConfig` field. A ceiling set apart from the task's own
/// deadline is the kind of number that drifts from it, which is what
/// `#2021`'s own "what to do" list warns against. Reading the same `Instant`
/// the between-step check reads leaves the two unable to disagree. The
/// argument is the absolute `Instant`, not a pre-subtracted `Duration`, and
/// this reads the clock itself. A caller may capture it once outside a
/// per-attempt retry closure, and each attempt is still bounded by what
/// remains at its own dispatch.
///
/// The trip is [`ProviderError::Terminal`], for the reason
/// [`bounded_generation`]'s is: the task is out of time, so retrying spends
/// more of a deadline that already ran out.
pub(crate) async fn deadline_bounded_generation<F>(
    sleeper: &dyn crate::retry::Sleeper,
    idle_limit: Option<Duration>,
    task_deadline: Option<std::time::Instant>,
    progress: &StreamProgress,
    call: F,
) -> Result<CompletionResult, ProviderError>
where
    F: Future<Output = Result<CompletionResult, ProviderError>>,
{
    let generation = bounded_generation(sleeper, idle_limit, progress, call);
    let Some(deadline) = task_deadline else {
        return generation.await;
    };
    let remaining = deadline.saturating_duration_since(sleeper.now());
    match crate::retry::bounded(sleeper, remaining, generation).await {
        Some(result) => result,
        None => Err(ProviderError::Terminal(format!(
            "generation exceeded the task's remaining wall clock ({}ms): \
             the task deadline ran out mid-call",
            remaining.as_millis()
        ))),
    }
}
