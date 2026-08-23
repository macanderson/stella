// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What the hook runner actually writes to a plugin's stdin, held to what the
//! install-consent prompt said would cross (#4310).
//!
//! # Why the guard is here
//!
//! `stella_plugin::HOOK_FIELDS` is the disclosure table and
//! `stella_plugin::consent_text` renders it, but `stella-plugin` is a near-leaf
//! — `stella-protocol` is its only workspace edge — and `HookPayload` lives in
//! `stella-core`. Neither crate can see both halves. This one can, and it is
//! also the crate whose `hook_runner` serializes the payload onto the pipe, so
//! "does what we send match what we disclosed" is a question about this crate's
//! own behaviour rather than a coincidence checked from outside.
//!
//! # Two checks, and they fail for different reasons
//!
//! - The **destructure** takes every payload type apart by name. A field added
//!   to `HookPayload`, `HookToolInfo`, `HookIssueInfo` or `HookIssueOutcome`
//!   stops this file compiling (`E0027`) until it has a row.
//! - The **serialization census** builds each event's payload with the real
//!   constructor, serializes it, and compares the keys that actually appear
//!   against the rows for that event. That is the half a destructure cannot
//!   do: `HookPayload` is one type serving seven events, and which of its
//!   fields are populated is a property of the constructors, not of the struct.

use std::collections::BTreeSet;

use serde_json::Value;
use stella_core::hooks::{HookIssueInfo, HookIssueOutcome, HookPayload, HookToolInfo};
use stella_plugin::{HOOK_FIELDS, HookEvent};

/// The dotted paths present in one serialized payload, in the spelling serde
/// wrote them — one level of nesting, which is every level these types have.
fn wire_paths(payload: &HookPayload) -> BTreeSet<String> {
    let Value::Object(fields) = serde_json::to_value(payload).expect("a payload serializes") else {
        panic!("a hook payload serializes as an object");
    };
    let mut paths = BTreeSet::new();
    for (key, value) in fields {
        match value {
            Value::Object(nested) if key != "tool" || nested.contains_key("name") => {
                for inner in nested.keys() {
                    paths.insert(format!("{key}.{inner}"));
                }
            }
            _ => {
                paths.insert(key);
            }
        }
    }
    paths
}

/// The rows `HOOK_FIELDS` holds for `event`.
fn rows_for(event: HookEvent) -> BTreeSet<String> {
    HOOK_FIELDS
        .iter()
        .filter(|field| field.event == event)
        .map(|field| field.path.to_string())
        .collect()
}

/// Every payload this workspace can build, one per event, populated as widely
/// as its constructor allows — the worst case a plugin at that hook receives.
fn widest_payloads() -> Vec<(HookEvent, Vec<HookPayload>)> {
    let issue = HookIssueInfo {
        number: "4310".into(),
        title: Some("a title".into()),
        branch: Some("fix/branch".into()),
    };
    vec![
        (
            HookEvent::SessionStart,
            vec![HookPayload::session_start("/w")],
        ),
        (
            HookEvent::PreToolUse,
            vec![HookPayload::pre_tool_use(
                "/w",
                "bash",
                serde_json::json!({ "command": "ls" }),
                false,
            )],
        ),
        (
            HookEvent::PostToolUse,
            vec![HookPayload::post_tool_use(
                "/w",
                "bash",
                serde_json::json!({ "command": "ls" }),
                false,
                "a.txt\n",
            )],
        ),
        (HookEvent::Stop, vec![HookPayload::stop("/w", "done")]),
        (HookEvent::PreCompact, vec![HookPayload::pre_compact("/w")]),
        (
            HookEvent::PreIssueWork,
            vec![HookPayload::pre_issue_work("/w", issue.clone())],
        ),
        // One payload per outcome arm: an internally tagged enum writes one
        // arm's field at a time, so the widest case for this event is the
        // union of the three rather than any one of them.
        (
            HookEvent::PostIssueWork,
            vec![
                HookIssueOutcome::Changed {
                    summary: "a summary".into(),
                },
                HookIssueOutcome::NoChange,
                HookIssueOutcome::Failed {
                    reason: "a reason".into(),
                },
            ]
            .into_iter()
            .map(|outcome| HookPayload::post_issue_work("/w", issue.clone(), outcome))
            .collect(),
        ),
    ]
}

/// **The anti-drift guard for #4310.** Every field of every payload type has a
/// row, checked by taking the types apart rather than by reading them.
#[test]
fn every_hook_payload_field_is_named_in_the_disclosure_table() {
    let HookPayload {
        event,
        cwd,
        tool,
        tool_result,
        final_text,
        issue,
        issue_outcome,
    } = HookPayload::post_tool_use("/w", "bash", serde_json::json!({}), false, "out");
    let HookToolInfo {
        name,
        input,
        read_only,
    } = tool.expect("the constructor above populates the tool");
    let HookIssueInfo {
        number,
        title,
        branch,
    } = HookIssueInfo::new("1");
    // `HookIssueOutcome` is an internally tagged enum: `status` is the tag, and
    // each arm's own field crosses beside it. Matched exhaustively so a fourth
    // arm does not compile until it has rows.
    let arm_fields: Vec<&'static str> = [
        HookIssueOutcome::Changed {
            summary: String::new(),
        },
        HookIssueOutcome::NoChange,
        HookIssueOutcome::Failed {
            reason: String::new(),
        },
    ]
    .into_iter()
    .filter_map(|outcome| match outcome {
        HookIssueOutcome::Changed { summary } => {
            let _ = summary;
            Some("issueOutcome.summary")
        }
        HookIssueOutcome::NoChange => None,
        HookIssueOutcome::Failed { reason } => {
            let _ = reason;
            Some("issueOutcome.reason")
        }
    })
    .collect();

    let mut declared: Vec<&'static str> = vec![
        named("event", event),
        named("cwd", cwd),
        named("tool.name", name),
        named("tool.input", input),
        named("tool.read_only", read_only),
        named("toolResult", tool_result),
        named("finalText", final_text),
        named("issue.number", number),
        named("issue.title", title),
        named("issue.branch", branch),
        named("issueOutcome.status", issue_outcome),
        named("issue", issue),
    ];
    // `issue` is not a leaf; its own fields are named above, so drop the
    // container from the list the table is checked against.
    declared.retain(|path| *path != "issue");
    declared.extend(arm_fields);

    let table: BTreeSet<&str> = HOOK_FIELDS.iter().map(|field| field.path).collect();
    for path in declared {
        assert!(
            table.contains(path),
            "`{path}` crosses to a plugin's process on the hook channel and has no row in \
             HOOK_FIELDS: give it one, disclosing it or saying `None` on purpose"
        );
    }
    for field in HOOK_FIELDS {
        assert!(
            HookEvent::ALL.contains(&field.event),
            "HOOK_FIELDS names an event that is not declarable: {}",
            field.event
        );
    }
}

/// Names one destructured field, consuming its binding — the same trick
/// `stella_plugin::wire`'s own guard uses, so a field left out of the list
/// beside its pattern is an unused binding clippy refuses.
fn named<T>(path: &'static str, _value: T) -> &'static str {
    path
}

/// **The census.** For each event, what the real constructor actually
/// serializes is exactly what the table says that event carries.
///
/// This is what makes a row a fact rather than an intention: a `Stop` observer
/// disclosed the tool arguments it never receives would be over-stating, and an
/// observer receiving a field with no row would be the #4310 defect again.
#[test]
fn each_events_rows_are_exactly_what_that_event_serializes() {
    for (event, payloads) in widest_payloads() {
        let sent: BTreeSet<String> = payloads.iter().flat_map(wire_paths).collect();
        let declared = rows_for(event);
        assert_eq!(
            sent, declared,
            "`{event}` sends {sent:?} and HOOK_FIELDS declares {declared:?}"
        );
    }
}

/// A hooks-only manifest's consent prompt names what its process receives.
///
/// The #4310 repro, asserted end to end: before this, the document said "It
/// asks for no tool capabilities." and nothing else, while the process was fed
/// the user's tool arguments.
#[test]
fn a_hooks_only_manifest_discloses_the_tool_inputs_its_process_receives() {
    let manifest = stella_plugin::PluginManifest::from_toml_str(
        "name = \"watcher\"\n\
         description = \"an observer\"\n\n\
         [loop]\nparticipation = \"steering\"\nhooks = [\"PreToolUse\"]\n\n\
         [runtime]\nargv = [\"python3\", \"watch.py\"]\ntimeout_secs = 30\nenv = [\"PATH\"]\n",
    )
    .expect("the fixture manifest parses");

    let text = stella_plugin::consent_text(&manifest);
    assert!(
        text.contains("before every tool call (`PreToolUse` hook)"),
        "the hook's moment is named:\n{text}"
    );
    assert!(
        text.contains("that tool's arguments in full, before it runs"),
        "and what crosses at it:\n{text}"
    );
    assert!(
        !text.contains("It asks for no tool capabilities."),
        "a plugin whose process is fed the user's tool inputs is not asking for nothing:\n{text}"
    );
}
