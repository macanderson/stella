// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella skill run <slug> [args…]` — the CLI door of the skill-function
//! invocation surface.
//!
//! A skill's own frontmatter declares how it runs
//! (`stella_core::skill_invocation`): `context:` inline or fork,
//! `allowed-tools:` the grant, `model:`/`effort:` the overrides. This verb
//! resolves the slug against the same loaders the recall engine and the ⚡
//! slash menu read, renders the invocation message with `$ARGUMENTS`
//! substituted, and launches it through the one-shot door with the scope the
//! directives ask for — `agent::run_turn` mounts the invocation plane, so the
//! grant is the `operator ∧ grant` intersection of
//! `stella_tools::skill_grant`, enforced at execution time.
//!
//! # No new callable tool
//!
//! This verb is the surface left room for: `invoke_skill` stays in
//! `stella_tools::catalog::RETIRED_TOOL_NAMES`, so a skill function runs only
//! when a human asks — here, or as an in-session `/slug` expansion — never
//! because the model called a tool.
//!
//! # `context: fork` at this door
//!
//! A CLI invocation *is* a fresh context: the run starts with no prior
//! transcript, exactly what `fork` asks for, so both modes launch the same
//! scoped one-shot here and the mode is reported rather than branched on.
//! In-session, the distinction is the expansion seam's
//! (`crate::extensions::skill_turn_scope`).

use std::path::Path;

use stella_core::skill_invocation::{self, DirectiveDiagnostic, SkillInvocationMode};

use crate::OutputFormat;
use crate::config::Config;
use crate::extensions::{InvokedSkill, skill_turn_scope};
use crate::failure::CliFailure;

/// Everything `run` needs, resolved before a provider exists so a typo'd
/// slug or an unreadable skill fails free of charge.
#[derive(Debug)]
pub(crate) struct SkillRunPlan {
    /// The prompt the run executes: the invocation-marker message with the
    /// arguments substituted into the body.
    pub(crate) prompt: String,
    /// The `model:` directive, raw — resolved to a provider by `main`'s
    /// config loader, exactly as `--model` is (the flag, when given, wins).
    pub(crate) model: Option<String>,
    /// How the skill asked to run, for the launch line.
    pub(crate) mode: SkillInvocationMode,
    /// Recognized keys with unusable values, surfaced instead of silently
    /// dropping author intent.
    pub(crate) diagnostics: Vec<DirectiveDiagnostic>,
    /// What rides the recall seam into the turn: the injection event and the
    /// turn scope (grant + effort) the drivers enforce.
    pub(crate) invoked: InvokedSkill,
}

/// Resolve `slug` against the loaded skills and plan its run.
///
/// Errors name what exists: an unknown slug lists the invocable slugs, so
/// the fix is a copy-paste away rather than a directory walk.
pub(crate) fn plan(
    workspace_root: &Path,
    authority: &crate::settings::AuthorityPolicy,
    slug: &str,
    args: &[String],
) -> Result<SkillRunPlan, String> {
    let loaded = crate::memory::load_workspace_skills_with_authority(
        workspace_root,
        authority.project_prompts_allowed,
    );
    let Some(skill) = loaded.skills.iter().find(|skill| skill.name == slug) else {
        let mut known: Vec<&str> = loaded.skills.iter().map(|s| s.name.as_str()).collect();
        known.sort_unstable();
        return Err(if known.is_empty() {
            format!(
                "no skill named `{slug}`: no skills are loaded — add one under \
                 .stella/skills/ or ~/.stella/skills/"
            )
        } else {
            format!(
                "no skill named `{slug}`. Loaded skills: {}",
                known.join(", ")
            )
        });
    };

    let directives = crate::extensions::invoke_directives_for(skill);
    let arguments = args.join(" ");
    let body = skill_invocation::substitute_arguments(&skill.body, arguments.trim());
    let prompt = skill_invocation::render_invocation_message(&skill.name, &body);
    Ok(SkillRunPlan {
        model: directives.model.clone(),
        mode: directives.mode,
        diagnostics: directives.diagnostics.clone(),
        invoked: InvokedSkill {
            name: skill.name.clone(),
            summary: skill.description.clone(),
            tokens: u32::try_from(stella_protocol::estimate_tokens(&prompt)).unwrap_or(u32::MAX),
            scope: skill_turn_scope(&skill.name, &directives),
        },
        prompt,
    })
}

/// One human-readable line per unusable directive value — printed, never
/// fatal, matching the parser's own degrade-gracefully contract.
pub(crate) fn diagnostic_lines(diagnostics: &[DirectiveDiagnostic]) -> Vec<String> {
    diagnostics
        .iter()
        .map(|diagnostic| match diagnostic {
            DirectiveDiagnostic::UnknownContext { value } => {
                format!("context: `{value}` is neither `inline` nor `fork` — running inline")
            }
            DirectiveDiagnostic::UnknownEffort { value } => {
                format!("effort: `{value}` names no effort level — override dropped")
            }
        })
        .collect()
}

/// The one-line launch report: slug, mode, and the scope actually applied.
pub(crate) fn launch_line(plan: &SkillRunPlan) -> String {
    let mode = match plan.mode {
        SkillInvocationMode::Inline => "inline",
        SkillInvocationMode::Fork => "fork (a one-shot run is already a fresh context)",
    };
    let scope = plan.invoked.scope.as_ref();
    let tools = match scope.and_then(|s| s.allowed_tools.as_ref()) {
        Some(granted) => format!("tools: {}", granted.join(", ")),
        None => "tools: session surface".to_string(),
    };
    let effort = match scope.and_then(|s| s.effort) {
        Some(effort) => format!(", effort: {effort:?}"),
        None => String::new(),
    };
    format!(
        "skill `{}` — context: {mode}, {tools}{effort}",
        plan.invoked.name
    )
}

/// Launch the planned run through the one-shot door. `cfg` already carries
/// the skill's `model:` resolution when one applied (`main` reloads the
/// config through the same path `--model` takes, flag still winning).
pub(crate) async fn run(
    cfg: &Config,
    plan: SkillRunPlan,
    budget_limit: Option<f64>,
    format: OutputFormat,
) -> Result<(), CliFailure> {
    if format == OutputFormat::Text {
        for line in diagnostic_lines(&plan.diagnostics) {
            eprintln!("  ! {line}");
        }
        println!("  {}", launch_line(&plan));
    }
    let prompt = plan.prompt.clone();
    crate::agent::run_one_shot(
        cfg,
        &prompt,
        budget_limit,
        format,
        // The raw loop, always: a skill's directives are the whole contract
        // of this door, and a wrapper option is `stella run --pipeline`'s
        // business.
        crate::wrapper_plugin::PipelineChoice::resolve(false, None).map_err(CliFailure::from)?,
        None,
        false,
        Some(plan.invoked),
    )
    .await
}

/// The stamped-flag carryover for a `model:` directive reload: `Config::load`
/// resolves provider and credentials, and these fields are `main`'s to stamp
/// (its own comment says why) — so a reload must carry them over or the
/// skill-run door would silently drop `--turn-timeout`, `--tools`, and the
/// rest. `model_pinned_by_flag` decides precedence *before* this is called.
pub(crate) fn carry_stamped_flags(fresh: &mut Config, base: &Config) {
    fresh.turn_timeout = base.turn_timeout;
    fresh.max_output_tokens = base.max_output_tokens;
    fresh.plan_mode = base.plan_mode;
    fresh.minimal_prompt = base.minimal_prompt;
    fresh.tool_policy = base.tool_policy.clone();
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::ReasoningEffort;

    fn authority() -> crate::settings::AuthorityPolicy {
        crate::settings::AuthorityPolicy {
            project_prompts_allowed: true,
            ..Default::default()
        }
    }

    /// The user tier redirected to an empty sandbox, so the developer's real
    /// `~/.stella/skills` cannot leak into a slug resolution whose whole
    /// subject is a temp directory.
    fn sandboxed_home(home: &Path) -> crate::paths::TestHomeGuard {
        crate::paths::test_user_home(home.to_path_buf())
    }

    fn write_skill(root: &Path, slug: &str, frontmatter_extra: &str, body: &str) {
        let dir = root.join(".stella/skills").join(slug);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!(
                "---\nname: {slug}\ndescription: about {slug}\n{frontmatter_extra}---\n{body}\n"
            ),
        )
        .unwrap();
    }

    /// **The skill-run CLI-verb witness.** `plan` resolves the slug through the
    /// production loader, parses the file's own directives, substitutes the
    /// arguments, and hands back exactly the scope the turn driver enforces
    /// — grant, effort, mode — plus the invocation-marker prompt.
    #[test]
    fn plan_resolves_the_slug_and_carries_the_directive_scope() {
        let home = tempfile::tempdir().unwrap();
        let _home = sandboxed_home(home.path());
        let dir = tempfile::tempdir().unwrap();
        write_skill(
            dir.path(),
            "generate-quarter-seed",
            "context: fork\nallowed-tools: bash, read_file\nmodel: openai/gpt-5.2\neffort: high\n",
            "Seed the quarter named by $ARGUMENTS.",
        );

        let plan = plan(
            dir.path(),
            &authority(),
            "generate-quarter-seed",
            &["--quarter".to_string(), "2025-Q3".to_string()],
        )
        .expect("the skill resolves");

        assert!(
            plan.prompt
                .starts_with(skill_invocation::SKILL_INVOCATION_PREFIX),
            "the prompt is the invocation message: {}",
            plan.prompt
        );
        assert!(
            plan.prompt
                .contains("Seed the quarter named by --quarter 2025-Q3."),
            "arguments are substituted: {}",
            plan.prompt
        );
        assert_eq!(plan.mode, SkillInvocationMode::Fork);
        assert_eq!(plan.model.as_deref(), Some("openai/gpt-5.2"));
        assert!(plan.diagnostics.is_empty());
        let scope = plan.invoked.scope.as_ref().expect("directives scope it");
        assert_eq!(
            scope.allowed_tools.as_deref(),
            Some(&["bash".to_string(), "read_file".to_string()][..])
        );
        assert_eq!(scope.effort, Some(ReasoningEffort::High));
        // What the launch line reports is the scope that will be enforced.
        let line = launch_line(&plan);
        assert!(line.contains("bash, read_file"), "{line}");
        assert!(line.contains("fork"), "{line}");
    }

    /// An unknown slug fails before any provider exists, naming what IS
    /// loaded; an empty skills directory names the fix instead.
    #[test]
    fn an_unknown_slug_is_refused_naming_the_loaded_skills() {
        let home = tempfile::tempdir().unwrap();
        let _home = sandboxed_home(home.path());
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "release-notes", "", "Lead with impact.");

        let error = plan(dir.path(), &authority(), "relaese-notes", &[]).unwrap_err();
        assert!(error.contains("relaese-notes"), "{error}");
        assert!(
            error.contains("release-notes"),
            "names what exists: {error}"
        );

        let empty = tempfile::tempdir().unwrap();
        let error = plan(empty.path(), &authority(), "anything", &[]).unwrap_err();
        assert!(error.contains(".stella/skills/"), "names the fix: {error}");
    }

    /// A directive-less skill still runs — unscoped, inline, session
    /// surface — and an unusable directive value degrades to a printed
    /// diagnostic rather than a refusal, the parser's own contract.
    #[test]
    fn a_directive_less_skill_runs_unscoped_and_bad_values_degrade() {
        let home = tempfile::tempdir().unwrap();
        let _home = sandboxed_home(home.path());
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "sql-style", "", "Lowercase keywords.");
        write_skill(
            dir.path(),
            "wonky",
            "context: detached\neffort: turbo\n",
            "Body.",
        );

        let plain = plan(dir.path(), &authority(), "sql-style", &[]).unwrap();
        assert!(plain.invoked.scope.is_none());
        assert_eq!(plain.mode, SkillInvocationMode::Inline);
        assert!(launch_line(&plain).contains("session surface"));

        let wonky = plan(dir.path(), &authority(), "wonky", &[]).unwrap();
        assert_eq!(wonky.diagnostics.len(), 2);
        let lines = diagnostic_lines(&wonky.diagnostics);
        assert!(lines[0].contains("detached"), "{lines:?}");
        assert!(lines[1].contains("turbo"), "{lines:?}");
    }
}
