// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Real-shell lookup for two spawn sites: [`crate::bash`] and the hook
//! runner's own-shell path ([`crate::hook_runner`]). Both call
//! `Command::new("bash")` and trust it to mean a real shell.
//!
//! On unix that trust is safe. `$PATH` finds a real shell there, and no
//! stub gets in the way. On Windows it is not safe. `Command::new("bash")`
//! resolves through `CreateProcessW`'s own search order. That order checks
//! `%SystemRoot%\System32` before it ever looks at `PATH`. Windows ships a
//! `bash.exe` stub there: the WSL launcher. With no WSL distro installed,
//! that stub prints an install prompt and exits with an error. It never
//! runs the script it was handed. No `PATH` order fixes this. The system
//! folder wins every time.
//!
//! [`bash_command`] replaces the bare call. On unix it is exactly
//! `Command::new("bash")`. On Windows it finds an absolute path to a real
//! shell first ([`resolve_bash`]) and runs that path directly. An absolute
//! path skips the search order entirely. A caller that gets back
//! [`ShellResolutionError`] knows no real shell was found. It can say so,
//! instead of quietly running whatever `bash` turned out to mean.

#[cfg(any(test, windows))]
use std::path::{Path, PathBuf};

use tokio::process::Command;

/// No real shell was found. Lists every place this looked, so the operator
/// knows what to install or add to `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellResolutionError {
    tried: Vec<String>,
}

impl std::fmt::Display for ShellResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no real shell found (checked: {}) — install Git for Windows, or put a real \
             bash.exe on PATH ahead of %SystemRoot%\\System32",
            self.tried.join(", ")
        )
    }
}

impl std::error::Error for ShellResolutionError {}

/// The lookup itself, over passed-in environment and filesystem checks
/// instead of the real ones. [`crate::hook_runner::plugin_command`] uses
/// the same shape for its `lookup` closure. A test can build the exact bad
/// case: a stub `bash.exe` sitting in the system folder, ahead of a real
/// one on `PATH`. No real Windows machine is needed to prove it.
///
/// `env` reads one environment variable by name. `path_dirs` lists the
/// `PATH` folders in order. `exists` checks whether a file is there. Git
/// for Windows' own install folder is checked first. Most Windows users
/// with `bash` on `PATH` have their shell there. Only then does this walk
/// `PATH` by hand, and it skips the system folder. That folder is the WSL
/// stub's home, never a real shell's.
///
/// `cfg(any(test, windows))` keeps this compiled only where it is used: on
/// Windows, by [`resolve_bash`]; under test, by the tests below. Anywhere
/// else it would sit unused and fail the lint that checks for that.
#[cfg(any(test, windows))]
fn resolve_bash_with(
    env: impl Fn(&str) -> Option<String>,
    path_dirs: impl Fn() -> Vec<PathBuf>,
    exists: impl Fn(&Path) -> bool,
) -> Result<PathBuf, ShellResolutionError> {
    let mut tried = Vec::new();

    for program_files in ["ProgramFiles", "ProgramFiles(x86)"] {
        let Some(base) = env(program_files) else {
            continue;
        };
        let candidate = PathBuf::from(base).join("Git").join("bin").join("bash.exe");
        if exists(&candidate) {
            return Ok(candidate);
        }
        tried.push(candidate.display().to_string());
    }

    let system_dir = env("SystemRoot").map(|root| PathBuf::from(root).join("System32"));
    for dir in path_dirs() {
        if system_dir.as_deref() == Some(dir.as_path()) {
            continue;
        }
        let candidate = dir.join("bash.exe");
        if exists(&candidate) {
            return Ok(candidate);
        }
        tried.push(candidate.display().to_string());
    }

    Err(ShellResolutionError { tried })
}

/// [`resolve_bash_with`] wired to the real environment and filesystem.
///
/// Windows-only. Its one caller is [`bash_command`]'s Windows branch, which
/// always runs on Windows, test or not. No separate test gate is needed
/// here.
#[cfg(windows)]
fn resolve_bash() -> Result<PathBuf, ShellResolutionError> {
    resolve_bash_with(
        |name| std::env::var(name).ok(),
        || {
            std::env::var_os("PATH")
                .map(|path| std::env::split_paths(&path).collect())
                .unwrap_or_default()
        },
        |candidate| candidate.is_file(),
    )
}

/// A `Command` for a real shell. Never the Windows WSL launcher stub in
/// `bash`'s place. See the module doc for why unix and Windows need
/// different answers.
pub(crate) fn bash_command() -> Result<Command, ShellResolutionError> {
    #[cfg(windows)]
    {
        resolve_bash().map(Command::new)
    }
    #[cfg(not(windows))]
    {
        Ok(Command::new("bash"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `PATH` shaped like the real bug report: the WSL stub's folder
    /// listed first, a real shell listed after it. No Git for Windows
    /// install folder is set, so the walk must reach `PATH`.
    /// `Command::new("bash")`'s own lookup would find the stub no matter
    /// the order. This function must not.
    ///
    /// This test cannot fail before this file existed: there was no
    /// lookup to fail. Only a bare `Command::new("bash")` stood there,
    /// with no logic to check.
    #[test]
    fn skips_the_system_directory_even_when_it_leads_path() {
        let system_root = PathBuf::from(r"C:\Windows");
        let system32 = system_root.join("System32");
        let git_bin = PathBuf::from(r"C:\Users\dev\scoop\shims");
        let dirs = vec![system32.clone(), git_bin.clone()];

        let stub = system32.join("bash.exe");
        let real = git_bin.join("bash.exe");

        let resolved = resolve_bash_with(
            |name| (name == "SystemRoot").then(|| system_root.display().to_string()),
            || dirs.clone(),
            |candidate| *candidate == stub || *candidate == real,
        )
        .expect("a real shell sits later on PATH");

        assert_eq!(
            resolved, real,
            "the system-directory stub must never be chosen, even first on PATH"
        );
    }

    /// Git for Windows' own install folder wins, even when a `bash.exe`
    /// also sits on `PATH`. It is checked first, because that is where
    /// most real installs put their shell. Finding it needs no `PATH`
    /// walk.
    #[test]
    fn prefers_the_documented_git_for_windows_install() {
        let program_files = PathBuf::from(r"C:\Program Files");
        let git_bash = program_files.join("Git").join("bin").join("bash.exe");
        let path_bash = PathBuf::from(r"C:\Windows\System32").join("bash.exe");

        let resolved = resolve_bash_with(
            |name| (name == "ProgramFiles").then(|| program_files.display().to_string()),
            || vec![PathBuf::from(r"C:\Windows\System32")],
            |candidate| *candidate == git_bash || *candidate == path_bash,
        )
        .expect("Git for Windows is installed at the documented location");

        assert_eq!(resolved, git_bash);
    }

    /// Nothing is found anywhere. The error names every place it looked.
    /// It never falls back to the stub in silence.
    #[test]
    fn names_every_location_it_tried_when_nothing_is_found() {
        let err = resolve_bash_with(
            |name| (name == "SystemRoot").then(|| r"C:\Windows".to_string()),
            || vec![PathBuf::from(r"C:\Windows\System32")],
            |_candidate| false,
        )
        .expect_err("no shell exists anywhere this walk looked");

        assert!(
            err.to_string().contains("bash.exe"),
            "the error should name what it looked for: {err}"
        );
    }

    /// On unix, `bash_command` is exactly `Command::new("bash")`. No
    /// lookup runs, because there is no stub here to trip on.
    #[cfg(not(windows))]
    #[test]
    fn unix_bash_command_needs_no_resolution() {
        let cmd = bash_command().expect("unix never fails to resolve bash");
        assert_eq!(cmd.as_std().get_program(), "bash");
    }
}
