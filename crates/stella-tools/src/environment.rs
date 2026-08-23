//! Environment identity: the facts about this session's environment,
//! collected from ONE place so nothing rendering them can drift.
//!
//! `stella-cli`'s byte-stable system-prompt block
//! (`crates/stella-cli/src/agent/prompt.rs::append_session_environment`,
//! #2692/#2719) and the [`GetEnvironment`] tool here both render from these
//! functions — [`git_worktree_bits`], [`os_release`], and [`login_shell`] —
//! rather than each keeping its own copy of the git-worktree test and the
//! `uname`/`SHELL` probes. Before this split, the CLI carried a private
//! copy of both probes with no way for a tool call to check its own answer
//! against the prompt's; now there is exactly one implementation of each
//! (#2697).
//!
//! [`GetEnvironment`] deliberately never states a model: the router owns
//! model identity, and a worker ref is only knowable — and only knowable
//! *truthfully* — by the CLI prompt layer that resolves it for the calls a
//! given prefix will ride (see `append_session_environment`'s doc comment
//! on why the model line is conditional there). A tool with no such
//! resolution would either omit the line (making it pointless) or guess
//! (making it wrong), so the omission here is structural, not an oversight.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::registry::Tool;

/// Whether `workspace_root` is a git repository, and — if so — whether it is
/// a **linked worktree** rather than the primary checkout.
///
/// A linked worktree is recognized by its `.git` being a gitfile (the text
/// pointer `git worktree add` writes) rather than a directory (the primary
/// checkout's shape). This is the exact test the session-environment prompt
/// block uses (#2692): the two callers read the same bytes off disk, so a
/// worktree the prompt calls isolated is a worktree this tool agrees is
/// isolated.
///
/// Returns `(is_git, is_linked_worktree)`. `is_linked_worktree` is only
/// ever `true` when `is_git` is also `true`.
pub fn git_worktree_bits(workspace_root: &Path) -> (bool, bool) {
    let git = workspace_root.join(".git");
    if git.is_file() {
        // A gitfile, not a directory: `git worktree add`'s link shape.
        (true, true)
    } else if git.is_dir() {
        (true, false)
    } else {
        (false, false)
    }
}

/// `uname -sr`, best-effort: one cheap spawn, and an absent or failing
/// `uname` simply omits the release rather than failing the caller — the
/// caller degrades, never blocks.
#[cfg(unix)]
pub fn os_release() -> Option<String> {
    let out = std::process::Command::new("uname")
        .arg("-sr")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let release = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!release.is_empty()).then_some(release)
}

#[cfg(not(unix))]
pub fn os_release() -> Option<String> {
    None
}

/// The user's login shell, by basename (`/bin/zsh` → `zsh`), because the
/// dialect is what matters — array syntax, word splitting, and heredoc
/// quirks all key off it. `SHELL` on unix, `COMSPEC` on Windows; absent
/// either, `None`.
pub fn login_shell() -> Option<String> {
    let var = if cfg!(windows) { "COMSPEC" } else { "SHELL" };
    let path = std::env::var(var).ok()?;
    let name = Path::new(&path).file_name()?.to_string_lossy().into_owned();
    (!name.is_empty()).then_some(name)
}

/// The executables whose presence or absence changes what a plan can be, in
/// the order they are reported.
///
/// Short and evidence-chosen, not a survey. Each one is a command a bench
/// transcript spent steps discovering the hard way: a task re-derived a plan
/// around `python3` before finding its interpreter had no `pip` to install
/// with, and `apt-get` — which worked in the same dataset — was never tried.
/// Absence is the useful half, so [`EnvironmentIdentity::render`] names what
/// is missing rather than listing only what is there.
///
/// It answers "is this command on `PATH`" and nothing further. A library's
/// presence (`numpy`) is a question about an interpreter's own search path,
/// and it needs that interpreter spawned to answer honestly — which is a
/// different probe from this one and does not belong behind the same line.
pub const PROBED_COMMANDS: &[&str] = &["python3", "pip", "pip3", "apt-get", "cc", "make", "xxd"];

/// Whether `command` resolves to an executable file in `path` — a `PATH`
/// value in the platform's own separator convention.
///
/// Takes the search path rather than reading the environment, which is what
/// lets a test drive both answers without mutating a process-global the rest
/// of the suite is reading concurrently.
///
/// A bare name only: a `/`-bearing argument is a path, not a `PATH` lookup,
/// and answering one here would report a file the shell would never find this
/// way.
#[must_use]
pub fn command_on_path(command: &str, path: &std::ffi::OsStr) -> bool {
    if command.is_empty() || command.contains('/') || command.contains('\\') {
        return false;
    }
    std::env::split_paths(path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .any(|dir| is_executable(&dir.join(command)))
}

/// Whether `candidate` is a file this process could execute.
#[cfg(unix)]
fn is_executable(candidate: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    candidate
        .metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// On Windows executability is carried by the extension, and none of
/// [`PROBED_COMMANDS`] is a Windows tool. Reporting a bare-name miss is the
/// truthful answer there rather than a `PATHEXT` walk nothing consumes.
#[cfg(not(unix))]
fn is_executable(candidate: &Path) -> bool {
    candidate.is_file()
}

/// Which of [`PROBED_COMMANDS`] this process can run, against the ambient
/// `PATH`. Preserves [`PROBED_COMMANDS`]' order, so two sessions on one
/// machine render the same line.
#[must_use]
pub fn available_commands() -> Vec<&'static str> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    PROBED_COMMANDS
        .iter()
        .copied()
        .filter(|command| command_on_path(command, &path))
        .collect()
}

/// The environment facts collected once per session and rendered as labeled
/// lines by [`GetEnvironment`]. Every field is session-constant — a process
/// cannot change its OS mid-session, and the workspace root and scratch
/// directory are both fixed at session open.
#[derive(Debug, Clone)]
pub struct EnvironmentIdentity {
    /// The workspace root this session is scoped to.
    pub workspace_root: PathBuf,
    /// Whether `workspace_root` is a git repository.
    pub is_git: bool,
    /// Whether `workspace_root` is a linked worktree (see
    /// [`git_worktree_bits`]). Always `false` when `is_git` is `false`.
    pub is_linked_worktree: bool,
    /// `std::env::consts::OS`.
    pub platform: &'static str,
    /// `std::env::consts::ARCH`.
    pub arch: &'static str,
    /// `uname -sr`, when available.
    pub os_release: Option<String>,
    /// The login shell's basename, when available.
    pub login_shell: Option<String>,
    /// The session scratch directory (`crate::scratch::ScratchDir`), when
    /// the scratch plane initialized for this session.
    pub scratch_dir: Option<PathBuf>,
    /// Which of [`PROBED_COMMANDS`] resolve on this process's `PATH`.
    pub available_commands: Vec<&'static str>,
}

impl EnvironmentIdentity {
    /// Collect the facts for `workspace_root`. `scratch_dir` is supplied by
    /// the caller — the registry owns the scratch plane's lifecycle, so
    /// this module never creates or reaches into one itself.
    pub fn collect(workspace_root: &Path, scratch_dir: Option<PathBuf>) -> Self {
        let (is_git, is_linked_worktree) = git_worktree_bits(workspace_root);
        Self {
            workspace_root: workspace_root.to_path_buf(),
            is_git,
            is_linked_worktree,
            platform: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            os_release: os_release(),
            login_shell: login_shell(),
            scratch_dir,
            available_commands: available_commands(),
        }
    }

    /// Render as labeled lines — [`GetEnvironment`]'s entire output.
    pub fn render(&self) -> String {
        let git_note = if self.is_linked_worktree {
            "a git repository, and a LINKED WORKTREE"
        } else if self.is_git {
            "a git repository"
        } else {
            "not a git repository"
        };
        let mut lines = vec![
            format!("Workspace root: {}", self.workspace_root.display()),
            format!("Git: {git_note}"),
            format!("Platform: {} {}", self.platform, self.arch),
        ];
        if let Some(release) = &self.os_release {
            lines.push(format!("OS release: {release}"));
        }
        if let Some(shell) = &self.login_shell {
            lines.push(format!("Shell: {shell}"));
        }
        lines.push(match &self.scratch_dir {
            Some(dir) => format!("Scratch directory: {}", dir.display()),
            None => "Scratch directory: unavailable this session".to_string(),
        });
        let absent: Vec<&str> = PROBED_COMMANDS
            .iter()
            .copied()
            .filter(|command| !self.available_commands.contains(command))
            .collect();
        lines.push(format!(
            "Commands on PATH: {}",
            join_or(&self.available_commands, "none of the probed set")
        ));
        lines.push(format!(
            "Commands NOT on PATH: {}",
            join_or(&absent, "none — every probed command resolves")
        ));
        lines.join("\n")
    }
}

/// `names` comma-joined, or `empty` when there are none. A line ending in
/// nothing reads as a rendering fault, and the reader cannot tell it from a
/// probe that did not run.
fn join_or(names: &[&str], empty: &str) -> String {
    if names.is_empty() {
        return empty.to_string();
    }
    names.join(", ")
}

/// `get_environment`: report the session's environment in one call —
/// workspace root, git/worktree status, platform, OS release, login shell,
/// the scratch directory, and which of [`PROBED_COMMANDS`] resolve on `PATH`.
/// Zero arguments, single purpose: report (invariant #9). No model line — see
/// the module doc.
///
/// The description tells the model when the call is worth its cost (#3102).
/// Two facts here are tool-only, and both are prompt-unsafe for the same
/// reason: they differ per process, and the CLI's "Session environment" block
/// must stay byte-stable (invariant 7). The scratch directory is one. The
/// command probe is the other, and it is what a turn otherwise buys with
/// steps — a bench task spent one 120s timeout, then a second, discovering
/// its interpreter had no `pip`, and never tried the `apt-get` that worked in
/// the same dataset (#2670). Everything else is already in that block
/// (`append_session_environment` renders from these same functions), so a
/// model holding it buys only those two by calling this. The tool also stays
/// because hosts that assemble their own prompts (`stella-serve`) may carry
/// no such block at all.
pub struct GetEnvironment {
    /// The session scratch directory the registry created — the same
    /// directory the `save_state`/`get_state` plane is backed by.
    pub scratch_dir: Option<PathBuf>,
}

#[async_trait]
impl Tool for GetEnvironment {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "get_environment".into(),
            description: "Report this session's environment: workspace root, whether it is a \
                git repository (and whether it is a linked worktree), platform/arch, OS \
                release, login shell dialect, the scratch directory path, and which of \
                python3, pip, pip3, apt-get, cc, make and xxd are on PATH. Your system \
                prompt's Session environment block already states everything here except \
                the scratch directory and the PATH probe — call this only when you need \
                one of those, or when no such block is in your prompt. Call it before \
                planning any task around an interpreter or a package manager, so a missing \
                one costs a call rather than a timed-out command. Never spend calls on \
                pwd, uname, which, or shell probing for these facts."
                .into(),
            input_schema: json!({"type": "object", "properties": {}}),
            read_only: true,
            speculation_safe: true,
        }
    }

    async fn execute(&self, _input: &Value, ctx: &crate::ctx::ToolCtx) -> ToolOutput {
        let identity = EnvironmentIdentity::collect(ctx.root(), self.scratch_dir.clone());
        ToolOutput::Ok {
            content: identity.render(),
            data: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A primary checkout's `.git` is a directory — not flagged as a linked
    /// worktree, and still recognized as a git repository.
    #[test]
    fn a_primary_checkout_is_git_but_not_a_linked_worktree() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".git")).expect("mkdir .git");
        let (is_git, is_linked_worktree) = git_worktree_bits(dir.path());
        assert!(is_git, "a directory .git is a git repository");
        assert!(
            !is_linked_worktree,
            "a directory .git is not a worktree link"
        );
    }

    /// The worktree bit that matters most for this repository: fleet
    /// workers and pipeline candidates run in linked worktrees, and a
    /// `.git` **file** (`git worktree add`'s on-disk shape) is what flags
    /// one. Mirrors the fixture `stella-cli`'s prompt test uses for the
    /// same shape (#2692), so the two never silently diverge.
    #[test]
    fn a_gitfile_checkout_is_flagged_as_a_linked_worktree() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(".git"),
            "gitdir: /elsewhere/.git/worktrees/x\n",
        )
        .expect("write gitfile");
        let (is_git, is_linked_worktree) = git_worktree_bits(dir.path());
        assert!(is_git, "a gitfile checkout is still a git repository");
        assert!(
            is_linked_worktree,
            "a gitfile .git is the worktree-link shape"
        );
    }

    /// No `.git` at all: neither bit is set.
    #[test]
    fn a_non_repository_is_neither_git_nor_a_worktree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (is_git, is_linked_worktree) = git_worktree_bits(dir.path());
        assert!(!is_git);
        assert!(!is_linked_worktree);
    }

    /// The witness for #2697 at the `stella-tools` boundary: `get_environment`
    /// reports the workspace root and the platform this process is actually
    /// running on. Fails on `main` because the tool does not exist there.
    #[tokio::test]
    async fn get_environment_reports_workspace_root_and_platform() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = GetEnvironment { scratch_dir: None };
        let ToolOutput::Ok { content, .. } = tool
            .execute(
                &json!({}),
                &crate::ctx::ToolCtx::bare(dir.path().to_path_buf()),
            )
            .await
        else {
            panic!("get_environment must succeed with zero arguments");
        };
        assert!(
            content.contains(&dir.path().display().to_string()),
            "must report the workspace root: {content}"
        );
        assert!(
            content.contains(std::env::consts::OS),
            "must report the platform: {content}"
        );
        assert!(
            content.contains("unavailable this session"),
            "no model line, and an absent scratch dir is stated, not silently omitted: {content}"
        );
        assert!(
            !content.to_lowercase().contains("model:"),
            "get_environment must never state a model — that is the CLI prompt layer's job: {content}"
        );
    }

    /// The witness for #3102 finding 4: the schema description must steer the
    /// model away from re-buying facts the prompt already carries. The audit
    /// found the tool is NOT a strict subset of the Session environment block
    /// — the scratch directory is tool-only (and the model line prompt-only)
    /// — so the tool survives, but its description has to name that margin or
    /// every call against a prompt-carrying session is a wasted one. The
    /// `PATH` probe (#2670) joined that margin and is named the same way.
    #[test]
    fn the_description_names_the_marginal_facts_the_prompt_block_lacks() {
        let description = GetEnvironment { scratch_dir: None }.schema().description;
        assert!(
            description.contains("except the scratch directory and the PATH probe"),
            "the description must say the prompt already carries everything else: {description}"
        );
        assert!(
            description.contains("call this only when"),
            "the description must scope when the call is worth making: {description}"
        );
        for command in PROBED_COMMANDS {
            assert!(
                description.contains(command),
                "the description must name {command}, or the model cannot tell whether \
                 the call answers the question it has: {description}"
            );
        }
    }

    /// **Witness (#2670, P6(c)).** A bare name resolves against `PATH` by
    /// executable bit, and a file that is merely present does not count — a
    /// probe that answered yes for an unexecutable file would send the model
    /// down the plan the probe exists to rule out.
    #[cfg(unix)]
    #[test]
    fn the_path_probe_answers_by_executable_bit() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let runnable = dir.path().join("runnable-probe");
        std::fs::write(&runnable, "#!/bin/sh\n").expect("write");
        std::fs::set_permissions(&runnable, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        std::fs::write(dir.path().join("inert-probe"), "not executable").expect("write");

        let path = std::ffi::OsString::from(dir.path());
        assert!(command_on_path("runnable-probe", &path));
        assert!(!command_on_path("inert-probe", &path));
        assert!(!command_on_path("absent-probe", &path));
        assert!(
            !command_on_path("./runnable-probe", &path),
            "a path is not a PATH lookup, and the shell would not find it this way"
        );
    }

    /// Every probed command is reported, present or absent — the absent half
    /// is the one that changes a plan, and a line that listed only what is
    /// there would leave the model to infer the rest from silence.
    #[tokio::test]
    async fn get_environment_places_every_probed_command_on_one_side_or_the_other() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = GetEnvironment { scratch_dir: None };
        let ToolOutput::Ok { content, .. } = tool
            .execute(
                &json!({}),
                &crate::ctx::ToolCtx::bare(dir.path().to_path_buf()),
            )
            .await
        else {
            panic!("get_environment must succeed with zero arguments");
        };

        let present = line_after(&content, "Commands on PATH: ");
        let absent = line_after(&content, "Commands NOT on PATH: ");
        for command in PROBED_COMMANDS {
            let named = present.split(", ").any(|found| found == *command)
                || absent.split(", ").any(|missing| missing == *command);
            assert!(named, "{command} is on neither line: {content}");
        }
    }

    fn line_after<'a>(content: &'a str, label: &str) -> &'a str {
        content
            .lines()
            .find_map(|line| line.strip_prefix(label))
            .unwrap_or_else(|| panic!("no {label:?} line in:\n{content}"))
    }

    /// A supplied scratch directory is reported by path, exactly as passed.
    #[tokio::test]
    async fn get_environment_reports_the_scratch_directory_when_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        let scratch = tempfile::tempdir().expect("scratch tempdir");
        let tool = GetEnvironment {
            scratch_dir: Some(scratch.path().to_path_buf()),
        };
        let ToolOutput::Ok { content, .. } = tool
            .execute(
                &json!({}),
                &crate::ctx::ToolCtx::bare(dir.path().to_path_buf()),
            )
            .await
        else {
            panic!("get_environment must succeed with zero arguments");
        };
        assert!(
            content.contains(&scratch.path().display().to_string()),
            "must report the scratch directory: {content}"
        );
    }
}
