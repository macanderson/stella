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
//! Four failure modes are detected, matching real agent stuck-loop
//! signatures. The first three read a **contiguous suffix** and reset at the
//! first mismatch; the fourth exists because that is a blind spot they all
//! share (#1851):
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
//! 4. **Interleaved repeat** — one identical call (same tool, same input,
//!    same output) recurring across the window with *other work in between*.
//!    Invisible to all three above precisely because they read a contiguous
//!    suffix: an agent alternating a failing `cargo test` with a varying
//!    `read_file` has period 2 with one varying element, so every one of them
//!    resets at offset 1 and the turn spins to `max_steps` unsteered. This is
//!    the shape a real wedge takes — re-run the failing test, peek at a
//!    different file, repeat. It is guarded by `window_is_progressing` so it
//!    cannot fire on correct polling, where a repeating spacer sits beside a
//!    query that IS answering differently each round.
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
    /// Occurrences of one identical call — same tool, same input, same output
    /// — anywhere in the window, required to flag an interleaved repeat.
    /// `0` or `1` disable the check.
    ///
    /// Equal to [`Self::exact_repeat_threshold`], not looser, and that is the
    /// point. Each occurrence is exactly as strong as an exact repeat's: the
    /// same call producing the same result taught the model nothing, whether
    /// or not something else happened in between. Interleaving does not
    /// weaken the evidence — it only hides it from a scan that reads a
    /// contiguous suffix, which is what every other check here does.
    pub interleaved_repeat_threshold: usize,
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
            interleaved_repeat_threshold: 3,
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
    /// One identical call — same tool, same input, byte-identical output —
    /// recurring across the window with other work in between.
    ///
    /// The shape every other check here is blind to, because they all read a
    /// contiguous suffix and reset at the first mismatch. An agent
    /// alternating a failing `cargo test` with a varying `read_file` has
    /// period 2 with one varying element: exact-repeat resets at offset 1,
    /// short-cycle breaks on the mismatched element, stagnation's run ends
    /// there too — and the turn spins to `max_steps` unsteered. That is what
    /// a real wedge looks like: re-run the failing test, peek at a different
    /// file, repeat (#1851).
    InterleavedRepeat {
        tool: String,
        input: serde_json::Value,
        count: usize,
    },
}

impl LoopVerdict {
    /// `true` for any detected loop variant; `false` for `NoLoop`.
    pub fn is_loop(&self) -> bool {
        !matches!(self, LoopVerdict::NoLoop)
    }

    /// A human-readable evidence string for the driver to surface when it
    /// steers or aborts. `None` for `NoLoop`.
    ///
    /// The quoted input is truncated through the same `truncate` and
    /// `MAX_DESCRIBED_INPUT` [`LoopIdentity::describe`] uses, not a second
    /// budget of its own. This string becomes an `AgentEvent::Error` message,
    /// the abort reason on `TurnOutcome::Aborted`, the red line in the Command
    /// Deck, and the `loop_detected` journal event — so an untruncated
    /// `edit_file` or `write_file` loop, whose input carries whole old/new file
    /// chunks, put kilobytes of JSON into all four (#1744). A `bash` loop's
    /// one-line command is well under the cap and reads exactly as it always
    /// did, which is why this went unnoticed.
    pub fn evidence(&self) -> Option<String> {
        match self {
            LoopVerdict::NoLoop => None,
            LoopVerdict::ExactRepeat { tool, input, count } => Some(format!(
                "the same `{tool}` call with identical arguments repeated {count} times \
                 consecutively, producing byte-identical output every time (input: {})",
                truncate(&input.to_string(), MAX_DESCRIBED_INPUT)
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
            LoopVerdict::InterleavedRepeat { tool, input, count } => Some(format!(
                "the same `{tool}` call with identical arguments has run {count} times in \
                 this turn — not consecutively, but with other work in between — and \
                 returned byte-identical output every time. The calls between it are not \
                 making it say anything new, so repeating it cannot be what unblocks you \
                 (input: {})",
                truncate(&input.to_string(), MAX_DESCRIBED_INPUT)
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
            // Same identity shape as an exact repeat, and deliberately so: it
            // is the same loop, seen through a scan that tolerates gaps. A
            // turn that steers on the interleaved rung and then trips the
            // contiguous one must be recognised as the SAME loop, or the
            // escalation ladder would restart and steer twice for one wedge.
            LoopVerdict::InterleavedRepeat { tool, input, .. } => Some(LoopIdentity {
                tools: vec![tool.clone()],
                inputs: Some(vec![input.to_string()]),
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
    if let Some(verdict) = detect_interleaved_repeat(records, config.interleaved_repeat_threshold) {
        return verdict;
    }
    if let Some(verdict) = detect_stagnation(records, config.stagnation_threshold) {
        return verdict;
    }
    LoopVerdict::NoLoop
}

/// Count occurrences of the last record ANYWHERE in the window — not only in
/// a trailing run — and report `InterleavedRepeat` if that count is
/// `>= threshold`. `threshold < 2` and empty `records` both return `None`.
///
/// The one check here that does not read a contiguous suffix, which is the
/// whole reason it exists: every other detector resets at the first mismatch,
/// so a single interleaved call blinds all three (#1851).
///
/// Placed AFTER exact-repeat and short-cycle and BEFORE stagnation, which is
/// the same "tightest description of the evidence wins" ordering the rest of
/// `detect_loop` follows. A contiguous run is already an exact repeat and is
/// reported as one; this claims strictly more than stagnation (identical
/// input AND output, versus same tool and output with the arguments moving),
/// so it would be swallowed if stagnation ran first.
///
/// `same_record` carries the unresolved-output rule, so a trailing call whose
/// result is not in the window counts zero and never fires — a loop is only
/// ever PROVEN by observed outputs.
fn detect_interleaved_repeat(records: &[CallRecord<'_>], threshold: usize) -> Option<LoopVerdict> {
    if threshold < 2 {
        return None;
    }
    let last = records.last()?;
    let count = records
        .iter()
        .filter(|record| same_record(record, last))
        .count();
    if count < threshold {
        return None;
    }
    if window_is_progressing(records) {
        return None;
    }
    Some(LoopVerdict::InterleavedRepeat {
        tool: last.call.name.clone(),
        input: last.call.input.clone(),
        count,
    })
}

/// Whether anything in this window is producing NEW information: the same
/// query, asked twice, answering differently.
///
/// This is the guard that keeps the gap-tolerant rung from steering a working
/// agent, and it is not hypothetical — the existing suite caught the shape.
/// Correct polling alternates `read_output` (same handle, new bytes every
/// time) with `bash sleep 5` (same command, empty output every time). The
/// sleep is a SPACER: it repeats identically by design, so counting its
/// occurrences flags a turn that is streaming a build log perfectly well.
///
/// The discriminator is not "did anything repeat" but "did any repeated query
/// answer differently". A `read_output` whose bytes change is a turn learning
/// something; a `read_file` on a *different* path each round is not the same
/// query at all, and so is not evidence of progress — which is exactly why
/// the wedge shape (re-run the failing test, peek at a different file) still
/// trips the rung.
///
/// O(n²) over a window of a few dozen records, which is what
/// [`detect_loop`]'s contract already assumes callers hand in.
fn window_is_progressing(records: &[CallRecord<'_>]) -> bool {
    records.iter().enumerate().any(|(i, a)| {
        records[i + 1..].iter().any(|b| {
            a.call.name == b.call.name
                && a.call.input == b.call.input
                && a.output.is_some()
                && b.output.is_some()
                && !same_output(a, b)
        })
    })
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
mod tests;
