//! What reflection is allowed to read of a turn: a *selection*, not a tail.
//!
//! ## The two defects this module replaces (#2460)
//!
//! Reflection used to build its digest by taking the last twelve messages and
//! truncating each to 300 characters. Both halves of that were wrong, and the
//! second is worse than the issue title says.
//!
//! **The window was in the wrong place.** A turn's expensive part is in the
//! MIDDLE — the approach taken at step 4 and abandoned at step 22, the tool
//! that errored and was never read again, the command that ran for twenty
//! minutes against the wrong target. The tail of a turn that went well is the
//! summary and the sign-off. So reflection was asked what would have made the
//! turn faster by a witness that could not see where it was slow.
//!
//! **And a `Tool` message carries no `content` at all.** The engine builds it
//! as `content: String::new()` with the payload in `tool_results` —
//! [`stella_protocol::CompletionMessage`]'s own field docs say so, and
//! `stella_core`'s driver is where it happens. The old digest read
//! `message.content`, so every tool result on every surface rendered as the six
//! characters `"tool: "`. Reflection had never seen what a tool returned — not
//! truncated, *absent*. The 300-character cut was never reached on the messages
//! it was meant to bound.
//!
//! ## What replaces it
//!
//! [`build`] renders the turn under a total character budget
//! ([`DIGEST_BUDGET_CHARS`]), spending it in priority order rather than in
//! reverse transcript order:
//!
//! 1. **Pinned** — the goal (the turn's first user message) and the last few
//!    messages (its outcome). These say what was asked and what was delivered,
//!    and they are bounded by count, so they are admitted before the budget is
//!    consulted at all.
//! 2. **Friction** — every message carrying an errored tool result, every
//!    assistant message that *requested* one, and every message naming a tool
//!    the event stream flagged as costly or failed ([`TurnFriction`]). This is
//!    the middle the tail window dropped.
//! 3. **Filler** — everything else, in transcript order, while the budget
//!    lasts.
//!
//! Whatever is not admitted is replaced by an explicit `… N messages elided …`
//! marker, so the model reading the digest knows it holds a selection and can
//! say so instead of reasoning confidently across a gap it cannot see.
//!
//! ## Purity
//!
//! Everything here is a pure function over borrowed data: no clock, no
//! filesystem, no store. `reflect_on_turn` reads no clock and assigns no
//! instant (#2320), and the mining log it feeds is reproducible only while that
//! holds — a digest that varied with the time of day would break replay
//! (`memory/replay.rs`) without anything failing. [`TurnFriction`] is a fold
//! over events the caller already observed, never a read-back of the journal:
//! the journal is written by the renderer task, which is still draining while
//! reflection runs, so reading it here would race and the digest would stop
//! being a function of the turn.

/// The selection's mechanics, and the before/after cost measurement #2460's
/// definition of done asks for. A child module so it can reach the private
/// helpers (`clip`, `human_ms`) and the caps without widening their visibility.
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use stella_protocol::{AgentEvent, CompletionMessage, MessageRole, ModelCallRole, ToolOutput};

/// The whole digest's character ceiling.
///
/// Chosen against what it replaces rather than picked round: the old tail
/// digest could reach 12 × ~310 ≈ 3.7k characters, so this raises the *ceiling*
/// by roughly 1.6× (~1.5k tokens of transcript). The realized input grows by
/// more than that on a tool-heavy turn, because the old digest rendered tool
/// results as nothing at all and so almost never approached its own ceiling —
/// the honest framing is that reflection now pays for evidence it was
/// previously billed nothing to ignore. Reflection dispatches pinned-low with a
/// 2048-token output cap on every tool-using turn, so this is the number to
/// argue about if its per-turn cost has to come down; #2155 (narrow the
/// trigger) is the other side of that trade.
pub(crate) const DIGEST_BUDGET_CHARS: usize = 6_000;

/// How many trailing messages are the turn's *outcome*, and pinned as such.
const OUTCOME_TAIL_MESSAGES: usize = 4;

/// Per-message character cap for a pinned message (the goal, the outcome).
const PINNED_CHARS: usize = 700;

/// Per-message character cap for a friction message. An error message and the
/// call that provoked it are short and are the entire point, so they get the
/// same room as the outcome.
const FRICTION_CHARS: usize = 700;

/// Per-message character cap for ordinary narration.
const FILLER_CHARS: usize = 200;

/// Floor on a single tool result's share of its message's cap.
///
/// A `Tool` message can carry several results (parallel calls), and dividing
/// the cap evenly can leave each too small to hold the assertion that actually
/// failed. Such a message may exceed its nominal cap; a truncated error message
/// is worth nothing, and the budget check below still bounds the digest.
const MIN_RESULT_CHARS: usize = 160;

/// Per-call character cap on a tool call's rendered arguments.
///
/// The arguments are load-bearing in a way the old `[called: bash]` rendering
/// was not: "ran the wrong command for twenty minutes" is a fact about the
/// argument, not about the tool.
const TOOL_INPUT_CHARS: usize = 140;

/// How many costliest steps and slowest tools the friction section names.
const FRICTION_TOP_N: usize = 3;

/// How many errored calls, retries and loop firings the friction section names
/// before it says how many more there were.
const FRICTION_LIST_CAP: usize = 6;

/// How many entries [`TurnFriction`] retains per kind before it counts drops
/// instead of growing. A turn cannot produce more friction than this and still
/// have a summarizable shape, and an unbounded fold on a long-running turn is a
/// leak in a best-effort path.
const FRICTION_ENTRY_CAP: usize = 512;

/// One model call's metering record, as [`AgentEvent::StepUsage`] reported it.
#[derive(Debug, Clone)]
struct StepCost {
    step: usize,
    role: String,
    cost_usd: f64,
    duration_ms: u64,
    tool_calls: usize,
}

/// One tool call's pass through the turn — opened by [`AgentEvent::ToolStart`],
/// closed by its [`AgentEvent::ToolResult`].
#[derive(Debug, Clone)]
struct ToolPass {
    call_id: String,
    /// Empty when the ledger saw the result but not the start (a tap installed
    /// mid-turn, or the entry cap). The transcript's own tool calls can still
    /// name it — see [`call_names`].
    name: String,
    duration_ms: u64,
    /// The failure message when the output was [`ToolOutput::Error`]. `None` is
    /// a success: a call still in flight is simply not closed yet, and one that
    /// never closes keeps its zero duration.
    error: Option<String>,
}

/// What a turn's event stream says about where its time and money went.
///
/// Folded live by the surface that owns the turn's [`AgentEvent`] stream and
/// handed to reflection as evidence the transcript does not carry: a
/// `CompletionMessage` records what was said, never what it cost or how long it
/// took, and a retry or a loop-detector firing leaves no message at all.
///
/// [`Default`] is an honest empty ledger, not a degraded one. A surface with no
/// tap wired still selects on the transcript's own typed tool results, and the
/// friction section is simply omitted — nothing here is required for [`build`]
/// to be correct.
#[derive(Debug, Clone, Default)]
pub struct TurnFriction {
    steps: Vec<StepCost>,
    tools: Vec<ToolPass>,
    /// Calls awaiting their result: `call_id` → index into `tools`.
    open: HashMap<String, usize>,
    retries: Vec<String>,
    loops: Vec<String>,
    dropped: usize,
}

impl TurnFriction {
    /// Fold one event into the ledger.
    ///
    /// Total over the variants that carry friction and deliberately silent on
    /// every other. The wildcard arm is a decision, not laziness: a new
    /// `AgentEvent` variant must not change what a turn's friction *is* until
    /// someone decides that it should.
    pub fn observe(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::ToolStart { call } => {
                if self.tools.len() >= FRICTION_ENTRY_CAP {
                    self.dropped += 1;
                    return;
                }
                self.open.insert(call.call_id.clone(), self.tools.len());
                self.tools.push(ToolPass {
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                    duration_ms: 0,
                    error: None,
                });
            }
            AgentEvent::ToolResult {
                call_id,
                output,
                duration_ms,
                ..
            } => {
                let Some(index) = self.close(call_id) else {
                    return;
                };
                let pass = &mut self.tools[index];
                pass.duration_ms = *duration_ms;
                pass.error = match output {
                    ToolOutput::Error { message } => Some(message.clone()),
                    ToolOutput::Ok { .. } => None,
                };
            }
            AgentEvent::StepUsage {
                step,
                role,
                cost_usd,
                duration_ms,
                tool_calls,
                ..
            } => {
                if self.steps.len() >= FRICTION_ENTRY_CAP {
                    self.dropped += 1;
                    return;
                }
                self.steps.push(StepCost {
                    step: *step,
                    role: role_token(*role),
                    cost_usd: *cost_usd,
                    duration_ms: *duration_ms,
                    tool_calls: *tool_calls,
                });
            }
            AgentEvent::Retry { attempt, reason } => {
                self.push_retry(format!("attempt {attempt}: {reason}"));
            }
            AgentEvent::RetriesExhausted {
                attempts, reasons, ..
            } => {
                let last = reasons.last().map(String::as_str).unwrap_or("no reason given");
                self.push_retry(format!("exhausted after {attempts} attempts: {last}"));
            }
            AgentEvent::LoopDetected {
                kind,
                pattern,
                repeats,
                aborted,
                ..
            } => {
                if self.loops.len() >= FRICTION_ENTRY_CAP {
                    self.dropped += 1;
                    return;
                }
                let outcome = if *aborted { "aborted" } else { "steered" };
                self.loops.push(format!(
                    "{kind} ×{repeats} on [{}] — {outcome}",
                    pattern.join(" → ")
                ));
            }
            _ => {}
        }
    }

    /// The `tools` slot a result belongs in, opening one for a result whose
    /// start was never seen. `None` means the entry cap refused it.
    fn close(&mut self, call_id: &str) -> Option<usize> {
        if let Some(index) = self.open.remove(call_id) {
            return Some(index);
        }
        if self.tools.len() >= FRICTION_ENTRY_CAP {
            self.dropped += 1;
            return None;
        }
        self.tools.push(ToolPass {
            call_id: call_id.to_string(),
            name: String::new(),
            duration_ms: 0,
            error: None,
        });
        Some(self.tools.len() - 1)
    }

    fn push_retry(&mut self, entry: String) {
        if self.retries.len() >= FRICTION_ENTRY_CAP {
            self.dropped += 1;
            return;
        }
        self.retries.push(entry);
    }

    /// Whether this ledger has anything to say. An empty one renders no section
    /// at all rather than an encouraging header over nothing.
    fn is_empty(&self) -> bool {
        self.steps.is_empty()
            && self.tools.is_empty()
            && self.retries.is_empty()
            && self.loops.is_empty()
    }

    /// A shared empty ledger, so [`TurnEvidence::from_transcript`] can hand out
    /// a reference without every caller owning a `Default`.
    fn empty() -> &'static Self {
        static EMPTY: std::sync::OnceLock<TurnFriction> = std::sync::OnceLock::new();
        EMPTY.get_or_init(TurnFriction::default)
    }

    /// Tool names the event stream saw, by call id — the half of the
    /// call-id → name map the transcript cannot supply on the staged pipeline
    /// path, where the worker's tool turns are deliberately kept out of
    /// `messages` for planner-context hygiene.
    fn call_names(&self) -> impl Iterator<Item = (&str, &str)> {
        self.tools
            .iter()
            .filter(|pass| !pass.name.is_empty())
            .map(|pass| (pass.call_id.as_str(), pass.name.as_str()))
    }

    /// The `tools` entries this ledger says a reflection must see: every call
    /// that failed, and the [`FRICTION_TOP_N`] that dominated wall clock.
    fn flagged(&self) -> Vec<&str> {
        let mut flagged: Vec<&str> = self
            .tools
            .iter()
            .filter(|pass| pass.error.is_some())
            .map(|pass| pass.call_id.as_str())
            .collect();
        flagged.extend(
            self.by_duration()
                .into_iter()
                .take(FRICTION_TOP_N)
                .map(|pass| pass.call_id.as_str()),
        );
        flagged
    }

    /// Closed calls, slowest first. Ties break on `call_id` so the ordering is
    /// total — two calls of equal duration must not reorder between runs, or
    /// the digest stops being a function of the turn.
    fn by_duration(&self) -> Vec<&ToolPass> {
        let mut passes: Vec<&ToolPass> = self
            .tools
            .iter()
            .filter(|pass| pass.duration_ms > 0)
            .collect();
        passes.sort_by(|a, b| {
            b.duration_ms
                .cmp(&a.duration_ms)
                .then_with(|| a.call_id.cmp(&b.call_id))
        });
        passes
    }

    /// The friction section: where this turn actually spent itself, bounded.
    ///
    /// Deliberately first in the digest. It is the part reflection could not
    /// have reconstructed at any window size, and it is short — putting it
    /// behind several thousand characters of transcript would make it the first
    /// thing a narrating model skims past.
    fn section(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut out = String::from("Where this turn spent itself (from its event stream):\n");
        if !self.steps.is_empty() {
            let calls = self.steps.len();
            let cost: f64 = self.steps.iter().map(|step| step.cost_usd).sum();
            let wall: u64 = self.steps.iter().map(|step| step.duration_ms).sum();
            let _ = writeln!(
                out,
                "- {calls} model calls, ${cost:.4}, {} of model wall clock",
                human_ms(wall)
            );
            let mut costliest: Vec<&StepCost> = self.steps.iter().collect();
            costliest.sort_by(|a, b| {
                b.cost_usd
                    .total_cmp(&a.cost_usd)
                    .then_with(|| a.step.cmp(&b.step))
            });
            for step in costliest.iter().take(FRICTION_TOP_N) {
                let _ = writeln!(
                    out,
                    "- costliest: step {} ({}) ${:.4}, {}, {} tool calls",
                    step.step,
                    step.role,
                    step.cost_usd,
                    human_ms(step.duration_ms),
                    step.tool_calls
                );
            }
        }
        for pass in self.by_duration().iter().take(FRICTION_TOP_N) {
            let _ = writeln!(
                out,
                "- slowest tool: {} took {}",
                tool_label(pass),
                human_ms(pass.duration_ms)
            );
        }
        let failed: Vec<&ToolPass> = self
            .tools
            .iter()
            .filter(|pass| pass.error.is_some())
            .collect();
        if !failed.is_empty() {
            let _ = writeln!(out, "- {} tool calls FAILED:", failed.len());
            for pass in failed.iter().take(FRICTION_LIST_CAP) {
                let message = pass.error.as_deref().unwrap_or_default();
                let _ = writeln!(
                    out,
                    "  - {}: {}",
                    tool_label(pass),
                    clip(message, FRICTION_CHARS)
                );
            }
            if failed.len() > FRICTION_LIST_CAP {
                let _ = writeln!(out, "  - … {} more", failed.len() - FRICTION_LIST_CAP);
            }
        }
        write_list(&mut out, "model call retried", &self.retries);
        write_list(&mut out, "loop detector fired", &self.loops);
        if self.dropped > 0 {
            let _ = writeln!(
                out,
                "- ({} further friction records were not retained)",
                self.dropped
            );
        }
        out
    }
}

/// Everything reflection reads about the turn it is judging.
///
/// One argument rather than three, because the three are one question and were
/// already threaded separately through four surfaces. The bundle is also what
/// makes [`build`] a pure function of the turn: there is nothing else it may
/// consult.
#[derive(Clone, Copy)]
pub struct TurnEvidence<'a> {
    /// The turn's messages, oldest first. May be a whole session's history (the
    /// REPL passes all of it) or a slice (the staged pipeline passes its planner
    /// messages plus the final answer).
    pub transcript: &'a [CompletionMessage],
    /// What the turn's event stream said about cost, wall clock, retries and
    /// loop firings.
    pub friction: &'a TurnFriction,
    /// Whether the turn succeeded — which reflection prompt template applies.
    pub succeeded: bool,
}

impl<'a> TurnEvidence<'a> {
    /// Evidence from a transcript alone: the shape every caller with no event
    /// ledger uses, and the shape every test uses.
    pub fn from_transcript(transcript: &'a [CompletionMessage], succeeded: bool) -> Self {
        Self {
            transcript,
            friction: TurnFriction::empty(),
            succeeded,
        }
    }
}

/// How much of the budget one message is entitled to, and in what order it is
/// spent. Declaration order is allocation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Priority {
    /// The goal and the outcome. Admitted before the budget is consulted.
    Pinned,
    /// The middle the tail window dropped: errors, and what provoked them.
    Friction,
    /// Ordinary narration, admitted while the budget lasts.
    Filler,
}

impl Priority {
    const ALL: [Priority; 3] = [Priority::Pinned, Priority::Friction, Priority::Filler];

    fn cap(self) -> usize {
        match self {
            Priority::Pinned => PINNED_CHARS,
            Priority::Friction => FRICTION_CHARS,
            Priority::Filler => FILLER_CHARS,
        }
    }
}

/// Build the digest reflection reads. Deterministic in `evidence` alone.
pub(crate) fn build(evidence: TurnEvidence<'_>) -> String {
    let transcript = evidence.transcript;
    let names = call_names(evidence);
    let priorities = classify(evidence);
    let mut kept: Vec<Option<String>> = vec![None; transcript.len()];
    let mut spent = 0usize;
    for tier in Priority::ALL {
        for (index, priority) in priorities.iter().enumerate() {
            if *priority != tier {
                continue;
            }
            let body = render(&transcript[index], &names, tier.cap());
            if body.is_empty() {
                continue;
            }
            let weight = body.chars().count();
            // Pinned messages are bounded by count (one goal plus
            // OUTCOME_TAIL_MESSAGES, each capped) and are admitted whatever the
            // budget says: a digest that dropped the goal to fit an error would
            // ask the model to judge work it cannot see the point of.
            if tier != Priority::Pinned && spent + weight > DIGEST_BUDGET_CHARS {
                continue;
            }
            spent += weight;
            kept[index] = Some(body);
        }
    }
    stitch(&kept, evidence.friction)
}

/// Join the kept renderings in transcript order, replacing each run of dropped
/// messages with a marker saying how many went missing.
fn stitch(kept: &[Option<String>], friction: &TurnFriction) -> String {
    let mut out = friction.section();
    let elided = kept.iter().filter(|body| body.is_none()).count();
    if elided > 0 {
        if !out.is_empty() {
            out.push('\n');
        }
        let _ = writeln!(
            out,
            "The transcript below is a SELECTION of {} messages — {elided} are not \
             included, and every gap is marked. Do not assume anything about a gap; \
             if the evidence you need is inside one, say so instead.",
            kept.len()
        );
    }
    if !out.is_empty() {
        out.push('\n');
    }
    let mut run = 0usize;
    for body in kept {
        match body {
            Some(body) => {
                if run > 0 {
                    let _ = writeln!(out, "… {run} messages elided …");
                    run = 0;
                }
                out.push_str(body);
                out.push('\n');
            }
            None => run += 1,
        }
    }
    if run > 0 {
        let _ = writeln!(out, "… {run} messages elided …");
    }
    // Writing line-wise leaves a trailing newline; the digest is interpolated
    // into a prompt, where a stray blank line is noise.
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

/// `call_id` → tool name, from both sources that know one.
///
/// The transcript knows it whenever the assistant's tool-calling message is in
/// the slice; the event ledger knows it whenever a tap was wired. Neither is
/// sufficient alone — the staged pipeline path has the events and not the
/// messages — so the map is the union, transcript last so a name recorded in
/// both resolves to the transcript's.
fn call_names<'a>(evidence: TurnEvidence<'a>) -> HashMap<&'a str, &'a str> {
    let mut names: HashMap<&str, &str> = evidence.friction.call_names().collect();
    for message in evidence.transcript {
        for call in &message.tool_calls {
            names.insert(call.call_id.as_str(), call.name.as_str());
        }
    }
    names
}

/// Assign every message its budget tier.
fn classify(evidence: TurnEvidence<'_>) -> Vec<Priority> {
    let transcript = evidence.transcript;
    let flagged: HashSet<&str> = transcript
        .iter()
        .flat_map(|message| message.tool_results.iter())
        .filter(|result| result.output.is_error())
        .map(|result| result.call_id.as_str())
        .chain(evidence.friction.flagged())
        .collect();
    let outcome_from = transcript.len().saturating_sub(OUTCOME_TAIL_MESSAGES);
    let goal = transcript
        .iter()
        .position(|message| message.role == MessageRole::User);
    transcript
        .iter()
        .enumerate()
        .map(|(index, message)| {
            if index >= outcome_from || goal == Some(index) {
                return Priority::Pinned;
            }
            let carries_friction = message
                .tool_results
                .iter()
                .any(|result| flagged.contains(result.call_id.as_str()))
                || message
                    .tool_calls
                    .iter()
                    .any(|call| flagged.contains(call.call_id.as_str()));
            if carries_friction {
                Priority::Friction
            } else {
                Priority::Filler
            }
        })
        .collect()
}

/// Render one message, capped.
///
/// Returns the empty string for a message that is not turn evidence: a `System`
/// message is the prompt prefix, byte-stable by design (invariant 7) and
/// identical on every turn, so budget spent on it teaches reflection nothing it
/// did not already know last turn. Recalled context is a `User` message
/// (`memory/recall.rs::inject_recall_block`) and is therefore kept.
fn render(message: &CompletionMessage, names: &HashMap<&str, &str>, cap: usize) -> String {
    if message.role == MessageRole::System {
        return String::new();
    }
    let mut parts: Vec<String> = Vec::new();
    let content = clip(&message.content, cap);
    if !content.is_empty() {
        parts.push(content);
    }
    for call in &message.tool_calls {
        parts.push(format!(
            "→ calls {}({})",
            call.name,
            clip(&compact(&call.input), TOOL_INPUT_CHARS)
        ));
    }
    // The half the old digest could not see at any window size: a `Tool`
    // message's payload is `tool_results`, and its `content` is empty by
    // construction.
    let share = (cap / message.tool_results.len().max(1)).max(MIN_RESULT_CHARS);
    for result in &message.tool_results {
        let name = names
            .get(result.call_id.as_str())
            .copied()
            .unwrap_or("tool");
        parts.push(match &result.output {
            ToolOutput::Error { message } => format!("{name} FAILED: {}", clip(message, share)),
            ToolOutput::Ok { content } => format!("{name} ok: {}", clip(content, share)),
        });
    }
    prefixed(role_label(message.role), parts)
}

/// The digest's name for a message's author.
fn role_label(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

/// `role: part`, or `role: first` with the remaining parts indented beneath it.
///
/// Nothing at all when there are no parts. A message that would render as a bare
/// role label is dropped instead, because a bare label is exactly what the old
/// digest emitted for every tool result Stella ever produced: it reads like
/// evidence while carrying none.
fn prefixed(role: &str, parts: Vec<String>) -> String {
    let mut parts = parts.into_iter().filter(|part| !part.trim().is_empty());
    let Some(first) = parts.next() else {
        return String::new();
    };
    let mut out = format!("{role}: {first}");
    for part in parts {
        let _ = write!(out, "\n  {part}");
    }
    out
}

/// A tool call's arguments as one line. `Value::to_string` is already compact; a
/// string argument loses its quotes, because the digest is prose, not JSON.
fn compact(input: &serde_json::Value) -> String {
    match input {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Truncate on a character boundary, naming what was dropped.
///
/// The count matters: a model told "1.2k more characters" can report that the
/// evidence it needed was cut, which an unmarked truncation makes impossible.
fn clip(text: &str, cap: usize) -> String {
    let text = text.trim();
    let total = text.chars().count();
    if total <= cap {
        return text.to_string();
    }
    let head: String = text.chars().take(cap).collect();
    format!("{head}… [{} more characters]", total - cap)
}

/// `450ms`, `1.2s`, `3m18s` — the shape a person reads a duration in.
fn human_ms(ms: u64) -> String {
    if ms < 1_000 {
        return format!("{ms}ms");
    }
    let secs = ms / 1_000;
    if secs < 60 {
        return format!("{}.{}s", secs, (ms % 1_000) / 100);
    }
    format!("{}m{:02}s", secs / 60, secs % 60)
}

/// The wire token for a call role (`plan_repair`, not `PlanRepair`), so a
/// reflection's account of a turn greps against `stella-events.jsonl` — which
/// is the record anyone checking the claim will open.
fn role_token(role: ModelCallRole) -> String {
    match serde_json::to_value(role) {
        Ok(serde_json::Value::String(token)) => token,
        // `ModelCallRole` is a plain `#[serde(rename_all)]` enum, so this arm is
        // unreachable in practice. It is a label, not a fallback to reason
        // about: nothing branches on it.
        _ => "unknown".to_string(),
    }
}

/// `name (call_id)`, or just the id when the ledger never saw the call's start.
fn tool_label(pass: &ToolPass) -> String {
    if pass.name.is_empty() {
        return pass.call_id.clone();
    }
    format!("{} ({})", pass.name, pass.call_id)
}

/// One capped `- label:` block per non-empty list.
fn write_list(out: &mut String, label: &str, entries: &[String]) {
    if entries.is_empty() {
        return;
    }
    let _ = writeln!(out, "- {label} {} times:", entries.len());
    for entry in entries.iter().take(FRICTION_LIST_CAP) {
        let _ = writeln!(out, "  - {}", clip(entry, FRICTION_CHARS));
    }
    if entries.len() > FRICTION_LIST_CAP {
        let _ = writeln!(out, "  - … {} more", entries.len() - FRICTION_LIST_CAP);
    }
}
