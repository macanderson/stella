//! Filesystem glue for custom extensions (`stella_core::extensions`): the
//! init-time symlink sync that adopts `.claude/`- and `.agents/`-authored
//! commands/skills/agents into stella's own directories, and the loaders the
//! chat surfaces use to offer them (⚡ slash-menu rows, `/agents`).
//!
//! All planning and parsing is pure and lives in `stella-core`; this module
//! owns exactly the I/O halves: directory
//! scanning with symlink detection, symlink creation, and definition-file
//! reads.
//!
//! ## Scopes
//!
//! The sync runs at both scopes, mirroring the settings/skills chain:
//!
//! - **workspace**: `<root>/{.claude,.agents}/<kind>` → `<root>/.stella/<kind>`
//! - **user**: `~/{.claude,.agents}/<kind>` → `~/.stella/<kind>`
//!
//! so definitions installed for other agents at either level are visible to
//! stella after `stella init` (or `/init`). The loaders then read the
//! `.stella`-side directories only — user-global first, workspace last, so a
//! workspace definition wins a name collision (same precedence as skills).

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use stella_core::extensions::{
    AgentDef, CommandDef, ExtensionKind, SyncEntry, SyncSource, agent_from_file, command_from_file,
    command_from_toml, expand_command, merge_by_name, plan_extension_sync,
};
use stella_core::skills::Skill;
use stella_tui::SlashCommand;

/// The other-agent directories the sync adopts from, in precedence order
/// (an entry in an earlier directory wins a real name collision).
const SOURCE_DIRS: [&str; 2] = [".claude", ".agents"];

// Scanning + symlink sync

/// The per-directory definition file each kind accepts alongside flat
/// `<slug>.md` (the `npx skills` ecosystem layout, generalized).
fn nested_file(kind: ExtensionKind) -> &'static str {
    match kind {
        ExtensionKind::Commands => "COMMAND.md",
        ExtensionKind::Skills => "SKILL.md",
        ExtensionKind::Agents => "AGENT.md",
    }
}

/// The name a definition at `path` will *load* under: the frontmatter
/// `name:` when readable (a directory entry is read through its
/// `SKILL.md`-style nested file), else the filename-derived slug. Sync
/// collision precedence keys on this — see `SyncEntry::definition_name`.
fn definition_name_for(path: &Path, kind: ExtensionKind) -> String {
    let fallback = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .map(|n| n.strip_suffix(".md").unwrap_or(&n).to_string())
        .unwrap_or_default();
    let file = if path.is_dir() {
        path.join(nested_file(kind))
    } else {
        path.to_path_buf()
    };
    std::fs::read_to_string(&file)
        .ok()
        .and_then(|raw| {
            stella_core::rules::parse_frontmatter(&raw)
                .data
                .get("name")
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
        })
        .unwrap_or(fallback)
}

/// Whether the loader can read a definition out of this entry.
///
/// A flat file always can. A directory can when it holds the kind's nested
/// definition file — or, for **commands only**, when it is a namespace
/// directory of `.md`/`.toml` children (`.claude/commands/vercel/deploy.md`
/// → `/vercel:deploy`), which `read_command_files` loads.
///
/// This must mirror, exactly, what the loaders actually read: an entry called
/// loadable here but skipped there becomes a dead symlink with no diagnostic
/// anywhere, and an entry called unloadable here is reported to the user as
/// something stella cannot use. Issue #104 was the first mismatch; the
/// commands arm is the second, in the opposite direction.
fn is_loadable_entry(path: &Path, kind: ExtensionKind) -> bool {
    if !path.is_dir() {
        return true;
    }
    if path.join(nested_file(kind)).symlink_metadata().is_ok() {
        return true;
    }
    // Commands only: a directory of `.md`/`.toml` files with no `COMMAND.md`
    // is a NAMESPACE (`.claude/commands/vercel/deploy.md` → `/vercel:deploy`),
    // and `read_command_files` now loads it. It used to be unloadable by
    // definition — nothing read it, so linking it created a dead symlink —
    // which is why the whole `/ns:name` convention was invisible. Skills and
    // agents have no namespace syntax, so they keep the stricter rule.
    kind == ExtensionKind::Commands
        && std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| {
                e.file_name().to_str().is_some_and(|n| {
                    !n.starts_with('.') && (n.ends_with(".md") || n.ends_with(".toml"))
                })
            })
}

/// Scan one source directory for one kind (`<source_root>/<kind>`), with
/// per-entry symlink detection and best-effort frontmatter-name resolution.
/// Hidden entries (`.DS_Store`, `.skill-lock.json`, …) are ignored. Sorted by
/// name so plans — and therefore init output — are deterministic.
fn scan_source(source_root: &Path, kind: ExtensionKind) -> SyncSource {
    let dir = source_root.join(kind.dir_name());
    let mut entries: Vec<SyncEntry> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                return None;
            }
            let is_symlink = entry
                .path()
                .symlink_metadata()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false);
            Some(SyncEntry {
                definition_name: definition_name_for(&entry.path(), kind),
                is_loadable: is_loadable_entry(&entry.path(), kind),
                name,
                path: entry.path().display().to_string(),
                is_symlink,
            })
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    SyncSource { kind, entries }
}

/// What already occupies one kind's destination directory: every entry
/// basename — real files, dirs, and symlinks alike (a dangling symlink still
/// occupies the name) — plus the names those definitions load under.
fn existing_targets(dir: &Path, kind: ExtensionKind) -> stella_core::extensions::ExistingTargets {
    let mut targets = stella_core::extensions::ExistingTargets::default();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with('.') {
            targets
                .definition_names
                .push(definition_name_for(&entry.path(), kind));
        }
        targets.file_names.push(name);
    }
    targets
}

/// One human-readable line for a `NotLoadable` skip — named individually
/// (unlike the benign skip reasons, which are just a count) because the bug
/// this reason exists to fix (#104) is exactly a silently-adopted entry;
/// folding it into "N already present" would also misdescribe it, since
/// nothing with this name was actually present anywhere.
fn describe_unloadable_skip(skip: &stella_core::extensions::SyncSkip) -> String {
    format!(
        "{} ({}): namespaced directory — no {}.md or {} found, not loadable",
        skip.name,
        skip.kind.dir_name(),
        skip.name,
        nested_file(skip.kind)
    )
}

/// The relative path from inside `from_dir` to `target` — symlinks are
/// created relative so a repo (or home) moved as a unit keeps working.
/// Falls back to the absolute target when the two share no common root.
fn relative_symlink_target(from_dir: &Path, target: &Path) -> PathBuf {
    let from: Vec<Component> = from_dir.components().collect();
    let to: Vec<Component> = target.components().collect();
    let common = from
        .iter()
        .zip(to.iter())
        .take_while(|(a, b)| a == b)
        .count();
    if common == 0 {
        return target.to_path_buf();
    }
    let mut rel = PathBuf::new();
    for _ in common..from.len() {
        rel.push("..");
    }
    for component in &to[common..] {
        rel.push(component);
    }
    rel
}

/// What one sync run did, for the init progress line. `errors` carries any
/// link that could not be created (permissions, non-unix platform) — the
/// sync is best-effort and never fails init.
#[derive(Debug, Default)]
pub struct SyncOutcome {
    /// Created links as `(kind, name)`.
    pub linked: Vec<(ExtensionKind, String)>,
    /// Entries skipped for the benign, expected reasons (symlink sources,
    /// already-present names, duplicate loaded names) — folded into the
    /// summary's "N already present" rather than named individually.
    /// Deliberately excludes `NotLoadable` skips, which are named
    /// individually in `unloadable` instead (see its doc).
    pub skipped: usize,
    /// One human-readable line per entry skipped as `NotLoadable` (a
    /// namespace directory with no nested definition file — see issue
    /// #104). Unlike `skipped`, these are always surfaced by
    /// `sync_extensions`, even when nothing else in this scope was linked
    /// or already present: the bug this reason exists to fix is exactly a
    /// silently-adopted entry, so going quiet here would reintroduce it.
    pub unloadable: Vec<String>,
    pub errors: Vec<String>,
}

impl SyncOutcome {
    /// `"2 commands, 12 skills"`-style summary of the created links, or
    /// `None` when nothing was linked.
    pub fn summary(&self) -> Option<String> {
        let parts: Vec<String> = ExtensionKind::ALL
            .iter()
            .filter_map(|kind| {
                let n = self.linked.iter().filter(|(k, _)| k == kind).count();
                (n > 0).then(|| format!("{n} {}", kind.dir_name()))
            })
            .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

/// Adopt every command/skill/agent found under `source_roots` (in precedence
/// order) into `dest_root` (`.stella` or `~/.stella`) as symlinks.
/// Idempotent: symlink sources, already-present names, duplicate names, and
/// unloadable entries (namespace directories with no nested definition
/// file) are skipped by the plan
/// (`stella_core::extensions::plan_extension_sync`).
fn sync_into(dest_root: &Path, source_roots: &[PathBuf]) -> SyncOutcome {
    let sources: Vec<SyncSource> = ExtensionKind::ALL
        .iter()
        .flat_map(|kind| source_roots.iter().map(|root| scan_source(root, *kind)))
        .filter(|s| !s.entries.is_empty())
        .collect();
    let existing = |kind: ExtensionKind| existing_targets(&dest_root.join(kind.dir_name()), kind);
    let plan = plan_extension_sync(&sources, &existing);

    let mut outcome = SyncOutcome {
        skipped: plan
            .skips
            .iter()
            .filter(|s| s.reason != stella_core::extensions::SyncSkipReason::NotLoadable)
            .count(),
        unloadable: plan
            .skips
            .iter()
            .filter(|s| s.reason == stella_core::extensions::SyncSkipReason::NotLoadable)
            .map(describe_unloadable_skip)
            .collect(),
        ..SyncOutcome::default()
    };
    for link in &plan.links {
        let dir = dest_root.join(link.kind.dir_name());
        if let Err(e) = std::fs::create_dir_all(&dir) {
            outcome.errors.push(format!("{}: {e}", dir.display()));
            continue;
        }
        let dest = dir.join(&link.name);
        let target = relative_symlink_target(&dir, Path::new(&link.source_path));
        match create_symlink(&target, &dest) {
            Ok(()) => outcome.linked.push((link.kind, link.name.clone())),
            Err(e) => outcome.errors.push(format!("{}: {e}", dest.display())),
        }
    }
    outcome
}

#[cfg(unix)]
fn create_symlink(target: &Path, dest: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, dest)
}

#[cfg(not(unix))]
fn create_symlink(_target: &Path, _dest: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "extension symlinks are only supported on unix platforms",
    ))
}

/// The user-global stella root user-scope **extensions** are loaded from
/// (`~/.stella`), or `None` when there is no user tier to read.
///
/// [`crate::paths::user_extension_root`] and not
/// [`crate::paths::stella_root`], which is what this resolved through until
/// #3864. The two are the same value in production and diverge in a test
/// build, where `UserPaths::extensions_visible` is false: the extension root
/// correctly answers "no user tier" while the bare root still resolves to the
/// developer's real `~/.stella`. So one function body resolved the user scope
/// through two policies — skills through `memory::skill_files`'s
/// `user_skills_dir` (correct), commands and
/// agents through here — and an un-redirected unit test saw no user-scope
/// skills while reading the developer's own `~/.stella/commands/` and
/// `~/.stella/agents/`. That is the exact outcome `extensions_visible`'s doc
/// comment says it exists to prevent.
pub(crate) fn user_config_root() -> Option<PathBuf> {
    crate::paths::user_extension_root()
}

/// The user tier [`sync_extensions`] adopts across: where the other agents
/// keep their definitions, and where stella's own copies go.
///
/// Two paths and not one, because `STELLA_HOME` moves one of them and not the
/// other: `~/.claude` and `~/.agents` hang off the OS home whatever the stella
/// root is set to, so deriving either from the other would be right only on a
/// default install.
#[derive(Clone, Copy, Debug)]
pub struct UserScope<'a> {
    /// The OS home, whose `.claude/` and `.agents/` are the sources.
    pub home: &'a Path,
    /// The user-global stella root, the symlink destination.
    pub stella_root: &'a Path,
}

/// Run the sync at the workspace scope and, when one is given, the user scope,
/// reporting through `emit` — the shared init hook
/// (`agent::init_workspace`) calls this so `stella init` and `/init` behave
/// identically. Quiet when there is nothing to do — except a `NotLoadable`
/// skip (a namespace directory adopted from another agent's dirs that stella's
/// loader could never read), which always gets a line, even in a scope where
/// nothing else linked (issue #104: the entire point is that this shape must
/// never go unmentioned).
///
/// `user` is a **parameter** rather than an ambient read, and that is the
/// whole of #3675: this function *creates symlinks*, so resolving the home
/// inside its body made every test that drove init — directly or through
/// `agent::init_workspace` — write into the developer's own
/// `~/.stella/{commands,skills,agents}/` however carefully the workspace root
/// was sandboxed. `None` means workspace scope only. `agent::init_workspace`
/// passes the pair `InitIo` already carries for the conversion offer, so one
/// resolved value feeds both instead of two ambient reads that can disagree.
/// The same repair `#3641` made one layer up, at the layer that writes.
pub fn sync_extensions(
    workspace_root: &Path,
    user: Option<UserScope<'_>>,
    emit: &mut dyn FnMut(String),
) {
    let mut scopes: Vec<(&str, PathBuf, Vec<PathBuf>)> = vec![(
        "workspace",
        workspace_root.join(".stella"),
        SOURCE_DIRS.iter().map(|d| workspace_root.join(d)).collect(),
    )];
    if let Some(user) = user {
        scopes.push((
            "user",
            user.stella_root.to_path_buf(),
            SOURCE_DIRS.iter().map(|d| user.home.join(d)).collect(),
        ));
    }

    for (scope, dest, sources) in scopes {
        let outcome = sync_into(&dest, &sources);
        emit_sync_outcome(scope, &outcome, emit);
    }
}

/// Emit one scope's progress lines: the `✓ adopted …` summary (only when
/// something linked), then a line for every `NotLoadable` skip — always,
/// even when nothing linked, so a scope holding only an unloadable
/// namespace directory is never silent (issue #104) — then any
/// link-creation errors.
fn emit_sync_outcome(scope: &str, outcome: &SyncOutcome, emit: &mut dyn FnMut(String)) {
    if let Some(summary) = outcome.summary() {
        let skipped = match outcome.skipped {
            0 => String::new(),
            n => format!(", {n} already present"),
        };
        emit(format!(
            "✓ adopted {summary} from .claude/.agents ({scope} scope{skipped})"
        ));
    }
    for note in &outcome.unloadable {
        emit(format!("! skipped {note} ({scope} scope)"));
    }
    for error in &outcome.errors {
        emit(format!("! extension link failed: {error}"));
    }
}

// Loading

/// Read one kind's definition files from `dir`: flat `<slug>.md` plus the
/// nested `<slug>/<nested_file>` layout, both read *through* symlinks (that
/// is the point of the sync). Returns `(path, content)` pairs; a file that
/// exists but cannot be read (e.g. a dangling symlink left by a deleted
/// source) lands in `problems` instead of vanishing.
fn read_definition_files(
    dir: &Path,
    nested_file: &str,
    problems: &mut Vec<String>,
) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return files,
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_none_or(|n| n.starts_with('.'))
        {
            continue;
        }
        if path.extension().is_some_and(|e| e == "md") {
            match std::fs::read_to_string(&path) {
                Ok(content) => files.push((path.display().to_string(), content)),
                Err(e) => problems.push(format!("{}: {e}", path.display())),
            }
        } else if path.is_dir() {
            let nested = path.join(nested_file);
            match std::fs::read_to_string(&nested) {
                Ok(content) => files.push((nested.display().to_string(), content)),
                // A directory without its nested definition file is not an
                // error (other agents keep auxiliary dirs here); one whose
                // definition file exists but won't read is.
                Err(e) if nested.symlink_metadata().is_ok() => {
                    problems.push(format!("{}: {e}", nested.display()));
                }
                Err(_) => {}
            }
        }
    }
    files
}

/// One human-readable line for a parse diagnostic.
fn describe_diagnostic(diag: &stella_core::extensions::ExtensionDiagnostic) -> String {
    let why = match diag.problem {
        stella_core::extensions::ExtensionProblem::MissingName => "no usable name",
        stella_core::extensions::ExtensionProblem::EmptyBody => "empty body",
        stella_core::extensions::ExtensionProblem::Malformed => "not valid TOML",
        stella_core::extensions::ExtensionProblem::NestedToolbelt => {
            "`tools:` is a nested mapping, which would have granted every tool — \
             write it as a list, like `tools: read, search`"
        }
    };
    format!("{}: {why}", diag.path)
}

/// The `.stella`-side directories one kind is loaded from, lowest precedence
/// first (user-global, then workspace — workspace wins, like skills).
fn load_dirs(workspace_root: &Path, kind: ExtensionKind, include_workspace: bool) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(config_root) = user_config_root() {
        dirs.push(config_root.join(kind.dir_name()));
    }
    if include_workspace {
        dirs.push(workspace_root.join(".stella").join(kind.dir_name()));
    }
    dirs
}

fn load_commands_from(dirs: &[PathBuf], problems: &mut Vec<String>) -> Vec<CommandDef> {
    let mut parsed = Vec::new();
    for dir in dirs {
        for found in read_command_files(dir, problems) {
            let result = if found.is_toml {
                command_from_toml(&found.path, &found.raw)
            } else {
                command_from_file(&found.path, &found.raw)
            };
            match result {
                Ok(mut cmd) => {
                    cmd.namespace = found.namespace;
                    parsed.push(cmd);
                }
                Err(diag) => problems.push(describe_diagnostic(&diag)),
            }
        }
    }
    // Merged on the INVOCATION, not the bare name: `/vercel:deploy` and
    // `/fly:deploy` are two commands, and collapsing them would silently drop
    // whichever loaded second.
    merge_by_name(parsed, |c: &CommandDef| c.invocation())
}

/// One command definition file found on disk, with the namespace its location
/// implies.
struct FoundCommand {
    path: String,
    raw: String,
    is_toml: bool,
    namespace: Option<String>,
}

/// Scan one commands directory: flat `<slug>.{md,toml}`, the nested
/// `<slug>/COMMAND.{md,toml}` layout, and — new — namespace directories whose
/// children become `/<dir>:<slug>`.
///
/// The two directory shapes are told apart by what is inside, not by naming: a
/// directory holding `COMMAND.md`/`COMMAND.toml` IS one command; anything else
/// is a namespace and its `.md`/`.toml` children are its commands. That rule
/// is what lets `.claude/commands/vercel/deploy.md` finally load — before this,
/// `is_loadable_entry` classified such a directory as unloadable and the sync
/// skipped it, so the whole `/ns:name` convention was invisible to stella.
///
/// One level only. Nesting deeper has no invocation syntax to reach it, and
/// silently loading a command nobody can type is worse than not loading it.
fn read_command_files(dir: &Path, problems: &mut Vec<String>) -> Vec<FoundCommand> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_none_or(|n| n.starts_with('.'))
        {
            continue;
        }
        if let Some(kind) = definition_extension(&path) {
            push_command_file(&path, kind, None, &mut found, problems);
            continue;
        }
        if !path.is_dir() {
            continue;
        }
        // The nested single-command layout wins when its file is present.
        if let Some((nested, kind)) = ["COMMAND.md", "COMMAND.toml"]
            .iter()
            .map(|f| path.join(f))
            .find_map(|p| definition_extension(&p).map(|k| (p, k)))
            .filter(|(p, _)| p.symlink_metadata().is_ok())
        {
            push_command_file(&nested, kind, None, &mut found, problems);
            continue;
        }
        let Some(namespace) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Ok(children) = std::fs::read_dir(&path) else {
            continue;
        };
        let mut child_paths: Vec<PathBuf> = children.flatten().map(|e| e.path()).collect();
        child_paths.sort();
        for child in child_paths {
            if child
                .file_name()
                .and_then(|n| n.to_str())
                .is_none_or(|n| n.starts_with('.'))
            {
                continue;
            }
            if let Some(kind) = definition_extension(&child) {
                push_command_file(
                    &child,
                    kind,
                    Some(namespace.to_string()),
                    &mut found,
                    problems,
                );
            }
        }
    }
    found
}

/// `Some(true)` for a `.toml` definition, `Some(false)` for `.md`, `None` for
/// anything else.
fn definition_extension(path: &Path) -> Option<bool> {
    match path.extension()?.to_str()? {
        "toml" => Some(true),
        "md" => Some(false),
        _ => None,
    }
}

fn push_command_file(
    path: &Path,
    is_toml: bool,
    namespace: Option<String>,
    out: &mut Vec<FoundCommand>,
    problems: &mut Vec<String>,
) {
    match std::fs::read_to_string(path) {
        Ok(raw) => out.push(FoundCommand {
            path: path.display().to_string(),
            raw,
            is_toml,
            namespace,
        }),
        Err(e) => problems.push(format!("{}: {e}", path.display())),
    }
}

fn load_agents_from(dirs: &[PathBuf], problems: &mut Vec<String>) -> Vec<AgentDef> {
    let mut parsed = Vec::new();
    for dir in dirs {
        for (path, raw) in read_definition_files(dir, "AGENT.md", problems) {
            match agent_from_file(&path, &raw) {
                Ok(agent) => parsed.push(agent),
                Err(diag) => problems.push(describe_diagnostic(&diag)),
            }
        }
    }
    merge_by_name(parsed, |a: &AgentDef| a.name.clone())
}

/// Everything custom a chat surface offers: commands (⚡ `/name`, prompt
/// templates), skills (⚡ `/name`, injected know-how — the same files the
/// recall engine auto-selects from), and agents (`/agents`).
#[derive(Debug, Default)]
pub struct CustomExtensions {
    pub commands: Vec<CommandDef>,
    pub skills: Vec<Skill>,
    pub agents: Vec<AgentDef>,
    /// Definition files that were found but skipped — unreadable, or
    /// malformed per the core parsers. One human-readable line each, so a
    /// missing `/name` or `/agents` row is diagnosable instead of silent.
    pub problems: Vec<String>,
}

/// A resolved `/name`: the prompt it runs, and the skill behind it when a
/// skill is what it resolved to.
pub struct Expansion {
    /// The prompt text the model runs.
    pub prompt: String,
    /// Set when the invocation was a skill, so the caller can record that the
    /// skill was used. `None` for a command or an agent persona.
    pub skill: Option<InvokedSkill>,
}

impl Expansion {
    /// An expansion no skill produced.
    fn plain(prompt: String) -> Self {
        Self {
            prompt,
            skill: None,
        }
    }
}

/// What an explicitly invoked skill puts in the prompt — the three facts a
/// `SkillInjected` event carries beyond its trigger.
pub struct InvokedSkill {
    /// The skill's slug.
    pub name: String,
    /// Its one-line description.
    pub summary: String,
    /// What the expansion cost, estimated over the prompt it produced.
    pub tokens: u32,
}

/// What a custom `/name` resolves to.
pub enum Invocation<'a> {
    Command(&'a CommandDef),
    Skill(&'a Skill),
    /// An installed agent persona: `/agent-name task…` runs the task under
    /// the agent's system-prompt-shaped body (and its toolbelt note). This
    /// is the invocation seam the agent-usage telemetry records.
    Agent(&'a AgentDef),
}

impl CustomExtensions {
    /// Load user-global definitions plus project definitions permitted by the
    /// session's immutable authority snapshot.
    pub fn load_with_authority(
        workspace_root: &Path,
        authority: &crate::settings::AuthorityPolicy,
    ) -> Self {
        if crate::settings::filesystem_settings_disabled() {
            return Self::default();
        }
        Self::load_with_workspace_extensions(workspace_root, authority.project_prompts_allowed)
    }

    fn load_with_workspace_extensions(workspace_root: &Path, include_workspace: bool) -> Self {
        let mut problems = Vec::new();
        let commands = load_commands_from(
            &load_dirs(workspace_root, ExtensionKind::Commands, include_workspace),
            &mut problems,
        );
        let agents = load_agents_from(
            &load_dirs(workspace_root, ExtensionKind::Agents, include_workspace),
            &mut problems,
        );
        let loaded_skills =
            crate::memory::load_workspace_skills_with_authority(workspace_root, include_workspace);
        for diag in &loaded_skills.diagnostics {
            let why = match diag.problem {
                stella_core::skills::SkillProblem::MissingName => "no usable name",
                stella_core::skills::SkillProblem::MissingDescription => "no description",
                stella_core::skills::SkillProblem::EmptyBody => "empty body",
            };
            problems.push(format!("{}: {why}", diag.path));
        }
        Self {
            commands,
            skills: loaded_skills.skills,
            agents,
            problems,
        }
    }

    /// The skipped-definition report, one line per file, or `None` when
    /// everything on disk loaded. Both chat surfaces print this so a
    /// definition that fails to parse is visible, not silently absent.
    pub fn problems_report(&self) -> Option<String> {
        if self.problems.is_empty() {
            return None;
        }
        let mut out = format!("! {} custom definition(s) skipped:\n", self.problems.len());
        for problem in &self.problems {
            out.push_str(&format!("  ! {problem}\n"));
        }
        Some(out)
    }

    /// The ⚡ slash-menu rows: commands first, then skills, then agents,
    /// names prefixed with `/`. A custom name shadowed by a productized
    /// command in `reserved` is dropped (builtins always win), and later
    /// kinds sharing an earlier kind's name are dropped (a command was
    /// authored as an invocation; it wins).
    pub fn slash_entries(&self, reserved: &[SlashCommand]) -> Vec<SlashCommand> {
        let mut taken: HashSet<String> = reserved.iter().map(|c| c.name.clone()).collect();
        let mut rows = Vec::new();
        // Namespaced commands list under the name they are TYPED with — a row
        // reading `/deploy` that only answers to `/vercel:deploy` teaches the
        // wrong thing. The `argument-hint` rides in the description so the menu
        // shows the shape the command expects.
        let commands = self.commands.iter().map(|c| {
            let description = match &c.argument_hint {
                Some(hint) => format!("{hint} — {}", c.description),
                None => c.description.clone(),
            };
            (format!("/{}", c.invocation()), description)
        });
        let skills = self
            .skills
            .iter()
            .map(|s| (format!("/{}", s.name), s.description.clone()));
        let agents = self
            .agents
            .iter()
            .map(|a| (format!("/{}", a.name), a.description.clone()));
        for (name, description) in commands.chain(skills).chain(agents) {
            if taken.insert(name.clone()) {
                rows.push(SlashCommand::custom(name, description));
            }
        }
        rows
    }

    /// Resolve a custom invocation: `head` is the leading `/word` of the
    /// input (slash included). Commands shadow skills shadow agents,
    /// matching [`Self::slash_entries`].
    pub fn lookup(&self, head: &str) -> Option<Invocation<'_>> {
        let name = head.strip_prefix('/')?;
        // Matched on the invocation, so `/vercel:deploy` resolves and a bare
        // `/deploy` reaches only a command that really is unnamespaced.
        if let Some(cmd) = self.commands.iter().find(|c| c.invocation() == name) {
            return Some(Invocation::Command(cmd));
        }
        if let Some(skill) = self.skills.iter().find(|s| s.name == name) {
            return Some(Invocation::Skill(skill));
        }
        self.agents
            .iter()
            .find(|a| a.name == name)
            .map(Invocation::Agent)
    }

    /// [`Self::expansion`]'s prompt alone, for the assertions that are about
    /// the text and nothing else.
    ///
    /// `#[cfg(test)]` because every live caller needs the other half: a
    /// skill that reaches the prompt has to be recorded (#5232), and the
    /// prompt text does not say whether a skill produced it.
    #[cfg(test)]
    fn expand(&self, input: &str, reserved: &[&str]) -> Option<String> {
        self.expansion(input, reserved).map(|e| e.prompt)
    }

    /// Expand `input` (`/name args…`) into the prompt the model runs and the
    /// skill that produced it, or `None` when the name is `reserved` (a
    /// productized command can never be shadowed — not in the menu, and not
    /// at invocation time either, argument-carrying forms included) or
    /// matches no custom command/skill. A command's body is its template
    /// ([`expand_command`]); a skill's body rides as context above the
    /// user's task.
    ///
    /// This module has no event channel, so it hands the skill back rather
    /// than emitting: the two surfaces that call it — the interactive loop
    /// and the deck — own the sender.
    pub fn expansion(&self, input: &str, reserved: &[&str]) -> Option<Expansion> {
        let trimmed = input.trim();
        let (head, args) = match trimmed.split_once(char::is_whitespace) {
            Some((head, args)) => (head, args),
            None => (trimmed, ""),
        };
        if reserved.contains(&head) {
            return None;
        }
        Some(match self.lookup(head)? {
            Invocation::Command(cmd) => Expansion::plain(expand_command(&cmd.body, args)),
            Invocation::Skill(skill) => {
                let prompt = skill_invocation_prompt(skill, args);
                Expansion {
                    skill: Some(InvokedSkill {
                        name: skill.name.clone(),
                        summary: skill.description.clone(),
                        // What the prompt paid, measured over the bytes the
                        // prompt carries — the same rule the auto path
                        // applies to its rendered block, over a different
                        // rendering.
                        tokens: u32::try_from(stella_protocol::estimate_tokens(&prompt))
                            .unwrap_or(u32::MAX),
                    }),
                    prompt,
                }
            }
            Invocation::Agent(agent) => Expansion::plain(agent_invocation_prompt(agent, args)),
        })
    }

    /// The `/agents` listing: every custom agent's name, description, and
    /// source, or a hint when none are defined.
    pub fn render_agent_list(&self) -> String {
        if self.agents.is_empty() {
            return "no custom agents found — add markdown definitions to .stella/agents/ \
                    or ~/.stella/agents/ (or run /init to adopt .claude/.agents ones)"
                .to_string();
        }
        let mut out = format!("custom agents ({}):\n", self.agents.len());
        for agent in &self.agents {
            out.push_str(&format!(
                "  ⚡ {} — {}  ({})\n",
                agent.name, agent.description, agent.source_path
            ));
        }
        out
    }
}

/// The prompt a `/agent-name` invocation runs: the agent's persona body as
/// explicit instructions (with its toolbelt grant stated when the
/// definition restricts tools — prompt-level today; hard enforcement is the
/// fleet-spawn seam's concern), and any trailing text as the task.
fn agent_invocation_prompt(agent: &AgentDef, args: &str) -> String {
    let mut out = format!(
        "Adopt the following agent persona for this task.\n\n# Agent: {}\n{}\n\n{}",
        agent.name, agent.description, agent.body
    );
    if let Some(tools) = &agent.tools {
        out.push_str(&format!(
            "\n\nThis agent's toolbelt is restricted to: {}.",
            tools.join(", ")
        ));
    }
    let args = args.trim();
    if !args.is_empty() {
        out.push_str(&format!("\n\n## Task\n{args}"));
    }
    out
}

/// The prompt a `/skill-name` invocation runs: the skill body as explicit
/// instructions, with any trailing text as the task to apply them to.
fn skill_invocation_prompt(skill: &Skill, args: &str) -> String {
    let mut out = format!(
        "Apply the following skill.\n\n# Skill: {}\n{}\n\n{}",
        skill.name, skill.description, skill.body
    );
    let args = args.trim();
    if !args.is_empty() {
        out.push_str(&format!("\n\n## Task\n{args}"));
    }
    out
}

#[cfg(test)]
mod tests;
