//! Retry + backoff for the step-driver. Pure
//! decision logic plus one narrow async driver — `stella-core` has zero I/O
//! of its own, so even the async loop here drives through an injectable
//! [`Sleeper`] port rather than calling `tokio::time::sleep` directly, the
//! same "ports, not direct dependencies" seam as [`crate::ports::Clock`].
//!
//! Binding lessons this module encodes:
//!
//! - **L-M4**: deterministic fast paths (triage/classification) run with
//!   `max_retries = 0` and fall through on the first failure — never hang,
//!   never retry. [`RetryPolicy::deterministic`] is that policy.
//! - **L-M7**: retry classification is never re-derived here. Every decision
//!   defers to [`stella_protocol::ProviderError::is_retryable`], which
//!   providers set at the source.
//! - **L-E10**: speculative side-effect events flush only when the step
//!   commits. Retry history follows that rule, while paid-call accounting is
//!   intentionally stricter: `retry_with_backoff_observed` synchronously
//!   exposes every failed dispatched attempt so unknown provider usage can be
//!   persisted even when a later attempt succeeds.
//! - **#2677 / #2742 — abort becomes recovery**: a failure whose recovery is
//!   a function of *waiting* is bounded by the caller's wall clock, not the
//!   attempt count. A 429 or a 529 the inline ladder cannot absorb parks
//!   through the injected [`ParkSupervisor`] — chunked sleeps, per-chunk
//!   keep-alive, budget-derived allowance — and only an unaffordable wait
//!   still fails fast. Which failures those are is decided at the adapter
//!   and asked here through [`ProviderError::is_park_eligible`], never
//!   re-derived (L-M7 again). Nothing here reads a clock or a budget
//!   directly; the supervisor is a port, like [`Sleeper`].
//!
//! Per-call timeouts (L-E4) are a caller concern layered on top of
//! `attempt_fn`; this module owns "should we try again, and if so after how
//! long" — and the jitter on that delay is part of the answer
//! ([`compute_backoff_delay_ms`]'s equal jitter, `server_hint_delay_ms`'s
//! additive nudge), not something a caller layers on.

use std::future::Future;

use async_trait::async_trait;
use rand::{Rng, RngExt};
use stella_protocol::ProviderError;

/// The delay port `retry_with_backoff` sleeps through between attempts.
/// Injectable so retry-loop tests run instantly and deterministically
/// instead of paying real wall-clock delays — the same seam
/// [`crate::ports::Clock`] provides for reading time, but for the one place
/// this crate needs to actually suspend a task. Only the trait lives here —
/// the production tokio-backed impl belongs to the binary that constructs
/// the engine (the CLI's `runtime` module).
#[async_trait]
pub trait Sleeper: Send + Sync {
    /// Suspend the current task for `duration_ms` milliseconds.
    async fn sleep(&self, duration_ms: u64);
}

/// Milliseconds per parked-wait chunk: a long rate-limit wait is slept in
/// pieces of this size so the supervisor is consulted (and can narrate
/// liveness or end the park) at a human cadence rather than once per
/// multi-minute sleep. 30s matches the keep-alive cadence the comparator
/// lineage uses for its unattended rate-limit retries.
pub const PARK_CHUNK_MS: u64 = 30_000;

/// First parked-wait backoff when the server sent no `Retry-After` hint.
/// Deliberately past the inline ladder's 8s cap: by the time a park is
/// reached the inline ladder has already failed, so the brownout is minutes
/// long, not seconds.
const PARK_BACKOFF_FLOOR_MS: u64 = 30_000;

/// Ceiling on the hint-less parked-wait backoff — 5 minutes, the same cap
/// the comparator lineage uses for sustained 429 retries. A server hint
/// larger than this is still honored verbatim (within the allowance): a
/// stated window always beats a guessed one.
const PARK_BACKOFF_CAP_MS: u64 = 300_000;

/// What [`ParkSupervisor::park_tick`] tells the retry driver to do with the
/// rest of a parked wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkDirective {
    /// Keep waiting: sleep the next chunk.
    Continue,
    /// End the park now and surface the parked error.
    ///
    /// Deliberately reasonless: *why* a park ended early is the supervisor's
    /// own knowledge, and the supervisor is caller-side, so it records the
    /// answer where it can act on it rather than routing it back through this
    /// module (`driver::rate_limit`'s `soft_stopped` is the engine's — it is
    /// what lets a mid-park Esc settle as the step-boundary soft stop instead
    /// of a provider failure, #2743). Carrying the reason here would teach
    /// `retry` about steering, which is the coupling this port exists to
    /// prevent.
    Abort,
}

/// The caller-side authority over parked waits (#2677): how much wall clock
/// a park may spend, and the per-chunk keep-alive hook. This is a
/// port in the same sense as [`Sleeper`] — the retry driver stays pure
/// decision logic plus injected effects, so the budget guard, the event
/// stream, and the soft-stop latch never leak into this module.
///
/// A park happens only when a park-eligible failure — a rate limit or a
/// provider overload ([`ProviderError::is_park_eligible`]) — outlives the
/// inline ladder (or its `Retry-After` exceeds
/// [`RetryPolicy::max_server_hint_ms`]); the supervisor then bounds it. [`NoParking`] — the [`retry_with_backoff`] default —
/// grants zero allowance, which preserves the pre-#2677 fail-fast exactly.
/// `Send` because the retry future that borrows the supervisor crosses
/// `tokio::spawn` in the hosts that drive turns on worker threads — the same
/// bound [`Sleeper`] carries for the same reason.
pub trait ParkSupervisor: Send {
    /// Wall-clock milliseconds of parked waiting still affordable, asked
    /// fresh before each park. `waited_ms` is the parked time this call has
    /// already spent, so a deadline-less caller can still enforce an
    /// absolute cap. Returning `0` declines the park and the underlying
    /// error surfaces as it did before parking existed.
    fn wait_allowance_ms(&mut self, waited_ms: u64) -> u64;
    /// A park is starting: `planned_ms` total, slept in `chunk_ms` pieces.
    /// `reason` is the parked error's rendered message — it names the class
    /// (rate limit, overload) so the narration does not have to guess.
    fn park_opened(&mut self, planned_ms: u64, chunk_ms: u64, reason: &str);
    /// Consulted before every chunk (including the first, at
    /// `waited_ms == 0`): the keep-alive/liveness hook, and the seam a soft
    /// stop uses to end a multi-minute wait early.
    fn park_tick(&mut self, waited_ms: u64, planned_ms: u64) -> ParkDirective;
    /// The park ended after `waited_ms` across `chunks` sleeps; `aborted`
    /// is true when a tick ended it early.
    fn park_closed(&mut self, waited_ms: u64, chunks: u64, aborted: bool);
}

/// The zero-allowance supervisor: never parks, so every park-eligible
/// failure behaves exactly as it did before #2677 — the default for
/// [`retry_with_backoff`] callers that have no budget authority to consult.
pub struct NoParking;

impl ParkSupervisor for NoParking {
    fn wait_allowance_ms(&mut self, _waited_ms: u64) -> u64 {
        0
    }
    // Unreachable while the allowance is 0, but no-ops rather than panics:
    // this is library code on a provider-driven path (invariant 5).
    fn park_opened(&mut self, _planned_ms: u64, _chunk_ms: u64, _reason: &str) {}
    fn park_tick(&mut self, _waited_ms: u64, _planned_ms: u64) -> ParkDirective {
        ParkDirective::Continue
    }
    fn park_closed(&mut self, _waited_ms: u64, _chunks: u64, _aborted: bool) {}
}

/// The pure park decision: how long to wait for a park-eligible failure that
/// outlived the inline ladder, given the (already jittered) server hint
/// delay, how many parks this call has taken, and the supervisor's allowance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkPlan {
    /// Wait `total_ms`, then try again.
    Wait { total_ms: u64 },
    /// No affordable wait: surface the error.
    GiveUp,
}

/// Plan one parked wait. Pure — all inputs are owned numbers, so the whole
/// decision surface is property-testable without sleeping:
///
/// - A server hint that does not fit inside the allowance gives up: waiting
///   less than the server's stated window guarantees the same refusal, so the
///   fail-fast the hint ceiling used to provide survives, now measured
///   against real headroom instead of a fixed constant.
/// - A hint that fits is honored, raised to the streak backoff when that is
///   politer (waiting longer than asked is always safe), and clamped back to
///   the allowance (which never dips below the hint itself).
/// - With no hint, the wait is the streak backoff — doubling from
///   `PARK_BACKOFF_FLOOR_MS` (30s) to `PARK_BACKOFF_CAP_MS` (5min) — clamped
///   to the allowance, so the final park before an exhausted budget still
///   gets its last, shorter chance.
pub fn plan_park(hint_delay_ms: Option<u64>, park_streak: u32, allowance_ms: u64) -> ParkPlan {
    if allowance_ms == 0 {
        return ParkPlan::GiveUp;
    }
    let backoff = PARK_BACKOFF_FLOOR_MS
        .saturating_mul(2u64.saturating_pow(park_streak))
        .min(PARK_BACKOFF_CAP_MS);
    match hint_delay_ms {
        Some(hint) if hint > allowance_ms => ParkPlan::GiveUp,
        Some(hint) => ParkPlan::Wait {
            total_ms: hint.max(backoff).min(allowance_ms),
        },
        None => ParkPlan::Wait {
            total_ms: backoff.min(allowance_ms),
        },
    }
}

/// Retry policy for one model or tool call: how many times to retry a
/// retryable failure, and the backoff envelope between attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Maximum number of retries *after* the initial attempt. `0` means the
    /// call is tried exactly once and never retried (L-M4).
    pub max_retries: u32,
    /// Backoff floor in milliseconds — the delay before the first retry, and
    /// the floor a server `Retry-After` hint is raised to.
    pub base_delay_ms: u64,
    /// Backoff ceiling in milliseconds — the *computed* exponential delay is
    /// never slept past this. A server-supplied `Retry-After` hint is not
    /// bound by it; the hint has its own, much higher [`Self::max_server_hint_ms`]
    /// ceiling.
    pub max_delay_ms: u64,
    /// Ceiling in milliseconds on a server-supplied `Retry-After` hint the
    /// *inline* ladder will sleep — separate from, and far higher than,
    /// [`Self::max_delay_ms`]. Providers legitimately ask for 30–60s on a
    /// hard rate limit; clamping that to the computed-backoff cap fires the
    /// retry back inside the server's stated window and re-429s. A hint at
    /// or under this ceiling is honored inline; a hint past it escalates to
    /// the parked-wait path (#2677), where the caller's [`ParkSupervisor`]
    /// decides whether the remaining wall-clock budget affords honoring it —
    /// and only when it does not is the call failed fast (surfacing the hint
    /// value).
    pub max_server_hint_ms: u64,
}

/// Default ceiling for a server `Retry-After` hint slept inline. High
/// enough to obey the 30–60s waits providers hand out on hard rate limits
/// (well past the 8s computed-backoff cap), low enough that a multi-minute
/// hint takes the supervised parked-wait path — chunked, narrated, and
/// budget-bounded — instead of an unannounced inline sleep.
const DEFAULT_MAX_SERVER_HINT_MS: u64 = 120_000;

impl RetryPolicy {
    /// Build a policy from explicit values. The server-hint ceiling defaults
    /// to `DEFAULT_MAX_SERVER_HINT_MS` — separate from `max_delay_ms`, so a
    /// caller that passes a small computed-backoff cap still honors a
    /// realistic server `Retry-After` rather than clamping it down.
    pub fn new(max_retries: u32, base_delay_ms: u64, max_delay_ms: u64) -> Self {
        Self {
            max_retries,
            base_delay_ms,
            max_delay_ms,
            max_server_hint_ms: DEFAULT_MAX_SERVER_HINT_MS,
        }
    }

    /// The deterministic fast-path policy (L-M4): `max_retries = 0`. Triage
    /// and classification calls use this so a flaky provider call fails
    /// fast and falls through to the full path instead of adding retry
    /// latency to what's supposed to be the cheap path.
    pub fn deterministic() -> Self {
        Self {
            max_retries: 0,
            base_delay_ms: 0,
            max_delay_ms: 0,
            // Never reached — `max_retries == 0` returns on the first failure
            // before any delay is computed — but kept zero to match the
            // policy's all-zeros "never wait, never retry" character.
            max_server_hint_ms: 0,
        }
    }

    /// The default policy for ordinary provider calls: up to 6 retries,
    /// 250ms floor, 8s computed-backoff ceiling, 120s server-hint ceiling.
    ///
    /// Six retries, not three, because the envelope has to actually span the
    /// ceiling: delays run 250 → 500 → 1000 → 2000 → 4000 → 8000ms, so the
    /// last retry reaches `max_delay_ms` exactly and the whole ladder waits
    /// up to ~15.75s. At three retries the worst case was 1.75s of total
    /// backoff — the 8s cap was dead configuration — and any provider
    /// brownout longer than ~2s (an overloaded-burst 5xx window, a 429 with
    /// no `Retry-After` header; real ones last tens of seconds) exhausted
    /// every attempt and aborted the turn, losing a task the agent was
    /// actively solving to a transient blip. The ladder is still bounded and
    /// still budget-checked at the step boundary; a genuinely down provider
    /// costs ~16 extra seconds before the same clean abort.
    pub fn standard() -> Self {
        Self {
            max_retries: 6,
            base_delay_ms: 250,
            max_delay_ms: 8_000,
            max_server_hint_ms: DEFAULT_MAX_SERVER_HINT_MS,
        }
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::standard()
    }
}

/// One retry taken during a [`retry_with_backoff`] call: which attempt
/// failed, why, and how long the driver slept before trying again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryAttempt {
    /// 1-indexed number of the attempt that failed and triggered this
    /// retry (matches `stella_protocol::AgentEvent::Retry::attempt`).
    pub attempt: u32,
    /// The failed attempt's error, rendered via `Display` — this is the
    /// `reason` a caller passes straight into `AgentEvent::Retry`.
    pub reason: String,
    /// Milliseconds slept before the next attempt.
    pub delay_ms: u64,
}

/// The result of a [`retry_with_backoff`] call that ultimately committed —
/// `attempt_fn` returned `Ok`, whether on the first try or after some
/// retries.
#[derive(Debug, Clone)]
pub struct RetryOutcome<T> {
    /// The successful value returned by `attempt_fn`.
    pub value: T,
    /// Total number of calls made to `attempt_fn` (1 if it succeeded on the
    /// first try).
    pub attempts: u32,
    /// One entry per retry taken, in order. Empty if the call succeeded on
    /// its first attempt. The caller emits one `AgentEvent::Retry` per
    /// entry (see the module doc's L-E10 note).
    pub retries: Vec<RetryAttempt>,
}

/// Compute the jittered exponential backoff delay before the next retry,
/// given the attempt number that just failed (`0`-indexed: `0` means the
/// very first call failed and this is the delay before retry #1).
///
/// Shape: `base_delay_ms * 2^attempt`, capped at `max_delay_ms`. The result
/// is drawn uniformly from `[base_delay_ms, cap]` — "equal jitter" rather
/// than full jitter down to zero: a retry should still wait at least the
/// configured floor before hammering the provider again, and callers get a
/// hard lower bound to assert against. Because the draw is uniform across
/// that range, concurrent callers retrying the same failure land on
/// different points in it instead of all waking at the same instant
/// (thundering herd).
///
/// Pure and synchronous by design — the only
/// non-determinism is the injected `rng`, so bounds and shape are directly
/// assertable without sleeping.
pub fn compute_backoff_delay_ms(policy: &RetryPolicy, attempt: u32, rng: &mut impl Rng) -> u64 {
    let base = policy.base_delay_ms;
    let cap = policy.max_delay_ms.max(base);
    let exponential = base.saturating_mul(2u64.saturating_pow(attempt));
    let high = exponential.min(cap);

    if high <= base {
        // Degenerate range (attempt 0, or a misconfigured policy where the
        // cap doesn't exceed the floor): nothing to jitter, return the
        // floor rather than call `rng.random_range` on an empty range.
        return base;
    }
    rng.random_range(base..=high)
}

/// Turn a server `Retry-After` hint into the actual delay to sleep: raised to
/// `base_delay_ms` (a `Retry-After: 0` must still wait the configured floor,
/// never fire instantly), then nudged upward by a small jitter so a fleet of
/// workers that all received the same hint don't wake in lockstep and
/// re-collide on the next attempt.
///
/// The jitter is additive and proportional (up to an eighth of the floored
/// delay) — never a draw down toward `base_delay_ms` like
/// [`compute_backoff_delay_ms`], because the whole point of honoring a server
/// hint is to retry *no earlier* than it asked. So the result is always at or
/// above the floored hint. Pure and synchronous; the only non-determinism is
/// the injected `rng`.
fn server_hint_delay_ms(policy: &RetryPolicy, hint_ms: u64, rng: &mut impl Rng) -> u64 {
    let floored = hint_ms.max(policy.base_delay_ms);
    let jitter_span = floored / 8;
    if jitter_span == 0 {
        return floored;
    }
    floored.saturating_add(rng.random_range(0..=jitter_span))
}

/// Drive `attempt_fn` to completion, retrying retryable
/// (`ProviderError::is_retryable`) failures with jittered exponential
/// backoff up to `policy.max_retries`. A terminal error — or a retryable
/// one once retries are exhausted — returns immediately as `Err`; this
/// function never re-derives retry classification, it only ever asks
/// `is_retryable()`.
///
/// A park-eligible error's `retry_after_ms` hint
/// ([`ProviderError::retry_after_hint_ms`]), when present, is honored
/// (floored at `policy.base_delay_ms`, nudged by a small jitter, and bounded
/// by its own `policy.max_server_hint_ms` ceiling rather than the
/// computed-backoff cap) instead of the computed jittered delay — respecting
/// a server's stated backoff beats guessing at one. This wrapper runs with
/// [`NoParking`], so a rate limit or overload that outlives the ladder (or a
/// hint past the inline ceiling) fails the call fast, exactly as it did
/// before #2677; callers with wall-clock authority use
/// `retry_with_backoff_observed`'s [`ParkSupervisor`] to convert that abort
/// into a bounded parked wait.
///
/// On success, [`RetryOutcome::retries`] carries the full retry history so
/// the caller (`driver.rs`) can walk it and emit one
/// `stella_protocol::AgentEvent::Retry { attempt, reason }` per entry —
/// `retry.rs` has no event channel of its own, so this is returned as data,
/// never raised as a side effect. On failure, only the terminal
/// `ProviderError` comes back: per the module doc's L-E10 note, a call that
/// never commits speaks through its failure alone, not through the doomed
/// attempts leading up to it.
pub async fn retry_with_backoff<F, Fut, T>(
    policy: &RetryPolicy,
    sleeper: &dyn Sleeper,
    attempt_fn: F,
) -> Result<RetryOutcome<T>, ProviderError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ProviderError>>,
{
    retry_with_backoff_observed(policy, sleeper, attempt_fn, |_, _, _| {}, &mut NoParking).await
}

/// Retry while synchronously observing every failed dispatched attempt.
///
/// Accounting callers use this hook to emit a durable, content-free usage
/// incompleteness envelope before any later attempt can succeed. A successful
/// retry can report its own usage, but can never make an earlier provider
/// attempt's unknown usage knowable after the fact.
///
/// The observer receives the 1-indexed attempt number, the error, and THAT
/// attempt's own elapsed duration — never a cumulative figure across earlier
/// attempts or the backoff sleeps between them, so a consumer characterizing
/// provider failure latency from the durable envelopes sees per-call truth.
pub(crate) async fn retry_with_backoff_observed<F, Fut, T, O>(
    policy: &RetryPolicy,
    sleeper: &dyn Sleeper,
    mut attempt_fn: F,
    mut observe_failure: O,
    park: &mut dyn ParkSupervisor,
) -> Result<RetryOutcome<T>, ProviderError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ProviderError>>,
    O: FnMut(u32, &ProviderError, std::time::Duration),
{
    let mut retries = Vec::new();
    let mut attempt: u32 = 0;
    // Parked-wait accounting across the whole call: how much wall clock the
    // parks have spent (fed back to the supervisor's allowance) and how many
    // parks have been taken (drives the hint-less park backoff).
    let mut parked_total_ms: u64 = 0;
    let mut park_streak: u32 = 0;
    loop {
        let attempt_started = std::time::Instant::now();
        match attempt_fn().await {
            Ok(value) => {
                return Ok(RetryOutcome {
                    value,
                    attempts: attempt + 1,
                    retries,
                });
            }
            Err(error) => {
                observe_failure(attempt + 1, &error, attempt_started.elapsed());
                if !error.is_retryable() {
                    return Err(error);
                }
                // Park eligibility is the adapter's classification, asked
                // for and never re-derived (L-M7): `Some(hint)` means this
                // failure's recovery is a function of waiting — a 429 or a
                // 529 brownout — and `hint` is the server's stated window
                // when it sent one.
                let park_hint = error
                    .is_park_eligible()
                    .then(|| error.retry_after_hint_ms());

                // The inline ladder — today's bounded backoff — handles
                // every retryable failure while attempts remain, except a
                // server hint past the inline ceiling (never sleepable
                // inline). Anything the ladder cannot absorb either parks
                // (a park-eligible failure with wall-clock headroom, below)
                // or surfaces: a dropped connection earns no park, because
                // waiting minutes makes it no likelier to reconnect, while
                // a throttle or an overloaded fleet is exactly what waiting
                // is for (#2677, #2742).
                let ladder_open = attempt < policy.max_retries;
                let inline_delay_ms = match park_hint {
                    None if ladder_open => {
                        Some(compute_backoff_delay_ms(policy, attempt, &mut rand::rng()))
                    }
                    None => return Err(error),
                    Some(hint)
                        if ladder_open && hint.is_none_or(|h| h <= policy.max_server_hint_ms) =>
                    {
                        Some(match hint {
                            Some(h) => server_hint_delay_ms(policy, h, &mut rand::rng()),
                            None => compute_backoff_delay_ms(policy, attempt, &mut rand::rng()),
                        })
                    }
                    Some(_) => None,
                };

                let delay_ms = match inline_delay_ms {
                    Some(delay_ms) => {
                        sleeper.sleep(delay_ms).await;
                        delay_ms
                    }
                    // The park path (#2677, #2742): a park-eligible failure
                    // the inline ladder could not absorb, converted into a
                    // supervised wait when the caller's wall-clock budget
                    // affords one.
                    None => {
                        // L-M4 holds for every park-eligible class: the
                        // deterministic fast path never waits, never parks.
                        if policy.max_retries == 0 {
                            return Err(error);
                        }
                        let hint_ms = park_hint.flatten();
                        let hint_delay_ms =
                            hint_ms.map(|h| server_hint_delay_ms(policy, h, &mut rand::rng()));
                        let allowance_ms = park.wait_allowance_ms(parked_total_ms);
                        let total_ms = match plan_park(hint_delay_ms, park_streak, allowance_ms) {
                            ParkPlan::Wait { total_ms } => total_ms,
                            // An unaffordable stated window fails fast —
                            // waiting less than the server asked
                            // guarantees the same refusal, so the wait
                            // would spend the budget and buy nothing. The
                            // provider's own message rides along: the
                            // hint explains the wait, the message
                            // explains the limit, and dropping either
                            // loses half the diagnosis.
                            ParkPlan::GiveUp => {
                                return Err(match hint_ms {
                                    Some(hint) if hint > policy.max_server_hint_ms => {
                                        ProviderError::Terminal(format!(
                                            "{error}; the server asked to wait {hint}ms \
                                                 before retrying, past this call's {ceiling}ms \
                                                 inline ceiling and beyond the {allowance_ms}ms \
                                                 of wall-clock headroom left for a parked wait — \
                                                 failing fast instead of waiting past the budget",
                                            ceiling = policy.max_server_hint_ms,
                                        ))
                                    }
                                    // Exhaustion the headroom cannot
                                    // absorb surfaces the provider's own
                                    // error, exactly as pre-park
                                    // exhaustion did — the error class
                                    // (and RetriesExhausted::retryable
                                    // downstream) stays truthful.
                                    _ => error,
                                });
                            }
                        };
                        park.park_opened(total_ms, PARK_CHUNK_MS, &error.to_string());
                        let mut waited_ms: u64 = 0;
                        let mut chunks: u64 = 0;
                        let aborted = loop {
                            if waited_ms >= total_ms {
                                break false;
                            }
                            if park.park_tick(waited_ms, total_ms) == ParkDirective::Abort {
                                break true;
                            }
                            let chunk = PARK_CHUNK_MS.min(total_ms - waited_ms);
                            sleeper.sleep(chunk).await;
                            waited_ms += chunk;
                            chunks += 1;
                        };
                        park.park_closed(waited_ms, chunks, aborted);
                        parked_total_ms = parked_total_ms.saturating_add(waited_ms);
                        if aborted {
                            return Err(error);
                        }
                        park_streak = park_streak.saturating_add(1);
                        waited_ms
                    }
                };

                retries.push(RetryAttempt {
                    attempt: attempt + 1,
                    reason: error.to_string(),
                    delay_ms,
                });
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    use rand::SeedableRng;
    use rand::rngs::StdRng;

    use super::*;

    /// A [`Sleeper`] that never actually waits — it just records every
    /// requested delay so async-loop tests can assert on retry timing
    /// without paying real wall-clock cost or flaking under load.
    #[derive(Default)]
    struct NoopSleeper {
        delays_ms: Mutex<Vec<u64>>,
    }

    #[async_trait]
    impl Sleeper for NoopSleeper {
        async fn sleep(&self, duration_ms: u64) {
            self.delays_ms
                .lock()
                .expect("mutex poisoned")
                .push(duration_ms);
        }
    }

    // ---- RetryPolicy ----------------------------------------------------

    #[test]
    fn deterministic_policy_never_retries() {
        assert_eq!(RetryPolicy::deterministic().max_retries, 0);
    }

    #[test]
    fn default_policy_is_standard() {
        assert_eq!(RetryPolicy::default(), RetryPolicy::standard());
    }

    #[test]
    fn standard_policy_backoff_envelope_actually_reaches_its_ceiling() {
        // The witness for the dead-cap defect: with max_retries: 3 the
        // exponential ladder topped out at 1000ms and the configured 8s
        // max_delay_ms was unreachable — config and behavior disagreed, and
        // any provider brownout longer than ~2s exhausted the ladder. The
        // last computed delay (attempt index max_retries - 1) must reach the
        // ceiling exactly, so every configured millisecond of backoff is
        // live.
        let policy = RetryPolicy::standard();
        let last_attempt_index = policy.max_retries - 1;
        let envelope = policy
            .base_delay_ms
            .saturating_mul(2u64.saturating_pow(last_attempt_index))
            .min(policy.max_delay_ms);
        assert_eq!(
            envelope, policy.max_delay_ms,
            "standard()'s ladder must span its own max_delay_ms"
        );
    }

    // ---- compute_backoff_delay_ms (pure, no sleeping) --------------------

    #[test]
    fn first_retry_delay_equals_the_base_when_no_jitter_room() {
        // attempt 0: base * 2^0 == base, so the jitter range is degenerate
        // and the result is deterministic.
        let policy = RetryPolicy::new(5, 250, 8_000);
        let mut rng = StdRng::seed_from_u64(1);
        assert_eq!(compute_backoff_delay_ms(&policy, 0, &mut rng), 250);
    }

    #[test]
    fn delay_stays_within_base_and_cap_bounds_across_many_attempts() {
        let policy = RetryPolicy::new(10, 100, 5_000);
        let mut rng = StdRng::seed_from_u64(7);
        for attempt in 0..30 {
            let delay = compute_backoff_delay_ms(&policy, attempt, &mut rng);
            assert!(
                (policy.base_delay_ms..=policy.max_delay_ms).contains(&delay),
                "attempt {attempt}: delay {delay} out of [{}, {}]",
                policy.base_delay_ms,
                policy.max_delay_ms
            );
        }
    }

    #[test]
    fn delay_grows_with_attempt_number_up_to_the_cap() {
        let policy = RetryPolicy::new(10, 50, 4_000);
        let mut rng = StdRng::seed_from_u64(42);
        // Use the upper bound of what's achievable at each attempt (the
        // exponential envelope) rather than one jittered sample, since a
        // single draw can dip low even as the ceiling climbs.
        let envelope = |attempt: u32| -> u64 {
            (policy
                .base_delay_ms
                .saturating_mul(2u64.saturating_pow(attempt)))
            .min(policy.max_delay_ms)
        };
        let mut previous = envelope(0);
        for attempt in 1..8 {
            let current = envelope(attempt);
            assert!(
                current >= previous,
                "envelope must be non-decreasing: attempt {attempt} gave {current} < {previous}"
            );
            previous = current;
        }
        // And the ceiling is actually reached for a large attempt number.
        assert_eq!(envelope(20), policy.max_delay_ms);
        // Sanity: real samples at a late attempt never exceed the cap.
        for _ in 0..20 {
            assert!(compute_backoff_delay_ms(&policy, 20, &mut rng) <= policy.max_delay_ms);
        }
    }

    #[test]
    fn delay_varies_across_calls_at_the_same_attempt() {
        // The jitter must be real, not a constant disguised as random: a
        // wide [base, cap] range sampled many times at a fixed attempt
        // should not collapse to a single value.
        let policy = RetryPolicy::new(10, 100, 10_000);
        let mut rng = StdRng::seed_from_u64(99);
        let samples: Vec<u64> = (0..50)
            .map(|_| compute_backoff_delay_ms(&policy, 5, &mut rng))
            .collect();
        let first = samples[0];
        assert!(
            samples.iter().any(|&d| d != first),
            "expected variance in jittered delay, got {samples:?}"
        );
    }

    #[test]
    fn zero_policy_never_panics_and_returns_zero() {
        let policy = RetryPolicy::deterministic();
        let mut rng = StdRng::seed_from_u64(3);
        assert_eq!(compute_backoff_delay_ms(&policy, 0, &mut rng), 0);
        assert_eq!(compute_backoff_delay_ms(&policy, 7, &mut rng), 0);
    }

    #[test]
    fn misconfigured_cap_below_base_does_not_panic() {
        // cap < base is a misconfiguration, but must degrade safely rather
        // than panic the sampler on an empty range.
        let policy = RetryPolicy::new(3, 5_000, 100);
        let mut rng = StdRng::seed_from_u64(11);
        let delay = compute_backoff_delay_ms(&policy, 4, &mut rng);
        assert_eq!(delay, policy.base_delay_ms);
    }

    proptest::proptest! {
        #[test]
        fn backoff_delay_always_within_base_and_cap(
            base in 0u64..2_000,
            extra in 0u64..10_000,
            attempt in 0u32..40,
        ) {
            let policy = RetryPolicy::new(10, base, base + extra);
            let mut rng = StdRng::seed_from_u64(u64::from(attempt) ^ base ^ extra);
            let delay = compute_backoff_delay_ms(&policy, attempt, &mut rng);
            proptest::prop_assert!(delay >= policy.base_delay_ms);
            proptest::prop_assert!(delay <= policy.max_delay_ms);
        }
    }

    // ---- retry_with_backoff (async loop, NoopSleeper — no real waiting) -

    #[tokio::test]
    async fn max_retries_zero_never_retries_even_on_a_retryable_error() {
        let policy = RetryPolicy::deterministic();
        let sleeper = NoopSleeper::default();
        let calls = AtomicU32::new(0);

        let result = retry_with_backoff(&policy, &sleeper, || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err::<(), _>(ProviderError::transport("timeout")) }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "must call attempt_fn exactly once"
        );
        assert!(
            sleeper.delays_ms.lock().unwrap().is_empty(),
            "must never sleep when max_retries == 0"
        );
    }

    #[tokio::test]
    async fn terminal_errors_never_retry_regardless_of_policy() {
        let policy = RetryPolicy::new(5, 1, 5);
        let sleeper = NoopSleeper::default();
        let calls = AtomicU32::new(0);

        let result = retry_with_backoff(&policy, &sleeper, || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err::<(), _>(ProviderError::Auth("bad key".into())) }
        })
        .await;

        assert!(matches!(result, Err(ProviderError::Auth(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(sleeper.delays_ms.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn retryable_errors_retry_up_to_the_cap_then_surface_the_last_error() {
        let policy = RetryPolicy::new(3, 1, 5);
        let sleeper = NoopSleeper::default();
        let calls = AtomicU32::new(0);

        let result = retry_with_backoff(&policy, &sleeper, || {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            async move { Err::<(), _>(ProviderError::transport(format!("attempt {n} failed"))) }
        })
        .await;

        // 1 initial attempt + 3 retries == 4 total calls.
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        match result {
            Err(ProviderError::Transport { message, .. }) => {
                assert!(message.contains("attempt 3"))
            }
            other => panic!("expected the LAST transport error, got {other:?}"),
        }
        assert_eq!(
            sleeper.delays_ms.lock().unwrap().len(),
            3,
            "one sleep per retry"
        );
    }

    #[tokio::test]
    async fn succeeds_after_retries_and_reports_full_history() {
        let policy = RetryPolicy::new(5, 1, 5);
        let sleeper = NoopSleeper::default();
        let calls = AtomicU32::new(0);

        let outcome = retry_with_backoff(&policy, &sleeper, || {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if n < 2 {
                    Err(ProviderError::transport(format!("flaky {n}")))
                } else {
                    Ok(42)
                }
            }
        })
        .await
        .expect("should eventually succeed");

        assert_eq!(outcome.value, 42);
        assert_eq!(outcome.attempts, 3);
        assert_eq!(outcome.retries.len(), 2);
        assert_eq!(outcome.retries[0].attempt, 1);
        assert!(outcome.retries[0].reason.contains("flaky 0"));
        assert_eq!(outcome.retries[1].attempt, 2);
        assert!(outcome.retries[1].reason.contains("flaky 1"));
    }

    #[tokio::test]
    async fn succeeds_on_the_first_try_reports_no_retries() {
        let policy = RetryPolicy::standard();
        let sleeper = NoopSleeper::default();

        let outcome =
            retry_with_backoff(&policy, &sleeper, || async { Ok::<_, ProviderError>("ok") })
                .await
                .expect("first try succeeds");

        assert_eq!(outcome.attempts, 1);
        assert!(outcome.retries.is_empty());
        assert!(sleeper.delays_ms.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn server_retry_after_hint_is_honored_above_the_backoff_cap() {
        // The regression (issue #361): a 30s server hint must NOT be clamped
        // to the 8s computed-backoff cap — clamping fires the retry back
        // inside the server's stated window and re-429s.
        let policy = RetryPolicy::new(2, 250, 8_000);
        let sleeper = NoopSleeper::default();
        let calls = AtomicU32::new(0);

        let _ = retry_with_backoff(&policy, &sleeper, || {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if n == 0 {
                    Err(ProviderError::RateLimited {
                        message: "slow down".into(),
                        retry_after_ms: Some(30_000),
                    })
                } else {
                    Ok(())
                }
            }
        })
        .await;

        let delays = sleeper.delays_ms.lock().unwrap();
        assert_eq!(delays.len(), 1);
        assert!(
            delays[0] >= 30_000,
            "a 30s hint must be honored, not clamped to the 8s cap: {}",
            delays[0]
        );
        // Honored, plus at most the small proportional jitter — never the
        // whole 8s-cap-ignoring exponential envelope or anything unbounded.
        assert!(
            delays[0] <= 30_000 + 30_000 / 8,
            "delay must be the hint plus a small jitter, got {}",
            delays[0]
        );
    }

    #[tokio::test]
    async fn zero_retry_after_hint_floors_at_base_delay() {
        // A `Retry-After: 0` must still wait the configured floor, never fire
        // instantly and hammer the provider.
        let policy = RetryPolicy::new(2, 250, 8_000);
        let sleeper = NoopSleeper::default();
        let calls = AtomicU32::new(0);

        let _ = retry_with_backoff(&policy, &sleeper, || {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if n == 0 {
                    Err(ProviderError::RateLimited {
                        message: "slow down".into(),
                        retry_after_ms: Some(0),
                    })
                } else {
                    Ok(())
                }
            }
        })
        .await;

        let delays = sleeper.delays_ms.lock().unwrap();
        assert_eq!(delays.len(), 1);
        assert!(
            delays[0] >= policy.base_delay_ms,
            "a 0 hint must floor at base_delay_ms, got {}",
            delays[0]
        );
        assert!(
            delays[0] <= policy.base_delay_ms + policy.base_delay_ms / 8,
            "floored hint must not balloon past floor + small jitter, got {}",
            delays[0]
        );
    }

    #[tokio::test]
    async fn server_hint_above_the_ceiling_fails_fast_with_the_hint_value() {
        // A runaway hint past the server-hint ceiling must fail the call fast
        // — never sleep for it, never retry back early — and name the value.
        let policy = RetryPolicy::new(3, 250, 8_000); // 120s hint ceiling
        let sleeper = NoopSleeper::default();
        let calls = AtomicU32::new(0);

        let result = retry_with_backoff(&policy, &sleeper, || {
            calls.fetch_add(1, Ordering::SeqCst);
            async {
                Err::<(), _>(ProviderError::RateLimited {
                    message: "slow down".into(),
                    retry_after_ms: Some(200_000), // past the 120s ceiling
                })
            }
        })
        .await;

        match result {
            Err(ProviderError::Terminal(msg)) => {
                assert!(
                    msg.contains("200000"),
                    "error must name the hint value: {msg}"
                );
                assert!(msg.contains("120000"), "error must name the ceiling: {msg}");
            }
            other => panic!("expected a fast Terminal failure, got {other:?}"),
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "must fail fast on the first attempt, not retry"
        );
        assert!(
            sleeper.delays_ms.lock().unwrap().is_empty(),
            "must never sleep on a hint past the ceiling"
        );
    }

    // ---- parked rate-limit recovery (#2677) ------------------------------

    /// A scripted [`ParkSupervisor`]: fixed allowance, full transcript of
    /// hook calls, optional abort on the Nth tick.
    #[derive(Default)]
    struct RecordingSupervisor {
        allowance_ms: u64,
        opened: Vec<(u64, u64)>,
        ticks: Vec<(u64, u64)>,
        closed: Vec<(u64, u64, bool)>,
        abort_on_tick: Option<usize>,
    }

    impl RecordingSupervisor {
        fn with_allowance(allowance_ms: u64) -> Self {
            Self {
                allowance_ms,
                ..Self::default()
            }
        }
    }

    impl ParkSupervisor for RecordingSupervisor {
        fn wait_allowance_ms(&mut self, waited_ms: u64) -> u64 {
            self.allowance_ms.saturating_sub(waited_ms)
        }
        fn park_opened(&mut self, planned_ms: u64, chunk_ms: u64, _reason: &str) {
            self.opened.push((planned_ms, chunk_ms));
        }
        fn park_tick(&mut self, waited_ms: u64, planned_ms: u64) -> ParkDirective {
            self.ticks.push((waited_ms, planned_ms));
            match self.abort_on_tick {
                Some(n) if self.ticks.len() >= n => ParkDirective::Abort,
                _ => ParkDirective::Continue,
            }
        }
        fn park_closed(&mut self, waited_ms: u64, chunks: u64, aborted: bool) {
            self.closed.push((waited_ms, chunks, aborted));
        }
    }

    #[test]
    fn plan_honors_a_hint_that_fits_and_gives_up_on_one_that_does_not() {
        // A stated window inside the allowance is honored (raised to the
        // streak backoff, which is always safe); one outside it gives up.
        assert_eq!(
            plan_park(Some(90_000), 0, 3_600_000),
            ParkPlan::Wait { total_ms: 90_000 }
        );
        assert_eq!(plan_park(Some(90_000), 0, 60_000), ParkPlan::GiveUp);
        // Hint-less waits follow the doubling backoff, clamped to the cap…
        assert_eq!(
            plan_park(None, 0, 3_600_000),
            ParkPlan::Wait { total_ms: 30_000 }
        );
        assert_eq!(
            plan_park(None, 10, 3_600_000),
            ParkPlan::Wait {
                total_ms: PARK_BACKOFF_CAP_MS
            }
        );
        // …and to the allowance, so the last affordable wait still happens.
        assert_eq!(
            plan_park(None, 3, 45_000),
            ParkPlan::Wait { total_ms: 45_000 }
        );
        assert_eq!(plan_park(None, 0, 0), ParkPlan::GiveUp);
    }

    proptest::proptest! {
        #[test]
        fn a_planned_park_never_exceeds_the_allowance_and_never_undercuts_the_hint(
            hint in proptest::option::of(0u64..1_000_000),
            streak in 0u32..20,
            allowance in 0u64..10_000_000,
        ) {
            match plan_park(hint, streak, allowance) {
                ParkPlan::Wait { total_ms } => {
                    proptest::prop_assert!(total_ms <= allowance);
                    proptest::prop_assert!(total_ms > 0);
                    if let Some(hint) = hint {
                        // Waiting less than the server asked guarantees a
                        // re-429 — a plan may wait longer, never shorter.
                        proptest::prop_assert!(total_ms >= hint);
                    }
                }
                ParkPlan::GiveUp => {
                    let unaffordable_hint = hint.is_some_and(|h| h > allowance);
                    proptest::prop_assert!(allowance == 0 || unaffordable_hint);
                }
            }
        }
    }

    /// The pure-loop witness for #2667: rate limiting that outlives the
    /// inline ladder parks and recovers instead of exhausting — the attempt
    /// count stops being the bound; the supervisor's wall clock is.
    #[tokio::test]
    async fn sustained_rate_limiting_beyond_the_ladder_parks_and_recovers() {
        let policy = RetryPolicy::new(2, 1, 5);
        let sleeper = NoopSleeper::default();
        let mut park = RecordingSupervisor::with_allowance(3_600_000);
        let calls = AtomicU32::new(0);

        let outcome = retry_with_backoff_observed(
            &policy,
            &sleeper,
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n < 5 {
                        Err(ProviderError::RateLimited {
                            message: "throttled".into(),
                            retry_after_ms: None,
                        })
                    } else {
                        Ok("recovered")
                    }
                }
            },
            |_, _, _| {},
            &mut park,
        )
        .await
        .expect("a brownout inside the allowance must recover");

        assert_eq!(outcome.value, "recovered");
        // 2 inline retries, then 3 parks (30s, 60s, 120s), each recorded in
        // the committed retry history like any other wait.
        assert_eq!(outcome.attempts, 6);
        assert_eq!(outcome.retries.len(), 5);
        assert_eq!(
            park.opened
                .iter()
                .map(|(planned, _)| *planned)
                .collect::<Vec<_>>(),
            vec![30_000, 60_000, 120_000],
            "hint-less parks follow the doubling backoff"
        );
        assert_eq!(
            park.closed,
            vec![(30_000, 1, false), (60_000, 2, false), (120_000, 4, false)],
            "every park runs to its planned length in 30s chunks"
        );
    }

    /// Long waits sleep in chunks with a tick before each one — the
    /// keep-alive/soft-stop cadence — and an Abort tick ends the park and
    /// surfaces the rate-limit error.
    #[tokio::test]
    async fn an_abort_tick_ends_the_park_and_surfaces_the_error() {
        let policy = RetryPolicy::new(0, 1, 5);
        // max_retries 0 would decline the park (L-M4), so use 1 with the
        // ladder pre-burned by a first failure.
        let policy = RetryPolicy {
            max_retries: 1,
            ..policy
        };
        let sleeper = NoopSleeper::default();
        let mut park = RecordingSupervisor::with_allowance(3_600_000);
        park.abort_on_tick = Some(3);
        let calls = AtomicU32::new(0);

        let result: Result<RetryOutcome<()>, _> = retry_with_backoff_observed(
            &policy,
            &sleeper,
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async {
                    Err(ProviderError::RateLimited {
                        message: "hard limit".into(),
                        retry_after_ms: Some(300_000),
                    })
                }
            },
            |_, _, _| {},
            &mut park,
        )
        .await;

        assert!(
            matches!(result, Err(ProviderError::RateLimited { .. })),
            "an aborted park surfaces the rate limit itself: {result:?}"
        );
        let (waited, chunks, aborted) = *park.closed.last().expect("park must close");
        assert!(aborted, "the close must record the abort");
        assert_eq!(chunks, 2, "two chunks slept before the aborting third tick");
        assert_eq!(waited, 60_000);
        let slept = sleeper.delays_ms.lock().unwrap();
        assert!(
            slept.iter().all(|&d| d <= PARK_CHUNK_MS),
            "no single park sleep may exceed the chunk: {slept:?}"
        );
    }

    /// L-M4 survives #2677: the deterministic fast path never parks, no
    /// matter how generous the supervisor's allowance is.
    #[tokio::test]
    async fn the_deterministic_policy_never_parks_even_with_allowance() {
        let policy = RetryPolicy::deterministic();
        let sleeper = NoopSleeper::default();
        let mut park = RecordingSupervisor::with_allowance(3_600_000);
        let calls = AtomicU32::new(0);

        let result: Result<RetryOutcome<()>, _> = retry_with_backoff_observed(
            &policy,
            &sleeper,
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async {
                    Err(ProviderError::RateLimited {
                        message: "throttled".into(),
                        retry_after_ms: None,
                    })
                }
            },
            |_, _, _| {},
            &mut park,
        )
        .await;

        assert!(matches!(result, Err(ProviderError::RateLimited { .. })));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(park.opened.is_empty(), "no park may open on the fast path");
        assert!(sleeper.delays_ms.lock().unwrap().is_empty());
    }

    /// The pure-loop witness for #2742: a 529 overload brownout that outlives
    /// the inline ladder parks and recovers, exactly as a 429 does. Before
    /// the split it arrived as `Transport`, which the park path declines —
    /// the ladder exhausted at attempt 3 and the call died with budget left.
    #[tokio::test]
    async fn sustained_overload_beyond_the_ladder_parks_and_recovers() {
        let policy = RetryPolicy::new(2, 1, 5);
        let sleeper = NoopSleeper::default();
        let mut park = RecordingSupervisor::with_allowance(3_600_000);
        let calls = AtomicU32::new(0);

        let outcome = retry_with_backoff_observed(
            &policy,
            &sleeper,
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n < 5 {
                        Err(ProviderError::Overloaded {
                            message: "anthropic HTTP 529 Overloaded: overloaded".into(),
                            retry_after_ms: None,
                        })
                    } else {
                        Ok("recovered")
                    }
                }
            },
            |_, _, _| {},
            &mut park,
        )
        .await
        .expect("a 529 brownout inside the allowance must recover");

        assert_eq!(outcome.value, "recovered");
        assert_eq!(outcome.attempts, 6);
        assert_eq!(
            park.opened
                .iter()
                .map(|(planned, _)| *planned)
                .collect::<Vec<_>>(),
            vec![30_000, 60_000, 120_000],
            "a hint-less overload reuses the 30s->5min park schedule, unchanged"
        );
    }

    /// The other half of the split, pinned so a later edit cannot widen the
    /// park to every retryable class: a transport fault exhausts the ladder
    /// and surfaces, never parking, because waiting does not un-reset a
    /// connection.
    #[tokio::test]
    async fn a_transport_fault_still_exhausts_the_ladder_without_parking() {
        let policy = RetryPolicy::new(2, 1, 5);
        let sleeper = NoopSleeper::default();
        let mut park = RecordingSupervisor::with_allowance(3_600_000);
        let calls = AtomicU32::new(0);

        let result: Result<RetryOutcome<()>, _> = retry_with_backoff_observed(
            &policy,
            &sleeper,
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err(ProviderError::transport("connection reset")) }
            },
            |_, _, _| {},
            &mut park,
        )
        .await;

        assert!(matches!(result, Err(ProviderError::Transport { .. })));
        assert_eq!(calls.load(Ordering::SeqCst), 3, "one call plus two retries");
        assert!(
            park.opened.is_empty(),
            "a transport fault must never open a park"
        );
    }

    /// L-M4 under #2742: the deterministic fast path never parks on an
    /// overload either, no matter how generous the allowance. This is the
    /// constraint most likely to be broken silently by widening the park
    /// predicate, so it is pinned per class.
    #[tokio::test]
    async fn the_deterministic_policy_never_parks_on_an_overload() {
        let policy = RetryPolicy::deterministic();
        let sleeper = NoopSleeper::default();
        let mut park = RecordingSupervisor::with_allowance(3_600_000);
        let calls = AtomicU32::new(0);

        let result: Result<RetryOutcome<()>, _> = retry_with_backoff_observed(
            &policy,
            &sleeper,
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async {
                    Err(ProviderError::Overloaded {
                        message: "anthropic HTTP 529 Overloaded: overloaded".into(),
                        // A stated wait must not tempt the fast path either.
                        retry_after_ms: Some(300_000),
                    })
                }
            },
            |_, _, _| {},
            &mut park,
        )
        .await;

        assert!(
            matches!(result, Err(ProviderError::Overloaded { .. })),
            "{result:?}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "tried exactly once");
        assert!(park.opened.is_empty(), "no park may open on the fast path");
        assert!(sleeper.delays_ms.lock().unwrap().is_empty());
    }

    #[test]
    fn server_hint_delay_floors_and_jitters_within_an_expected_band() {
        // Floor + additive proportional jitter: every sample lands in
        // [floored, floored + floored/8], and the draw actually varies so a
        // fleet decorrelates instead of waking in lockstep.
        let policy = RetryPolicy::new(3, 250, 8_000);
        let mut rng = StdRng::seed_from_u64(2024);
        let hint = 30_000u64;
        let floor = hint; // hint already above base_delay_ms
        let ceil = floor + floor / 8;
        let samples: Vec<u64> = (0..64)
            .map(|_| server_hint_delay_ms(&policy, hint, &mut rng))
            .collect();
        for &d in &samples {
            assert!(
                (floor..=ceil).contains(&d),
                "sample {d} out of band [{floor}, {ceil}]"
            );
        }
        let first = samples[0];
        assert!(
            samples.iter().any(|&d| d != first),
            "expected jitter variance across samples, got {samples:?}"
        );
    }
}
