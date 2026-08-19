//! The static prompts and the tool catalog, coupled in both directions
//! (#3557).
//!
//! `tool_steering!()` is the one place a prompt can say what the schemas
//! cannot — which tool to reach for first, and where each capability comes
//! from. To do that it names all eighteen built-ins in prose, and until this
//! module existed that prose was tied to nothing:
//!
//! - **A retired name could linger.** This is the defect #3557 was filed for,
//!   found in `bash`'s sleep advisory (#3555) rather than here, but the prompt
//!   is the larger surface: it is sent on every turn, not on a threshold.
//! - **A new tool could be left out.** Add a tool, register it, declare its
//!   catalog row, and every existing test stays green while the steering
//!   paragraph still describes the surface as it was. The model is then told
//!   its coordination tools are an enumeration that is missing one, which is a
//!   *closed* list in the reader's eyes — the paragraph ends "use exactly what
//!   is advertised and never assume a capability no schema names".
//!
//! Both directions are checked against `stella_tools::catalog`, which is the
//! canonical table (#450), over the assembled prompt bytes rather than
//! `prompt.rs`'s source text — the bytes are what the model receives.
//!
//! # Why over both prompts
//!
//! `PIPELINE_SYSTEM_PROMPT` is what `stella run` sends and what every bench
//! measurement exercises, so a steering paragraph correct in `SYSTEM_PROMPT`
//! alone is invisible to the numbers this project makes decisions from — the
//! asymmetry `parity.rs` was written for. Both are iterated from the same
//! `STATIC_PROMPTS` registry that module derives, so a third prompt joins
//! these checks by existing.

use stella_tools::catalog;

use super::parity::STATIC_PROMPTS;

/// Whether `text` names `word` as a whole word.
///
/// Substring matching cannot answer this question here: a retired name sits
/// inside a live identifier often enough that `contains` reads the wrong
/// answer in both directions — `read_output` is a deleted tool and
/// `read_output_buffer` is ordinary prose, and the six `task_*` rows would
/// satisfy a search for a shorter `task_`-prefixed name that nothing
/// introduces. Underscore counts as a word character for exactly that
/// reason.
fn names_word(text: &str, word: &str) -> bool {
    let is_word_char = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(word).any(|(at, _)| {
        let before_ok = text[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word_char(c));
        let after_ok = text[at + word.len()..]
            .chars()
            .next()
            .is_none_or(|c| !is_word_char(c));
        before_ok && after_ok
    })
}

/// Anti-vacuity: these checks iterate two registries, and either coming up
/// empty would read as a pass.
#[test]
fn the_registries_under_test_are_not_empty() {
    assert!(
        !STATIC_PROMPTS.is_empty(),
        "no static prompt to scan — the checks below would pass vacuously"
    );
    assert!(
        !catalog::ALL_NAMES.is_empty(),
        "the tool catalog is empty — the checks below would pass vacuously"
    );
    assert!(
        !catalog::RETIRED_TOOL_NAMES.is_empty(),
        "the retired-name list is empty, so the forward check proves nothing"
    );
}

/// The reverse direction of #3557: a built-in that exists but goes unmentioned.
///
/// The remedy is a sentence in `tool_steering!()`, in the group the tool
/// belongs to — not a deletion here. A tool the steering block cannot be
/// bothered to introduce is one the model will under-reach for, which is the
/// exact failure the paragraph leads with `search` to fix.
#[test]
fn every_built_in_tool_is_named_by_the_static_prompts() {
    let mut missing = Vec::new();
    for (label, prompt) in STATIC_PROMPTS {
        for name in catalog::ALL_NAMES {
            if !names_word(prompt, name) {
                missing.push(format!("{label}: never names the `{name}` tool"));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "a built-in tool is absent from the cross-tool steering. The prompt \
         presents its groups as complete lists, so an unmentioned tool reads \
         as one that does not exist — introduce it in `tool_steering!()` \
         alongside its group:\n\n{}",
        missing.join("\n")
    );
}

/// The forward direction: prose promising an instrument that was deleted.
///
/// `grep` and `glob` are deliberately not scanned — see
/// [`catalog::RETIRED_NAMES_TOO_AMBIGUOUS_TO_SCAN`]; the steering block says
/// "bash with grep only when you need every occurrence of one exact literal
/// string", which is about the shell command and is true.
#[test]
fn no_static_prompt_names_a_retired_tool() {
    let mut violations = Vec::new();
    for (label, prompt) in STATIC_PROMPTS {
        for retired in catalog::RETIRED_TOOL_NAMES {
            if names_word(prompt, retired) {
                violations.push(format!("{label}: names `{retired}`, retired in #3244"));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "a static prompt names a tool the agent does not have. Every turn then \
         opens by pointing the model at an instrument that is not on its menu, \
         which is worse than saying nothing (#3031):\n\n{}",
        violations.join("\n")
    );
}

/// The checks above are only as good as `names_word`, and both of them pass
/// when it is too lax *and* when it is too strict. This pins the two
/// confusions that would matter.
#[test]
fn whole_word_matching_separates_a_prefix_from_the_name_it_sits_in() {
    // Too lax: a shorter name must not be satisfied by a longer row that
    // merely starts with it. `task` is the retired delegation name (#3192)
    // and the six board rows must not read as a mention of it.
    assert!(!names_word(
        "the board: task_create, task_list, task_start",
        "task"
    ));
    assert!(names_word("task_create, task_list", "task_create"));
    // Too strict: ordinary punctuation and sentence position still count.
    assert!(names_word("call search first", "search"));
    assert!(names_word("(search)", "search"));
    assert!(names_word("search", "search"));
    assert!(names_word("use search.", "search"));
    // A retired name inside a longer identifier is not a mention of it.
    assert!(!names_word("read_output_buffer", "read_output"));
}
