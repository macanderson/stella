// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Working-set restoration (#2685): the pure decision half of putting back
//! what overflow summarization destroys.
//!
//! When the overflow summarizer replaces the oldest span of a conversation
//! with a model-written summary (`driver::restore`), the working set inside
//! that span is gone — in particular the body of any skill the model was
//! mid-way through executing. The summary says *that* things happened; the
//! agent then proceeds on a lossy paraphrase of procedure text it was
//! following. This module decides **what** to restore and renders the one
//! tail message that carries it; the engine half (`driver::restore`) appends
//! it — zero model calls, zero I/O (invariant 2).
//!
//! # What is restored, and what deliberately is not
//!
//! - **The full body of any active invoked skill**
//!   ([`crate::skills::invoke::ActiveSkillInvocations`] is the tracking
//!   half): procedure text the model is executing must survive verbatim,
//!   never as the summary's paraphrase. An invocation whose span already
//!   ended is history, not working set, and is not restored.
//! - **Not restored**: anything still in context — a skill re-invoked in the
//!   kept tail, the skill listing in the stable prefix, roster
//!   announcements. Nothing outside the summarized span was lost, so
//!   nothing outside it is re-sent.
//! - **Deliberately not restored: file contents.** A file the span had read
//!   is recoverable on demand — the summary's own marker tells the model to
//!   re-read whatever it still needs through the session's tools, and a
//!   fresh on-demand read is always current, whereas procedure text
//!   mid-execution has no re-read path at all once its invocation message
//!   is folded away. Only the unrecoverable half is working set.
//!
//! # The budget is the compaction budget
//!
//! Restoration is charged against the same budget whose breach triggered the
//! summarization round ([`render_restoration`]'s `budget_tokens` is the
//! post-splice headroom under it), so restoring can never defeat compacting:
//! the rendered message is proven to fit before it is emitted. When it
//! cannot all fit, whole skill bodies are dropped deterministically — a
//! procedure is restored whole or not at all — and the drops are *named* in
//! the message, never silently elided.
//!
//! # Restoration survives the next round
//!
//! The restoration message is itself a `User`-role message in the volatile
//! tail, so a later summarization round will fold it away too. Its sections
//! are therefore rendered under parseable fences, and
//! [`collect_working_set`] re-collects skill bodies embedded in a prior
//! restoration message that falls inside the new span — an active skill's
//! body chains across rounds for as long as its invocation is live.

use std::collections::HashSet;

use stella_protocol::{CompletionMessage, MessageRole};

use crate::skills::invoke::invocation_slug;

/// Prefix of the engine-authored restoration tail message. Registered in
/// [`crate::engine_markers::ENGINE_MARKERS`] like every other engine-injected
/// `User`-role marker, taught to the model by the static prompts, and skipped
/// by loop detection's turn-boundary walk (`driver::loop_evidence`).
pub const RESTORE_MARKER_PREFIX: &str = "[working set restored";

/// Section fences. Parseable on the way back in ([`embedded_skills`]) so a
/// restoration message summarized away in a *later* round re-contributes its
/// skill bodies. A skill body containing a literal fence line would truncate
/// its own chained copy — a bounded, self-inflicted edge documented rather
/// than defended.
const SKILL_SECTION_OPEN: &str = "--- skill: ";
const SKILL_SECTION_CLOSE: &str = "--- end skill ---";
const SECTION_HEADER_CLOSE: &str = " ---";

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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkingSet {
    /// Active skills in span order, excluding slugs the kept tail
    /// re-invoked.
    pub skills: Vec<SkillBody>,
}

impl WorkingSet {
    /// Whether there is nothing to restore — the guard that keeps a
    /// skill-free summarization from emitting an empty marker message.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

/// The rendered restoration, plus the ledger of what it contains and what
/// it dropped — counts for the lifecycle event, names for tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restoration {
    /// The one tail message, marker-prefixed, proven to fit the budget.
    pub message: String,
    /// Estimated token cost of `message` (the shared byte heuristic).
    pub tokens: u64,
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
    let mut skills: Vec<SkillBody> = Vec::new();

    for message in span {
        if message.role != MessageRole::User {
            continue;
        }
        if let Some(slug) = invocation_slug(&message.content) {
            if active_slugs.iter().any(|active| active == slug) {
                upsert_skill(&mut skills, slug, &message.content, true);
            }
        } else if message.content.starts_with(RESTORE_MARKER_PREFIX) {
            // A prior round's restoration message: re-contribute what it
            // carried, without overriding anything the span observed
            // directly.
            for skill in embedded_skills(&message.content) {
                if active_slugs.iter().any(|active| active == &skill.slug) {
                    upsert_skill(&mut skills, &skill.slug, &skill.content, false);
                }
            }
        }
    }

    // Anything the kept tail still holds was never lost: a re-invoked skill,
    // or one embedded in a restoration message that survives the splice.
    let mut tail_slugs: HashSet<String> = HashSet::new();
    for message in kept_tail {
        if message.role != MessageRole::User {
            continue;
        }
        if let Some(slug) = invocation_slug(&message.content) {
            tail_slugs.insert(slug.to_string());
        } else if message.content.starts_with(RESTORE_MARKER_PREFIX) {
            tail_slugs.extend(
                embedded_skills(&message.content)
                    .into_iter()
                    .map(|s| s.slug),
            );
        }
    }
    skills.retain(|s| !tail_slugs.contains(&s.slug));

    WorkingSet { skills }
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
/// final message estimates at or under the budget by construction. A skill
/// body is restored whole or not at all — procedure text mid-execution is
/// exactly what must never survive as a truncated paraphrase.
#[must_use]
pub fn render_restoration(skills: &[SkillBody], budget_tokens: u64) -> Option<Restoration> {
    if skills.is_empty() {
        return None;
    }

    let header = format!(
        "{RESTORE_MARKER_PREFIX} after summarization] The summarized span contained working \
         context, re-attached below. Skill procedures are restored verbatim and still apply."
    );

    // Reserve the worst-case trailer up front: the actual trailer names a
    // subset of these items, so it can only be shorter.
    let worst_trailer = render_trailer(&skills.iter().map(|s| s.slug.clone()).collect::<Vec<_>>());

    let mut body = header;
    let mut restored_skills = Vec::new();
    let mut dropped_skills = Vec::new();

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

    body.push_str(&render_trailer(&dropped_skills));
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

/// The trailer naming what this round could not restore. Empty when nothing
/// was dropped.
fn render_trailer(dropped_skills: &[String]) -> String {
    if dropped_skills.is_empty() {
        return String::new();
    }
    let names: Vec<String> = dropped_skills
        .iter()
        .map(|slug| format!("skill:{slug}"))
        .collect();
    format!(
        "\n\nDropped for budget (re-invoke on demand): {}",
        names.join(", ")
    )
}

/// Parse the skill bodies embedded in a restoration message, so a later
/// summarization round can re-contribute them ([`collect_working_set`]).
/// Tolerant: an unterminated section contributes nothing past what its
/// header names.
fn embedded_skills(content: &str) -> Vec<SkillBody> {
    let mut skills = Vec::new();
    let mut lines = content.lines();
    while let Some(line) = lines.next() {
        if let Some(rest) = line.strip_prefix(SKILL_SECTION_OPEN)
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
    skills
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::invoke::render_invocation_message;
    use stella_protocol::CompletionMessage;

    // ── collect_working_set ─────────────────────────────────────────────────

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

    #[test]
    fn nothing_to_restore_renders_no_message() {
        assert_eq!(render_restoration(&[], 10_000), None);
    }

    #[test]
    fn the_fit_is_proven_never_exceeded() {
        // Whatever the budget, the emitted message estimates at or under it.
        let skills: Vec<SkillBody> = (0..4)
            .map(|i| SkillBody {
                slug: format!("proc-{i}"),
                content: render_invocation_message(
                    &format!("proc-{i}"),
                    &"step\n".repeat(100 * (i + 1)),
                ),
            })
            .collect();
        for budget in [50u64, 200, 500, 2_000, 100_000] {
            if let Some(restoration) = render_restoration(&skills, budget) {
                assert!(
                    restoration.tokens <= budget,
                    "budget {budget}: message estimated at {}",
                    restoration.tokens
                );
            }
        }
        // And a generous budget restores everything with no drops.
        let all = render_restoration(&skills, 100_000).expect("everything fits");
        assert_eq!(all.restored_skills.len(), 4);
        assert!(all.dropped_skills.is_empty());
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
        let restoration = render_restoration(&skills, 300).expect("the drop notice itself fits");
        assert!(restoration.restored_skills.is_empty());
        assert_eq!(restoration.dropped_skills, vec!["long-procedure"]);
        assert!(
            restoration.message.contains("Dropped for budget")
                && restoration.message.contains("skill:long-procedure"),
            "drops are named, never silent: {}",
            restoration.message
        );
        assert!(
            !restoration.message.contains("step\nstep"),
            "no truncated procedure text — a paraphrased or clipped procedure \
             is the exact failure restoration exists to prevent"
        );
    }

    #[test]
    fn a_hopeless_headroom_renders_nothing() {
        let skills = vec![SkillBody {
            slug: "proc".into(),
            content: render_invocation_message("proc", "one step"),
        }];
        assert_eq!(
            render_restoration(&skills, 5),
            None,
            "below even the drop-naming floor, stay silent — the summary \
             marker already says to re-read"
        );
    }

    // ── chaining across rounds ──────────────────────────────────────────────

    #[test]
    fn a_prior_restoration_message_in_the_span_chains_its_active_skills() {
        let skills = vec![SkillBody {
            slug: "deploy".into(),
            content: render_invocation_message("deploy", "Step 1.\nStep 2."),
        }];
        let prior = render_restoration(&skills, 100_000).expect("fits");

        // Next round: the prior restoration message falls inside the span.
        let span = vec![CompletionMessage::user(prior.message)];
        let set = collect_working_set(&span, &[], &["deploy".to_string()]);
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
    fn a_direct_span_invocation_outranks_an_embedded_copy_of_the_same_slug() {
        let embedded = vec![SkillBody {
            slug: "deploy".into(),
            content: render_invocation_message("deploy", "Old steps."),
        }];
        let prior = render_restoration(&embedded, 100_000).expect("fits");
        let direct = render_invocation_message("deploy", "New steps.");
        let span = vec![
            CompletionMessage::user(prior.message),
            CompletionMessage::user(direct.clone()),
        ];
        let set = collect_working_set(&span, &[], &["deploy".to_string()]);
        assert_eq!(set.skills.len(), 1);
        assert_eq!(
            set.skills[0].content, direct,
            "the direct invocation message wins over the embedded copy"
        );
    }
}
