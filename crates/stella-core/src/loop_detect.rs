//! Loop detection — pure synchronous analysis of recent tool calls and
//! the results they produced: plain synchronous functions over owned
//! data, easy to property-test, run by the step-driver alongside
//! compaction and budget eviction.
//!
//! A flat iteration cap alone burns the *entire* step budget before
//! giving up, even when the model got stuck after three steps. This
//! module gives the step-driver (`driver.rs`, which every CLI path
//! drives) a real, typed verdict it can act on early: steer or abort with
//! a clear reason instead of grinding to the cap.
//!
//! Three failure modes are detected, matching real agent stuck-loop
//! signatures:
//!
//! 1. **Exact repeat** — the same tool called with byte-identical input,
//!    over and over (`read_file` on the same path, `bash` re-running the
//!    same failing command).
//! 2. **Short cycle** — a fixed sequence of 2 to `MAX_CYCLE_PERIOD`
//!    distinct calls repeating with no other call interleaved
//!    (`read_file` → `edit_file` that keeps getting rejected →
//!    `read_file` again; or the period-3 read → failing edit → failing
//!    test grind). Invisible to exact-repeat detection because no single
//!    call repeats consecutively.
//! 3. **Stagnation** — the same tool called over and over with arguments
//!    that keep *changing*, every call still returning byte-identical
//!    output. Invisible to both checks above: no input repeats, so there is
//!    no exact repeat, and the arguments never settle into a fixed cycle
//!    either. This is the shape a model falls into when it is fiddling with
//!    one call's parameters instead of changing strategy — the observed
//!    case was `grep` on one file with a regex alternation the model kept
//!    reshuffling (`a|b|c|a|b` → `a|b|c|a`), 38 consecutive calls returning
//!    the same 906 bytes. Varying the arguments is not progress; learning
//!    something is, and byte-identical output proves nothing was learned.
//!
//! **Progress is part of the loop definition.** A repeat or cycle only
//! counts when the *outputs* are byte-identical too: identical input with
//! identical output means the model gained no new information, which is
//! the actual pathology. Identical input with *changing* output is
//! legitimate work — polling a running process, re-reading a file another
//! call just modified, re-running a test whose failures are shrinking —
//! and must never be flagged. That is why the detector consumes
//! [`CallRecord`]s (call + result) rather than bare calls.
//!
//! Calls are compared by **tool name + input + output**, deliberately
//! ignoring `ToolCall::call_id`. `ToolCall` derives `PartialEq` over *all*
//! fields including `call_id`, which providers assign fresh per call — two
//! semantically identical calls almost never share a `call_id`, so using
//! derived equality here would silently never fire. `same_record` below is
//! the one place that distinction is made.
//!
//! The output a record carries is a *live* conversation message, and the
//! conversation is not immutable: context compaction rewrites older tool
//! results in place. A caller that can preserve what a call really produced
//! passes it as [`CallRecord::identity`], and comparison uses that instead
//! of the (possibly rewritten) bytes — see `same_record`. This module stays
//! a pure function over owned data either way; it never learns what
//! compaction is.

use std::borrow::Cow;

use stella_protocol::{ToolCall, ToolOutput};

/// Longest trailing cycle period the short-cycle detector considers.
/// Real stuck signatures observed so far are periods 2 and 3 (the
/// read → failing edit → failing test grind); 4 adds headroom without
/// scanning for long "cycles" that are really just varied work.
const MAX_CYCLE_PERIOD: usize = 4;

/// One tool call paired with the output it produced — the unit the
/// detector inspects. `output` is `None` while unresolved (the call never
/// ran, or its result message is gone from the window); an unresolved
/// output never matches anything, because progress cannot be ruled out
/// without seeing the result.
///
/// `identity` is an optional stable id for the output the call *actually*
/// produced, supplied by the caller. It exists because the output in the
/// live conversation is not durable: context compaction rewrites older
/// tool results in place (dedup/supersession stubs, middle-out aging, the
/// eviction stub), so by the time the detector runs, a streak of
/// byte-identical outputs can look like `[stub, stub, real]` and no longer
/// compare equal (#554). A caller that snapshots each result's identity
/// *before* compaction can hand it in here and keep the evidence.
/// `None` means "no snapshot available" and falls back to comparing the
/// outputs themselves.
/// The output is a [`Cow`] because the caller normalizes it before comparing
/// (`driver::comparable_output` strips `read_file`'s volatile session-tally
/// footer) and that normalization changes nothing on the overwhelmingly common
/// path. Borrowing there is the whole point: this window is rebuilt on EVERY
/// step, so an owned output meant a full heap copy of every tool result in the
/// turn — quadratic in steps across a long turn — to compare bytes the
/// transcript was already holding.
///
/// The call is a [`Cow`] for the same quadratic-copy reason: the driver
/// borrows each call straight out of the transcript it is scanning (an
/// `edit_file` input carries whole old/new file chunks, so an owned
/// `ToolCall` re-cloned every step was the same cost the output's `Cow`
/// was introduced to eliminate), while test fixtures own theirs.
#[derive(Debug, Clone, PartialEq)]
pub struct CallRecord<'a> {
    pub call: Cow<'a, ToolCall>,
    pub output: Option<Cow<'a, ToolOutput>>,
    pub identity: Option<String>,
}

/// Threshold configuration for [`detect_loop`]. `Default` gives sensible
/// starting values; callers (the step-driver) may tune per session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopDetectionConfig {
    /// Consecutive identical (name + input + output) calls required to
    /// flag an exact-repeat loop. `0` or `1` disable exact-repeat
    /// detection — a single call can't be "repeated" by definition.
    pub exact_repeat_threshold: usize,
    /// Full cycles (of any period `2..=MAX_CYCLE_PERIOD`) required to
    /// flag a short-cycle loop. `0` disables short-cycle detection.
    ///
    /// `1` is legal but degenerate and should not be configured: "one full
    /// cycle" is satisfied by the trailing `period` records matching
    /// *themselves*, so any two consecutive resolved calls that are not
    /// identical (identical ones are caught by `detect_short_cycle`'s
    /// all-same guard and left to exact-repeat) read as a period-2 loop. A
    /// cycle is only evidence once it has actually recurred, so the lowest
    /// meaningful value is `2`.
    pub short_cycle_repeats: usize,
    /// Consecutive calls to the SAME tool that all produced byte-identical
    /// output — whatever their arguments — required to flag stagnation.
    /// `0` or `1` disable the check.
    ///
    /// Deliberately looser than [`Self::exact_repeat_threshold`]: an
    /// identical input repeated is unambiguous evidence, while a *varying*
    /// input is weaker on its own, so it takes more of it before the
    /// detector will speak. Six consecutive searches that each came back
    /// with the same bytes is a model turning knobs, not one exploring.
    pub stagnation_threshold: usize,
}

impl Default for LoopDetectionConfig {
    /// Three consecutive identical calls, or three full cycles — enough to
    /// rule out coincidence without flagging a legitimately-repeated
    /// read-then-fix-then-verify pattern (which changes some output every
    /// pass and so never matches anyway). These thresholds are the PRIMARY
    /// stuck-turn defense and fire orders of magnitude before the
    /// step-driver's belt-and-suspenders backstop
    /// (`EngineConfig::max_steps`, 200 by default), so a stuck turn costs
    /// a handful of wasted calls, never a whole cap's worth.
    ///
    /// Stagnation sits at double the exact-repeat threshold. It is the
    /// backstop for the two above rather than a peer of them: it asks only
    /// "did this tool tell us anything new", which is true of *every*
    /// stuck shape, so it must be the slowest to fire or it would preempt
    /// the tighter, better-described verdicts.
    fn default() -> Self {
        Self {
            exact_repeat_threshold: 3,
            short_cycle_repeats: 3,
            stagnation_threshold: 6,
        }
    }
}

/// Verdict returned by [`detect_loop`]. Never a bare bool — matching this
/// crate's convention of typed, inspectable outputs (`ToolOutput` in
/// `stella_protocol::tool` is never a bare string; `CompactionReport` in
/// `compaction.rs` is a named struct).
#[derive(Debug, Clone, PartialEq)]
pub enum LoopVerdict {
    /// No loop detected in the inspected window. The default/healthy
    /// verdict: empty history, history shorter than every configured
    /// threshold, genuinely varied history, and identical calls whose
    /// outputs kept changing (visible progress) all return this.
    NoLoop,
    /// The same tool call (name + byte-identical input) was made `count`
    /// times consecutively at the end of the inspected history, every time
    /// producing byte-identical output, at or above
    /// `LoopDetectionConfig::exact_repeat_threshold`.
    ExactRepeat {
        tool: String,
        input: serde_json::Value,
        count: usize,
    },
    /// A fixed sequence of `pattern.len()` calls (2 to
    /// `MAX_CYCLE_PERIOD`, in cycle order — oldest position first)
    /// repeated with no other call interleaved and byte-identical outputs
    /// at every position, for `repeats` full cycles at the end of the
    /// inspected history, at or above
    /// `LoopDetectionConfig::short_cycle_repeats`.
    ShortCycle {
        pattern: Vec<ToolCall>,
        repeats: usize,
    },
    /// `count` consecutive calls to `tool` at the end of the inspected
    /// history all produced byte-identical output, at or above
    /// `LoopDetectionConfig::stagnation_threshold`, while their inputs did
    /// NOT all match — the arguments moved and the answer did not.
    ///
    /// The weakest of the three verdicts and the last one checked: it makes
    /// no claim about the *shape* of the repetition, only that the tool
    /// stopped being informative.
    Stagnant { tool: String, count: usize },
}

impl LoopVerdict {
    /// `true` for any detected loop variant; `false` for `NoLoop`.
    pub fn is_loop(&self) -> bool {
        !matches!(self, LoopVerdict::NoLoop)
    }

    /// A human-readable evidence string for the driver to surface when it
    /// steers or aborts. `None` for `NoLoop`.
    pub fn evidence(&self) -> Option<String> {
        match self {
            LoopVerdict::NoLoop => None,
            LoopVerdict::ExactRepeat { tool, input, count } => Some(format!(
                "the same `{tool}` call with identical arguments repeated {count} times \
                 consecutively, producing byte-identical output every time (input: {input})"
            )),
            LoopVerdict::ShortCycle { pattern, repeats } => {
                let names: Vec<String> = pattern.iter().map(|c| format!("`{}`", c.name)).collect();
                Some(format!(
                    "calls cycled through {} for {repeats} cycles with byte-identical \
                     outputs and no progress",
                    names.join(" → ")
                ))
            }
            LoopVerdict::Stagnant { tool, count } => Some(format!(
                "the last {count} `{tool}` calls used DIFFERENT arguments but every one \
                 returned byte-identical output — varying the arguments is not learning \
                 anything, so this tool has nothing left to tell you here"
            )),
        }
    }

    /// The detected loop's [`LoopIdentity`], for deciding whether a later
    /// detection in the same turn is *this* loop again or a fresh one.
    /// `None` only for [`LoopVerdict::NoLoop`], which is not a loop to
    /// identify.
    #[must_use]
    pub fn identity(&self) -> Option<LoopIdentity> {
        match self {
            LoopVerdict::NoLoop => None,
            LoopVerdict::ExactRepeat { tool, input, .. } => Some(LoopIdentity {
                tools: vec![tool.clone()],
                inputs: Some(vec![input.to_string()]),
            }),
            LoopVerdict::ShortCycle { pattern, .. } => Some(LoopIdentity {
                tools: pattern.iter().map(|c| c.name.clone()).collect(),
                inputs: Some(pattern.iter().map(|c| c.input.to_string()).collect()),
            }),
            // Stagnation is the one verdict whose loop spans *differing*
            // arguments, so naming any of them would misidentify it. The
            // tool is the whole identity, and an empty (not absent) input
            // list is how that is said.
            LoopVerdict::Stagnant { tool, .. } => Some(LoopIdentity {
                tools: vec![tool.clone()],
                inputs: Some(Vec::new()),
            }),
        }
    }
}

/// What makes two loop detections *the same loop*.
///
/// Not the tool name on its own. `bash` is the tool most loops are made of,
/// so tool-name identity says every `bash` loop in a turn is one loop, and
/// the steer→abort ladder above it then blames a warning the model *obeyed*
/// for a loop it was never told about. That is #1524's defect one level
/// down: that fix taught the ladder to tell a `write_file` loop from an
/// `edit_file` one, and still could not tell two `bash` calls apart. Its
/// witness is a real run — steered about
/// `grep -n "fn open\b" … store.rs`, the model changed command as asked,
/// looped on `grep -n "fn index_all" … graph.rs`, and was killed by
/// "persisted after a steering warning".
///
/// Identity mirrors what the detector actually keyed on, so the two can
/// never disagree about what a loop is: [`LoopVerdict::ExactRepeat`] and
/// [`LoopVerdict::ShortCycle`] are keyed on byte-identical *input*, so the
/// input is part of the loop; [`LoopVerdict::Stagnant`] spans deliberately
/// varying inputs, so for it the tool alone is the whole loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopIdentity {
    /// The tools in the loop, in cycle order — what a message names.
    pub tools: Vec<String>,
    /// The arguments the loop repeated, as JSON, one per entry of
    /// [`Self::tools`]. Empty when arguments are not part of this loop's
    /// identity, which is exactly the `Stagnant` verdict.
    ///
    /// `None` means the arguments were never *recorded* — not that there
    /// were none. It is what a turn resumed from a checkpoint written
    /// before identities carried arguments knows about the warning it
    /// already spent, and such an identity can never be compared
    /// ([`Self::same_loop_as`]).
    pub inputs: Option<Vec<String>>,
}

impl LoopIdentity {
    /// Whether `other` is a re-detection of this same loop — or `None` when
    /// this identity is too incomplete to say, because its arguments were
    /// never recorded.
    ///
    /// Three answers, not two, and callers must handle all three: the only
    /// honest thing to say about a loop you cannot identify is nothing. A
    /// `bool` here would force "unknown" to collapse into one of the two
    /// claims, which is precisely the bug this type exists to end.
    #[must_use]
    pub fn same_loop_as(&self, other: &LoopIdentity) -> Option<bool> {
        let (mine, theirs) = (self.inputs.as_ref()?, other.inputs.as_ref()?);
        Some(self.tools == other.tools && mine == theirs)
    }

    /// How to name this loop in a message: its tools, plus the arguments
    /// when the loop is one repeated call.
    ///
    /// The arguments are what make the name useful — two `bash` loops read
    /// as the same "about `bash`" without them, so a message distinguishing
    /// them would look self-contradictory. They are truncated because a
    /// looping `edit_file` input carries whole file chunks, and a reason
    /// line is read by a human. A cycle's inputs are left out for the same
    /// reason: the tool sequence already tells cycles apart.
    #[must_use]
    pub fn describe(&self) -> String {
        let tools = self
            .tools
            .iter()
            .map(|t| format!("`{t}`"))
            .collect::<Vec<_>>()
            .join(" → ");
        match self.inputs.as_deref() {
            Some([input]) => format!("{tools} with {}", truncate(input, MAX_DESCRIBED_INPUT)),
            _ => tools,
        }
    }
}

/// How much of a looped call's arguments a reason line quotes. Enough to
/// tell two commands or two paths apart, short enough that the sentence
/// around it survives.
const MAX_DESCRIBED_INPUT: usize = 160;

/// `s` capped at `max` *characters* (never bytes — a cut inside a multi-byte
/// char panics), with an ellipsis marking what was dropped.
fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        None => s.to_string(),
        Some((cut, _)) => format!("{}…", &s[..cut]),
    }
}

/// Two records are "the same" for loop-detection purposes iff the tool
/// name, the JSON input, AND the produced output all match exactly.
/// Comparing name alone would false-positive on legitimate repeated calls
/// to the same tool with different arguments (`read_file` on two
/// different paths); comparing name + input alone would false-positive on
/// legitimate polling (`read_output` on the same handle returning new
/// bytes every call). An unresolved output (`None`) matches nothing —
/// including another `None` — because a loop can only be *proven* by two
/// observed identical outputs.
///
/// When BOTH records carry a [`CallRecord::identity`], the identities are
/// compared instead of the output bytes: the identity is the caller's
/// snapshot of what the call really produced, and the live output may have
/// been rewritten in place by compaction since (#554). One or both
/// identities missing falls back to the output bytes. Either way an
/// unresolved output still matches nothing — an identity is evidence about
/// an output that was observed, never a substitute for observing one.
fn same_record(a: &CallRecord<'_>, b: &CallRecord<'_>) -> bool {
    a.call.name == b.call.name && a.call.input == b.call.input && same_output(a, b)
}

/// Whether two records produced the same *information*, ignoring what was
/// asked. Split out of [`same_record`] because [`detect_stagnation`] needs
/// exactly this half: it asks "did the tool tell us anything new" without
/// asking whether the arguments matched. Every rule stated in `same_record`'s
/// docs about identities and unresolved outputs lives here, so the two checks
/// can never drift apart on what "the same output" means.
fn same_output(a: &CallRecord<'_>, b: &CallRecord<'_>) -> bool {
    match (&a.output, &b.output) {
        (Some(a_out), Some(b_out)) => match (&a.identity, &b.identity) {
            (Some(a_id), Some(b_id)) => a_id == b_id,
            _ => a_out == b_out,
        },
        _ => false,
    }
}

/// Inspect the tail of recent tool calls for a non-progress loop.
/// `records` should be the recent window of [`CallRecord`]s in
/// chronological order (oldest first, most recent last) — the caller
/// decides how much history to hand in; a few dozen calls is plenty since
/// both checks only look at the trailing run.
///
/// Checks, in order:
/// 1. **Exact repeat** (see the module docs). Checked first so the tightest
///    classification of the evidence wins — `detect_short_cycle`'s all-same
///    guard already refuses to report a run of identical calls as a cycle,
///    so this ordering decides which verdict the caller sees, not how two
///    overlapping classifications get disentangled.
/// 2. **Short cycle** (see the module docs), shortest period first so the
///    tightest description of the evidence wins (a period-2 loop is never
///    reported as the period-4 loop it also technically is).
/// 3. **Stagnation** (see the module docs), checked LAST because it is the
///    weakest claim: "this tool stopped being informative" is true of every
///    exact repeat and every same-tool cycle too, so checking it earlier
///    would swallow both of the better-described verdicts above.
///
/// Never panics on any input — empty history, a single call, history
/// shorter than every threshold, and a zeroed-out `config` (which disables
/// every check) all return `NoLoop` rather than indexing out of bounds.
pub fn detect_loop(records: &[CallRecord<'_>], config: LoopDetectionConfig) -> LoopVerdict {
    if let Some(verdict) = detect_exact_repeat(records, config.exact_repeat_threshold) {
        return verdict;
    }
    if let Some(verdict) = detect_short_cycle(records, config.short_cycle_repeats) {
        return verdict;
    }
    if let Some(verdict) = detect_stagnation(records, config.stagnation_threshold) {
        return verdict;
    }
    LoopVerdict::NoLoop
}

/// Count the trailing run of records identical (by [`same_record`]) to the
/// last record; report `ExactRepeat` if that run is `>= threshold`.
/// `threshold < 2` and empty `records` both return `None` (no detection).
fn detect_exact_repeat(records: &[CallRecord<'_>], threshold: usize) -> Option<LoopVerdict> {
    if threshold < 2 {
        return None;
    }
    let last = records.last()?;
    let count = records
        .iter()
        .rev()
        .take_while(|record| same_record(record, last))
        .count();
    if count >= threshold {
        Some(LoopVerdict::ExactRepeat {
            tool: last.call.name.clone(),
            input: last.call.input.clone(),
            count,
        })
    } else {
        None
    }
}

/// For each period `2..=MAX_CYCLE_PERIOD` (shortest first), count how
/// far the trailing history repeats its last `period` records; report
/// `ShortCycle` if any period spans `>= repeats_threshold` full cycles.
/// `repeats_threshold == 0`, history too short for every period, and a
/// candidate pattern that is itself an exact repeat (one record against
/// itself, not distinct calls) all return `None`.
fn detect_short_cycle(records: &[CallRecord<'_>], repeats_threshold: usize) -> Option<LoopVerdict> {
    if repeats_threshold == 0 {
        return None;
    }
    for period in 2..=MAX_CYCLE_PERIOD {
        // Reaching the threshold takes `period * repeats_threshold`
        // records; longer periods need strictly more, so stop entirely.
        // Saturating: the module's contract is "never panics on any input",
        // and a pathological threshold must not overflow the multiply in a
        // debug build.
        if records.len() < period.saturating_mul(repeats_threshold) {
            break;
        }
        let pattern = &records[records.len() - period..];
        // A run of one record repeating against itself is exact-repeat's
        // territory, not a genuine cycle of distinct calls.
        if pattern
            .iter()
            .all(|record| same_record(record, &pattern[0]))
        {
            continue;
        }

        // Walk backward from the end: the record at reverse-offset `o`
        // must match the pattern position `period - 1 - (o % period)`
        // (the pattern itself is stored oldest-first). Count how many
        // records in a row satisfy that alternation.
        let mut matched = 0usize;
        for (offset, record) in records.iter().rev().enumerate() {
            let expected = &pattern[period - 1 - (offset % period)];
            if same_record(record, expected) {
                matched += 1;
            } else {
                break;
            }
        }

        let repeats = matched / period;
        if repeats >= repeats_threshold {
            return Some(LoopVerdict::ShortCycle {
                pattern: pattern
                    .iter()
                    .map(|record| record.call.as_ref().clone())
                    .collect(),
                repeats,
            });
        }
    }
    None
}

/// Count the trailing run of records that call the same tool as the last
/// record AND produced the same output as it ([`same_output`]); report
/// `Stagnant` if that run is `>= threshold` and its inputs are not all
/// identical. `threshold < 2` and empty `records` both return `None`.
///
/// The all-same-input guard hands a run of byte-identical calls back to
/// [`detect_exact_repeat`], which describes it far better — mirroring
/// [`detect_short_cycle`]'s all-same guard, and for the same reason: with
/// exact-repeat detection disabled (`threshold < 2`) this check would
/// otherwise start reporting exact repeats under a vaguer name.
///
/// Note what is deliberately NOT required: that the inputs differ from each
/// other pairwise, or differ in any particular way. A run of `a, a, b, b, c`
/// stagnates just as a run of `a, b, c, d, e` does — what makes it stagnation
/// is that none of them moved the answer.
fn detect_stagnation(records: &[CallRecord<'_>], threshold: usize) -> Option<LoopVerdict> {
    if threshold < 2 {
        return None;
    }
    let last = records.last()?;
    // `same_output(last, last)` is false for an unresolved output, so a
    // trailing call whose result is not in the window yields a count of 0 and
    // never fires — the same "a loop can only be PROVEN by observed outputs"
    // rule the other two checks follow.
    let count = records
        .iter()
        .rev()
        .take_while(|record| record.call.name == last.call.name && same_output(record, last))
        .count();
    if count < threshold {
        return None;
    }
    let run = &records[records.len() - count..];
    if run
        .iter()
        .all(|record| record.call.input == last.call.input)
    {
        return None;
    }
    Some(LoopVerdict::Stagnant {
        tool: last.call.name.clone(),
        count,
    })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn record(
        name: &str,
        input: serde_json::Value,
        output: Option<ToolOutput>,
    ) -> CallRecord<'static> {
        // Distinct `call_id` per invocation, on purpose — providers never
        // reuse call ids, and the detector must not depend on them.
        static NEXT_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        CallRecord {
            call: Cow::Owned(ToolCall {
                call_id: format!("call_{id}"),
                name: name.into(),
                input,
            }),
            output: output.map(Cow::Owned),
            // No snapshot: these fixtures exercise the raw-output
            // comparison. The identity path has its own witness in
            // `driver::tests::audit_fixes`.
            identity: None,
        }
    }

    /// The same record with a caller-supplied output identity attached —
    /// the shape the driver builds once it has a pre-compaction snapshot.
    fn with_identity(mut record: CallRecord<'static>, identity: &str) -> CallRecord<'static> {
        record.identity = Some(identity.to_string());
        record
    }

    fn call(name: &str, input: serde_json::Value, output: &str) -> CallRecord<'static> {
        record(
            name,
            input,
            Some(ToolOutput::Ok {
                content: output.into(),
            }),
        )
    }

    /// Re-reading an unchanged file: same input, same output — the classic
    /// no-progress ingredient.
    fn read(path: &str) -> CallRecord<'static> {
        call(
            "read_file",
            serde_json::json!({ "path": path }),
            "fn main() {}",
        )
    }

    /// An edit that keeps failing the same way: same input, same error.
    fn edit(path: &str) -> CallRecord<'static> {
        record(
            "edit_file",
            serde_json::json!({ "path": path, "old": "x", "new": "y" }),
            Some(ToolOutput::Error {
                message: "old text not found".into(),
            }),
        )
    }

    #[test]
    fn empty_history_is_never_a_loop() {
        let verdict = detect_loop(&[], LoopDetectionConfig::default());
        assert_eq!(verdict, LoopVerdict::NoLoop);
        assert!(!verdict.is_loop());
    }

    #[test]
    fn single_call_is_never_a_loop() {
        let records = vec![read("a.rs")];
        assert_eq!(
            detect_loop(&records, LoopDetectionConfig::default()),
            LoopVerdict::NoLoop
        );
    }

    #[test]
    fn history_shorter_than_exact_repeat_threshold_is_not_a_loop() {
        // Two identical calls, but the threshold requires three.
        let records = vec![read("a.rs"), read("a.rs")];
        let config = LoopDetectionConfig {
            exact_repeat_threshold: 3,
            short_cycle_repeats: 100, // disable short-cycle for this test
            stagnation_threshold: 0,  // disabled: this test isolates another check
        };
        assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
    }

    #[test]
    fn exact_repeat_at_threshold_is_detected() {
        let records = vec![read("a.rs"), read("a.rs"), read("a.rs")];
        let config = LoopDetectionConfig {
            exact_repeat_threshold: 3,
            short_cycle_repeats: 100,
            stagnation_threshold: 0, // disabled: this test isolates another check
        };
        let verdict = detect_loop(&records, config);
        assert_eq!(
            verdict,
            LoopVerdict::ExactRepeat {
                tool: "read_file".into(),
                input: serde_json::json!({ "path": "a.rs" }),
                count: 3,
            }
        );
        assert!(verdict.is_loop());
    }

    #[test]
    fn exact_repeat_above_threshold_reports_full_count() {
        // Five in a row, threshold 3 — the full count is reported, not
        // capped at the threshold.
        let records = vec![read("a.rs"); 5];
        let config = LoopDetectionConfig {
            exact_repeat_threshold: 3,
            short_cycle_repeats: 100,
            stagnation_threshold: 0, // disabled: this test isolates another check
        };
        match detect_loop(&records, config) {
            LoopVerdict::ExactRepeat { count, .. } => assert_eq!(count, 5),
            other => panic!("expected ExactRepeat, got {other:?}"),
        }
    }

    #[test]
    fn identical_input_with_changing_output_is_not_a_loop() {
        // Polling a still-running process: `read_output` on the same
        // handle, no cursor field — the input is byte-identical every
        // time, but each poll returns new bytes. Visible progress, never a
        // loop, however long it goes on.
        let records: Vec<CallRecord<'static>> = (0..10)
            .map(|i| {
                call(
                    "read_output",
                    serde_json::json!({ "handle": "proc-5" }),
                    &format!("[{i}s] compiling stella-core..."),
                )
            })
            .collect();
        assert_eq!(
            detect_loop(&records, LoopDetectionConfig::default()),
            LoopVerdict::NoLoop
        );
    }

    #[test]
    fn identical_input_with_unresolved_output_is_not_a_loop() {
        // No result observed (the result message is gone from the window,
        // or the call never ran): progress cannot be ruled out, so
        // repetition alone is not evidence.
        let records: Vec<CallRecord<'static>> = (0..5)
            .map(|_| record("read_file", serde_json::json!({ "path": "a.rs" }), None))
            .collect();
        assert_eq!(
            detect_loop(&records, LoopDetectionConfig::default()),
            LoopVerdict::NoLoop
        );
    }

    #[test]
    fn different_arguments_to_the_same_tool_is_not_a_loop() {
        // Same tool name every time, but a different path each call — must
        // compare full input, not just tool name.
        let records = vec![read("a.rs"), read("b.rs"), read("c.rs"), read("d.rs")];
        let config = LoopDetectionConfig {
            exact_repeat_threshold: 3,
            short_cycle_repeats: 100,
            stagnation_threshold: 0, // disabled: this test isolates another check
        };
        assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
    }

    #[test]
    fn call_id_is_ignored_when_comparing_calls() {
        // `record()` assigns a fresh call_id every time; ToolCall's derived
        // PartialEq would see these as all-different. The detector must
        // still catch the repeat.
        let records: Vec<CallRecord<'static>> = (0..3).map(|_| read("a.rs")).collect();
        let ids: std::collections::HashSet<_> =
            records.iter().map(|r| r.call.call_id.clone()).collect();
        assert_eq!(ids.len(), 3, "test fixture sanity: call_ids must differ");

        let config = LoopDetectionConfig {
            exact_repeat_threshold: 3,
            short_cycle_repeats: 100,
            stagnation_threshold: 0, // disabled: this test isolates another check
        };
        assert!(detect_loop(&records, config).is_loop());
    }

    #[test]
    fn matching_identities_outrank_outputs_rewritten_since() {
        // The #554 shape: the two older results were stubbed in place by
        // compaction, so their live outputs no longer match the newest —
        // but all three carry the same pre-compaction identity.
        let stub = |path: &str| {
            with_identity(
                call(
                    "read_file",
                    serde_json::json!({ "path": path }),
                    "[evicted]",
                ),
                "blk_same",
            )
        };
        let records = vec![
            stub("a.rs"),
            stub("a.rs"),
            with_identity(read("a.rs"), "blk_same"),
        ];
        let config = LoopDetectionConfig {
            exact_repeat_threshold: 3,
            short_cycle_repeats: 100,
            stagnation_threshold: 0, // disabled: this test isolates another check
        };
        assert!(
            detect_loop(&records, config).is_loop(),
            "identical identities must count as a repeat even when the live outputs differ"
        );
    }

    #[test]
    fn differing_identities_outrank_outputs_collapsed_since() {
        // The mirror image: three DIFFERENT outputs all rewritten to the
        // same stub. Comparing bytes alone would now call this a loop; the
        // identities prove each call produced different information.
        let stub = |identity: &str| {
            with_identity(
                call(
                    "read_file",
                    serde_json::json!({ "path": "a.rs" }),
                    "[evicted]",
                ),
                identity,
            )
        };
        let records = vec![stub("blk_1"), stub("blk_2"), stub("blk_3")];
        let config = LoopDetectionConfig {
            exact_repeat_threshold: 3,
            short_cycle_repeats: 100,
            stagnation_threshold: 0, // disabled: this test isolates another check
        };
        assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
    }

    #[test]
    fn an_identity_never_resurrects_an_unresolved_output() {
        // An identity is evidence ABOUT an observed output, never a
        // substitute for observing one: `None` still matches nothing.
        let unresolved = || {
            with_identity(
                record("read_file", serde_json::json!({ "path": "a.rs" }), None),
                "blk_same",
            )
        };
        let records = vec![unresolved(), unresolved(), unresolved()];
        let config = LoopDetectionConfig {
            exact_repeat_threshold: 3,
            short_cycle_repeats: 100,
            stagnation_threshold: 0, // disabled: this test isolates another check
        };
        assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
    }

    #[test]
    fn one_sided_identity_falls_back_to_comparing_outputs() {
        // Only some records were snapshotted (a result that entered the
        // window after the snapshot). Comparison degrades to the outputs
        // rather than silently failing to match.
        let records = vec![
            with_identity(read("a.rs"), "blk_same"),
            read("a.rs"),
            read("a.rs"),
        ];
        let config = LoopDetectionConfig {
            exact_repeat_threshold: 3,
            short_cycle_repeats: 100,
            stagnation_threshold: 0, // disabled: this test isolates another check
        };
        assert!(detect_loop(&records, config).is_loop());
    }

    #[test]
    fn short_cycle_below_threshold_is_not_a_loop() {
        // Only two full A-B cycles; threshold requires three.
        let records = vec![read("a.rs"), edit("a.rs"), read("a.rs"), edit("a.rs")];
        let config = LoopDetectionConfig {
            exact_repeat_threshold: 100, // disable exact-repeat for this test
            short_cycle_repeats: 3,
            stagnation_threshold: 0, // disabled: this test isolates another check
        };
        assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
    }

    #[test]
    fn short_cycle_at_threshold_is_detected() {
        // read, edit, read, edit, read, edit — 3 full cycles, the "read
        // file / edit rejected or no-op / read again" failure mode.
        let records = vec![
            read("a.rs"),
            edit("a.rs"),
            read("a.rs"),
            edit("a.rs"),
            read("a.rs"),
            edit("a.rs"),
        ];
        let config = LoopDetectionConfig {
            exact_repeat_threshold: 100,
            short_cycle_repeats: 3,
            stagnation_threshold: 0, // disabled: this test isolates another check
        };
        let verdict = detect_loop(&records, config);
        match &verdict {
            LoopVerdict::ShortCycle { pattern, repeats } => {
                assert_eq!(*repeats, 3);
                let names: Vec<&str> = pattern.iter().map(|c| c.name.as_str()).collect();
                assert_eq!(names, ["read_file", "edit_file"]);
            }
            other => panic!("expected ShortCycle, got {other:?}"),
        }
        assert!(verdict.is_loop());
    }

    #[test]
    fn short_cycle_above_threshold_reports_full_repeat_count() {
        let mut records = Vec::new();
        for _ in 0..5 {
            records.push(read("a.rs"));
            records.push(edit("a.rs"));
        }
        let config = LoopDetectionConfig {
            exact_repeat_threshold: 100,
            short_cycle_repeats: 3,
            stagnation_threshold: 0, // disabled: this test isolates another check
        };
        match detect_loop(&records, config) {
            LoopVerdict::ShortCycle { repeats, .. } => assert_eq!(repeats, 5),
            other => panic!("expected ShortCycle, got {other:?}"),
        }
    }

    #[test]
    fn two_distinct_calls_alternating_with_changing_outputs_is_not_a_loop() {
        // The "correct" polling alternation: read_output / bash sleep,
        // read_output / bash sleep — identical inputs at every position,
        // but each poll returns new bytes. Progress, not a cycle.
        let mut records = Vec::new();
        for i in 0..6 {
            records.push(call(
                "read_output",
                serde_json::json!({ "handle": "proc-5" }),
                &format!("[{i}s] compiling..."),
            ));
            records.push(call("bash", serde_json::json!({ "cmd": "sleep 5" }), ""));
        }
        assert_eq!(
            detect_loop(&records, LoopDetectionConfig::default()),
            LoopVerdict::NoLoop
        );
    }

    #[test]
    fn period_three_cycle_with_identical_outputs_is_detected() {
        // The most common real stuck signature: read → failing edit →
        // failing test, over and over, nothing changing.
        let cycle = || {
            vec![
                read("a.rs"),
                edit("a.rs"),
                call(
                    "bash",
                    serde_json::json!({ "cmd": "cargo test" }),
                    "2 failed",
                ),
            ]
        };
        let records: Vec<CallRecord<'static>> = (0..3).flat_map(|_| cycle()).collect();
        let config = LoopDetectionConfig {
            exact_repeat_threshold: 100,
            short_cycle_repeats: 3,
            stagnation_threshold: 0, // disabled: this test isolates another check
        };
        let verdict = detect_loop(&records, config);
        match &verdict {
            LoopVerdict::ShortCycle { pattern, repeats } => {
                assert_eq!(*repeats, 3);
                let names: Vec<&str> = pattern.iter().map(|c| c.name.as_str()).collect();
                assert_eq!(names, ["read_file", "edit_file", "bash"]);
            }
            other => panic!("expected a period-3 ShortCycle, got {other:?}"),
        }
    }

    #[test]
    fn period_three_cycle_with_differing_outputs_is_not_a_loop() {
        // The same A, B, C shape — but the test output improves every
        // cycle (2 failed → 1 failed → 0 failed). That is a productive
        // fix loop, not a stuck one.
        let cycle = |failures: usize| {
            vec![
                read("a.rs"),
                edit("a.rs"),
                call(
                    "bash",
                    serde_json::json!({ "cmd": "cargo test" }),
                    &format!("{failures} failed"),
                ),
            ]
        };
        let records: Vec<CallRecord<'static>> = (0..3).rev().flat_map(cycle).collect();
        let config = LoopDetectionConfig {
            exact_repeat_threshold: 100,
            short_cycle_repeats: 2,
            stagnation_threshold: 0, // disabled: this test isolates another check
        };
        assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
    }

    #[test]
    fn period_four_cycle_with_identical_outputs_is_detected() {
        let cycle = || {
            vec![
                read("a.rs"),
                read("b.rs"),
                edit("a.rs"),
                call(
                    "bash",
                    serde_json::json!({ "cmd": "cargo test" }),
                    "2 failed",
                ),
            ]
        };
        let records: Vec<CallRecord<'static>> = (0..2).flat_map(|_| cycle()).collect();
        let config = LoopDetectionConfig {
            exact_repeat_threshold: 100,
            short_cycle_repeats: 2,
            stagnation_threshold: 0, // disabled: this test isolates another check
        };
        match detect_loop(&records, config) {
            LoopVerdict::ShortCycle { pattern, repeats } => {
                assert_eq!(repeats, 2);
                assert_eq!(pattern.len(), 4);
            }
            other => panic!("expected a period-4 ShortCycle, got {other:?}"),
        }
    }

    #[test]
    fn period_five_cycle_is_beyond_the_detector() {
        // Five distinct calls repeating: outside MAX_CYCLE_PERIOD, left to
        // the step cap. Pins the k <= 4 bound.
        let cycle = || {
            vec![
                read("a.rs"),
                read("b.rs"),
                read("c.rs"),
                edit("a.rs"),
                call(
                    "bash",
                    serde_json::json!({ "cmd": "cargo test" }),
                    "2 failed",
                ),
            ]
        };
        let records: Vec<CallRecord<'static>> = (0..3).flat_map(|_| cycle()).collect();
        let config = LoopDetectionConfig {
            exact_repeat_threshold: 100,
            short_cycle_repeats: 2,
            stagnation_threshold: 0, // disabled: this test isolates another check
        };
        assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
    }

    #[test]
    fn identical_calls_repeated_are_not_misreported_as_a_short_cycle() {
        // Six identical calls: exact-repeat detection is disabled here so
        // we can prove detect_short_cycle's own all-same guard holds at
        // every period — this must stay NoLoop, not a degenerate
        // ShortCycle{X, X}.
        let records = vec![read("a.rs"); 6];
        let config = LoopDetectionConfig {
            exact_repeat_threshold: 0, // disabled
            short_cycle_repeats: 1,
            stagnation_threshold: 0, // disabled: this test isolates another check
        };
        assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
    }

    #[test]
    fn drift_attributed_edit_recovery_is_not_a_loop() {
        // #331: when `edit_file` attributes a match failure to an
        // out-of-band file change, the error embeds the CURRENT content —
        // so as long as the file keeps changing, the outputs differ and the
        // legitimate read→edit-retry recovery is progress by construction.
        // Only when the file stops changing (and the model keeps replaying
        // the same failing edit verbatim) do outputs repeat byte-identically
        // and detection rightly fires. Pins the contract that the drift echo
        // is what keeps recovery progress-eligible.
        let drift_fail = |content: &str| {
            record(
                "edit_file",
                serde_json::json!({ "path": "a.rs", "old": "x", "new": "y" }),
                Some(ToolOutput::Error {
                    message: format!("old_string not found — the file CHANGED; current: {content}"),
                }),
            )
        };
        let read_v =
            |content: &str| call("read_file", serde_json::json!({ "path": "a.rs" }), content);
        let records = vec![
            read_v("v1"),
            drift_fail("v2"),
            read_v("v2"),
            drift_fail("v3"),
            read_v("v3"),
            drift_fail("v4"),
        ];
        assert_eq!(
            detect_loop(&records, LoopDetectionConfig::default()),
            LoopVerdict::NoLoop
        );
    }

    #[test]
    fn healthy_varied_sequence_is_not_a_loop() {
        // A realistic productive trajectory: no false positives.
        let records = vec![
            read("src/lib.rs"),
            call(
                "grep",
                serde_json::json!({ "pattern": "fn run_turn" }),
                "src/agent.rs:42",
            ),
            read("src/agent.rs"),
            edit("src/agent.rs"),
            call(
                "bash",
                serde_json::json!({ "cmd": "cargo test -p stella-cli" }),
                "2 failed",
            ),
            read("src/agent.rs"),
            edit("src/agent.rs"),
            call(
                "bash",
                serde_json::json!({ "cmd": "cargo test -p stella-cli -- --nocapture" }),
                "1 failed",
            ),
        ];
        assert_eq!(
            detect_loop(&records, LoopDetectionConfig::default()),
            LoopVerdict::NoLoop
        );
    }

    #[test]
    fn exact_repeat_takes_precedence_over_a_would_be_cycle_read() {
        // The trailing three calls are an exact repeat; that a short-cycle
        // check might also find something (given a different config) is
        // irrelevant — exact-repeat is checked first and wins.
        let records = vec![edit("a.rs"), read("a.rs"), read("a.rs"), read("a.rs")];
        let config = LoopDetectionConfig::default(); // 3/3
        match detect_loop(&records, config) {
            LoopVerdict::ExactRepeat { count, tool, .. } => {
                assert_eq!(count, 3);
                assert_eq!(tool, "read_file");
            }
            other => panic!("expected ExactRepeat to win, got {other:?}"),
        }
    }

    #[test]
    fn zero_or_one_exact_repeat_threshold_disables_that_check() {
        let records = vec![read("a.rs"); 10];
        for threshold in [0, 1] {
            let config = LoopDetectionConfig {
                exact_repeat_threshold: threshold,
                short_cycle_repeats: 0,  // also disabled, so overall NoLoop
                stagnation_threshold: 0, // ditto
            };
            assert_eq!(
                detect_loop(&records, config),
                LoopVerdict::NoLoop,
                "threshold {threshold} should disable exact-repeat detection"
            );
        }
    }

    #[test]
    fn zero_short_cycle_repeats_disables_that_check() {
        let records = vec![
            read("a.rs"),
            edit("a.rs"),
            read("a.rs"),
            edit("a.rs"),
            read("a.rs"),
            edit("a.rs"),
        ];
        let config = LoopDetectionConfig {
            exact_repeat_threshold: 0,
            short_cycle_repeats: 0,
            stagnation_threshold: 0, // disabled: this test isolates another check
        };
        assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
    }

    #[test]
    fn pathological_thresholds_do_not_overflow_the_cycle_arithmetic() {
        // The "never panics on any input" contract covers the config too: a
        // usize::MAX threshold must saturate, not overflow the
        // `period * repeats_threshold` multiply in a debug build.
        let records = vec![read("a.rs"); 8];
        let config = LoopDetectionConfig {
            exact_repeat_threshold: usize::MAX,
            short_cycle_repeats: usize::MAX,
            stagnation_threshold: 0, // disabled: this test isolates another check
        };
        assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
    }

    #[test]
    fn evidence_is_none_for_no_loop() {
        assert_eq!(LoopVerdict::NoLoop.evidence(), None);
    }

    #[test]
    fn evidence_describes_exact_repeat() {
        let verdict = LoopVerdict::ExactRepeat {
            tool: "read_file".into(),
            input: serde_json::json!({ "path": "a.rs" }),
            count: 4,
        };
        let evidence = verdict.evidence().expect("loop verdict has evidence");
        assert!(evidence.contains("read_file"));
        assert!(evidence.contains('4'));
    }

    #[test]
    fn evidence_describes_short_cycle() {
        let verdict = LoopVerdict::ShortCycle {
            pattern: vec![
                read("a.rs").call.into_owned(),
                edit("a.rs").call.into_owned(),
            ],
            repeats: 3,
        };
        let evidence = verdict.evidence().expect("loop verdict has evidence");
        assert!(evidence.contains("read_file"));
        assert!(evidence.contains("edit_file"));
        assert!(evidence.contains('3'));
    }

    // ---- stagnation: same tool, moving arguments, unmoving answer --------

    /// One `grep` whose pattern differs from every other, over one file,
    /// always finding the same thing — the field shape from the journal of
    /// the turn that motivated this check.
    fn grep_variant(pattern: &str) -> CallRecord<'static> {
        call(
            "grep",
            serde_json::json!({ "pattern": pattern, "path": "stella-tui/src/deck.rs" }),
            "118:    /// Cumulative prompt-cache *write* tokens",
        )
    }

    /// THE regression. A model reshuffling one regex alternation gets a
    /// different `input` every call and byte-identical output every call.
    /// Neither exact-repeat (inputs never match) nor short-cycle (the
    /// arguments never settle into a period) can see it; before stagnation
    /// existed this ran 38 calls deep on a live turn.
    #[test]
    fn a_tool_whose_arguments_move_but_whose_answer_never_does_is_a_loop() {
        let records: Vec<CallRecord<'static>> = (0..6)
            .map(|i| grep_variant(&format!("cache_write|total_cache|savings_{i}")))
            .collect();
        let verdict = detect_loop(&records, LoopDetectionConfig::default());
        match &verdict {
            LoopVerdict::Stagnant { tool, count } => {
                assert_eq!(tool, "grep");
                assert_eq!(*count, 6);
            }
            other => panic!("expected Stagnant, got {other:?}"),
        }
        assert!(verdict.is_loop());
        let evidence = verdict.evidence().expect("stagnation has evidence");
        assert!(evidence.contains("grep"), "evidence names the tool");
        assert!(
            evidence.contains("DIFFERENT arguments"),
            "evidence must say why this is not an exact repeat: {evidence}"
        );
    }

    #[test]
    fn stagnation_below_the_threshold_is_not_a_loop() {
        // Five is exploration; the default only speaks at six.
        let records: Vec<CallRecord<'static>> = (0..5)
            .map(|i| grep_variant(&format!("pattern_{i}")))
            .collect();
        assert_eq!(
            detect_loop(&records, LoopDetectionConfig::default()),
            LoopVerdict::NoLoop
        );
    }

    #[test]
    fn a_tool_that_keeps_answering_differently_never_stagnates() {
        // Six searches, six different answers: the arguments moved AND the
        // answer moved. That is exactly what productive searching looks
        // like, and it must survive however long it goes on.
        let records: Vec<CallRecord<'static>> = (0..6)
            .map(|i| {
                call(
                    "grep",
                    serde_json::json!({ "pattern": format!("p{i}") }),
                    &format!("src/lib.rs:{i}: hit"),
                )
            })
            .collect();
        assert_eq!(
            detect_loop(&records, LoopDetectionConfig::default()),
            LoopVerdict::NoLoop
        );
    }

    #[test]
    fn another_tool_interleaved_breaks_the_stagnation_run() {
        // A model that greps, reads what it found, then greps again is
        // acting on the results. Only an UNBROKEN run of one tool telling
        // us nothing counts, so the run is measured from the tail.
        let mut records: Vec<CallRecord<'static>> =
            (0..5).map(|i| grep_variant(&format!("p{i}"))).collect();
        records.push(read("src/deck.rs"));
        records.extend((5..9).map(|i| grep_variant(&format!("p{i}"))));
        assert_eq!(
            detect_loop(&records, LoopDetectionConfig::default()),
            LoopVerdict::NoLoop,
            "the trailing grep run is 4 — the read reset it"
        );
    }

    #[test]
    fn identical_calls_are_reported_as_an_exact_repeat_not_stagnation() {
        // Stagnation is the vaguer description of the same evidence, so it
        // must never claim a run exact-repeat describes precisely.
        let records = vec![grep_variant("same"); 8];
        match detect_loop(&records, LoopDetectionConfig::default()) {
            LoopVerdict::ExactRepeat { count, .. } => assert_eq!(count, 8),
            other => panic!("expected ExactRepeat to win, got {other:?}"),
        }
        // And with exact-repeat disabled it still refuses to relabel them:
        // the all-same-input guard holds on its own, not by ordering luck.
        let config = LoopDetectionConfig {
            exact_repeat_threshold: 0,
            short_cycle_repeats: 0,
            stagnation_threshold: 3,
        };
        assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
    }

    #[test]
    fn stagnation_needs_observed_outputs_like_every_other_check() {
        // Unresolved results prove nothing, however many there are.
        let records: Vec<CallRecord<'static>> = (0..8)
            .map(|i| record("grep", serde_json::json!({ "pattern": i }), None))
            .collect();
        assert_eq!(
            detect_loop(&records, LoopDetectionConfig::default()),
            LoopVerdict::NoLoop
        );
    }

    #[test]
    fn zero_or_one_stagnation_threshold_disables_that_check() {
        let records: Vec<CallRecord<'static>> =
            (0..10).map(|i| grep_variant(&format!("p{i}"))).collect();
        for threshold in [0, 1] {
            let config = LoopDetectionConfig {
                exact_repeat_threshold: 0,
                short_cycle_repeats: 0,
                stagnation_threshold: threshold,
            };
            assert_eq!(
                detect_loop(&records, config),
                LoopVerdict::NoLoop,
                "threshold {threshold} should disable stagnation detection"
            );
        }
    }

    #[test]
    fn stagnation_compares_identities_when_compaction_rewrote_the_outputs() {
        // The #554 rule applies here too: the older results were stubbed in
        // place, but their pre-compaction identities still match, so the run
        // is still evidence.
        let records: Vec<CallRecord<'static>> = (0..6)
            .map(|i| {
                let stubbed = i < 4;
                with_identity(
                    call(
                        "grep",
                        serde_json::json!({ "pattern": format!("p{i}") }),
                        if stubbed { "[evicted]" } else { "118: hit" },
                    ),
                    "blk_same",
                )
            })
            .collect();
        assert!(
            detect_loop(&records, LoopDetectionConfig::default()).is_loop(),
            "matching identities must survive an in-place rewrite here too"
        );
    }

    /// Small, deliberately overlapping alphabet of names/inputs/outputs so
    /// property-test runs actually exercise repeats and cycles instead of
    /// almost always generating trivially-varied (and thus trivially
    /// NoLoop) sequences. Includes unresolved (`None`) outputs.
    fn arb_call_record() -> impl Strategy<Value = CallRecord<'static>> {
        (0..3usize, 0..2usize, 0..3usize).prop_map(|(name_idx, input_idx, output_idx)| {
            let names = ["read_file", "edit_file", "bash"];
            let inputs = [
                serde_json::json!({ "path": "a.rs" }),
                serde_json::json!({ "path": "b.rs" }),
            ];
            let outputs = [
                Some(ToolOutput::Ok {
                    content: "ok".into(),
                }),
                Some(ToolOutput::Error {
                    message: "boom".into(),
                }),
                None,
            ];
            record(
                names[name_idx],
                inputs[input_idx].clone(),
                outputs[output_idx].clone(),
            )
        })
    }

    proptest! {
        /// Property: `detect_loop` never panics or indexes out of bounds,
        /// for any history length and any threshold configuration
        /// (including the degenerate `0` thresholds) — required by the
        /// "runs on live untrusted model output" quality bar.
        #[test]
        fn detect_loop_never_panics(
            records in proptest::collection::vec(arb_call_record(), 0..16),
            exact_repeat_threshold in 0usize..8,
            short_cycle_repeats in 0usize..8,
            stagnation_threshold in 0usize..8,
        ) {
            let config = LoopDetectionConfig { exact_repeat_threshold, short_cycle_repeats, stagnation_threshold };
            let verdict = detect_loop(&records, config);
            // Whatever the verdict, `is_loop`/`evidence` must not panic either.
            let _ = verdict.is_loop();
            let _ = verdict.evidence();
        }

        /// Property: history shorter than EVERY threshold is always
        /// `NoLoop` — there's no way for any check to have enough evidence
        /// (the shortest cycle period is 2).
        #[test]
        fn short_history_is_always_no_loop(
            records in proptest::collection::vec(arb_call_record(), 0..12),
            exact_repeat_threshold in 2usize..8,
            short_cycle_repeats in 1usize..8,
            stagnation_threshold in 2usize..8,
        ) {
            if records.len() < exact_repeat_threshold
                && records.len() < 2 * short_cycle_repeats
                && records.len() < stagnation_threshold
            {
                let config = LoopDetectionConfig { exact_repeat_threshold, short_cycle_repeats, stagnation_threshold };
                prop_assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
            }
        }

        /// Property: when every output in the history is unique, nothing
        /// is ever flagged (at meaningful thresholds — a threshold below 2
        /// is disabled or degenerate). Unique outputs = every call
        /// produced new information = progress by definition; this is the
        /// class-level guarantee that legitimate polling can never trip
        /// the detector, whatever the inputs look like.
        #[test]
        fn unique_outputs_are_never_a_loop(
            records in proptest::collection::vec(arb_call_record(), 0..16),
            exact_repeat_threshold in 2usize..8,
            short_cycle_repeats in 2usize..8,
            stagnation_threshold in 2usize..8,
        ) {
            let records: Vec<CallRecord<'static>> = records
                .into_iter()
                .enumerate()
                .map(|(i, mut record)| {
                    record.output = Some(Cow::Owned(ToolOutput::Ok { content: format!("output {i}") }));
                    record
                })
                .collect();
            let config = LoopDetectionConfig { exact_repeat_threshold, short_cycle_repeats, stagnation_threshold };
            prop_assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
        }
    }
}
