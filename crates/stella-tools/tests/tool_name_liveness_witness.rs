//! Runtime-facing prose may only name a tool that exists (#3557).
//!
//! `bash`'s long-sleep advisory told the model to *"poll with
//! `read_output`/`wait_for` instead"* for weeks after #3244 deleted both
//! tools and the restore that followed declined to bring them back. Every
//! bare `sleep` over the threshold handed the model a directive with nothing
//! behind it — worse than the silence it replaced, because a directive the
//! model cannot follow teaches it to discount the next one too (#3031).
//!
//! It was found by a manual issue sweep, and the test covering that exact
//! string had been holding it green: the assertion was that the note
//! *contains* `read_output`. #3555 fixed the string. This is the guard that
//! makes the next one fail a test instead of shipping.
//!
//! # What is scanned, and why only that
//!
//! Two layers, both authoritative over a different thing:
//!
//! - [`every_advertised_schema_names_only_live_tools`] walks the live
//!   [`ToolRegistry`] and reads each `ToolSchema` as the model receives it.
//!   Complete by construction: a tool added tomorrow is covered without this
//!   file being touched, because the registry is the enumeration.
//! - [`no_runtime_string_names_a_retired_tool`] reads this crate's own
//!   sources, which is the only way to reach the strings appended to tool
//!   *output* — `SEARCH_TIP`, `drift_advisory`, `sleep_advisory`, `read.rs`'s
//!   binary-file refusal, `edit.rs`'s drift echo, `ctx.rs`'s scope refusals.
//!   Those are emitted from a dozen `format!` sites and reach the model just
//!   as surely as a schema does.
//!
//! # The token rule errs toward missing
//!
//! Deciding in general which prose token is tool-shaped cannot be done
//! without false-firing on ordinary English, and a guard that cries wolf gets
//! deleted. So the scan looks only for the names that actually bite: the
//! retired ones, declared in [`catalog::RETIRED_TOOL_NAMES`] with the two
//! ambiguous exclusions recorded beside them. A name that never existed is
//! not caught, and that is the deliberate trade.
//!
//! # Doc comments stay legal
//!
//! Only runtime strings reach the model, so only they are scanned. `bash.rs`
//! legitimately explains that a constant is sized the way it is because "the
//! managed process family's `read_output`, deleted in #3244" was its original
//! consumer — the reasoning survives its example, and deleting it would make
//! a future reader re-derive the ratio argument. Comment lines are skipped,
//! and `#[cfg(test)]` code is too: a test that pins the *absence* of a
//! retired name has to be able to write it down.

use std::path::{Path, PathBuf};

use stella_tools::catalog;
use stella_tools::registry::ToolRegistry;

/// A line every reader of `bash.rs` recognises. Asserted before anything else
/// so "the scan found nothing" can never be confused with "the scan was
/// pointed at the wrong text" — the anti-vacuity discipline
/// `stella-cli/src/agent/prompt/parity.rs` sets out.
const BASH_ANCHOR: &str = "fn sleep_advisory";

/// Files whose whole contents are test code. `#[cfg(test)]` truncation
/// (below) handles inline modules; a crate that puts its tests in their own
/// file needs the filename rule too.
fn is_test_only_file(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == "tests.rs")
}

fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).expect("crate src/ is readable");
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            source_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Every `` `backticked` `` token in `text`.
///
/// Backticks are how this codebase spells a tool name in prose, and requiring
/// them is most of what keeps the scan off ordinary English: the word "search"
/// in a sentence is not a claim about a tool, `` `search` `` is.
fn backticked_tokens(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else { break };
        let token = &after[..close];
        if !token.is_empty() {
            tokens.push(token);
        }
        rest = &after[close + 1..];
    }
    tokens
}

/// Retired names named by `text`, matched as whole backticked tokens.
fn retired_names_in(text: &str) -> Vec<&'static str> {
    let tokens = backticked_tokens(text);
    catalog::RETIRED_TOOL_NAMES
        .iter()
        .copied()
        .filter(|retired| tokens.contains(retired))
        .collect()
}

/// The retired list is only meaningful while it is disjoint from the live one.
/// Restoring a tool means deleting its line from `RETIRED_TOOL_NAMES` in the
/// same change; otherwise this guard would forbid prose from naming a tool the
/// agent actually has.
#[test]
fn no_retired_name_is_also_a_live_tool() {
    for retired in catalog::RETIRED_TOOL_NAMES {
        assert!(
            !catalog::ALL_NAMES.contains(retired),
            "`{retired}` is declared retired but is a live catalog row — a restored \
             tool must lose its RETIRED_TOOL_NAMES line in the same change"
        );
    }
    for ambiguous in catalog::RETIRED_NAMES_TOO_AMBIGUOUS_TO_SCAN {
        assert!(
            !catalog::ALL_NAMES.contains(ambiguous),
            "`{ambiguous}` is recorded as an unscannable retired name but is live — \
             delete its line, it is a real tool again"
        );
        assert!(
            !catalog::RETIRED_TOOL_NAMES.contains(ambiguous),
            "`{ambiguous}` is in both retired lists; the scannable one wins or the \
             exclusion is meaningless"
        );
    }
}

/// The schemas as the model receives them. `ToolSchema::description` is what
/// decides whether the model calls a tool at all, so a description pointing
/// at a tool that does not exist misroutes the call it was meant to steer.
#[test]
fn every_advertised_schema_names_only_live_tools() {
    let root = tempfile::tempdir().expect("a tempdir for the registry root");
    let registry = ToolRegistry::new(root.path().to_path_buf());
    let schemas = registry.schemas();
    assert!(
        !schemas.is_empty(),
        "the registry advertised nothing — this test would pass vacuously"
    );

    let mut violations = Vec::new();
    for schema in &schemas {
        // The input schema's own `description` fields reach the model too.
        let input = serde_json::to_string(&schema.input_schema).expect("a schema serializes");
        for text in [schema.description.as_str(), input.as_str()] {
            for retired in retired_names_in(text) {
                violations.push(format!(
                    "tool `{}`: schema names `{retired}`, retired in #3244",
                    schema.name
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "an advertised schema names a tool the agent does not have — name a live \
         tool from catalog::ALL_NAMES, or describe the capability without \
         promising an instrument:\n\n{}",
        violations.join("\n")
    );
}

/// The strings appended to tool output, which no schema walk can reach.
#[test]
fn no_runtime_string_names_a_retired_tool() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    source_files(&src, &mut files);
    files.retain(|path| !is_test_only_file(path));
    assert!(!files.is_empty(), "found no sources to scan under {src:?}");
    assert!(
        files.iter().any(|path| path.ends_with("bash.rs")),
        "bash.rs is not among the scanned files — the scan is pointed at the \
         wrong tree, not at a clean one"
    );

    let mut violations = Vec::new();
    let mut anchor_seen = false;
    for file in &files {
        let text = std::fs::read_to_string(file).expect("source file is valid UTF-8");
        // Tests must be able to write a retired name down in order to pin its
        // absence. By convention they sit at the end of the file.
        let runtime = match text.find("#[cfg(test)]") {
            Some(cut) => &text[..cut],
            None => &text[..],
        };
        for (index, line) in runtime.lines().enumerate() {
            if line.contains(BASH_ANCHOR) {
                anchor_seen = true;
            }
            // A doc comment explaining why a retired tool mattered is history,
            // not a directive: it never reaches the model.
            if line.trim_start().starts_with("//") {
                continue;
            }
            for retired in retired_names_in(line) {
                violations.push(format!(
                    "{}:{}: names `{retired}`, retired in #3244\n    {}",
                    file.display(),
                    index + 1,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        anchor_seen,
        "`{BASH_ANCHOR}` was not found in any scanned runtime source — the \
         truncation or comment rules are eating the code under test"
    );
    assert!(
        violations.is_empty(),
        "a runtime string names a tool the agent does not have. The model is \
         told to reach for an instrument that is not on its menu, which is \
         worse than saying nothing (#3031). Name a live tool from \
         catalog::ALL_NAMES, or give guidance the current surface can \
         follow:\n\n{}",
        violations.join("\n")
    );
}

/// The historical string this guard exists for, verbatim from `bash.rs` at
/// `4db593a8d^` — the advisory that shipped for weeks before #3555.
///
/// Kept as a fixture rather than only checked out once, so the detector's
/// power is re-proved on every run. Without it, a later "simplification" of
/// [`retired_names_in`] could quietly stop catching the original defect while
/// the two scans above stayed green against a clean tree.
const PRE_3555_SLEEP_ADVISORY: &str = "\
    it. If you are waiting on a background process, poll with `read_output`/`wait_for` \
    instead";

#[test]
fn the_scan_catches_the_advisory_that_shipped() {
    let caught = retired_names_in(PRE_3555_SLEEP_ADVISORY);
    assert!(
        caught.contains(&"read_output") && caught.contains(&"wait_for"),
        "the pre-#3555 advisory must be caught by name, got {caught:?}"
    );
}

/// The exclusions are load-bearing in the other direction: the prompt says
/// "bash with grep only when you need every occurrence of one exact literal
/// string", which is true and must stay sayable.
#[test]
fn ordinary_prose_and_ambiguous_names_do_not_fire() {
    for benign in [
        "use `bash` with grep only when you need one exact literal string",
        "call `search` first; it returns the files that answer the question",
        "reading the output of a `read_file` call",
        "glob patterns and grep expressions are shell vocabulary",
        "the search results are ranked",
    ] {
        assert!(
            retired_names_in(benign).is_empty(),
            "false positive on benign prose: {benign:?}"
        );
    }
}

#[test]
fn the_tokenizer_reads_only_backticked_tokens() {
    assert_eq!(backticked_tokens("a `one` b `two`"), vec!["one", "two"]);
    // An unterminated backtick contributes nothing rather than swallowing the
    // rest of the line as one token.
    assert!(backticked_tokens("dangling `read_output").is_empty());
    // A bare mention is not a claim about a tool.
    assert!(retired_names_in("read_output as prose").is_empty());
}
