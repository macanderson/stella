//! Per-feature attribution: turning one event into counters the A/B can
//! difference (#2382).
//!
//! The distiller's original counters answer "was the loop healthy?" — tool
//! calls, model calls, spend, a verdict. Only two features ever got a column
//! of their own, and every other feature in the tree
//! got none, so a paired ablation of recall, of the witness stage, of
//! subagents, of an adopted foundry tool produced **no number**: the events
//! were all there in `stella-events.jsonl`, and answering the question meant a
//! fresh `jq` pass written per experiment, run beside the report rather than
//! inside it. That is where a measurement artifact creeps in — a figure
//! computed over a different trial set than the one the report's pass rate
//! came from is not comparable with it, and nothing on the page says so.
//!
//! So attribution rides the trial and is folded by
//! [`stella_core::comparison`] over the same **paired** trials as every other
//! figure. See [`stella_core::comparison::feature`] for that contract; this
//! module owns only the vocabulary.
//!
//! # The vocabulary
//!
//! Every key is `<namespace>.<what>`, and the value counts exactly what the
//! key names — events, frames, tokens, entries. Units differ across keys on
//! purpose: a key is only ever compared against itself in the other arm.
//!
//! | Key | One per |
//! |---|---|
//! | `tool.<name>` | `tool_start`, keyed by the tool's own name |
//! | `stage.<kind>` | `stage`, keyed by the canonical [`StageKind`] tag |
//! | `recall.calls` / `.frames` / `.tokens` / `.frames_rejected` | `context_recall` — the event, its admitted frames, its injected tokens, and the frames the host rejected whole |
//! | `context_write.calls` / `.upserts` / `.superseded` | `context_write` and its two counts |
//! | `subagent.spawned` / `.refused` | `sub_agent` `started`, and a `finished` whose status is `refused` |
//!
//! Three choices in there are load-bearing:
//!
//! * **Tools are keyed by name, never by a class.** A foundry-authored tool is
//!   indistinguishable from a built-in on the wire — `ToolCall` carries a
//!   name and arguments and nothing about where the tool came from — so any
//!   `tool.foundry` counter would be this module *guessing* from a name it
//!   does not own, and a guess presented as an observation is the failure mode
//!   the whole harness exists to avoid. Keying by name reports what the stream
//!   actually says, and an operator ablating their adopted tool knows its
//!   name. It also gives every archived stream's tool names their own keys
//!   for free, which #2382 asked for on its own account: F3 and F5 were
//!   separate cards with separate kill criteria and could not share a bucket
//!   — an archived stream replayed today reports each tool under its own
//!   name.
//! * **Stages are canonicalized through [`StageKind`], not read as strings.**
//!   The verdict stage shipped on the wire as `judge` and is still aliased
//!   that way, so a raw-string key would split one stage across `stage.judge`
//!   and `stage.verdict` depending on when the stream was recorded — two half
//!   counters, each reading as a stage that half-fired.
//! * **A sub-agent phase is read off its tag, not deserialized whole.** The
//!   `Started` payload carries five fields today; parsing the variant would
//!   make the spawn counter fail closed the day a sixth is added, and a
//!   counter that silently loses spawns is worse than one keyed off the
//!   discriminator that has to stay stable anyway.
//!
//! # What is deliberately absent
//!
//! `no_evidence_cut` — recall's evidence-gate refusal count, which #2382 asks
//! for. It is real (`stella-context`'s `RetrievalResult`) but it **never
//! reaches the wire**: `AgentEvent::ContextRecall` carries the frames that
//! were admitted and, via `usage`, the ones the *host* rejected, and neither
//! is the evidence gate's refusal. Counting `recall.frames_rejected` and
//! calling it the evidence cut would be a different number wearing the name of
//! the one the experiment asked for. Tracked as #2398.

use std::collections::BTreeMap;

use serde_json::Value;
use stella_protocol::StageKind;

/// Counter keys, as [`stella_core::comparison::TrialRecord::features`] holds
/// them.
pub type FeatureCounts = BTreeMap<String, u64>;

/// Prefix for the per-tool-name counters.
const TOOL: &str = "tool.";
/// Prefix for the per-[`StageKind`] counters.
const STAGE: &str = "stage.";

/// Longest tool name that becomes a key verbatim.
///
/// Tool names are model-supplied runtime data and reach `tool_start` before
/// anything has vouched for them, so an absurd one must not become an absurd
/// map key in a published report. Truncation rather than a cap on distinct
/// keys: dropping a counter silently is how a report claims coverage it does
/// not have, and every real name in the catalog is far under this.
const MAX_TOOL_NAME: usize = 48;

/// Add one event's contribution to `counts`.
///
/// Unrecognized events contribute nothing — a stream from a newer stella is
/// read for what this build understands, never rejected. Missing fields
/// contribute zero for the same reason: a counter is an observation about what
/// the stream said, and a stream that said nothing about recall tokens has a
/// recall token count of zero, not an error.
pub fn record(event: &Value, counts: &mut FeatureCounts) {
    match event.get("type").and_then(Value::as_str) {
        Some("tool_start") => {
            if let Some(name) = event
                .get("call")
                .and_then(|call| call.get("name"))
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
            {
                bump(counts, &format!("{TOOL}{}", clamp(name)), 1);
            }
        }
        Some("stage") => {
            if let Some(name) = event.get("name").and_then(Value::as_str) {
                bump(counts, &format!("{STAGE}{}", canonical_stage(name)), 1);
            }
        }
        Some("context_recall") => {
            bump(counts, "recall.calls", 1);
            let frames = event
                .get("frames")
                .and_then(Value::as_array)
                .map_or(0, |frames| frames.len() as u64);
            bump(counts, "recall.frames", frames);
            bump(counts, "recall.tokens", u64_at(event, "tokens"));
            bump(counts, "recall.frames_rejected", frames_rejected(event));
        }
        Some("context_write") => {
            bump(counts, "context_write.calls", 1);
            bump(counts, "context_write.upserts", u64_at(event, "upserts"));
            bump(
                counts,
                "context_write.superseded",
                u64_at(event, "superseded"),
            );
        }
        Some("sub_agent") => record_subagent(event.get("phase"), counts),
        _ => {}
    }
}

/// A spawn, and separately a spawn that never ran.
///
/// A refusal (no budget headroom, the nesting cap) still brackets, so it is
/// counted in `subagent.spawned` too — the parent *asked*. Reporting the two
/// side by side is the difference between "subagents did nothing for this arm"
/// and "subagents were never allowed to run", which are opposite findings with
/// opposite fixes.
fn record_subagent(phase: Option<&Value>, counts: &mut FeatureCounts) {
    let Some(phase) = phase else { return };
    match phase.get("phase").and_then(Value::as_str) {
        Some("started") => bump(counts, "subagent.spawned", 1),
        Some("finished") if phase.get("status").and_then(Value::as_str) == Some("refused") => {
            bump(counts, "subagent.refused", 1);
        }
        _ => {}
    }
}

/// The frames the CGP host rejected whole, summed over the recall's providers.
///
/// Distinct from the evidence gate's `no_evidence_cut` (see the module docs):
/// this is the host refusing a provider's frame — a misdeclared cost, a
/// consent gate, a failed leg — which is a fact about the plumbing rather than
/// about relevance.
fn frames_rejected(event: &Value) -> u64 {
    event
        .get("usage")
        .and_then(|usage| usage.get("providers"))
        .and_then(Value::as_array)
        .map_or(0, |providers| {
            providers
                .iter()
                .map(|provider| u64_at(provider, "frames_rejected"))
                .fold(0u64, u64::saturating_add)
        })
}

/// The stage's canonical wire tag, resolving the `judge`/`verifier` aliases
/// onto `verdict`.
///
/// A tag this build does not know is kept verbatim rather than bucketed into
/// an "other": a newer stella's stage should appear under its own name, and
/// the operator reading a key they do not recognize has learned exactly the
/// true thing.
fn canonical_stage(name: &str) -> String {
    let Ok(kind) = serde_json::from_value::<StageKind>(Value::String(name.to_string())) else {
        return clamp(name);
    };
    match serde_json::to_value(kind) {
        Ok(Value::String(tag)) => tag,
        _ => clamp(name),
    }
}

fn clamp(name: &str) -> String {
    crate::truncate(name, MAX_TOOL_NAME)
}

fn u64_at(value: &Value, field: &str) -> u64 {
    value.get(field).and_then(Value::as_u64).unwrap_or(0)
}

/// Saturating, because these are counts folded over an unbounded stream and a
/// bench harness must not be the thing that overflows on a pathological trial.
fn bump(counts: &mut FeatureCounts, key: &str, by: u64) {
    if let Some(slot) = counts.get_mut(key) {
        *slot = slot.saturating_add(by);
    } else {
        counts.insert(key.to_string(), by);
    }
}

/// Fold `from` into `into` — how a multi-step trial's per-step counters become
/// the trial's.
pub fn merge(into: &mut FeatureCounts, from: &FeatureCounts) {
    for (key, count) in from {
        bump(into, key, *count);
    }
}
