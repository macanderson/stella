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
//! them would become the exfiltration channel it exists to prevent. [`survey`]
//! reads no rule text: five of its counts are `read_dir` plus a name test, and
//! the sixth is a `count(*)` over an immutable connection
//! (`stella_store::rules_peek`), so no body reaches this module to leak by
//! accident.
//!
//! It answers a second question the first one cannot: **which authority
//! withheld it**, because that decides the only remedy the line may honestly
//! name. Repository trust and the org's ceiling both resolve
//! `project_prompts_allowed` to `false`, and they are not interchangeable — a
//! `STELLA_TRUST_PROJECT=1` printed against a managed `project_prompts = "off"`
//! tells a user who has *already set that flag* to set it again. See
//! [`Withholder`].
//!
//! Returning the notice as data and letting the caller print it is the shape
//! `plugin_cmd::roster::read_project_tier` established for the plugin tier's
//! identical refusal (#3509): it is what lets the arms below be asserted by a
//! unit test. It does not replace the end-to-end witness — the notice is only
//! *user-visible* because [`super::Settings::load`] prints it, and that wiring
//! is proved by a process test
//! (`crates/stella-cli/tests/settings_warnings_cli.rs`), not by this module.
//!
//! **Three carriers, one answer.** #2302 asks for the counts on stderr *and*
//! in `--output-format stream-json`, so a harness sees them without scraping
//! the human channel. [`WithheldNotice::line`] is the first and
//! [`WithheldNotice::event`] the second (#3616), and both read one
//! [`withheld`] survey rather than deriving anything of their own — a second
//! derivation is how two carriers of one fact start disagreeing. The `json`
//! summary's `withheld` object is the third (#4465), and it goes one better:
//! `agent::summary::withheld` reads the emitted event back off the turn's
//! journal, so it is not a third derivation at all.

use std::path::Path;

use super::Toggle;
use super::authority::ManagedAuthoritySettings;

/// Which authority is holding the steering back, and therefore what the notice
/// may tell the user to do about it.
///
/// [`super::AuthorityPolicy::compute`] resolves `project_prompts_allowed` as
/// `project_trusted && managed.project_prompts != Off` — a conjunction, so a
/// `false` has two possible causes with two different remedies. The managed arm
/// is a **ceiling**: it is not lifted by trusting the repository, so a notice
/// that named `STELLA_TRUST_PROJECT=1` there would be advice that cannot work.
///
/// The protocol's type rather than a local twin, because the same
/// discriminant now rides the wire on [`stella_protocol::AgentEvent`] — two
/// enums for one distinction is how the stderr line and the event start
/// disagreeing about which authority refused (#3616).
pub(crate) use stella_protocol::Withholder;

/// Attribute an already-resolved withholding to the authority that caused it,
/// or `None` when the steering was in fact loaded.
///
/// **`project_prompts_allowed` is read, never re-derived** — it is the verdict
/// the session actually ran under, and the whole point of the call site passing
/// it is that a second derivation could announce the opposite of what happened.
/// The managed block is consulted only to *attribute* a withholding this
/// argument has already established, which is why the ceiling is checked first:
/// when both causes hold, the one the user cannot lift is the one to report.
pub(crate) fn withholder(
    project_prompts_allowed: bool,
    managed: Option<&ManagedAuthoritySettings>,
) -> Option<Withholder> {
    if project_prompts_allowed {
        return None;
    }
    Some(
        if managed.and_then(|managed| managed.project_prompts) == Some(Toggle::Off) {
            Withholder::ManagedCeiling
        } else {
            Withholder::ProjectUntrusted
        },
    )
}

/// How much project steering the trust gate is holding back, by category.
///
/// Each count is a question about the **steering the workspace holds**, not a
/// prediction of what the matching loader would have produced. A definition
/// that would have failed to parse was withheld just the same, and re-deriving
/// each loader's discovery rules here would be a second copy of them, free to
/// drift from the first.
///
/// The one source that is not a directory is the extension-authored rules the
/// same gate suppresses out of `.stella/private/store.db`
/// (`rules::load_workspace_rules`'s `include_project` arm). Counting those used
/// to mean `stella_store::Store::open`, which is not a read — it creates and
/// hardens the state directory, runs migrations, and writes through
/// `reconcile_interrupted_executions` — so a workspace whose *only* steering
/// was store rules stayed silent, the exact defect #2302 was filed for in a
/// narrower case. `stella_store::rules_peek` answers it read-only instead
/// (#3617), and the count folds into [`records`](Self::records) because the
/// notice names what a reader would look for, and a published rule reads as a
/// context record wherever it was stored. It is a separate field so the two
/// sources stay separately assertable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct WithheldSteering {
    /// `<root>/.stella/memories/*.md`.
    pub(crate) memories: usize,
    /// The rule files under `<root>/.claude/rules` and `<root>/.stella/rules`
    /// — markdown rules and published TOML context records alike, since one
    /// gate withholds both.
    pub(crate) records: usize,
    /// Extension-authored rules published into `.stella/private/store.db`,
    /// counted read-only. Rendered as part of the `records` total.
    pub(crate) store_records: usize,
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
            part(
                self.records + self.store_records,
                "context record",
                "context records",
            ),
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

/// What a checkout whose steering was withheld is owed, or `None`.
///
/// `None` on two arms, and both are required: a workspace whose steering
/// **was loaded** (`withheld_by` is `None`) has nothing to be told, and one with
/// **nothing to withhold** must not warn about a suppression that cost it
/// nothing — a notice printed in every repository is one nobody reads.
pub(crate) fn withheld(
    workspace_root: &Path,
    withheld_by: Option<Withholder>,
) -> Option<WithheldNotice> {
    let by = withheld_by?;
    let counts = survey(workspace_root);
    (!counts.is_empty()).then_some(WithheldNotice { by, counts })
}

/// One workspace's refusal: which authority held its steering back, and how
/// much. Surveyed once, because #2302 asks for two carriers of it and a second
/// derivation is how two carriers of one fact start disagreeing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct WithheldNotice {
    by: Withholder,
    counts: WithheldSteering,
}

impl WithheldNotice {
    /// The human channel's line: the workspace, the inventory, and the one
    /// remedy that works on the arm it is actually on.
    pub(crate) fn line(&self, workspace_root: &Path) -> String {
        let remedy = match self.by {
            Withholder::ProjectUntrusted => {
                "set STELLA_TRUST_PROJECT=1 to let this repo's memories, rules, skills, commands \
                 and agents steer this session"
            }
            Withholder::ManagedCeiling => {
                "your org's managed settings set authority.project_prompts = \"off\", so no \
                 repository may steer a session on this machine — STELLA_TRUST_PROJECT does not \
                 lift it"
            }
        };
        format!(
            "  ! project steering in {} was NOT loaded ({}) — {remedy}",
            workspace_root.display(),
            self.counts.parts(),
        )
    }

    /// The machine channel's event — what `--output-format stream-json`
    /// carries, so a harness sees the refusal without scraping the human
    /// channel (#3616).
    ///
    /// Counts and the authority, and deliberately **not** the workspace path:
    /// a harness knows its own cwd, and the path is the one field of
    /// [`Self::line`] that is not a count.
    pub(crate) fn event(&self) -> stella_protocol::AgentEvent {
        stella_protocol::AgentEvent::SteeringWithheld {
            withheld_by: self.by,
            memories: self.counts.memories,
            // Folded exactly as `parts` folds it for the human line: a
            // published rule reads as a context record wherever it was
            // stored, and the two carriers must not disagree about the
            // number (#3617).
            records: self.counts.records + self.counts.store_records,
            skills: self.counts.skills,
            commands: self.counts.commands,
            agents: self.counts.agents,
        }
    }
}

/// Count the project steering present on disk, creating nothing and writing
/// nothing — this runs on the `Settings::load` path every `stella` invocation
/// takes, `stella --version` included.
pub(crate) fn survey(workspace_root: &Path) -> WithheldSteering {
    let stella = workspace_root.join(".stella");
    WithheldSteering {
        memories: count_in(&stella.join("memories"), is_markdown),
        // Both rule directories `stella_core::rules::rule_search_dirs` hands
        // the project tier, folded into one count because one gate withholds
        // them.
        records: count_in(&workspace_root.join(".claude").join("rules"), is_rule_file)
            + count_in(&stella.join("rules"), is_rule_file),
        // The one source that is not a directory. Asked of the rules loader's
        // own module rather than re-derived here, and read-only by
        // construction — `survey` runs on the `Settings::load` path every
        // `stella` invocation takes.
        store_records: crate::rules::store_rule_count(workspace_root),
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
