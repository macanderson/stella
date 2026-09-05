//! A `git` command built to ignore the calling process's own environment.
//!
//! Plain code builds a `git` command from `Command::new("git")` plus a few
//! `.env()` calls. That command still copies every other variable from the
//! process that started it. This is risky in two ways.
//!
//! First, `git` reads `GIT_CONFIG_GLOBAL` before it reads `HOME`. If that
//! variable is set, `git` uses the file it names instead of
//! `$HOME/.gitconfig`. So setting `HOME` alone does not keep `git` away
//! from a real config file.
//!
//! Second, building the command reads the whole environment table at the
//! moment it spawns. Another thread can change that table at the same
//! time, with `std::env::set_var`. POSIX calls that a data race. A test
//! binary runs many tests on many threads, so this can really happen. When
//! it does, the `git` spawn can fail. The caller drops that error and
//! treats the write as done. A later read then finds nothing, because
//! nothing was ever written. The same test run alone never hits this,
//! because no other thread is there to race it.
//!
//! [`sealed_git`] fixes both: it starts from no environment at all, then
//! adds back only `PATH`, `HOME`, and `GIT_CONFIG_NOSYSTEM`. `PATH` stays
//! because without it `git` is found only through a built-in default path,
//! which can miss a Homebrew or rustup install.

use std::path::Path;
use std::process::Command;

/// Build a `git` command that starts from an empty environment. It adds
/// back only `PATH`, `HOME` (pointed at `git_dir`, away from any real
/// `~/.gitconfig`), and `GIT_CONFIG_NOSYSTEM`. See the module doc for why.
pub(crate) fn sealed_git(git_dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        cmd.env("PATH", path);
    }
    // The work tree is the user's; a hook or config of theirs must never run
    // as a side effect of stella recording history.
    cmd.env("GIT_CONFIG_NOSYSTEM", "1").env("HOME", git_dir);
    cmd
}
