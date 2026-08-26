// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Every hook event the loop owns is dispatched from somewhere (#4017).
//!
//! `stella_protocol::hook` states the rule this enforces: a hook point nothing
//! dispatches is a declaration that quietly does nothing — an operator
//! registers it, nothing runs, and no error is raised anywhere. The compiler
//! checks every event's *shape*; whether a line of code constructs one is a
//! question about call sites, and the only place to ask it is outside.
//!
//! It reads source text because the alternative is to run the loop against a
//! forge. `self_driving_cmd/hooks/tests.rs` covers the other half — what a
//! subscriber reads when one fires — and neither is sufficient alone.

use std::fs;
use std::path::{Path, PathBuf};

use stella_core::hooks::HookEvent;

/// Every `.rs` file under the self-driving command, recursively.
///
/// Recursive because `drive/` and `hooks/` are already subdirectories, and a
/// non-recursive walk would have looked green while covering less than it
/// claimed — `one_oracle_story`'s reasoning, for the same shape of guard.
fn sources() -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, into: &mut Vec<(PathBuf, String)>) {
        for entry in fs::read_dir(dir).expect("the directory is readable") {
            let path = entry.expect("a readable directory entry").path();
            if path.is_dir() {
                walk(&path, into);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = fs::read_to_string(&path).expect("a readable source file");
                into.push((path, text));
            }
        }
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/self_driving_cmd");
    let mut files = vec![(
        root.with_extension("rs"),
        fs::read_to_string(root.with_extension("rs")).expect("the parent module is readable"),
    )];
    walk(&root, &mut files);
    files
}

/// Whether `text` dispatches `event` — names it in a position that is a
/// dispatch rather than a mention.
///
/// **Two spellings, because there are two ways to build a payload.** Most
/// events are passed as a value (`HookEvent::DriveIdle`), and the two that
/// predate the family have a constructor of their own
/// (`HookPayload::post_issue_work`). Accepting only the first read the older
/// pair as undispatched, which is how this guard found its own blind spot the
/// first time it ran.
///
/// A test file's own fixture names every event and dispatches none, so the
/// tests are excluded by the caller rather than by a cleverer pattern here: a
/// guard that tried to tell a dispatch from a fixture by looking at the text
/// around it would be a parser, and a wrong one.
fn dispatches(text: &str, event: HookEvent) -> bool {
    text.contains(&format!("HookEvent::{event}")) || text.contains(&format!("::{}(", snake(event)))
}

/// An event's name as a constructor spells it: `PostIssueWork` →
/// `post_issue_work`.
fn snake(event: HookEvent) -> String {
    let mut out = String::new();
    for (index, ch) in event.as_str().char_indices() {
        if ch.is_ascii_uppercase() && index > 0 {
            out.push('_');
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

/// The shipping source: everything the walk found except the test files.
///
/// The tests name every event by construction — one fixture per event, and a
/// total match over the whole enum — so counting them would make either guard
/// below pass on a vocabulary nothing ships.
fn shipping() -> Vec<(PathBuf, String)> {
    sources()
        .into_iter()
        .filter(|(path, _)| !path.ends_with("tests.rs"))
        .collect()
}

#[test]
fn every_loop_hook_event_is_dispatched_by_a_verb() {
    let files = shipping();
    assert!(
        files.len() > 5,
        "the walk found {} files, so a green result would prove nothing",
        files.len()
    );

    let mut undispatched: Vec<&'static str> = Vec::new();
    for event in HookEvent::ALL {
        // The in-turn five are dispatched by the engine's driver, from inside
        // a turn, and never from here.
        if event.in_turn() {
            continue;
        }
        if !files.iter().any(|(_, text)| dispatches(text, event)) {
            undispatched.push(event.as_str());
        }
    }

    assert!(
        undispatched.is_empty(),
        "these events are declarable and dispatched by nothing, so registering one \
         does nothing and says nothing: {undispatched:?}. Fire it from the verb it \
         names, or do not declare it."
    );
}

/// The other direction: the loop must not dispatch an event the engine owns.
///
/// Two dispatchers for one event would mean a subscriber could not tell which
/// caller woke it, and `PreToolUse` fired from outside a turn would be a hook
/// with no tool in its payload — a shape no disclosure row describes.
#[test]
fn the_loop_dispatches_no_in_turn_event() {
    let files = shipping();
    for event in HookEvent::ALL.into_iter().filter(|event| event.in_turn()) {
        for (path, text) in &files {
            assert!(
                !dispatches(text, event),
                "{} dispatches {event}, which the engine's driver owns",
                path.display()
            );
        }
    }
}
