// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Working-set restoration (#2685): the pure decision half of putting back
//! what overflow summarization destroys.
//!
//! When the overflow summarizer replaces the oldest span of a conversation
//! with a model-written summary (`driver::restore`), the working set inside
//! that span is gone: file contents the model had read, and the body of any
//! skill it was mid-way through executing. The summary says *that* things
//! happened; the agent then re-reads files it already had — or worse,
//! proceeds on a lossy paraphrase of procedure text it was following. This
//! module decides **what** to restore and renders the one tail message that
//! carries it; the engine half (`driver::restore`, the `waiting.rs` /
//! `driver/waiting.rs` split) owns the fresh reads through the existing
//! [`crate::ports::ToolExecutor`] port — zero model calls, no new I/O
//! surface (invariant 2).
//!
//! # What is restored, and what deliberately is not
//!
//! - **Files read inside the summarized span** ([`collect_working_set`]):
//!   the most recent read per path, most-recent-first, capped at
//!   [`RESTORE_MAX_FILES`]. Content is re-read fresh from disk at restore
//!   time — deliberately, so an external edit between read and restore
//!   surfaces the *current* bytes rather than resurrecting a stale copy.
//! - **The full body of any active invoked skill**
//!   ([`crate::skills::invoke::ActiveSkillInvocations`] is the tracking
//!   half): procedure text the model is executing must survive verbatim,
//!   never as the summary's paraphrase. An invocation whose span already
//!   ended is history, not working set, and is not restored.
//! - **Not restored**: anything still in context — a path re-read in the
//!   kept tail, a skill re-invoked there, the skill listing in the stable
//!   prefix, roster announcements. Nothing outside the summarized span was
//!   lost, so nothing outside it is re-sent.
//!
//! # The budget is the compaction budget
//!
//! Restoration is charged against the same budget whose breach triggered the
//! summarization round ([`render_restoration`]'s `budget_tokens` is the
//! post-splice headroom under it), so restoring can never defeat compacting:
//! the rendered message is proven to fit before it is emitted. When it
//! cannot all fit, whole items are dropped deterministically — most recent
//! first wins — and the drops are *named* in the message, never silently
//! elided. The only mid-content truncation is the per-file cap
//! ([`RESTORE_FILE_BYTE_CAP`]), and it is marked in place.
//!
//! # Restoration survives the next round
//!
//! The restoration message is itself a `User`-role message in the volatile
//! tail, so a later summarization round will fold it away too. Its sections
//! are therefore rendered under parseable fences, and
//! [`collect_working_set`] re-collects paths and skill bodies embedded in a
//! prior restoration message that falls inside the new span — an active
//! skill's body chains across rounds for as long as its invocation is live,
//! at the priority of the (old) message that carried it.

use std::collections::HashSet;

use stella_protocol::{CompletionMessage, MessageRole};

use crate::skills::invoke::invocation_slug;

/// Prefix of the engine-authored restoration tail message. Registered in
/// [`crate::engine_markers::ENGINE_MARKERS`] like every other engine-injected
/// `User`-role marker, taught to the model by the static prompts, and skipped
/// by loop detection's turn-boundary walk (`driver::loop_evidence`).
pub const RESTORE_MARKER_PREFIX: &str = "[working set restored";

/// The read tool restoration replays. One definition, two crates, same as
/// `driver::loop_evidence`'s footer fragments: `stella_tools::read` names its
/// schema after this constant's value, and a pin test there fails if either
/// side drifts — a renamed read tool that restoration no longer recognizes is
/// not cosmetic, it is the working set silently ceasing to restore.
pub const READ_TOOL: &str = "read_file";

/// The read tool's path parameter, used both to extract the path from a
/// span's recorded calls and to synthesize the replay input for a path
/// re-collected from a prior restoration message.
pub const READ_PATH_PARAM: &str = "path";

/// Ceiling on restored files per round. Eight matches the workspace's
/// standing notion of "the recent work the model is actively reasoning over"
/// (`EngineConfig::summarize_keep_recent` and `tool_result_horizon_steps`
/// both default to 8); anything older than the eight most recent reads is
/// cheaper to re-read on demand than to carry speculatively.
pub const RESTORE_MAX_FILES: usize = 8;

/// Per-file byte cap on restored content — the one place mid-content
/// truncation is allowed, and it is marked in place with
/// `FILE_CAP_MARKER` (private). Bounds the worst case where one huge file would eat
/// the whole headroom that several smaller, equally recent files could
/// share. ~4k tokens at the shared byte heuristic.
pub const RESTORE_FILE_BYTE_CAP: usize = 16_384;

/// Marker appended when a file's restored content hit
/// [`RESTORE_FILE_BYTE_CAP`], so the truncation is visible and actionable
/// rather than silent.
const FILE_CAP_MARKER: &str =
    "[… truncated at the restoration cap — re-read the file for the rest …]";

/// Section fences. Parseable on the way back in ([`embedded_sections`]) so a
/// restoration message summarized away in a *later* round re-contributes its
/// paths and skill bodies. File content is line-numbered by the read tool, so
/// a payload line cannot collide with a fence; a skill body containing a
/// literal fence line would truncate its own chained copy — a bounded,
/// self-inflicted edge documented rather than defended.
const FILE_SECTION_OPEN: &str = "--- file: ";
const FILE_SECTION_CLOSE: &str = "--- end file ---";
const SKILL_SECTION_OPEN: &str = "--- skill: ";
const SKILL_SECTION_CLOSE: &str = "--- end skill ---";
const SECTION_HEADER_CLOSE: &str = " ---";

/// One file read the summarized span contained: the path, and the most
/// recent read call's exact input — replayed verbatim so a windowed read
/// (`offset`/`limit`) restores the window the model was actually looking at.
#[derive(Debug, Clone, PartialEq)]
pub struct SpanRead {
    /// The workspace-relative path, as the recorded call spelled it.
    pub path: String,
    /// The full input of the most recent read of `path` in the span.
    pub input: serde_json::Value,
}

/// One active skill body the summarized span contained: the slug and the
/// full invocation message (marker line included), restored verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillBody {
    pub slug: String,
    /// The whole invocation message content —
    /// [`crate::skills::invoke::render_invocation_message`]'s shape.
    pub content: String,
}

/// What the summarized span held that is worth restoring. Owned data,
/// captured *before* the splice destroys the span.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkingSet {
    /// Most recent first, deduplicated by path, capped at
    /// [`RESTORE_MAX_FILES`], excluding paths the kept tail re-read.
    pub reads: Vec<SpanRead>,
    /// Active skills in span order, excluding slugs the kept tail
    /// re-invoked.
    pub skills: Vec<SkillBody>,
}

impl WorkingSet {
    /// Whether there is nothing to restore — the guard that keeps a
    /// read-free, skill-free summarization from emitting an empty marker
    /// message.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.reads.is_empty() && self.skills.is_empty()
    }
}

/// A fresh read's outcome, produced by the engine half replaying the read
/// through the tool port after the splice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreshContent {
    /// Current on-disk content, as the read tool rendered it.
    Current(String),
    /// The re-read failed (deleted, permission, hook block) — named in the
    /// restoration message so the model learns the file is *gone*, which is
    /// itself current on-disk state.
    Unreadable(String),
}

/// One path's fresh re-read, paired back to the [`SpanRead`] that asked for
/// it. Order is the working set's: most recent first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreshRead {
    pub path: String,
    pub content: FreshContent,
}

/// The rendered restoration, plus the ledger of what it contains and what
/// it dropped — counts for the lifecycle event, names for tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restoration {
    /// The one tail message, marker-prefixed, proven to fit the budget.
    pub message: String,
    /// Estimated token cost of `message` (the shared byte heuristic).
    pub tokens: u64,
    pub restored_files: Vec<String>,
    /// Files whose content did not fit the headroom — named in the message.
    pub dropped_files: Vec<String>,
    /// Files whose fresh read failed — named in the message.
    pub unreadable_files: Vec<String>,
    pub restored_skills: Vec<String>,
    /// Skills whose body did not fit the headroom — named in the message.
    pub dropped_skills: Vec<String>,
}

/// Collect the working set of `span` (the messages the summarizer is about
/// to splice away), excluding anything `kept_tail` (the messages that
/// survive) still holds. `active_slugs` is the live-invocation set from the
/// tool stack ([`crate::ports::ToolExecutor::active_skill_slugs`]); a skill
/// body is working set only while its invocation is live.
///
/// Pure and total: owned data in, owned data out, no I/O, no panics.
#[must_use]
pub fn collect_working_set(
    span: &[CompletionMessage],
    kept_tail: &[CompletionMessage],
    active_slugs: &[String],
) -> WorkingSet {
    // A read whose recorded result was an error produced no content the
    // splice can lose.
    let failed_calls: HashSet<&str> = span
        .iter()
        .flat_map(|m| m.tool_results.iter())
        .filter(|r| r.output.is_error())
        .map(|r| r.call_id.as_str())
        .collect();

    struct TrackedRead {
        path: String,
        input: serde_json::Value,
        seq: usize,
    }
    let mut reads: Vec<TrackedRead> = Vec::new();
    let mut skills: Vec<SkillBody> = Vec::new();
    let mut seq = 0usize;

    for message in span {
        match message.role {
            MessageRole::Assistant => {
                for call in &message.tool_calls {
                    if call.name != READ_TOOL || failed_calls.contains(call.call_id.as_str()) {
                        continue;
                    }
                    let Some(path) = call.input.get(READ_PATH_PARAM).and_then(|v| v.as_str())
                    else {
                        continue;
                    };
                    seq += 1;
                    match reads.iter_mut().find(|t| t.path == path) {
                        // A later read of the same path is the fresher view:
                        // its input (offset/limit included) wins outright.
                        Some(tracked) => {
                            tracked.input = call.input.clone();
                            tracked.seq = seq;
                        }
                        None => reads.push(TrackedRead {
                            path: path.to_string(),
                            input: call.input.clone(),
                            seq,
                        }),
                    }
                }
            }
            MessageRole::User => {
                if let Some(slug) = invocation_slug(&message.content) {
                    if active_slugs.iter().any(|active| active == slug) {
                        upsert_skill(&mut skills, slug, &message.content, true);
                    }
                } else if message.content.starts_with(RESTORE_MARKER_PREFIX) {
                    // A prior round's restoration message: re-contribute what
                    // it carried, at this (old) position's priority, without
                    // overriding anything the span observed directly.
                    let (paths, embedded_skills) = embedded_sections(&message.content);
                    for path in paths {
                        seq += 1;
                        if !reads.iter().any(|t| t.path == path) {
                            reads.push(TrackedRead {
                                input: serde_json::json!({ READ_PATH_PARAM: path.clone() }),
                                path,
                                seq,
                            });
                        }
                    }
                    for skill in embedded_skills {
                        if active_slugs.iter().any(|active| active == &skill.slug) {
                            upsert_skill(&mut skills, &skill.slug, &skill.content, false);
                        }
                    }
                }
            }
            MessageRole::System | MessageRole::Tool => {}
        }
    }

    // Anything the kept tail still holds was never lost: a re-read path, a
    // re-invoked skill, or either embedded in a restoration message that
    // survives the splice.
    let mut tail_paths: HashSet<String> = HashSet::new();
    let mut tail_slugs: HashSet<String> = HashSet::new();
    for message in kept_tail {
        for call in &message.tool_calls {
            if call.name == READ_TOOL
                && let Some(path) = call.input.get(READ_PATH_PARAM).and_then(|v| v.as_str())
            {
                tail_paths.insert(path.to_string());
            }
        }
        if message.role == MessageRole::User {
            if let Some(slug) = invocation_slug(&message.content) {
                tail_slugs.insert(slug.to_string());
            } else if message.content.starts_with(RESTORE_MARKER_PREFIX) {
                let (paths, embedded_skills) = embedded_sections(&message.content);
                tail_paths.extend(paths);
                tail_slugs.extend(embedded_skills.into_iter().map(|s| s.slug));
            }
        }
    }

    reads.retain(|t| !tail_paths.contains(&t.path));
    // Most recent first — the drop policy's priority order. `seq` is unique
    // by construction, so the order is total and deterministic.
    reads.sort_by_key(|read| std::cmp::Reverse(read.seq));
    reads.truncate(RESTORE_MAX_FILES);
    skills.retain(|s| !tail_slugs.contains(&s.slug));

    WorkingSet {
        reads: reads
            .into_iter()
            .map(|t| SpanRead {
                path: t.path,
                input: t.input,
            })
            .collect(),
        skills,
    }
}

/// Insert or refresh one tracked skill body. A direct invocation message
/// (`direct`) always wins over an embedded copy from a prior restoration;
/// an embedded copy never overwrites what the span observed directly.
fn upsert_skill(skills: &mut Vec<SkillBody>, slug: &str, content: &str, direct: bool) {
    match skills.iter_mut().find(|s| s.slug == slug) {
        Some(existing) => {
            if direct {
                existing.content = content.to_string();
            }
        }
        None => skills.push(SkillBody {
            slug: slug.to_string(),
            content: content.to_string(),
        }),
    }
}

/// Render the single restoration tail message, fitting `budget_tokens` (the
/// post-splice headroom under the compaction budget that triggered the
/// round). `None` means nothing to say: no candidates at all, or a headroom
/// too small even for the message that would name the drops.
///
/// The fit is proven, not hoped: every acceptance is checked against the
/// budget with the *worst-case* trailer (every candidate named as dropped)
/// reserved, and the actual trailer only ever names fewer items, so the
/// final message estimates at or under the budget by construction. Skills
/// are fitted before files — procedure text mid-execution is the working
/// set's most valuable item and is restored whole or not at all — then
/// files most recent first, each either fitting whole (per-file cap aside)
/// or dropped by name.
#[must_use]
pub fn render_restoration(
    skills: &[SkillBody],
    reads: &[FreshRead],
    budget_tokens: u64,
) -> Option<Restoration> {
    let mut readable: Vec<(&str, &str)> = Vec::new();
    let mut unreadable_files: Vec<String> = Vec::new();
    for read in reads {
        match &read.content {
            FreshContent::Current(content) => readable.push((&read.path, content)),
            FreshContent::Unreadable(_) => unreadable_files.push(read.path.clone()),
        }
    }
    if skills.is_empty() && readable.is_empty() && unreadable_files.is_empty() {
        return None;
    }

    let header = format!(
        "{RESTORE_MARKER_PREFIX} after summarization] The summarized span contained working \
         context, re-attached below. File contents are re-read fresh from disk NOW — they show \
         the current state, including any edits made since the earlier read. Skill procedures \
         are restored verbatim and still apply."
    );

    // Reserve the worst-case trailer up front: the actual trailer names a
    // subset of these items, so it can only be shorter.
    let worst_trailer = render_trailer(
        &skills.iter().map(|s| s.slug.clone()).collect::<Vec<_>>(),
        &readable
            .iter()
            .map(|(path, _)| (*path).to_string())
            .collect::<Vec<_>>(),
        &unreadable_files,
    );

    let mut body = header;
    let mut restored_skills = Vec::new();
    let mut dropped_skills = Vec::new();
    let mut restored_files = Vec::new();
    let mut dropped_files = Vec::new();

    for skill in skills {
        let section = format!(
            "\n\n{SKILL_SECTION_OPEN}{}{SECTION_HEADER_CLOSE}\n{}\n{SKILL_SECTION_CLOSE}",
            skill.slug, skill.content
        );
        if fits(&body, &section, &worst_trailer, budget_tokens) {
            body.push_str(&section);
            restored_skills.push(skill.slug.clone());
        } else {
            dropped_skills.push(skill.slug.clone());
        }
    }
    for (path, content) in &readable {
        let capped = cap_content(content);
        let section = format!(
            "\n\n{FILE_SECTION_OPEN}{path}{SECTION_HEADER_CLOSE}\n{capped}\n{FILE_SECTION_CLOSE}"
        );
        if fits(&body, &section, &worst_trailer, budget_tokens) {
            body.push_str(&section);
            restored_files.push((*path).to_string());
        } else {
            dropped_files.push((*path).to_string());
        }
    }

    body.push_str(&render_trailer(
        &dropped_skills,
        &dropped_files,
        &unreadable_files,
    ));
    let tokens = message_tokens(&body);
    if tokens > budget_tokens {
        // Nothing fit and even the drop-naming message overruns the
        // headroom. The summary's own marker already tells the model to
        // re-read; adding an over-budget message would defeat the very
        // compaction that just ran.
        return None;
    }
    Some(Restoration {
        message: body,
        tokens,
        restored_files,
        dropped_files,
        unreadable_files,
        restored_skills,
        dropped_skills,
    })
}

/// Whether `body + section + worst_trailer` still fits the budget.
fn fits(body: &str, section: &str, worst_trailer: &str, budget_tokens: u64) -> bool {
    let mut candidate = String::with_capacity(body.len() + section.len() + worst_trailer.len());
    candidate.push_str(body);
    candidate.push_str(section);
    candidate.push_str(worst_trailer);
    message_tokens(&candidate) <= budget_tokens
}

/// Estimated token cost of the restoration message, via the same estimator
/// the compaction decision uses — comparing anything else against the
/// compaction budget would be comparing two currencies.
fn message_tokens(content: &str) -> u64 {
    crate::estimator::estimate_message_tokens(&CompletionMessage::user(content))
}

/// Cap one file's content at [`RESTORE_FILE_BYTE_CAP`] on a char boundary,
/// marking the cut in place.
fn cap_content(content: &str) -> String {
    if content.len() <= RESTORE_FILE_BYTE_CAP {
        return content.to_string();
    }
    let mut cut = RESTORE_FILE_BYTE_CAP;
    while !content.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n{FILE_CAP_MARKER}", &content[..cut])
}

/// The trailer naming what this round could not restore. Empty when nothing
/// was dropped and nothing was unreadable.
fn render_trailer(
    dropped_skills: &[String],
    dropped_files: &[String],
    unreadable_files: &[String],
) -> String {
    let mut trailer = String::new();
    if !dropped_skills.is_empty() || !dropped_files.is_empty() {
        let mut names: Vec<String> = dropped_skills
            .iter()
            .map(|slug| format!("skill:{slug}"))
            .collect();
        names.extend(dropped_files.iter().cloned());
        trailer.push_str(&format!(
            "\n\nDropped for budget (most recent kept; re-read on demand): {}",
            names.join(", ")
        ));
    }
    if !unreadable_files.is_empty() {
        trailer.push_str(&format!(
            "\n\nNo longer readable (changed on disk since the earlier read): {}",
            unreadable_files.join(", ")
        ));
    }
    trailer
}

/// Parse the file paths and skill bodies embedded in a restoration message,
/// so a later summarization round can re-contribute them
/// ([`collect_working_set`]). Tolerant: an unterminated section contributes
/// nothing past what its header names.
fn embedded_sections(content: &str) -> (Vec<String>, Vec<SkillBody>) {
    let mut paths = Vec::new();
    let mut skills = Vec::new();
    let mut lines = content.lines();
    while let Some(line) = lines.next() {
        if let Some(rest) = line.strip_prefix(FILE_SECTION_OPEN) {
            if let Some(path) = rest.strip_suffix(SECTION_HEADER_CLOSE) {
                paths.push(path.to_string());
            }
        } else if let Some(rest) = line.strip_prefix(SKILL_SECTION_OPEN)
            && let Some(slug) = rest.strip_suffix(SECTION_HEADER_CLOSE)
        {
            let mut body_lines: Vec<&str> = Vec::new();
            for body_line in lines.by_ref() {
                if body_line == SKILL_SECTION_CLOSE {
                    break;
                }
                body_lines.push(body_line);
            }
            skills.push(SkillBody {
                slug: slug.to_string(),
                content: body_lines.join("\n"),
            });
        }
    }
    (paths, skills)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::invoke::render_invocation_message;
    use stella_protocol::{CompletionMessage, ToolCall, ToolOutput, ToolResult};

    fn read_call(call_id: &str, path: &str) -> CompletionMessage {
        CompletionMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                call_id: call_id.into(),
                name: READ_TOOL.into(),
                input: serde_json::json!({ READ_PATH_PARAM: path }),
            }],
            tool_results: vec![],
            attachments: Vec::new(),
        }
    }

    fn read_result(call_id: &str, output: ToolOutput) -> CompletionMessage {
        CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: vec![],
            tool_results: vec![ToolResult {
                call_id: call_id.into(),
                output,
            }],
            attachments: Vec::new(),
        }
    }

    fn ok(content: &str) -> ToolOutput {
        ToolOutput::Ok {
            content: content.into(),
        }
    }

    // ── collect_working_set ─────────────────────────────────────────────────

    #[test]
    fn reads_are_deduped_by_path_most_recent_first_and_capped() {
        let mut span = Vec::new();
        for i in 0..(RESTORE_MAX_FILES + 3) {
            span.push(read_call(&format!("c{i}"), &format!("src/f{i}.rs")));
            span.push(read_result(&format!("c{i}"), ok("content")));
        }
        // Re-read of the oldest path, now with a window: it is the freshest
        // read and its input must win whole.
        span.push(CompletionMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                call_id: "again".into(),
                name: READ_TOOL.into(),
                input: serde_json::json!({ READ_PATH_PARAM: "src/f0.rs", "offset": 10 }),
            }],
            tool_results: vec![],
            attachments: Vec::new(),
        });

        let set = collect_working_set(&span, &[], &[]);
        assert_eq!(set.reads.len(), RESTORE_MAX_FILES, "capped");
        assert_eq!(set.reads[0].path, "src/f0.rs", "freshest read first");
        assert_eq!(
            set.reads[0].input,
            serde_json::json!({ READ_PATH_PARAM: "src/f0.rs", "offset": 10 }),
            "the most recent read's full input wins"
        );
        assert_eq!(
            set.reads[1].path,
            format!("src/f{}.rs", RESTORE_MAX_FILES + 2)
        );
    }

    #[test]
    fn failed_reads_and_tail_reread_paths_are_not_working_set() {
        let span = vec![
            read_call("c1", "gone.rs"),
            read_result("c1", ToolOutput::error("no such file")),
            read_call("c2", "kept.rs"),
            read_result("c2", ok("kept content")),
            read_call("c3", "lost.rs"),
            read_result("c3", ok("lost content")),
        ];
        // The tail re-read `kept.rs`, so it was never lost.
        let tail = vec![read_call("t1", "kept.rs"), read_result("t1", ok("fresh"))];
        let set = collect_working_set(&span, &tail, &[]);
        assert_eq!(
            set.reads
                .iter()
                .map(|r| r.path.as_str())
                .collect::<Vec<_>>(),
            vec!["lost.rs"],
            "an errored read produced nothing to lose, a tail re-read is still present"
        );
    }

    #[test]
    fn only_active_skill_bodies_are_collected_and_tail_reinvocations_excluded() {
        let active = render_invocation_message("deploy", "Step 1. Step 2.");
        let stale = render_invocation_message("audit", "Old procedure.");
        let tail_covered = render_invocation_message("format", "Uppercase.");
        let span = vec![
            CompletionMessage::user(active.clone()),
            CompletionMessage::user(stale),
            CompletionMessage::user(tail_covered.clone()),
        ];
        let tail = vec![CompletionMessage::user(tail_covered)];
        let set = collect_working_set(&span, &tail, &["deploy".to_string(), "format".to_string()]);
        assert_eq!(
            set.skills,
            vec![SkillBody {
                slug: "deploy".into(),
                content: active,
            }],
            "inactive bodies are history; tail-covered bodies were never lost"
        );
    }

    #[test]
    fn an_empty_span_yields_an_empty_set() {
        let span = vec![CompletionMessage::user("just text")];
        assert!(collect_working_set(&span, &[], &[]).is_empty());
    }

    // ── render + budget ─────────────────────────────────────────────────────

    fn current(path: &str, content: &str) -> FreshRead {
        FreshRead {
            path: path.into(),
            content: FreshContent::Current(content.into()),
        }
    }

    #[test]
    fn nothing_to_restore_renders_no_message() {
        assert_eq!(render_restoration(&[], &[], 10_000), None);
    }

    #[test]
    fn drops_are_deterministic_most_recent_first_wins_and_named() {
        // Most recent (first) is small and fits; the older one is too big
        // for what remains and must be dropped BY NAME.
        let reads = vec![
            current("recent.rs", "fn recent() {}"),
            current("older.rs", &"x".repeat(40_000)),
        ];
        let restoration = render_restoration(&[], &reads, 600).expect("the small file fits");
        assert_eq!(restoration.restored_files, vec!["recent.rs"]);
        assert_eq!(restoration.dropped_files, vec!["older.rs"]);
        assert!(restoration.message.contains("fn recent() {}"));
        assert!(
            restoration.message.contains("Dropped for budget")
                && restoration.message.contains("older.rs"),
            "drops are named, never silent: {}",
            restoration.message
        );
        assert!(
            restoration.tokens <= 600,
            "the rendered message is proven to fit: {} tokens",
            restoration.tokens
        );
    }

    #[test]
    fn the_fit_is_proven_never_exceeded() {
        // Whatever the budget, the emitted message estimates at or under it.
        let reads: Vec<FreshRead> = (0..6)
            .map(|i| current(&format!("f{i}.rs"), &"line\n".repeat(200 * (i + 1))))
            .collect();
        let skills = vec![SkillBody {
            slug: "proc".into(),
            content: render_invocation_message("proc", &"step\n".repeat(100)),
        }];
        for budget in [50u64, 200, 500, 2_000, 100_000] {
            if let Some(restoration) = render_restoration(&skills, &reads, budget) {
                assert!(
                    restoration.tokens <= budget,
                    "budget {budget}: message estimated at {}",
                    restoration.tokens
                );
            }
        }
        // And a generous budget restores everything with no drops.
        let all = render_restoration(&skills, &reads, 100_000).expect("everything fits");
        assert_eq!(all.restored_files.len(), 6);
        assert_eq!(all.restored_skills, vec!["proc"]);
        assert!(all.dropped_files.is_empty() && all.dropped_skills.is_empty());
        assert!(
            !all.message.contains("Dropped for budget"),
            "no empty drop trailer"
        );
    }

    #[test]
    fn a_skill_body_is_restored_whole_or_not_at_all() {
        let skills = vec![SkillBody {
            slug: "long-procedure".into(),
            content: render_invocation_message("long-procedure", &"step\n".repeat(2_000)),
        }];
        let restoration =
            render_restoration(&skills, &[], 300).expect("the drop notice itself fits");
        assert!(restoration.restored_skills.is_empty());
        assert_eq!(restoration.dropped_skills, vec!["long-procedure"]);
        assert!(
            !restoration.message.contains("step\nstep"),
            "no truncated procedure text — a paraphrased or clipped procedure \
             is the exact failure restoration exists to prevent"
        );
    }

    #[test]
    fn per_file_cap_truncates_in_place_with_a_marker() {
        let reads = vec![current("big.rs", &"y".repeat(RESTORE_FILE_BYTE_CAP * 2))];
        let restoration = render_restoration(&[], &reads, 100_000).expect("fits after capping");
        assert_eq!(restoration.restored_files, vec!["big.rs"]);
        assert!(
            restoration.message.contains(FILE_CAP_MARKER),
            "a capped file says so in place"
        );
    }

    #[test]
    fn unreadable_files_are_named() {
        let reads = vec![FreshRead {
            path: "deleted.rs".into(),
            content: FreshContent::Unreadable("no such file".into()),
        }];
        let restoration = render_restoration(&[], &reads, 10_000).expect("the notice fits");
        assert!(restoration.restored_files.is_empty());
        assert_eq!(restoration.unreadable_files, vec!["deleted.rs"]);
        assert!(
            restoration.message.contains("No longer readable")
                && restoration.message.contains("deleted.rs")
        );
    }

    #[test]
    fn a_hopeless_headroom_renders_nothing() {
        let reads = vec![current("a.rs", "content")];
        assert_eq!(
            render_restoration(&[], &reads, 5),
            None,
            "below even the drop-naming floor, stay silent — the summary \
             marker already says to re-read"
        );
    }

    // ── chaining across rounds ──────────────────────────────────────────────

    #[test]
    fn a_prior_restoration_message_in_the_span_chains_its_paths_and_active_skills() {
        let skills = vec![SkillBody {
            slug: "deploy".into(),
            content: render_invocation_message("deploy", "Step 1.\nStep 2."),
        }];
        let reads = vec![current("src/chained.rs", "1\tfn chained() {}")];
        let prior = render_restoration(&skills, &reads, 100_000).expect("fits");

        // Next round: the prior restoration message falls inside the span.
        let span = vec![CompletionMessage::user(prior.message)];
        let set = collect_working_set(&span, &[], &["deploy".to_string()]);
        assert_eq!(
            set.reads,
            vec![SpanRead {
                path: "src/chained.rs".into(),
                input: serde_json::json!({ READ_PATH_PARAM: "src/chained.rs" }),
            }],
            "embedded paths re-contribute with a synthesized whole-file input"
        );
        assert_eq!(set.skills.len(), 1);
        assert_eq!(set.skills[0].slug, "deploy");
        assert!(
            set.skills[0].content.contains("Step 1.\nStep 2."),
            "the chained body is the verbatim invocation message: {}",
            set.skills[0].content
        );

        // Once the invocation ends, the chain ends with it.
        assert!(
            collect_working_set(&span, &[], &[]).skills.is_empty(),
            "an ended invocation stops chaining"
        );
    }

    #[test]
    fn a_direct_span_read_outranks_an_embedded_copy_of_the_same_path() {
        let prior = render_restoration(&[], &[current("same.rs", "old")], 100_000).expect("fits");
        let span = vec![
            CompletionMessage::user(prior.message),
            CompletionMessage {
                role: MessageRole::Assistant,
                content: String::new(),
                tool_calls: vec![ToolCall {
                    call_id: "d1".into(),
                    name: READ_TOOL.into(),
                    input: serde_json::json!({ READ_PATH_PARAM: "same.rs", "limit": 5 }),
                }],
                tool_results: vec![],
                attachments: Vec::new(),
            },
        ];
        let set = collect_working_set(&span, &[], &[]);
        assert_eq!(set.reads.len(), 1);
        assert_eq!(
            set.reads[0].input,
            serde_json::json!({ READ_PATH_PARAM: "same.rs", "limit": 5 }),
            "the direct read's richer input wins over the embedded copy"
        );
    }
}
