//! The monotonic-sweep rung (#4042): many calls to one tool, addressing one
//! target, with arguments that keep advancing and outputs that never repeat.
//!
//! Every other rung in `super` is defined on byte-identical **output**, on the
//! argument that "identical input with identical output means the model gained
//! no new information". A linear sweep never repeats an output, so all four
//! reset — which is why a paging model can burn a whole turn unsteered
//! (#4034). `read_file` grew a tool-side ceiling for the observed case (#4041)
//! and that closes it for `read_file` alone; the same shape arrives through
//! `search` with a marching `offset`, through `bash` with `sed -n 'N,Mp'`, and
//! through any MCP tool that pages.
//!
//! # The discriminator is the wrap, not the count
//!
//! Counting calls per target false-positives on legitimate paging of a
//! genuinely large file, and a false loop verdict steers or aborts a working
//! agent. What cannot be legitimate paging is a sweep that **wrapped**: the
//! cursor ran past the end, restarted at or before where the window began, and
//! then resumed advancing. The model is re-walking ground it already covered,
//! and it is doing so without any of the byte-identical evidence the other
//! rungs need. It fires late — a wrap costs a whole pass first — and that is
//! the trade taken deliberately, because the alternative is fitting a constant
//! to "how many pages is too many", which #4034's own definition of done
//! stopped short of.
//!
//! The call count is still a floor, but it is doing a different job: it rules
//! out a target small enough that reading it through and then going back to
//! the top is ordinary navigation rather than a sweep at all.
//!
//! # Target and cursor, from the input alone
//!
//! No plumbing and no port (invariant 2 — and see #4034, which suggested
//! threading `ReadLedger`'s tally in, for data the detector can already
//! derive). Each [`super::CallRecord`] carries the call's `name` and `input`,
//! and that is enough: **canonicalize the input by replacing every integer
//! with `#`** — JSON numbers and digit runs inside strings alike — and what is
//! left is the target, while the integers pulled out in traversal order are
//! the cursor.
//!
//! One rule covers every dialect that way. `read_file {path, offset, limit}`
//! canonicalizes to the same target across a sweep with `offset` moving;
//! `bash {command: "sed -n '1,50p' f.rs"}` and `sed -n '51,100p' f.rs` do too,
//! with the cursor `[1, 50]` then `[51, 100]`. Two calls whose *shape* differs
//! — a different key set, a different number of integers — canonicalize
//! differently and are simply not the same target, which is the answer that
//! wants giving.

use std::cmp::Ordering;

use super::{CallRecord, LoopVerdict};

/// The placeholder an integer leaves behind in a canonicalized input. Any
/// character would do; `#` is chosen because it does not appear in JSON's own
/// punctuation, so a target string reads as the call it came from.
const CURSOR_PLACEHOLDER: char = '#';

/// One call's input split into what it addresses and where in it the call was
/// looking.
#[derive(Debug, PartialEq, Eq)]
struct Addressed {
    /// The input with every integer replaced by [`CURSOR_PLACEHOLDER`].
    target: String,
    /// Those integers, in traversal order (object keys sorted, array elements
    /// in order), so two calls to the same target yield comparable cursors.
    cursor: Vec<i64>,
}

/// Report [`LoopVerdict::MonotonicSweep`] when the window's trailing target has
/// been swept at least `threshold` times, wrapped at least once, and resumed
/// advancing after the wrap. `threshold < 2` disables the check, matching every
/// other rung's `0`/`1` convention.
///
/// Anchored on the last record, like every sibling: a verdict describes what
/// the turn is doing now, not the most alarming thing it did earlier. Calls to
/// other targets in between do not break the sweep — the grouping is by target,
/// not by adjacency — which is deliberate, since a model that pages a file while
/// also thinking out loud is running the same sweep.
pub(super) fn detect_monotonic_sweep(
    records: &[CallRecord<'_>],
    threshold: usize,
) -> Option<LoopVerdict> {
    if threshold < 2 {
        return None;
    }
    let last = records.last()?;
    let anchor = addressed(&last.call.input)?;
    // Nothing to sweep: a call carrying no integer at all has no cursor, so
    // "advancing" is not a question that can be asked of it.
    if anchor.cursor.is_empty() {
        return None;
    }
    let cursors: Vec<Vec<i64>> = records
        .iter()
        .filter(|record| record.call.name == last.call.name)
        .filter_map(|record| addressed(&record.call.input))
        .filter(|other| other.target == anchor.target)
        .map(|other| other.cursor)
        .collect();
    if cursors.len() < threshold {
        return None;
    }
    let wraps = count_wraps(&cursors)?;
    Some(LoopVerdict::MonotonicSweep {
        tool: last.call.name.clone(),
        target: anchor.target,
        calls: cursors.len(),
        wraps,
    })
}

/// How many times `cursors` restarted, or `None` when they are not a sweep at
/// all.
///
/// Every consecutive pair must be one of exactly two things, and anything else
/// disqualifies the window outright:
///
/// - an **advance** — strictly greater than its predecessor;
/// - a **wrap** — strictly less than its predecessor AND at or below every
///   cursor seen so far, having previously gone above it. Restarting at or
///   before the window's own beginning is what distinguishes "ran off the end
///   and began again" from "jumped back to re-check something", and the second
///   is not this rung's shape.
///
/// A repeat (`==`) is exact-repeat's territory and a partial regression is
/// nobody's, so both answer `None` rather than being tolerated. `None` is also
/// the answer when nothing wrapped, or when the last wrap was not followed by
/// at least one advance: a turn that has just this moment gone back to the top
/// may be re-reading a header, and one more advancing call is what settles that
/// it is sweeping again.
fn count_wraps(cursors: &[Vec<i64>]) -> Option<usize> {
    let mut min_seen = cursors.first()?;
    let mut max_seen = cursors.first()?;
    let mut wraps = 0usize;
    let mut advances_since_wrap = 0usize;
    for pair in cursors.windows(2) {
        let (previous, current) = (&pair[0], &pair[1]);
        match current.cmp(previous) {
            Ordering::Greater => advances_since_wrap += 1,
            Ordering::Less if current <= min_seen && max_seen > current => {
                wraps += 1;
                advances_since_wrap = 0;
            }
            _ => return None,
        }
        min_seen = min_seen.min(current);
        max_seen = max_seen.max(current);
    }
    (wraps > 0 && advances_since_wrap > 0).then_some(wraps)
}

/// Split a call's input into its target and its cursor.
///
/// `None` for an input that is not a JSON object — every tool declares an
/// object schema, and a scalar input has no shape worth canonicalizing.
fn addressed(input: &serde_json::Value) -> Option<Addressed> {
    input.as_object()?;
    let mut addressed = Addressed {
        target: String::new(),
        cursor: Vec::new(),
    };
    canonicalize(input, &mut addressed);
    Some(addressed)
}

/// Append `value`'s canonical form to `out.target`, pushing every integer it
/// carries onto `out.cursor`.
///
/// Object keys are visited in sorted order rather than in the order they
/// happen to be stored, so the target is a property of the call rather than of
/// how its JSON was built.
fn canonicalize(value: &serde_json::Value, out: &mut Addressed) {
    match value {
        serde_json::Value::Number(number) => match number.as_i64() {
            Some(n) => {
                out.target.push(CURSOR_PLACEHOLDER);
                out.cursor.push(n);
            }
            // A float is not a paging cursor, so it stays part of the target
            // and two different floats are two different targets.
            None => out.target.push_str(&number.to_string()),
        },
        serde_json::Value::String(text) => canonicalize_text(text, out),
        serde_json::Value::Array(items) => {
            out.target.push('[');
            for item in items {
                canonicalize(item, out);
                out.target.push(',');
            }
            out.target.push(']');
        }
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            out.target.push('{');
            for key in keys {
                out.target.push_str(key);
                out.target.push(':');
                if let Some(child) = map.get(key) {
                    canonicalize(child, out);
                }
                out.target.push(',');
            }
            out.target.push('}');
        }
        other => out.target.push_str(&other.to_string()),
    }
}

/// The same split inside a string, so a cursor spelled into a shell command
/// (`sed -n '51,100p'`) is read exactly as one spelled as a JSON field.
///
/// A digit run that does not fit an `i64` stays in the target verbatim: it is
/// runtime data, and a saturating read would make two different absurd numbers
/// look like one cursor position.
fn canonicalize_text(text: &str, out: &mut Addressed) {
    let mut rest = text;
    while let Some(start) = rest.find(|c: char| c.is_ascii_digit()) {
        let (before, from_digit) = rest.split_at(start);
        out.target.push_str(before);
        let end = from_digit
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(from_digit.len());
        let (digits, after) = from_digit.split_at(end);
        match digits.parse::<i64>() {
            Ok(n) => {
                out.target.push(CURSOR_PLACEHOLDER);
                out.cursor.push(n);
            }
            Err(_) => out.target.push_str(digits),
        }
        rest = after;
    }
    out.target.push_str(rest);
}
