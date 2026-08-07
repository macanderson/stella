//! Token estimation for compaction decisions. The estimator returns a
//! deliberately conservative (over-)estimate — over-estimating triggers
//! compaction earlier, the safe direction. Provider-reported usage flows
//! through `AgentEvent::StepUsage` for telemetry AND back into the
//! estimator: [`Calibration`] tracks the observed actual/estimated
//! input-token ratio per model and corrects the
//! heuristic with a bounded factor, so the estimate converges on what the
//! provider's tokenizer actually reports over a session. The uncalibrated
//! functions remain the shared byte-heuristic baseline
//! ([`stella_protocol::tokens`] — the one place the rule is written down, so
//! this module and the receipts plane cannot disagree on what a token is made
//! of the way they did before #925); the correction is applied on top, never
//! by mutating the heuristic's constants.

use std::collections::HashMap;
use std::sync::Mutex;

use stella_protocol::CompletionMessage;
use stella_protocol::tokens::estimate_tokens_for_bytes;

/// Fixed per-message framing overhead (role tags, separators) in tokens.
const PER_MESSAGE_OVERHEAD: u64 = 4;

/// A [`std::io::Write`] sink that counts the bytes written and discards them.
/// Exists so a JSON value's serialized length can be measured without
/// materializing it.
#[derive(Default)]
struct CountingWriter(usize);

impl std::io::Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Serialized byte length of a JSON value — what `value.to_string().len()`
/// returns, without the throwaway String.
///
/// Byte-identical by construction: `serde_json::to_writer` and `Value`'s
/// `Display` both drive the same compact serializer, so every estimate,
/// calibration ratio, and receipt number this feeds is unchanged. It matters
/// because `estimate_conversation_tokens` runs several times per step over the
/// whole transcript, and a transcript with hundreds of recorded tool calls
/// re-serialized hundreds of KB of JSON into the allocator on every pass.
fn json_len(value: &serde_json::Value) -> usize {
    let mut counter = CountingWriter::default();
    // Infallible in practice: a `Value` always serializes, and this sink never
    // returns an error. A partial count is still the right answer if it did.
    let _ = serde_json::to_writer(&mut counter, value);
    counter.0
}

/// Estimate the token cost of one message, including any tool calls, tool
/// results, and multimodal attachments it carries.
///
/// Sums every piece's UTF-8 byte length and divides **once**, rather than
/// estimating each piece: `ceil` is not additive, so a per-piece sum would
/// grow with how many tool calls a message happens to carry rather than with
/// what it contains.
pub fn estimate_message_tokens(message: &CompletionMessage) -> u64 {
    // Bytes throughout — `str::len` and `json_len` are both byte lengths, and
    // the variable is named for what it holds so a later reader cannot mistake
    // this for a character count the way #925's second estimator did.
    let mut bytes = message.content.len();
    for call in &message.tool_calls {
        bytes += call.name.len();
        bytes += json_len(&call.input);
    }
    for result in &message.tool_results {
        bytes += result.call_id.len();
        bytes += match &result.output {
            stella_protocol::ToolOutput::Ok { content } => content.len(),
            stella_protocol::ToolOutput::Error { message } => message.len(),
        };
    }
    let bytes = bytes as u64;
    let attachment_tokens: u64 = message
        .attachments
        .iter()
        .map(estimate_attachment_tokens)
        .sum();
    estimate_tokens_for_bytes(bytes) + attachment_tokens + PER_MESSAGE_OVERHEAD
}

/// Rough token cost of an attachment payload. Providers price media by their
/// own units (image tiles, PDF pages, audio seconds), but the request itself
/// carries the payload as base64 — `bytes × 4/3 ÷ BYTES_PER_TOKEN` tracks
/// the request-size pressure that budgeting and compaction care about, and
/// deliberately overestimates in the safe direction for media whose billed
/// token cost is lower.
fn estimate_attachment_tokens(attachment: &stella_protocol::Attachment) -> u64 {
    // base64 is ASCII by definition, so this is the one place where "bytes"
    // and "characters" were never in tension.
    let base64_bytes = attachment.byte_len.saturating_mul(4) / 3;
    estimate_tokens_for_bytes(base64_bytes)
}

// Counts whole-transcript estimate walks so a test can pin how many the step
// loop performs, rather than trusting that a refactor did not quietly add one
// back. This is the counter for the thing the cost actually scales with: each
// walk is Θ(transcript), so on a long turn the difference between two per step
// and four is the difference between two and four full re-reads of the history
// for every model call. Test-only; thread-local so it measures one turn's own
// work (see `stella_context`'s `cost_counters` module for the same reasoning).
#[cfg(test)]
thread_local! {
    static CONVERSATION_WALKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Zero this thread's whole-transcript-walk counter and return the previous
/// value.
#[cfg(test)]
pub(crate) fn take_conversation_walks() -> usize {
    CONVERSATION_WALKS.with(|c| c.replace(0))
}

/// Estimate the total token cost of a conversation.
///
/// Θ(transcript) — it re-reads every message's content and every tool call's
/// serialized input. Callers on the step path should thread the result rather
/// than recompute it.
pub fn estimate_conversation_tokens(messages: &[CompletionMessage]) -> u64 {
    #[cfg(test)]
    CONVERSATION_WALKS.with(|c| c.set(c.get() + 1));
    messages.iter().map(estimate_message_tokens).sum()
}

/// The attachment share of [`estimate_conversation_tokens`] — what to
/// subtract to get the calibration-facing text estimate.
///
/// The attachment weight is a deliberate ~80× over-estimate of what
/// providers actually bill for media (base64 request pressure vs. image
/// tiles), which is the safe direction for the compaction decision but
/// poison as a drift sample: one screenshot-bearing step's ratio clamps to
/// the sample floor, drags the factor toward `CALIBRATION_MIN_FACTOR`, and
/// the inflated effective budget then lets a text-dominated transcript grow
/// past the provider's real context window before compaction fires. So the
/// pressure decision keeps the full estimate and calibration records the
/// estimate minus this. Cheap — it touches only attachment metadata, never
/// the transcript's text.
pub fn estimate_conversation_attachment_tokens(messages: &[CompletionMessage]) -> u64 {
    messages
        .iter()
        .flat_map(|m| m.attachments.iter())
        .map(estimate_attachment_tokens)
        .sum()
}

/// EWMA smoothing weight for new drift samples. 0.3 converges within a
/// handful of steps (visible improvement inside one session) while a single
/// noisy sample moves the state by at most 30%.
const CALIBRATION_ALPHA: f64 = 0.3;

/// Samples required before a correction is applied at all — below this the
/// factor is exactly 1.0, so one or two flukes can never steer budgeting.
const CALIBRATION_MIN_SAMPLES: u32 = 3;

/// Bounds on the applied correction factor. The lower bound matters most:
/// the raw estimator is deliberately biased HIGH (see
/// [`stella_protocol::tokens::BYTES_PER_TOKEN`]),
/// and a factor below 1.0 spends that safety margin — capping it at 0.5
/// means calibration can at most halve the conservative estimate, so the
/// calibrated number stays an early-compaction bias, never an invitation to
/// silent provider truncation. The upper bound keeps a run of pathological
/// samples (a tokenizer bug, a provider mis-reporting usage) from doubling
/// every estimate and thrashing compaction.
const CALIBRATION_MIN_FACTOR: f64 = 0.5;
const CALIBRATION_MAX_FACTOR: f64 = 2.0;

/// One raw sample may not move the EWMA state by more than this ratio band
/// (2× the applied-factor bounds): a single absurd sample (estimated 10,
/// actual 50 000) is truncated before it enters the average, so recovery
/// takes a few ordinary samples instead of dozens.
const CALIBRATION_SAMPLE_MIN_RATIO: f64 = CALIBRATION_MIN_FACTOR / 2.0;
const CALIBRATION_SAMPLE_MAX_RATIO: f64 = CALIBRATION_MAX_FACTOR * 2.0;

/// Drift correction for one provider/model: an EWMA of the observed
/// actual/estimated input-token ratio, fed by `(estimated, actual)` pairs
/// from committed steps (`AgentEvent::StepUsage`) and applied as a bounded
/// multiplicative factor on the byte-heuristic estimate.
///
/// The ratio also absorbs systematic per-request overhead the heuristic
/// cannot see (tool schemas, provider framing): those inflate `actual`
/// uniformly, so they surface as a stable ratio rather than noise.
///
/// Pure state — no I/O, no clock. Persistence of samples across sessions is
/// `stella-store`'s job; replay them through [`Calibration::record`] (oldest
/// first, so the EWMA weights the most recent session highest) to rebuild
/// the state.
#[derive(Debug, Clone)]
pub struct Calibration {
    ratio_ewma: f64,
    samples: u32,
    /// Sum of the raw pre-call estimates behind `samples`, unclamped.
    ///
    /// Kept alongside the EWMA rather than derived from it because they answer
    /// different questions. The EWMA is what *corrects* the next estimate, and
    /// it is deliberately smoothed and clamped so one bad sample cannot steer
    /// budgeting; these totals are what *reports* the gap, and a report that
    /// inherited the clamp would understate exactly the drift it exists to
    /// surface. See [`CalibrationMap::report`].
    estimated_total: u64,
    /// Sum of the provider-reported actuals behind `samples`, unclamped.
    actual_total: u64,
    /// Exponentially-weighted least-squares accumulators over the same
    /// `(estimated, actual)` pairs the EWMA sees (#1841).
    ///
    /// The ratio model above assumes the estimator's error is proportional.
    /// A large part of it is not: the serialized tool schemas are a constant
    /// ~6-9k tokens on every request, present in `actual` and invisible to
    /// `estimate_conversation_tokens`. A constant term does not surface as a
    /// stable ratio — with an 8k block a 5k-token conversation observes 2.6
    /// and a 100k one observes 1.08 — so early small-conversation samples
    /// drove the factor to its 2.0 clamp and halved the compaction budget
    /// for the rest of the session, then seeded that into the next one.
    ///
    /// A line separates the two: `actual ≈ slope · estimated + intercept`,
    /// where the intercept is the per-request overhead and the slope is the
    /// estimator's genuine proportional error. Decayed by the same
    /// [`CALIBRATION_ALPHA`] as the EWMA, so "most recent session weighted
    /// highest" holds for both models and oldest-first replay still rebuilds
    /// the state.
    w: f64,
    wx: f64,
    wy: f64,
    wxx: f64,
    wxy: f64,
}

impl Default for Calibration {
    fn default() -> Self {
        Self::new()
    }
}

impl Calibration {
    /// Fresh, uncorrected state: factor 1.0 until enough samples arrive.
    pub fn new() -> Self {
        Self {
            ratio_ewma: 1.0,
            samples: 0,
            estimated_total: 0,
            actual_total: 0,
            w: 0.0,
            wx: 0.0,
            wy: 0.0,
            wxx: 0.0,
            wxy: 0.0,
        }
    }

    /// Feed one observed pair: the raw (uncalibrated) pre-call estimate —
    /// attachments excluded, see [`estimate_conversation_attachment_tokens`]
    /// — and the provider-reported actual input tokens. The actual is a TRUE
    /// total: fresh input plus cache reads plus cache writes, because cache
    /// writes are real prompt tokens the provider read (adapters split them
    /// out of `input_tokens` for pricing, not because they were not sent) and
    /// omitting them fed falsely low ratios on every cache-writing step.
    /// Zero on either side carries no signal (scripted tests, providers that
    /// omit usage) and is ignored rather than recorded as ratio 0/∞.
    pub fn record(&mut self, estimated: u64, actual: u64) {
        if estimated == 0 || actual == 0 {
            return;
        }
        let ratio = (actual as f64 / estimated as f64)
            .clamp(CALIBRATION_SAMPLE_MIN_RATIO, CALIBRATION_SAMPLE_MAX_RATIO);
        if self.samples == 0 {
            self.ratio_ewma = ratio;
        } else {
            self.ratio_ewma =
                CALIBRATION_ALPHA * ratio + (1.0 - CALIBRATION_ALPHA) * self.ratio_ewma;
        }
        self.samples += 1;
        // The RAW pair, before the clamp above: the report's whole job is to
        // say how far the estimate is from the bill, and a truncated sample
        // would make a catastrophic tokenizer mismatch read as a mild one.
        self.estimated_total = self.estimated_total.saturating_add(estimated);
        self.actual_total = self.actual_total.saturating_add(actual);
        self.accumulate_fit(estimated as f64, actual as f64);
    }

    /// Fold one raw pair into the exponentially-weighted least-squares
    /// accumulators (#1841). Raw, not the ratio-clamped value: the clamp above
    /// exists to stop one absurd sample steering the EWMA, and the fit has its
    /// own protection in the conditioning guard on [`Self::line`].
    fn accumulate_fit(&mut self, x: f64, y: f64) {
        let decay = 1.0 - CALIBRATION_ALPHA;
        self.w = self.w * decay + CALIBRATION_ALPHA;
        self.wx = self.wx * decay + CALIBRATION_ALPHA * x;
        self.wy = self.wy * decay + CALIBRATION_ALPHA * y;
        self.wxx = self.wxx * decay + CALIBRATION_ALPHA * x * x;
        self.wxy = self.wxy * decay + CALIBRATION_ALPHA * x * y;
    }

    /// The fitted `(slope, intercept)`, or `None` when the samples carry no
    /// slope to fit.
    ///
    /// The denominator is the weighted variance of `estimated`. A session
    /// whose conversations were all about one size drives it to zero — and
    /// that is not a numerical edge case to paper over, it is the honest
    /// answer that one cluster of points determines no line. The caller falls
    /// back to the ratio model, which is exactly what this state was before.
    fn line(&self) -> Option<(f64, f64)> {
        let denominator = self.w * self.wxx - self.wx * self.wx;
        // Scale-relative, not an absolute epsilon: `estimated` is in tokens,
        // so the variance is in tokens SQUARED and an absolute threshold
        // would mean something different at 1k than at 100k.
        if denominator <= f64::EPSILON * self.wxx.max(1.0) {
            return None;
        }
        let slope = (self.w * self.wxy - self.wx * self.wy) / denominator;
        let intercept = (self.wy - slope * self.wx) / self.w;
        if !slope.is_finite() || !intercept.is_finite() || slope <= 0.0 {
            return None;
        }
        Some((slope, intercept))
    }

    /// How many samples have been recorded.
    pub fn samples(&self) -> u32 {
        self.samples
    }

    /// Total raw estimated input tokens across every recorded sample.
    pub fn estimated_total(&self) -> u64 {
        self.estimated_total
    }

    /// Total provider-reported input tokens across every recorded sample.
    pub fn actual_total(&self) -> u64 {
        self.actual_total
    }

    /// Observed drift over the whole sample history: `actual / estimated`,
    /// unsmoothed and unclamped. `1.0` with no samples (nothing observed is
    /// reported as no drift, never as a division by zero).
    ///
    /// Distinct from [`Calibration::factor`] on purpose. `factor` is the
    /// number the engine multiplies by — smoothed, bounded, and exactly 1.0
    /// until enough samples exist. This is the number a *human* reads: the
    /// measured gap, whatever it is.
    pub fn drift_ratio(&self) -> f64 {
        if self.estimated_total == 0 {
            return 1.0;
        }
        self.actual_total as f64 / self.estimated_total as f64
    }

    /// The correction factor to multiply a raw estimate by: exactly 1.0
    /// below `CALIBRATION_MIN_SAMPLES`, otherwise the EWMA ratio clamped
    /// to \[`CALIBRATION_MIN_FACTOR`, `CALIBRATION_MAX_FACTOR`\] — see the
    /// bounds' doc for why that keeps the safe over-estimate direction.
    pub fn factor(&self) -> f64 {
        if self.samples < CALIBRATION_MIN_SAMPLES {
            return 1.0;
        }
        self.ratio_ewma
            .clamp(CALIBRATION_MIN_FACTOR, CALIBRATION_MAX_FACTOR)
    }

    /// The conversation size whose corrected estimate equals `budget`, plus
    /// the factor that size implies — what compaction actually needs (#1841).
    ///
    /// The ratio model answers this as `budget / factor`, which is right only
    /// while the correction is purely proportional. With the fitted line the
    /// question is `slope · B + intercept = budget`, so:
    ///
    /// ```text
    /// B = (budget − intercept) / slope
    /// ```
    ///
    /// The per-request overhead is subtracted ONCE rather than scaling the
    /// whole budget by a factor that absorbed it — which is the halving this
    /// fixes.
    ///
    /// The returned factor is the *implied* one, `budget / B`, and it carries
    /// the same `CALIBRATION_MIN_FACTOR`/`CALIBRATION_MAX_FACTOR` clamp the
    /// scalar model had. That is deliberate: the lower bound is what stops
    /// calibration spending the estimator's conservative bias, and the
    /// argument for it does not change because the model underneath got a
    /// second term.
    pub fn effective_budget(&self, budget: u64) -> (u64, f64) {
        if self.samples < CALIBRATION_MIN_SAMPLES {
            return (budget, 1.0);
        }
        let budget_f = budget as f64;
        let sized = match self.line() {
            // An overhead at or above the whole budget leaves no room to
            // solve for, and a non-positive size is not a conversation.
            // Fall back rather than return a nonsense budget.
            Some((slope, intercept)) if budget_f > intercept => {
                Some((budget_f - intercept) / slope)
            }
            _ => None,
        };
        let Some(sized) = sized.filter(|b| *b > 0.0) else {
            let factor = self.factor();
            return ((budget_f / factor) as u64, factor);
        };
        let implied = (budget_f / sized).clamp(CALIBRATION_MIN_FACTOR, CALIBRATION_MAX_FACTOR);
        ((budget_f / implied) as u64, implied)
    }

    /// [`estimate_message_tokens`] corrected by the current factor.
    ///
    /// Slope-only by construction: the fitted intercept is a **per-request**
    /// overhead, and adding it to each message would multiply it by the
    /// message count. Nothing outside this module's own tests calls this
    /// today, which is why the distinction costs nothing.
    pub fn calibrated_message_tokens(&self, message: &CompletionMessage) -> u64 {
        (estimate_message_tokens(message) as f64 * self.factor()).ceil() as u64
    }

    /// [`estimate_conversation_tokens`] corrected by the current factor.
    pub fn calibrated_conversation_tokens(&self, messages: &[CompletionMessage]) -> u64 {
        (estimate_conversation_tokens(messages) as f64 * self.factor()).ceil() as u64
    }
}

/// Per-model calibration state, keyed by the model string the provider
/// reports on each result (tokenizers differ per model, so their drift must
/// never blend). The provider dimension is handled where samples are
/// persisted and re-loaded: `stella-store` keys telemetry by
/// `(provider, model)` and a session seeds only its own resolved pair, so
/// one map never mixes same-named models from different providers.
///
/// Interior-mutable (a `Mutex` never held across an await) because the
/// engine drives turns through `&self` — the caller owns the map across
/// turns and hands the engine a shared reference, mirroring how
/// `BudgetGuard` outlives individual turns.
#[derive(Debug, Default)]
pub struct CalibrationMap {
    inner: Mutex<HashMap<String, Calibration>>,
}

impl CalibrationMap {
    /// An empty map: every model reads factor 1.0 until samples arrive.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replay persisted `(estimated, actual)` pairs — oldest first — into
    /// `model`'s state, so a new session starts where the last one left off.
    pub fn seed(&self, model: &str, samples: &[(u64, u64)]) {
        let mut inner = self.lock();
        let calibration = inner.entry(model.to_string()).or_default();
        for &(estimated, actual) in samples {
            calibration.record(estimated, actual);
        }
    }

    /// Feed one observed pair into `model`'s state.
    pub fn record(&self, model: &str, estimated: u64, actual: u64) {
        self.lock()
            .entry(model.to_string())
            .or_default()
            .record(estimated, actual);
    }

    /// The correction factor for `model`. `None` (the driver hasn't seen a
    /// result yet, so no model string exists) falls back to the map's single
    /// entry when there is exactly one — the session-seeded state — and to
    /// 1.0 otherwise; an ambiguous multi-model map never guesses.
    pub fn factor(&self, model: Option<&str>) -> f64 {
        let inner = self.lock();
        match model {
            Some(model) => inner.get(model).map(Calibration::factor).unwrap_or(1.0),
            None if inner.len() == 1 => inner
                .values()
                .next()
                .map(Calibration::factor)
                .unwrap_or(1.0),
            None => 1.0,
        }
    }

    /// [`Calibration::effective_budget`] for `model`, resolved by the same
    /// rule as [`Self::factor`]. An unknown model is uncalibrated, so the
    /// budget passes through untouched.
    pub fn effective_budget(&self, model: Option<&str>, budget: u64) -> (u64, f64) {
        let inner = self.lock();
        let calibration = match model {
            Some(model) => inner.get(model),
            None if inner.len() == 1 => inner.values().next(),
            None => None,
        };
        calibration.map_or((budget, 1.0), |c| c.effective_budget(budget))
    }

    /// Every model's observed drift, sorted by model id.
    ///
    /// The read half of the loop `record` feeds: an operator (or a host
    /// reading it through an API) can see how far Stella's own estimate is
    /// from what the provider actually billed, per model, instead of
    /// believing the estimate until the invoice disagrees.
    ///
    /// Sorted rather than in `HashMap` order so two consecutive reads of an
    /// unchanged map are byte-identical — a report that reshuffles itself
    /// every call is unusable for diffing and untestable.
    pub fn report(&self) -> Vec<ModelDrift> {
        let inner = self.lock();
        let mut rows: Vec<ModelDrift> = inner
            .iter()
            .map(|(model, calibration)| ModelDrift {
                model: model.clone(),
                samples: calibration.samples(),
                estimated_input_tokens: calibration.estimated_total(),
                actual_input_tokens: calibration.actual_total(),
                drift_ratio: calibration.drift_ratio(),
                applied_factor: calibration.factor(),
            })
            .collect();
        rows.sort_by(|a, b| a.model.cmp(&b.model));
        rows
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Calibration>> {
        // Poisoning means a panic mid-record; the state is a plain f64+u32
        // pair that cannot be left torn — keep calibrating.
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One model's cost-drift row — what [`CalibrationMap::report`] hands back.
///
/// Counts and identifiers only. Nothing here is derived from transcript
/// content, so a host may publish it (`stella-serve` does, at
/// `GET /v1/calibration`) without opening a second exposure path for the
/// conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelDrift {
    /// The model string the provider reported on the results behind this row.
    pub model: String,
    /// Samples recorded. Below the minimum the correction needs (three),
    /// `applied_factor` is still exactly 1.0 — the drift is measured but
    /// deliberately not yet acted on.
    pub samples: u32,
    /// Raw estimated input tokens summed across those samples.
    pub estimated_input_tokens: u64,
    /// Provider-reported input tokens (fresh + cache reads + cache writes)
    /// summed across the same samples.
    pub actual_input_tokens: u64,
    /// `actual / estimated` over the whole history — the gap itself.
    /// Above 1.0 means Stella under-estimated what the provider billed.
    pub drift_ratio: f64,
    /// The bounded correction currently multiplied into estimates
    /// ([`Calibration::factor`]), which is *not* `drift_ratio`: it is smoothed,
    /// clamped, and 1.0 until enough samples exist.
    pub applied_factor: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::{MessageRole, ToolOutput, ToolResult};

    #[test]
    fn empty_message_costs_only_overhead() {
        let m = CompletionMessage {
            role: MessageRole::User,
            content: String::new(),
            tool_calls: vec![],
            tool_results: vec![],
            attachments: Vec::new(),
        };
        assert_eq!(estimate_message_tokens(&m), PER_MESSAGE_OVERHEAD);
    }

    #[test]
    fn attachments_add_base64_scaled_tokens() {
        let plain = CompletionMessage::user("hi");
        let with_media = CompletionMessage::user_with_attachments(
            "hi",
            vec![stella_protocol::Attachment::from_path(
                "shot.png",
                "image/png",
                350_000,
                "/tmp/shot.png",
            )],
        );
        let delta = estimate_message_tokens(&with_media) - estimate_message_tokens(&plain);
        // 350 KB payload → ~466k base64 chars → ~133k estimated tokens.
        assert!(delta > 100_000, "attachment must weigh in: {delta}");
    }

    #[test]
    fn estimate_grows_with_content_length() {
        let short = CompletionMessage::user("hi");
        let long = CompletionMessage::user("a".repeat(3500));
        assert!(estimate_message_tokens(&long) > estimate_message_tokens(&short));
        // 3500 chars / 3.5 = 1000 tokens + overhead
        assert_eq!(estimate_message_tokens(&long), 1000 + PER_MESSAGE_OVERHEAD);
    }

    #[test]
    fn tool_results_count_toward_the_estimate() {
        let bare = CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: vec![],
            tool_results: vec![],
            attachments: Vec::new(),
        };
        let loaded = CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: vec![],
            tool_results: vec![ToolResult {
                call_id: "c1".into(),
                output: ToolOutput::Ok {
                    content: "x".repeat(7000),
                },
            }],
            attachments: Vec::new(),
        };
        assert!(estimate_message_tokens(&loaded) > estimate_message_tokens(&bare) + 1900);
    }

    #[test]
    fn counting_sink_matches_the_string_it_replaces() {
        // The counting sink must be byte-identical to `to_string().len()` or
        // every stored estimate, calibration ratio, and receipt number moves.
        let values = [
            serde_json::json!(null),
            serde_json::json!(0.1 + 0.2),
            serde_json::json!(-1.5e-7),
            serde_json::json!("quote \" backslash \\ newline \n tab \t"),
            serde_json::json!("héllo — 日本語 🎉"),
            serde_json::json!({
                "nested": { "deep": [1, 2.5, true, null, "ünïcödé"] },
                "empty_obj": {},
                "empty_arr": [],
                "esc\"aped key": "\u{1}",
            }),
        ];
        for value in values {
            assert_eq!(
                json_len(&value),
                value.to_string().len(),
                "counting sink diverged on {value}"
            );
        }
    }

    #[test]
    fn conversation_estimate_is_sum_of_messages() {
        let messages = vec![
            CompletionMessage::system("sys"),
            CompletionMessage::user("hello"),
        ];
        let total = estimate_conversation_tokens(&messages);
        let sum: u64 = messages.iter().map(estimate_message_tokens).sum();
        assert_eq!(total, sum);
    }

    // ---- Calibration -------------------------------------------------------

    #[test]
    fn factor_is_identity_below_the_minimum_sample_count() {
        let mut cal = Calibration::new();
        assert_eq!(cal.factor(), 1.0);
        cal.record(1000, 1500);
        cal.record(1000, 1500);
        assert_eq!(
            cal.factor(),
            1.0,
            "two samples must not steer budgeting yet (min is {CALIBRATION_MIN_SAMPLES})"
        );
        cal.record(1000, 1500);
        assert!(
            (cal.factor() - 1.5).abs() < 1e-9,
            "at the minimum count the correction applies: {}",
            cal.factor()
        );
    }

    #[test]
    fn calibration_converges_and_the_estimate_error_shrinks() {
        // A provider whose tokenizer consistently runs 40% over the char
        // heuristic: the calibrated estimate's error against `actual` must
        // shrink monotonically-ish over a session and land near zero.
        let mut cal = Calibration::new();
        let estimated = 10_000u64;
        let actual = 14_000u64;
        let initial_error = estimated.abs_diff(actual);
        let mut last_error = u64::MAX;
        for step in 0..10 {
            let calibrated = (estimated as f64 * cal.factor()).ceil() as u64;
            let error = calibrated.abs_diff(actual);
            if step >= 3 {
                assert!(
                    error <= last_error,
                    "error must not grow once corrections apply: step {step}, {error} > {last_error}"
                );
                last_error = error;
            }
            cal.record(estimated, actual);
        }
        let final_error = ((estimated as f64 * cal.factor()).ceil() as u64).abs_diff(actual);
        assert!(
            final_error < initial_error / 10,
            "after 10 steady samples the error must be <10% of the raw drift: \
             {final_error} vs initial {initial_error}"
        );
    }

    #[test]
    fn factor_is_clamped_so_wild_ratios_cannot_destabilize_budgets() {
        let mut low = Calibration::new();
        let mut high = Calibration::new();
        for _ in 0..20 {
            low.record(100_000, 1); // absurd under-run
            high.record(1, 100_000); // absurd over-run
        }
        assert_eq!(
            low.factor(),
            CALIBRATION_MIN_FACTOR,
            "the correction may at most halve the conservative estimate"
        );
        assert_eq!(
            high.factor(),
            CALIBRATION_MAX_FACTOR,
            "the correction may at most double the estimate"
        );
    }

    #[test]
    fn one_noisy_sample_is_bounded_before_it_enters_the_average() {
        let mut cal = Calibration::new();
        for _ in 0..5 {
            cal.record(1000, 1000); // steady, perfectly calibrated
        }
        cal.record(1, 1_000_000); // one absurd spike
        // The spike enters as at most CALIBRATION_SAMPLE_MAX_RATIO with
        // weight alpha: 0.3·4.0 + 0.7·1.0 = 1.9 — never the raw 1e6 ratio.
        assert!(
            cal.factor() <= 1.9 + 1e-9,
            "a single spike must move the factor by a bounded step, got {}",
            cal.factor()
        );
        // …and a couple of ordinary samples pull it back down.
        cal.record(1000, 1000);
        cal.record(1000, 1000);
        cal.record(1000, 1000);
        assert!(
            cal.factor() < 1.35,
            "recovery must be quick: {}",
            cal.factor()
        );
    }

    #[test]
    fn zero_sided_samples_carry_no_signal_and_are_ignored() {
        let mut cal = Calibration::new();
        for _ in 0..10 {
            cal.record(0, 5_000); // no estimate recorded (legacy rows)
            cal.record(5_000, 0); // provider omitted usage
        }
        assert_eq!(cal.samples(), 0);
        assert_eq!(cal.factor(), 1.0);
    }

    #[test]
    fn calibrated_estimates_scale_the_raw_heuristic() {
        let mut cal = Calibration::new();
        for _ in 0..5 {
            cal.record(1000, 2000);
        }
        let msg = CompletionMessage::user("a".repeat(3500));
        let raw = estimate_message_tokens(&msg);
        assert_eq!(cal.calibrated_message_tokens(&msg), raw * 2);
        let convo = vec![CompletionMessage::system("sys"), msg];
        assert_eq!(
            cal.calibrated_conversation_tokens(&convo),
            (estimate_conversation_tokens(&convo) as f64 * 2.0).ceil() as u64
        );
    }

    #[test]
    fn map_keys_models_independently_and_seeds_replay_history() {
        let map = CalibrationMap::new();
        map.seed("glm-5.2", &[(1000, 1500), (1000, 1500), (1000, 1500)]);
        map.record("claude-fable-5", 1000, 800);
        assert!((map.factor(Some("glm-5.2")) - 1.5).abs() < 1e-9);
        assert_eq!(
            map.factor(Some("claude-fable-5")),
            1.0,
            "one sample is below the minimum — and glm's drift must not leak in"
        );
        assert_eq!(map.factor(Some("never-seen")), 1.0);
    }

    #[test]
    fn map_falls_back_to_its_single_entry_before_any_model_is_known() {
        let map = CalibrationMap::new();
        assert_eq!(map.factor(None), 1.0, "empty map: no correction");
        map.seed("glm-5.2", &[(1000, 1400), (1000, 1400), (1000, 1400)]);
        assert!(
            (map.factor(None) - 1.4).abs() < 1e-9,
            "a single seeded entry serves the first pre-call read of a session"
        );
        map.seed("other-model", &[(1000, 1400), (1000, 1400), (1000, 1400)]);
        assert_eq!(
            map.factor(None),
            1.0,
            "two entries and no model named: never guess"
        );
    }

    // ---- Drift reporting ---------------------------------------------------

    /// The report is the read half of the calibration loop: it must say what
    /// was estimated, what was billed, and the gap — per model, sorted, and
    /// from the RAW pairs rather than the clamped EWMA the engine budgets on.
    #[test]
    fn the_report_names_every_model_and_its_measured_gap() {
        let map = CalibrationMap::new();
        map.seed("zeta-1", &[(1000, 1500), (1000, 1500), (1000, 1500)]);
        map.record("alpha-1", 2000, 1000);

        let report = map.report();
        assert_eq!(
            report.iter().map(|r| r.model.as_str()).collect::<Vec<_>>(),
            ["alpha-1", "zeta-1"],
            "rows are sorted by model, so two reads of an unchanged map match"
        );

        let zeta = &report[1];
        assert_eq!(zeta.samples, 3);
        assert_eq!(zeta.estimated_input_tokens, 3000);
        assert_eq!(zeta.actual_input_tokens, 4500);
        assert!((zeta.drift_ratio - 1.5).abs() < 1e-9);
        assert!(
            (zeta.applied_factor - 1.5).abs() < 1e-9,
            "three samples is the minimum, so the correction is live"
        );

        let alpha = &report[0];
        assert_eq!(alpha.samples, 1);
        assert!(
            (alpha.drift_ratio - 0.5).abs() < 1e-9,
            "the gap is measured…"
        );
        assert_eq!(
            alpha.applied_factor, 1.0,
            "…while one sample is still below the minimum to act on it"
        );
    }

    /// The reported gap must not inherit the sample clamp. The clamp exists so
    /// one absurd pair cannot steer budgeting; a *report* that hid the same
    /// pair would understate exactly the tokenizer mismatch it exists to
    /// surface.
    #[test]
    fn the_report_shows_the_raw_gap_not_the_clamped_one() {
        let map = CalibrationMap::new();
        // 100x drift — far past CALIBRATION_SAMPLE_MAX_RATIO (4.0).
        map.record("wild-1", 1_000, 100_000);
        let row = &map.report()[0];
        assert_eq!(row.estimated_input_tokens, 1_000);
        assert_eq!(row.actual_input_tokens, 100_000);
        assert!(
            (row.drift_ratio - 100.0).abs() < 1e-9,
            "the report must show 100x, not the 4x the EWMA was allowed to see"
        );
        assert!(
            row.applied_factor <= CALIBRATION_MAX_FACTOR,
            "…while the applied correction stays bounded"
        );
    }

    /// A model whose pairs were all zero-sided recorded nothing, so it must
    /// not appear as a row claiming zero drift over zero tokens.
    #[test]
    fn an_empty_map_reports_nothing() {
        let map = CalibrationMap::new();
        assert!(map.report().is_empty());
        map.record("silent-1", 0, 0);
        assert_eq!(
            map.report().len(),
            1,
            "the model was seen, so it is listed…"
        );
        let row = &map.report()[0];
        assert_eq!(row.samples, 0);
        assert_eq!(row.drift_ratio, 1.0, "…with no drift claimed from no data");
    }

    /// The per-request overhead, in tokens — a serialized tool-schema block.
    const SCHEMA_OVERHEAD: u64 = 8_000;

    /// Feed samples from an estimator that is perfectly accurate about
    /// message content, against an actual that also carries a CONSTANT
    /// per-request overhead. Any correction the calibration learns is
    /// therefore attributable to the overhead alone.
    fn with_constant_overhead(sizes: &[u64]) -> Calibration {
        let mut calibration = Calibration::new();
        for size in sizes {
            calibration.record(*size, size + SCHEMA_OVERHEAD);
        }
        calibration
    }

    /// **Witness (#1841).** A constant per-request overhead must not be
    /// modelled as a multiplicative factor.
    ///
    /// The tool schemas are ~6–9k tokens on every request, present in the
    /// provider's `actual` and invisible to `estimate_conversation_tokens`.
    /// A constant term does not "surface as a stable ratio": with an 8k block
    /// a 5k conversation observes 2.6 and a 100k one observes 1.08. Averaged
    /// into one ratio and clamped at 2.0, that halved the compaction budget
    /// for the whole session — and `seed_calibration` carried it into the
    /// next one.
    #[test]
    fn a_constant_overhead_does_not_halve_the_compaction_budget() {
        let budget = 150_000;
        let calibration = with_constant_overhead(&[4_000, 6_000, 5_000, 7_000]);

        // The ratio model is what the samples look like to the old code, and
        // it is pinned here so the witness states the defect rather than
        // merely avoiding it.
        assert_eq!(
            calibration.factor(),
            CALIBRATION_MAX_FACTOR,
            "small conversations plus a constant overhead saturate the ratio clamp"
        );
        let ratio_budget = (budget as f64 / calibration.factor()) as u64;
        assert_eq!(ratio_budget, 75_000, "which is the halving this fixes");

        let (fitted, factor) = calibration.effective_budget(budget);
        assert!(
            fitted > 130_000,
            "the overhead is additive: a 150k budget minus an 8k block leaves \
             ~142k of conversation, not 75k — got {fitted} (factor {factor})"
        );
        assert!(
            fitted <= budget,
            "and never MORE than the budget, which would invite truncation: {fitted}"
        );
    }

    /// The fit recovers the two terms it was given, rather than merely
    /// producing a bigger number than the ratio model.
    #[test]
    fn the_fit_separates_the_overhead_from_the_proportional_error() {
        let calibration = with_constant_overhead(&[4_000, 20_000, 60_000, 120_000]);
        let (slope, intercept) = calibration.line().expect("four distinct sizes fit a line");
        assert!(
            (slope - 1.0).abs() < 0.05,
            "the content estimator was exact here, so the slope is ~1: {slope}"
        );
        assert!(
            (intercept - SCHEMA_OVERHEAD as f64).abs() < 500.0,
            "and the whole error is the constant block: {intercept}"
        );
    }

    /// One conversation size determines no line, and the honest answer is to
    /// say so rather than divide by a variance of zero.
    #[test]
    fn samples_at_a_single_size_fall_back_to_the_ratio_model() {
        let calibration = with_constant_overhead(&[5_000, 5_000, 5_000, 5_000]);
        assert!(
            calibration.line().is_none(),
            "no spread in `estimated` means no slope to fit"
        );
        let (fitted, factor) = calibration.effective_budget(150_000);
        assert_eq!(
            (fitted, factor),
            (75_000, CALIBRATION_MAX_FACTOR),
            "which is exactly the previous behaviour — a fallback, not a crash"
        );
    }

    /// Below the sample floor nothing is corrected, so a fresh session and a
    /// seeded one that has not warmed up both budget at face value.
    #[test]
    fn an_unwarmed_calibration_leaves_the_budget_alone() {
        assert_eq!(Calibration::new().effective_budget(150_000), (150_000, 1.0));
        let barely = with_constant_overhead(&[4_000, 6_000]);
        assert_eq!(barely.effective_budget(150_000), (150_000, 1.0));
    }

    /// An overhead that swallows the whole budget has no size to solve for.
    /// The clamp is what keeps that from becoming a nonsense budget.
    #[test]
    fn an_overhead_larger_than_the_budget_degrades_rather_than_inverting() {
        let mut calibration = Calibration::new();
        for size in [1_000, 2_000, 3_000, 4_000] {
            calibration.record(size, size + 200_000);
        }
        let (fitted, factor) = calibration.effective_budget(150_000);
        assert!(
            fitted > 0,
            "a budget of zero would compact every turn forever"
        );
        assert!(
            (CALIBRATION_MIN_FACTOR..=CALIBRATION_MAX_FACTOR).contains(&factor),
            "the safety clamp still bounds the answer: {factor}"
        );
    }
}
