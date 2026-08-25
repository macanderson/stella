// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Where a lane is, as one sentence — read off its own fold.
//!
//! Shared by the SUB-AGENTS overlay's third row and the pulse row
//! (`v2::pulse`), so the two surfaces never disagree about what a lane is
//! doing. Nothing here is a tool's raw arguments or a JSON result; the
//! fold's humanized one-liner is the most a sentence quotes.

use crate::deck::AgentEntry;
use crate::envelope::{AgentMeta, AgentStatus};
use crate::model::TranscriptEntry;
use crate::v2::cards;

/// What the lane is for: the purpose the driver supplied, else its title.
pub fn purpose(meta: &AgentMeta) -> &str {
    meta.purpose
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or(&meta.title)
}

/// The present participle and past tense of a built-in tool's verb. Unknown
/// tools — MCP, custom — are called by name.
fn verb(tool: &str) -> (String, String) {
    let (doing, done) = match tool {
        "read_file" => ("reading", "read"),
        "write_file" => ("writing", "wrote"),
        "edit_file" => ("editing", "edited"),
        "delete_file" => ("deleting", "deleted"),
        "search" => ("searching", "searched"),
        "bash" => ("running", "ran"),
        "delegate" => ("delegating", "delegated"),
        "ask_question" => ("asking", "asked"),
        "get_environment" => ("probing the environment", "probed the environment"),
        _ => return (format!("calling {tool}"), format!("called {tool}")),
    };
    (doing.to_string(), done.to_string())
}

/// Where the lane is in its task, as one sentence.
///
/// A supervisor state (queued, paused, killed) is stated as such — with the
/// last tool beside it where there is one, so a paused lane says what it was
/// doing when it stopped. A live lane is read off the tail of its fold: an
/// open tool call is `reading <path>`, a closed one `read <path>`, prose is
/// the report being written, and an empty fold is a lane that has not made
/// its first model call yet.
pub fn lifecycle(lane: &AgentEntry) -> String {
    let tail = last_tool(lane);
    match lane.status {
        AgentStatus::Queued => "waiting for a worker slot".to_string(),
        AgentStatus::Paused => match tail {
            Some(t) => format!("paused after {} {}", verb(&t.name).1, t.input),
            None => "paused before its first tool call".to_string(),
        },
        AgentStatus::Killed => match tail {
            Some(t) => format!("stopped after {} {}", verb(&t.name).1, t.input),
            None => "stopped before its first tool call".to_string(),
        },
        AgentStatus::WaitingInput => "waiting on an answer from you".to_string(),
        AgentStatus::Failed => lane
            .model
            .transcript
            .iter()
            .rev()
            .find_map(|e| match e {
                TranscriptEntry::Error { message, .. } => Some(format!("failed: {message}")),
                _ => None,
            })
            .unwrap_or_else(|| "failed".to_string()),
        AgentStatus::Done => lane
            .model
            .transcript
            .iter()
            .rev()
            .find_map(|e| match e {
                TranscriptEntry::Complete { cost_usd, .. } => {
                    Some(format!("finished · report delivered · ${cost_usd:.2}"))
                }
                _ => None,
            })
            .unwrap_or_else(|| "finished · report delivered".to_string()),
        AgentStatus::Running => running(lane),
    }
}

/// The most recent tool call on the lane.
struct LastTool {
    name: String,
    input: String,
}

fn last_tool(lane: &AgentEntry) -> Option<LastTool> {
    lane.model.transcript.iter().rev().find_map(|e| match e {
        TranscriptEntry::ToolStart { name, input, .. } => Some(LastTool {
            name: name.clone(),
            input: input.clone(),
        }),
        _ => None,
    })
}

/// A running lane's sentence, from the newest entry that says anything
/// about progress.
fn running(lane: &AgentEntry) -> String {
    for entry in lane.model.transcript.iter().rev() {
        match entry {
            TranscriptEntry::ToolResult {
                name, ok, summary, ..
            } => {
                // The result names the call; the start row before it holds
                // the input. Find it so the sentence says what was read.
                let input = input_of(lane, name);
                let (_, done) = verb(name);
                return if *ok {
                    format!("{done} {input}")
                } else {
                    let why = cards::truncate_cols(summary, 40);
                    format!("{done} {input} — failed: {why}")
                };
            }
            TranscriptEntry::ToolStart { name, input, .. } => {
                // An open call is the newest entry only when no result has
                // followed it; the `ToolResult` arm above wins otherwise.
                let (doing, _) = verb(name);
                return format!("{doing} {input}");
            }
            TranscriptEntry::Text(_) => return "writing its report".to_string(),
            TranscriptEntry::Reasoning(_) => return "thinking".to_string(),
            TranscriptEntry::Retry { attempt, .. } => {
                return format!("retrying the model call · attempt {attempt}");
            }
            TranscriptEntry::Parked { description, .. } => {
                return format!("parked: {description}");
            }
            TranscriptEntry::Compaction { .. } => {
                return "compacting its context".to_string();
            }
            TranscriptEntry::User(_) => {
                return "starting: reading its briefing".to_string();
            }
            _ => {}
        }
    }
    "starting: no model call yet".to_string()
}

/// The input of the newest `ToolStart` named `name` on the lane.
fn input_of(lane: &AgentEntry, name: &str) -> String {
    lane.model
        .transcript
        .iter()
        .rev()
        .find_map(|e| match e {
            TranscriptEntry::ToolStart { name: n, input, .. } if n == name => Some(input.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck::WorkspaceModel;
    use crate::envelope::Inbound;
    use stella_protocol::{AgentEvent, ToolCall, ToolOutput};

    fn model_with(lanes: &[(&str, AgentStatus)]) -> WorkspaceModel {
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(
            AgentMeta::new("lead", "stella", 0).with_role("lead"),
        ));
        for (id, status) in lanes {
            model.apply_inbound(&Inbound::Register(
                AgentMeta::new(*id, format!("task {id}"), 0).with_role("subagent"),
            ));
            model.apply_inbound(&Inbound::Status {
                agent: (*id).to_string(),
                status: *status,
            });
        }
        model
    }

    fn tool_start(model: &mut WorkspaceModel, agent: &str, name: &str, path: &str) {
        model.apply_inbound(&Inbound::Event {
            agent: agent.to_string(),
            event: AgentEvent::ToolStart {
                call: ToolCall {
                    call_id: format!("c-{name}-{path}"),
                    name: name.to_string(),
                    input: serde_json::json!({ "path": path }),
                },
                sub_agent_id: None,
            },
        });
    }

    fn tool_result(model: &mut WorkspaceModel, agent: &str, name: &str, path: &str) {
        model.apply_inbound(&Inbound::Event {
            agent: agent.to_string(),
            event: AgentEvent::ToolResult {
                call_id: format!("c-{name}-{path}"),
                output: ToolOutput::ok("ok"),
                duration_ms: 3,
                speculated: false,
                sub_agent_id: None,
            },
        });
    }

    fn lane<'a>(model: &'a WorkspaceModel, id: &str) -> &'a AgentEntry {
        model
            .agents
            .iter()
            .find(|a| a.meta.id == id)
            .expect("lane registered")
    }

    /// **The witness.** The second sentence is where the lane is, read off
    /// its fold: inside a call it is `reading <path>`; once the call returns
    /// it is `read <path>`; once it narrates, it is writing its report. None
    /// of those is the call's arguments or its result.
    #[test]
    fn the_lifecycle_sentence_follows_the_lanes_fold() {
        let mut model = model_with(&[("sub:2", AgentStatus::Running)]);
        assert_eq!(
            lifecycle(lane(&model, "sub:2")),
            "starting: no model call yet"
        );

        tool_start(&mut model, "sub:2", "read_file", "docs/spec/a.md");
        assert_eq!(
            lifecycle(lane(&model, "sub:2")),
            "reading docs/spec/a.md",
            "an open call is the participle"
        );

        tool_result(&mut model, "sub:2", "read_file", "docs/spec/a.md");
        // A result re-folds the lane to Running; the sentence is the past tense.
        assert_eq!(lifecycle(lane(&model, "sub:2")), "read docs/spec/a.md");

        model.apply_inbound(&Inbound::Event {
            agent: "sub:2".into(),
            event: AgentEvent::Text {
                text: "Done. I simplified".into(),
            },
        });
        assert_eq!(lifecycle(lane(&model, "sub:2")), "writing its report");
    }

    /// A supervisor state names itself and what the lane was doing.
    #[test]
    fn paused_and_stopped_lanes_say_what_they_were_doing() {
        let mut model = model_with(&[("sub:3", AgentStatus::Running)]);
        tool_start(&mut model, "sub:3", "edit_file", "README.md");
        model.apply_inbound(&Inbound::Status {
            agent: "sub:3".into(),
            status: AgentStatus::Paused,
        });
        assert_eq!(
            lifecycle(lane(&model, "sub:3")),
            "paused after edited README.md"
        );
        model.apply_inbound(&Inbound::Status {
            agent: "sub:3".into(),
            status: AgentStatus::Killed,
        });
        assert_eq!(
            lifecycle(lane(&model, "sub:3")),
            "stopped after edited README.md"
        );
        // A killed lane stays killed — the fold refuses to revive a terminal
        // lane — so the queued sentence is pinned on a lane that never ran.
        let queued = model_with(&[("sub:4", AgentStatus::Queued)]);
        assert_eq!(
            lifecycle(lane(&queued, "sub:4")),
            "waiting for a worker slot"
        );
    }

    /// The purpose is the driver's sentence, the title until there is one.
    #[test]
    fn purpose_prefers_the_drivers_sentence_over_the_title() {
        let meta = AgentMeta::new("sub:1", "task #1: Simplify", 0);
        assert_eq!(purpose(&meta), "task #1: Simplify");
        let meta = meta.with_purpose("Simplify the crate READMEs into plain words.");
        assert_eq!(
            purpose(&meta),
            "Simplify the crate READMEs into plain words."
        );
    }
}
