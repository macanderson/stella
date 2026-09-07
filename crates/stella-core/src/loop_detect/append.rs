//! The self-appending rung (`#5863`). One tool is called again and again.
//! Each call sends the whole of the last call, and adds more to the end.
//!
//! The four rungs in `super` that read output need bytes that repeat. The
//! `sweep` rung needs a number that climbs and then wraps. A command that
//! grows has neither. Each input is new, so the three repeat rungs reset.
//! Each output is new, so the stagnation rung resets. There is no number to
//! climb, so the sweep rung has nothing to read. The turn grinds on. Each
//! step costs more than the last, since the command and its answer both grow.
//!
//! The trial `build-pov-ray__3DYrafv` in the local arenabench corpus shows
//! the shape. Calls 196 through 302 each sent the last command plus one more
//! copy of the same `grep` block. The command went from three copies and 263
//! chars to 109 copies and 8,774. The trial still won. Its real work had
//! landed a hundred calls back. A third of its steps bought nothing.
//!
//! # The nesting is the proof, and it needs no output
//!
//! Say a call holds the whole of the call before it. Then it asked once more
//! for all the text the last answer gave. What the model wants is not further
//! down this command. It has that text, and it is fetching it again. So the
//! run proves itself, with no output read at all. That is what makes the
//! shape reachable here, since no output in it ever repeats.
//!
//! # A prefix, and not a match in the middle
//!
//! One call extends the one before it when three things hold. Both inputs
//! carry the same keys. Each value is the same, or is a string the old one
//! starts. And at least one value grew. A model that pastes its addition into
//! the middle of a command is missed. Prefix growth is the shape the corpus
//! was scored on. The false-alarm counts in `#5863` are counts about it. A
//! wider net has no numbers behind it yet, so it would need its own.
//!
//! # Why the run has to be long
//!
//! Run length splits nothing here. Across 371 scored trials, a floor of 3 and
//! a floor of 20 fire on the same three, and all three are grinding. So the
//! number is picked for what it says. Three calls is a model building a pipe:
//! write it, add a filter, add a count. Eight calls is not. Seven of them ask
//! again for all the last one asked for. Eight is also
//! `super::LoopDetectionConfig::monotonic_sweep_threshold`. That is the other
//! rung that reads no output, and its floor does the same job.

use serde_json::Value;

use super::{CallRecord, LoopVerdict};

/// Report [`LoopVerdict::SelfAppending`] for a run at the end of the window.
/// The run is `threshold` or more calls to one tool. Each call sent the whole
/// of the call before it, and added more. A `threshold` under 2 turns the
/// check off, as it does for every rung beside it. One call extends nothing.
///
/// The run is read back from the last record, and it must be unbroken. The
/// three repeat rungs read a run the same way. The trial `#5863` measured has
/// no gap in it, and a rung that let gaps through would claim more than the
/// corpus can show.
///
/// This rung reads no output. So a tool's [`super::ToolOrigin`] does not let
/// it off the way it lets stagnation off. That rule is about what the same
/// bytes mean, and no bytes are read here.
pub(super) fn detect_self_appending(
    records: &[CallRecord<'_>],
    threshold: usize,
) -> Option<LoopVerdict> {
    if threshold < 2 {
        return None;
    }
    let last = records.last()?;
    let mut calls = 1usize;
    for pair in records.windows(2).rev() {
        let (earlier, later) = (&pair[0], &pair[1]);
        if earlier.call.name != last.call.name || !extends(&earlier.call.input, &later.call.input) {
            break;
        }
        calls += 1;
    }
    if calls < threshold {
        return None;
    }
    Some(LoopVerdict::SelfAppending {
        tool: last.call.name.clone(),
        // The run's first call, which is its shortest by construction: every
        // later one is a strict extension of the one before it.
        base: records[records.len() - calls].call.input.clone(),
        calls,
    })
}

/// Whether `later` sends the whole of `earlier` and adds to it.
///
/// Both inputs have to be JSON objects over the same keys. Every tool
/// declares an object schema. Two calls with a different key set are two
/// different asks, not one ask that grew. Each value is then the same, or is
/// a string the old one starts. And one value at least has to have grown. So
/// two equal inputs answer `false` here. `super::detect_exact_repeat` names
/// that shape, and it names it better.
///
/// Values are read one key at a time. Reading the whole input as text would
/// miss the shell case: `{"command": "a"}` is text that `{"command": "a &&
/// b"}` does not start with, since a quote and a brace sit in between.
fn extends(earlier: &Value, later: &Value) -> bool {
    let (Some(earlier), Some(later)) = (earlier.as_object(), later.as_object()) else {
        return false;
    };
    if earlier.len() != later.len() {
        return false;
    }
    let mut grew = false;
    for (key, before) in earlier {
        let Some(after) = later.get(key) else {
            return false;
        };
        if before == after {
            continue;
        }
        match (before.as_str(), after.as_str()) {
            (Some(before), Some(after))
                if after.len() > before.len() && after.starts_with(before) =>
            {
                grew = true;
            }
            _ => return false,
        }
    }
    grew
}
