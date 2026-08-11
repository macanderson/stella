// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Skill invocation — the pure half of the `invoke_skill` tool (#2682).
//!
//! Skills have been *context* since they existed: selected, rendered, and
//! followed, never executed. This module gives them an execution vocabulary
//! while keeping every decision pure and I/O-free (invariant #2): the CLI's
//! session layer owns the tool, the file reads, and the sub-agent dispatch;
//! everything here is string-in/value-out and unit-tested below.
//!
//! # Behavior is the skill's, never a parameter's
//!
//! Invariant #9: a tool parameter may scope an operation, never select one.
//! How an invocation runs — inline expansion vs. a forked sub-agent, which
//! tools it may use, which effort serves it — is declared by the **skill's
//! own frontmatter** ([`parse_invoke_directives`]) and the tool input carries
//! only the slug and its arguments.
//!
//! # The invocation marker
//!
//! An inline invocation lands in the transcript as a **user-role tail
//! message** under [`SKILL_INVOCATION_PREFIX`], mirroring the engine's other
//! recognizable user-role injections (`driver::LOOP_STEER_PREFIX`,
//! `driver::SUMMARY_MARKER_PREFIX`, `receipts::RECALL_MARKER`). User-role on
//! the wire, engine-generated: the marker is what lets every downstream
//! consumer — receipts block-typing, loop evidence, and the compaction seam
//! below — tell an invoked skill body from a real user turn.
//!
//! # The compaction seam (#2685's anchor)
//!
//! [`ActiveSkillInvocations`] tracks which invocations are live, and
//! [`ActiveSkillInvocations::protects_message`] answers the one question the
//! overflow summarizer needs: "is this message an *active* skill's invoked
//! body?". The pure compaction passes never touch user-role text, so the only
//! consumer that can destroy an invoked body is the overflow summarizer's
//! span splice — wiring this predicate into its span selection (and restoring
//! bodies after a splice) is ticket #2685; this module deliberately ships the
//! tracking and the predicate only.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use stella_protocol::ReasoningEffort;

/// Prefix of the user-role message carrying an inline skill invocation.
///
/// The full first line is `[skill invocation: <slug>]`; the body follows
/// after a blank line. Kept beside the engine's other marker prefixes in
/// spirit — `driver.rs` (their historical home) is closed to growth, so the
/// constant lives with the machinery that mints the message.
pub const SKILL_INVOCATION_PREFIX: &str = "[skill invocation";

/// The placeholder [`substitute_arguments`] replaces in a skill body.
pub const ARGUMENTS_PLACEHOLDER: &str = "$ARGUMENTS";

/// How a skill runs when invoked, declared by its `context:` frontmatter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SkillInvocationMode {
    /// Expand the full body into the parent transcript (the default).
    #[default]
    Inline,
    /// Run in a sub-agent; only the result crosses back (`context: fork`).
    Fork,
}

/// A frontmatter value this parser recognized the key of but not the value —
/// degraded gracefully to the default, never a parse failure (#2682).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectiveDiagnostic {
    /// `context:` named neither `inline` nor `fork`; the mode fell back to
    /// [`SkillInvocationMode::Inline`].
    UnknownContext { value: String },
    /// `effort:` named no [`ReasoningEffort`] level; the override was
    /// dropped.
    UnknownEffort { value: String },
}

/// The invocation directives a skill's frontmatter declares (#2682): mode,
/// tool grant, and model/effort overrides. Unknown *keys* are someone else's
/// frontmatter and are ignored wholesale; unknown *values* of known keys
/// degrade to the default with a [`DirectiveDiagnostic`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InvokeDirectives {
    /// `context:` — inline (default) or fork.
    pub mode: SkillInvocationMode,
    /// `allowed-tools:` (or `allowed_tools:`) — tool names/groups the skill
    /// may use, comma- or whitespace-separated. `None` when absent: the
    /// skill inherits the session surface unchanged. The grant only ever
    /// **narrows** — intersection with the operator policy is enforced by
    /// `stella-tools::skill_grant`, never here.
    pub allowed_tools: Option<Vec<String>>,
    /// `model:` — the model the skill asks to run under (fork mode). Carried
    /// as the raw slug; resolving it to a provider is the host's business.
    pub model: Option<String>,
    /// `effort:` — reasoning effort override for the skill's duration
    /// (fork mode).
    pub effort: Option<ReasoningEffort>,
    /// Values that were recognized keys but unusable values, kept so the
    /// tool can surface them instead of silently dropping author intent.
    pub diagnostics: Vec<DirectiveDiagnostic>,
}

/// Parse a raw `SKILL.md` file's invocation directives. Tolerant by
/// construction: no frontmatter, unknown keys, and unusable values all
/// produce a usable [`InvokeDirectives`] — never an error, never a panic.
pub fn parse_invoke_directives(raw: &str) -> InvokeDirectives {
    let fm = crate::rules::parse_frontmatter(raw);
    let mut directives = InvokeDirectives::default();

    match fm
        .data
        .get("context")
        .map(|v| v.trim().to_ascii_lowercase())
    {
        None => {}
        Some(value) if value == "inline" || value.is_empty() => {}
        Some(value) if value == "fork" => directives.mode = SkillInvocationMode::Fork,
        Some(value) => directives
            .diagnostics
            .push(DirectiveDiagnostic::UnknownContext { value }),
    }

    let allowed = fm
        .data
        .get("allowed-tools")
        .or_else(|| fm.data.get("allowed_tools"));
    if let Some(raw_list) = allowed {
        let names: Vec<String> = raw_list
            .split([',', ' ', '\t'])
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect();
        if !names.is_empty() {
            directives.allowed_tools = Some(names);
        }
    }

    if let Some(model) = fm.data.get("model").map(|v| v.trim())
        && !model.is_empty()
    {
        directives.model = Some(model.to_string());
    }

    if let Some(effort) = fm.data.get("effort").map(|v| v.trim()) {
        match parse_effort(effort) {
            Some(level) => directives.effort = Some(level),
            None if effort.is_empty() => {}
            None => directives
                .diagnostics
                .push(DirectiveDiagnostic::UnknownEffort {
                    value: effort.to_string(),
                }),
        }
    }

    directives
}

/// Parse one `effort:` value. Case-insensitive over the wire spellings of
/// [`ReasoningEffort`]; `None` for anything else.
fn parse_effort(value: &str) -> Option<ReasoningEffort> {
    match value.to_ascii_lowercase().as_str() {
        "low" => Some(ReasoningEffort::Low),
        "medium" => Some(ReasoningEffort::Medium),
        "high" => Some(ReasoningEffort::High),
        "xhigh" => Some(ReasoningEffort::Xhigh),
        "max" => Some(ReasoningEffort::Max),
        _ => None,
    }
}

/// Substitute the invocation arguments into a skill body.
///
/// Every [`ARGUMENTS_PLACEHOLDER`] occurrence is replaced with `arguments`
/// (which may be empty — an argument-less invocation of a parameterized
/// skill reads better with the placeholder gone than with it dangling).
/// A body with **no** placeholder and non-empty arguments gets them appended
/// under an `ARGUMENTS:` trailer, so the author's omission never silently
/// discards what the caller passed.
pub fn substitute_arguments(body: &str, arguments: &str) -> String {
    if body.contains(ARGUMENTS_PLACEHOLDER) {
        return body.replace(ARGUMENTS_PLACEHOLDER, arguments);
    }
    if arguments.is_empty() {
        return body.to_string();
    }
    format!("{body}\n\nARGUMENTS: {arguments}")
}

/// Render the user-role message an inline invocation injects: the marker
/// line naming the slug, a blank line, then the (already argument-
/// substituted) body.
pub fn render_invocation_message(slug: &str, body: &str) -> String {
    format!("{SKILL_INVOCATION_PREFIX}: {slug}]\n\n{body}")
}

/// The slug named by an invocation message's marker line, or `None` when
/// `content` is not an invocation message.
pub fn invocation_slug(content: &str) -> Option<&str> {
    let rest = content.strip_prefix(SKILL_INVOCATION_PREFIX)?;
    let rest = rest.strip_prefix(':')?;
    let line = rest.lines().next()?;
    let slug = line.trim().strip_suffix(']')?.trim();
    (!slug.is_empty()).then_some(slug)
}

/// Which skill invocations are currently live — the tracking half of the
/// compaction seam (#2685).
///
/// Cloneable handle over shared state so the tool layer (which begins spans)
/// and a future summarizer hook (which reads them) can hold the same set
/// without naming each other. Spans end **structurally**: [`Self::begin`]
/// returns a [`SkillSpan`] guard and the entry leaves the set when the guard
/// drops — on the ordinary path, on an unwind, and when the per-turn tool
/// stack holding it is torn down.
#[derive(Clone, Default)]
pub struct ActiveSkillInvocations {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    /// `(span id, slug)` pairs. A Vec, not a set: the same skill invoked
    /// twice is two spans, and the body stays protected until the *last*
    /// one ends.
    spans: Mutex<Vec<(u64, String)>>,
    next_id: AtomicU64,
}

impl ActiveSkillInvocations {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark `slug` active until the returned guard drops.
    #[must_use = "the invocation ends the moment this guard is dropped"]
    pub fn begin(&self, slug: &str) -> SkillSpan {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.inner
            .spans
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((id, slug.to_string()));
        SkillSpan {
            inner: self.inner.clone(),
            id,
        }
    }

    /// Whether any live span names `slug`.
    #[must_use]
    pub fn is_active(&self, slug: &str) -> bool {
        self.inner
            .spans
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .any(|(_, active)| active == slug)
    }

    /// Every live slug, oldest span first (duplicates preserved).
    #[must_use]
    pub fn active_slugs(&self) -> Vec<String> {
        self.inner
            .spans
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(_, slug)| slug.clone())
            .collect()
    }

    /// The compaction-seam predicate: whether `content` is the invocation
    /// message of a skill whose span is still live. The overflow summarizer
    /// consults this before folding a message into a span (#2685); the pure
    /// compaction passes never touch user-role text, so they need no hook.
    #[must_use]
    pub fn protects_message(&self, content: &str) -> bool {
        invocation_slug(content).is_some_and(|slug| self.is_active(slug))
    }
}

/// A live invocation span. Dropping it ends the span — see
/// [`ActiveSkillInvocations`].
pub struct SkillSpan {
    inner: Arc<Inner>,
    id: u64,
}

impl Drop for SkillSpan {
    fn drop(&mut self) {
        self.inner
            .spans
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|(id, _)| *id != self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Frontmatter directives (#2682 witness d) ────────────────────────────

    /// The full directive surface round-trips through a real SKILL.md shape,
    /// and keys this module has never heard of (`max-risk` is #2716's) are
    /// tolerated rather than fatal.
    #[test]
    fn directives_round_trip_and_tolerate_unknown_keys() {
        let raw = "---\n\
                   name: release-notes\n\
                   description: Write release notes\n\
                   context: fork\n\
                   allowed-tools: read_file, grep glob\n\
                   model: gpt-5.2\n\
                   effort: High\n\
                   max-risk: high\n\
                   ---\n\
                   Lead with user impact for $ARGUMENTS.\n";
        let directives = parse_invoke_directives(raw);
        assert_eq!(
            directives,
            InvokeDirectives {
                mode: SkillInvocationMode::Fork,
                allowed_tools: Some(vec![
                    "read_file".to_string(),
                    "grep".to_string(),
                    "glob".to_string(),
                ]),
                model: Some("gpt-5.2".to_string()),
                effort: Some(ReasoningEffort::High),
                diagnostics: vec![],
            }
        );
        // Parsing is idempotent over the same bytes — the "round-trip" half.
        assert_eq!(parse_invoke_directives(raw), directives);
        // And the existing skill parser still accepts the file whole: the new
        // keys ride beside name/description/body without disturbing them.
        let skill = crate::skills::skill_from_file("skills/release-notes/SKILL.md", raw)
            .expect("directive keys must not break skill parsing");
        assert_eq!(skill.name, "release-notes");
        assert!(skill.body.contains(ARGUMENTS_PLACEHOLDER));
    }

    /// Unknown *values* of known keys degrade to the default with a typed
    /// diagnostic — never a panic, never a refused skill.
    #[test]
    fn unknown_values_degrade_with_a_diagnostic() {
        let raw = "---\nname: x\ndescription: d\ncontext: detached\neffort: turbo\n---\nbody\n";
        let directives = parse_invoke_directives(raw);
        assert_eq!(directives.mode, SkillInvocationMode::Inline);
        assert_eq!(directives.effort, None);
        assert_eq!(
            directives.diagnostics,
            vec![
                DirectiveDiagnostic::UnknownContext {
                    value: "detached".to_string()
                },
                DirectiveDiagnostic::UnknownEffort {
                    value: "turbo".to_string()
                },
            ]
        );
    }

    /// A skill with no directives at all is the common case and must parse to
    /// the defaults: inline, no grant, no overrides, no diagnostics.
    #[test]
    fn absent_directives_mean_inline_with_no_overrides() {
        let raw = "---\nname: x\ndescription: d\n---\nbody\n";
        assert_eq!(parse_invoke_directives(raw), InvokeDirectives::default());
        // No frontmatter at all degrades the same way.
        assert_eq!(
            parse_invoke_directives("just a body"),
            InvokeDirectives::default()
        );
    }

    #[test]
    fn effort_values_parse_case_insensitively() {
        for (spelled, level) in [
            ("low", ReasoningEffort::Low),
            ("Medium", ReasoningEffort::Medium),
            ("HIGH", ReasoningEffort::High),
            ("xhigh", ReasoningEffort::Xhigh),
            ("Max", ReasoningEffort::Max),
        ] {
            let raw = format!("---\nname: x\ndescription: d\neffort: {spelled}\n---\nbody\n");
            assert_eq!(
                parse_invoke_directives(&raw).effort,
                Some(level),
                "effort `{spelled}`"
            );
        }
    }

    // ── Argument substitution and the marker ────────────────────────────────

    #[test]
    fn arguments_replace_every_placeholder() {
        let body = "Check $ARGUMENTS, then re-check $ARGUMENTS.";
        assert_eq!(
            substitute_arguments(body, "src/lib.rs"),
            "Check src/lib.rs, then re-check src/lib.rs."
        );
        // Empty arguments erase the placeholder rather than dangling it.
        assert_eq!(substitute_arguments(body, ""), "Check , then re-check .");
    }

    #[test]
    fn arguments_without_a_placeholder_are_appended_never_dropped() {
        assert_eq!(
            substitute_arguments("Fixed procedure.", "extra context"),
            "Fixed procedure.\n\nARGUMENTS: extra context"
        );
        assert_eq!(
            substitute_arguments("Fixed procedure.", ""),
            "Fixed procedure."
        );
    }

    #[test]
    fn the_invocation_message_carries_the_marker_and_names_its_slug() {
        let message = render_invocation_message("release-notes", "Lead with impact.");
        assert!(message.starts_with(SKILL_INVOCATION_PREFIX));
        assert_eq!(invocation_slug(&message), Some("release-notes"));
        // Non-invocation content parses to nothing.
        assert_eq!(invocation_slug("ordinary user text"), None);
        assert_eq!(invocation_slug("[skill invocation but no close"), None);
    }

    // ── Active-invocation tracking (the #2685 seam) ─────────────────────────

    #[test]
    fn spans_end_structurally_when_their_guard_drops() {
        let active = ActiveSkillInvocations::new();
        let message = render_invocation_message("sql-formatting", "Uppercase keywords.");
        assert!(!active.protects_message(&message), "nothing active yet");

        let span = active.begin("sql-formatting");
        assert!(active.is_active("sql-formatting"));
        assert!(active.protects_message(&message));
        // A different skill's message is not protected by this span.
        assert!(!active.protects_message(&render_invocation_message("other", "x")));
        // Ordinary user text is never protected, whatever is active.
        assert!(!active.protects_message("please also run the tests"));

        drop(span);
        assert!(!active.is_active("sql-formatting"));
        assert!(!active.protects_message(&message), "teardown is structural");
    }

    #[test]
    fn overlapping_spans_of_one_skill_protect_until_the_last_ends() {
        let active = ActiveSkillInvocations::new();
        let first = active.begin("audit");
        let second = active.begin("audit");
        drop(first);
        assert!(
            active.is_active("audit"),
            "the second span must keep the skill active"
        );
        drop(second);
        assert!(!active.is_active("audit"));
        assert!(active.active_slugs().is_empty());
    }
}
