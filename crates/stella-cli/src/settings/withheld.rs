//! What an untrusted checkout's project steering would have contributed, and
//! the one line that says it did not (#2302).
//!
//! [`AuthorityPolicy::project_prompts_allowed`](super::AuthorityPolicy) is the
//! fail-closed answer to "may this repository steer this session?", and every
//! consumer of it drops its project tier without a word: workspace memories
//! never reach the system prompt (`agent::prompt::assemble_system_prompt`),
//! `.claude/rules` and `.stella/rules` never reach the rules section or the
//! record registry (`rules::load_workspace_rules`), and workspace skills,
//! commands and agents never reach the extension menus
//! (`extensions::CustomExtensions`). Withholding
//! them is correct — a `git clone` must not steer a session before the user
//! opts in — but the silence is not: the user believes their memories and rules
//! are live, and until this module the only way to find out otherwise was to
//! reconstruct the wire prompt with `stella inspect`. It is the same defect
//! architecture invariant #8 names for a pinned reasoning effort against a
//! provider that cannot honour it: configuration the user wrote, dropped
//! without a boot notice.
//!
//! So this module answers the one question a notice needs — **is there anything
//! on disk the trust gate is holding back, and how much of it?** Counts only.
//! The notice must never carry a memory body, a record's text, or even a
//! filename: those are repository-controlled strings, and a refusal that echoed
//! them would become the exfiltration channel it exists to prevent. Nothing
//! here opens a file — [`survey`] is `read_dir` plus a name test — so there is
//! no body to leak by accident.
//!
//! Returning the notice as data and letting the caller print it is the shape
//! `plugin_cmd::roster::read_project_tier` established for the plugin tier's
//! identical refusal (#3509): it is what lets the three arms below be asserted
//! by a test instead of scraped off stderr.

use std::path::Path;

/// How much project steering the trust gate is holding back, by category.
///
/// Each count is a question about the **filesystem**, not a prediction of what
/// the matching loader would have produced. A definition that would have failed
/// to parse was withheld just the same, and re-deriving each loader's discovery
/// rules here would be a second copy of them, free to drift from the first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct WithheldSteering {
    /// `<root>/.stella/memories/*.md`.
    pub(crate) memories: usize,
    /// The rule files under `<root>/.claude/rules` and `<root>/.stella/rules`
    /// — markdown rules and published TOML context records alike, since one
    /// gate withholds both.
    pub(crate) records: usize,
    /// `<root>/.stella/skills` — a `<slug>.md` file or a `<slug>/SKILL.md`
    /// directory, the two layouts the skill source reads.
    pub(crate) skills: usize,
    /// Visible top-level entries of `<root>/.stella/commands`. A namespace
    /// directory counts once: the notice reports that the repository has
    /// commands here, not how many `/name`s the loader would have resolved.
    pub(crate) commands: usize,
    /// Visible top-level entries of `<root>/.stella/agents`, counted exactly
    /// like [`commands`](Self::commands).
    pub(crate) agents: usize,
}

impl WithheldSteering {
    /// Whether the trust gate is holding anything back at all. The overwhelming
    /// majority of repositories ship no `.stella/` steering and must stay
    /// silent, which is the same reason the plugin tier speaks only when its
    /// directory exists.
    pub(crate) fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The parenthesised inventory — `"3 memories, 2 context records, 1 skill"`.
    /// Zero-count categories are omitted rather than printed as `0`, and each
    /// surviving one is singular or plural to match its count.
    fn parts(&self) -> String {
        [
            part(self.memories, "memory", "memories"),
            part(self.records, "context record", "context records"),
            part(self.skills, "skill", "skills"),
            part(self.commands, "command", "commands"),
            part(self.agents, "agent", "agents"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ")
    }
}

fn part(count: usize, one: &str, many: &str) -> Option<String> {
    (count > 0).then(|| format!("{count} {}", if count == 1 { one } else { many }))
}

/// The one line an untrusted checkout with steering on disk is owed, or `None`.
///
/// `None` on two arms, and both are load-bearing: a **trusted** workspace is
/// getting its steering and has nothing to be told, and an untrusted workspace
/// with **nothing to withhold** must not warn about a suppression that cost it
/// nothing — a notice printed in every repository is one nobody reads.
pub(crate) fn notice(workspace_root: &Path, project_prompts_allowed: bool) -> Option<String> {
    if project_prompts_allowed {
        return None;
    }
    let withheld = survey(workspace_root);
    if withheld.is_empty() {
        return None;
    }
    Some(format!(
        "  ! project steering in {} was NOT loaded ({}) — set STELLA_TRUST_PROJECT=1 to let \
         this repo's memories, rules, skills, commands and agents steer this session",
        workspace_root.display(),
        withheld.parts(),
    ))
}

/// Count the project steering present on disk, opening nothing.
pub(crate) fn survey(workspace_root: &Path) -> WithheldSteering {
    let stella = workspace_root.join(".stella");
    WithheldSteering {
        memories: count_in(&stella.join("memories"), is_markdown),
        // Both rule directories `stella_core::rules::rule_search_dirs` hands
        // the project tier, folded into one count because one gate withholds
        // them.
        records: count_in(&workspace_root.join(".claude").join("rules"), is_rule_file)
            + count_in(&stella.join("rules"), is_rule_file),
        skills: count_in(&stella.join("skills"), |path| {
            is_markdown(path) || path.join("SKILL.md").is_file()
        }),
        commands: count_in(&stella.join("commands"), is_visible),
        agents: count_in(&stella.join("agents"), is_visible),
    }
}

/// Entries of `dir` satisfying `keep`. A missing or unreadable directory is
/// zero: it is the common case, and a permissions error is not something a
/// trust notice can act on.
fn count_in(dir: &Path, keep: impl Fn(&Path) -> bool) -> usize {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| keep(&entry.path()))
        .count()
}

fn is_markdown(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("md")
}

/// The filter `rules::FsRuleSource` applies, reading its constants rather than
/// restating them so the two answers cannot disagree about what a rule file is.
fn is_rule_file(path: &Path) -> bool {
    let has_rule_extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| crate::rules::RULE_EXTENSIONS.contains(&ext));
    let is_reserved = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| crate::rules::RESERVED_RULE_FILENAMES.contains(&name));
    has_rule_extension && !is_reserved
}

/// The extension loaders skip dot-entries (`.DS_Store`, lock files); so does
/// the count, or a repository with no commands at all would report one.
fn is_visible(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| !name.starts_with('.'))
}
