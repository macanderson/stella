//! Filesystem-backed skill discovery for session recall and extension menus.

use std::path::Path;

#[cfg(test)]
use stella_core::skills::Skill;
use stella_core::skills::{self, LoadSkillsOptions, SkillSource};

/// Filesystem-backed [`SkillSource`] reading the workspace + user-global
/// skill directories. Outside consumers use the loading functions below.
struct FsSkillSource;

impl SkillSource for FsSkillSource {
    fn read_skill_files(&self, roots: &[String]) -> Vec<skills::SkillFile> {
        let mut files = Vec::new();
        for root in roots {
            // Flat layout: <root>/<slug>.md
            if let Ok(entries) = std::fs::read_dir(root) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "md") {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            files.push(skills::SkillFile {
                                path: path.display().to_string(),
                                content,
                            });
                        }
                    } else if path.is_dir() {
                        // Ecosystem layout: <root>/<slug>/SKILL.md
                        let nested = path.join("SKILL.md");
                        if let Ok(content) = std::fs::read_to_string(&nested) {
                            files.push(skills::SkillFile {
                                path: nested.display().to_string(),
                                content,
                            });
                        }
                    }
                }
            }
        }
        files
    }
}

/// `<workspace>/.stella/skills` — the workspace-scope skills directory.
pub(crate) fn workspace_skills_dir(workspace_root: &Path) -> String {
    workspace_root
        .join(".stella")
        .join("skills")
        .display()
        .to_string()
}

/// Every `*.md` file physically present in `dir`, as the exact path strings
/// [`stella_core::skills::decide_auto_creation`] builds and compares against.
///
/// Deliberately a directory read rather than a view of the loaded skill list:
/// a file can sit on disk and still be absent from [`load_workspace_skills_with_authority`]
/// — disabled from the SKILLS tab, excluded by authority, or dropped by a load
/// diagnostic — and the no-clobber guard has to see it anyway (#737). Paths are
/// rebuilt from `dir` plus the entry's file name so they match the guard's own
/// `format!("{dir}/{name}.md")` spelling exactly. Unreadable dir ⇒ empty, which
/// is safe only because the caller unions this with the loaded paths.
pub(crate) fn skill_paths_on_disk(dir: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|e| e == "md"))
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            Some(format!("{}/{name}", dir.trim_end_matches('/')))
        })
        .collect()
}

/// `~/.stella/skills` — the user-global skills directory (empty
/// string without a home, which the loader skips silently).
fn user_skills_dir() -> String {
    crate::paths::user_extension_root()
        .map(|root| root.join("skills").display().to_string())
        .unwrap_or_default()
}

/// Load user-global skills and, when permitted, workspace skill definitions.
pub(crate) fn load_workspace_skills_with_authority(
    workspace_root: &Path,
    include_workspace: bool,
) -> skills::LoadedSkills {
    if crate::settings::filesystem_settings_disabled() {
        return skills::LoadedSkills::default();
    }
    let mut loaded = skills::load_skills_with_diagnostics(
        &FsSkillSource,
        &LoadSkillsOptions {
            workspace_skills_dir: if include_workspace {
                workspace_skills_dir(workspace_root)
            } else {
                String::new()
            },
            user_skills_dir: user_skills_dir(),
        },
    );
    append_plugin_skills(workspace_root, &mut loaded);
    // A skill disabled from the SKILLS tab is excluded from recall/selection
    // and the ⚡ slash menu — its file stays on disk (see `crate::skill_manager`).
    crate::skill_manager::retain_enabled(&mut loaded.skills, workspace_root);
    loaded
}

/// Add the skills installed plugins ship (`<plugin_dir>/skills/<slug>/SKILL.md`,
/// #3380), after the user's own.
///
/// # A plugin's skill never displaces one of yours
///
/// Appended, and only where the name is still free, rather than merged
/// through [`skills::load_skills_with_diagnostics`]'s own later-wins
/// precedence. That loader's rule is "workspace beats user-global", which is
/// a statement about *your* two directories; a package arriving with a
/// `SKILL.md` named like one you wrote would, under the same rule, silently
/// replace the procedure in your prompts with a third party's — the one
/// outcome the package precedence rule forbids everywhere it appears
/// (`crate::plugin_cmd::package`). Loading each package separately is also
/// what keeps a plugin's malformed skill a diagnostic about that package
/// instead of a name collision inside your own load.
///
/// Roster order decides plugin-versus-plugin, matching the tool surface.
fn append_plugin_skills(workspace_root: &Path, loaded: &mut skills::LoadedSkills) {
    for contributed in crate::plugin_cmd::package::contributed_skill_dirs(workspace_root) {
        let found = skills::load_skills_with_diagnostics(
            &FsSkillSource,
            &LoadSkillsOptions {
                workspace_skills_dir: contributed.dir.display().to_string(),
                user_skills_dir: String::new(),
            },
        );
        loaded.diagnostics.extend(found.diagnostics);
        for skill in found.skills {
            if loaded.skills.iter().any(|held| held.name == skill.name) {
                continue;
            }
            loaded.skills.push(skill);
        }
    }
}

/// Compatibility seam for callers that deliberately request the historical
/// user-plus-workspace skill view. Authority-aware session assembly uses
/// [`load_workspace_skills_with_authority`] directly.
#[cfg(test)]
pub(crate) fn load_workspace_skills(workspace_root: &Path) -> Vec<Skill> {
    load_workspace_skills_with_authority(workspace_root, true).skills
}
