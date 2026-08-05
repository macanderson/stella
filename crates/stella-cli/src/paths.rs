//! The one seam through which `stella-cli` resolves user-global paths.
//!
//! Home-directory and data-directory resolution used to be an ambient
//! `std::env::var_os("HOME")` at eleven independent call sites — settings, TOML
//! config, extensions, env files, attachments, the deck's debug log, the agent
//! and candidate tool surfaces, skills, and the tool foundry. Eleven readers of
//! one process-global is why a test that wanted to point Stella at a temporary
//! home had to mutate the real process environment, and why POSIX
//! `setenv`/`getenv` races (documented UB, and the test runner is
//! multi-threaded) had to be contained with a binary-wide lock and a
//! panic-safe restore guard rather than deleted (#911, #1098, #1139).
//!
//! This module is the port that deletes them. Resolution happens **once**, in
//! [`UserPaths::from_environment`], and `main` installs the result before
//! anything else runs. Everything downstream reads the resolved value.
//!
//! # Resolving once is sound, not merely convenient
//!
//! Every name read here — `HOME`, `USERPROFILE`, `XDG_STATE_HOME`,
//! `STELLA_HOME`, `STELLA_DATA_DIR`, `STELLA_NO_SETTINGS` — is on the
//! `env_files` deny-list, so a project `.env` cannot change any of them. That
//! is what makes "resolve at startup" equivalent to "resolve on every read"
//! rather than a behaviour change: there is no point in the process's life at
//! which these answers legitimately differ. (Denying them is not a
//! convenience for this module —
//! the USER settings scope is `$HOME/.stella/settings.json` and is *trusted*,
//! so a repository that could move `HOME` would walk straight through that
//! trust boundary. See `env_files::DENIED_EXACT`.)
//!
//! # Redirecting without touching the process environment
//!
//! Tests install a **per-thread** override — `test_paths` and its two
//! narrower spellings, all `#[cfg(test)]`, which is why nothing here links to
//! them. No `unsafe`, no process mutation, no lock, nothing another test
//! running in parallel can observe — so a test that redirects home or
//! data-dir resolution needs neither `test_env::lock` nor `EnvRestore`. Those
//! remain for genuinely env-shaped fixtures, such as provider credential
//! variables.

use std::path::PathBuf;
use std::sync::OnceLock;

/// `$XDG_STATE_HOME` — the XDG base directory for state that should persist
/// but is not configuration and not a cache.
const XDG_STATE_HOME_ENV: &str = "XDG_STATE_HOME";

/// The launcher signal that closes the filesystem-isolation boundary.
const STELLA_NO_SETTINGS_ENV: &str = "STELLA_NO_SETTINGS";

/// Where a stella process resolves everything user-global.
///
/// One value, built once, rather than a scatter of reads at arbitrary depths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UserPaths {
    /// The OS user home (`$HOME`), or `None` on a machine without one.
    home: Option<PathBuf>,
    /// `$XDG_STATE_HOME`, else `$HOME/.local/state`.
    state_home: Option<PathBuf>,
    /// The user-tier stella data dir — the `usage.db` hub, the session
    /// registry, notifications, the catalog, the enterprise spool.
    data_dir: PathBuf,
    /// Whether user-scope **extensions** (rules, skills, custom tools,
    /// published context records) may be loaded from [`Self::home`].
    ///
    /// Production always says yes. A unit test says no until it installs a
    /// home of its own, because a suite that reads the developer's real
    /// `~/.stella/rules` passes or fails for reasons that have nothing to do
    /// with the code under test — and would then disagree with CI, where that
    /// directory does not exist. This is the invariant the `TEST_USER_HOME`
    /// thread-local used to carry by answering `None` for the home itself;
    /// naming it as policy keeps one home while keeping the isolation.
    extensions_visible: bool,
    /// Whether the trusted launcher's filesystem-isolation boundary is closed
    /// (`STELLA_NO_SETTINGS`). Settings and config use it to disable
    /// filesystem configuration and credentials; session assembly also uses it
    /// to exclude Stella-specific prompt steering, executable extensions, and
    /// persisted learning state.
    filesystem_isolated: bool,
}

impl UserPaths {
    /// Resolve from the ambient process environment.
    ///
    /// **This function is the only place `stella-cli` reads a home or data
    /// directory out of `std::env`.** Anything else that wants one asks the
    /// accessors below.
    pub(crate) fn from_environment() -> Self {
        let home = std::env::var_os(stella_home::HOME_ENV).map(PathBuf::from);
        let state_home = std::env::var_os(XDG_STATE_HOME_ENV)
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|home| home.join(".local/state")));
        Self {
            home,
            state_home,
            // `stella-home` is the workspace's single implementation, shared
            // with `stella-store` and `stella-observatory` (#1139), so the CLI
            // cannot drift from the crates that own the files it names.
            data_dir: stella_home::data_dir(),
            extensions_visible: true,
            filesystem_isolated: crate::settings::env_flag(STELLA_NO_SETTINGS_ENV),
        }
    }

    /// Every user-global path under one directory, resolved from nothing at
    /// all. The shape a test wants: one `TempDir`, one call, no environment.
    #[cfg(test)]
    pub(crate) fn rooted_at(home: &std::path::Path) -> Self {
        Self {
            home: Some(home.to_path_buf()),
            state_home: Some(home.join(".local/state")),
            data_dir: home.join(".stella"),
            extensions_visible: true,
            filesystem_isolated: false,
        }
    }

    /// What an un-redirected unit test sees.
    ///
    /// The ambient home is kept — ~20 tests set `HOME` to steer loaders in
    /// *other* crates (`stella-store`'s home, `stella-tools`' custom-tool
    /// discovery) and must keep working — but the two policy bits are forced
    /// to their isolated values, exactly as the `TEST_USER_HOME` and
    /// `TEST_FILESYSTEM_ISOLATION` thread-locals did before this existed.
    #[cfg(test)]
    fn test_default() -> Self {
        Self {
            extensions_visible: false,
            filesystem_isolated: false,
            ..Self::from_environment()
        }
    }
}

/// The process's resolved paths, installed once by `main`.
static RESOLVED: OnceLock<UserPaths> = OnceLock::new();

/// Install the process's user-global paths. Called by `main` at startup, in
/// one place, before anything resolves a path.
///
/// A second call is ignored rather than a panic: the first value is the one
/// every subsequent read has already been given, so replacing it would be the
/// bug, not the duplicate call.
pub(crate) fn install(paths: UserPaths) {
    let _ = RESOLVED.set(paths);
}

/// Read one field of the paths in effect for this thread.
fn with_paths<T>(read: impl FnOnce(&UserPaths) -> T) -> T {
    #[cfg(test)]
    {
        // Deliberately NOT cached in a test build. The harness runs this
        // binary's tests on parallel threads in one process, so a value frozen
        // by whichever test happened to run first would leak into every test
        // after it — including the tests that still steer sibling crates by
        // setting `HOME` or `STELLA_DATA_DIR`. Resolving per read keeps those
        // working unchanged; a test that installs an override never consults
        // the environment at all.
        TEST_PATHS.with(|cell| match cell.borrow().as_ref() {
            Some(paths) => read(paths),
            None => read(&UserPaths::test_default()),
        })
    }
    #[cfg(not(test))]
    {
        read(RESOLVED.get_or_init(UserPaths::from_environment))
    }
}

/// The OS user home (`$HOME`), or `None` on a machine without one.
pub(crate) fn home() -> Option<PathBuf> {
    with_paths(|paths| paths.home.clone())
}

/// `~/.stella` — the user-global stella root.
///
/// Deliberately **not** `stella_home::stella_home`: the CLI's user scope has
/// always been `$HOME/.stella` and honours neither `STELLA_HOME` nor
/// `STELLA_CONFIG_DIR`. `stella-observatory`'s settings tab mirrors that rule
/// and has a test pinning it, so widening it here would make the dashboard
/// name a settings file the loader never reads.
pub(crate) fn stella_root() -> Option<PathBuf> {
    with_paths(|paths| paths.home.as_ref().map(|home| home.join(".stella")))
}

/// `$XDG_STATE_HOME`, else `$HOME/.local/state`.
pub(crate) fn state_home() -> Option<PathBuf> {
    with_paths(|paths| paths.state_home.clone())
}

/// The user-tier stella data dir: the `usage.db` hub, the session registry,
/// notifications, the catalog, the enterprise spool.
pub(crate) fn data_dir() -> PathBuf {
    with_paths(|paths| paths.data_dir.clone())
}

/// The home that user-scope **extension** loaders resolve against — rules,
/// skills, custom tools, published context records.
///
/// Identical to [`home`] in production. In a test build it answers `None`
/// until the test installs a home, so a suite never reads the developer's own
/// `~/.stella`; see [`UserPaths::extensions_visible`].
pub(crate) fn user_extension_home() -> Option<PathBuf> {
    with_paths(|paths| {
        paths
            .extensions_visible
            .then(|| paths.home.clone())
            .flatten()
    })
}

/// Whether the trusted launcher enabled the benchmark's filesystem-isolation
/// boundary.
pub(crate) fn filesystem_settings_disabled() -> bool {
    with_paths(|paths| paths.filesystem_isolated)
}

// A per-thread redirect, so a test can point resolution somewhere else without
// mutating the process environment that every other thread shares.
#[cfg(test)]
std::thread_local! {
    static TEST_PATHS: std::cell::RefCell<Option<UserPaths>> = const { std::cell::RefCell::new(None) };
}

/// Restores whatever this thread saw before the override, including on an
/// unwinding panic — so a failing assertion cannot leak a redirect into the
/// next test on the same thread.
#[cfg(test)]
#[must_use]
pub(crate) struct TestPathsGuard {
    previous: Option<UserPaths>,
}

#[cfg(test)]
impl Drop for TestPathsGuard {
    fn drop(&mut self) {
        TEST_PATHS.with(|cell| *cell.borrow_mut() = self.previous.take());
    }
}

/// Redirect every user-global path on this thread.
#[cfg(test)]
pub(crate) fn test_paths(paths: UserPaths) -> TestPathsGuard {
    amend(|current| *current = paths)
}

/// Redirect the home directory alone — user scope, extensions included.
#[cfg(test)]
pub(crate) fn test_user_home(home: PathBuf) -> TestPathsGuard {
    amend(move |current| {
        current.state_home = Some(home.join(".local/state"));
        current.home = Some(home);
        current.extensions_visible = true;
    })
}

/// Close (or reopen) the filesystem-isolation boundary alone.
#[cfg(test)]
pub(crate) fn test_filesystem_isolation(enabled: bool) -> TestPathsGuard {
    amend(move |current| current.filesystem_isolated = enabled)
}

/// Apply `change` to the paths this thread sees right now.
///
/// Amending rather than replacing is what lets the narrow spellings nest:
/// `test_user_home(…)` and `test_filesystem_isolation(true)` are held together
/// by more than one test, and each must keep the other's effect.
#[cfg(test)]
fn amend(change: impl FnOnce(&mut UserPaths)) -> TestPathsGuard {
    let previous = TEST_PATHS.with(|cell| cell.borrow().clone());
    let mut next = previous.clone().unwrap_or_else(UserPaths::test_default);
    change(&mut next);
    TEST_PATHS.with(|cell| *cell.borrow_mut() = Some(next));
    TestPathsGuard { previous }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// Witness for #1139: redirecting home and data-dir resolution takes no
    /// process-environment mutation, so it needs neither the binary-wide env
    /// lock nor a restore guard — and cannot race a test on another thread.
    ///
    /// Note what this test does NOT do: there is no `test_env::lock()`, no
    /// `EnvRestore`, and no `unsafe`. That absence is the assertion.
    #[test]
    fn a_redirect_needs_no_process_environment_mutation() {
        let home = PathBuf::from("/tmp/stella-paths-witness");
        let ambient = std::env::var_os(stella_home::HOME_ENV);

        let _guard = test_paths(UserPaths::rooted_at(&home));

        assert_eq!(super::home(), Some(home.clone()));
        assert_eq!(stella_root(), Some(home.join(".stella")));
        assert_eq!(data_dir(), home.join(".stella"));
        assert_eq!(state_home(), Some(home.join(".local/state")));
        assert_eq!(user_extension_home(), Some(home));
        assert_eq!(
            std::env::var_os(stella_home::HOME_ENV),
            ambient,
            "the process environment is exactly as it was"
        );
    }

    /// The override is thread-scoped, which is the property that makes it safe
    /// under the parallel runner: the redirect above is invisible here.
    #[test]
    fn a_redirect_does_not_escape_its_thread() {
        let _guard = test_paths(UserPaths::rooted_at(Path::new("/tmp/stella-paths-thread")));
        let elsewhere = std::thread::spawn(home).join().expect("thread joined");
        assert_ne!(
            elsewhere,
            Some(PathBuf::from("/tmp/stella-paths-thread")),
            "a per-thread redirect that reached another thread would be the \
             process-global mutation this replaces"
        );
    }

    /// Dropping a guard restores what the thread saw before it, so nesting the
    /// narrow spellings composes instead of clobbering.
    #[test]
    fn narrow_overrides_nest_and_unwind_cleanly() {
        let home = PathBuf::from("/tmp/stella-paths-nesting");
        let outer = test_user_home(home.clone());
        assert!(!filesystem_settings_disabled());

        {
            let _inner = test_filesystem_isolation(true);
            assert!(filesystem_settings_disabled(), "the inner guard applies");
            assert_eq!(
                super::home(),
                Some(home.clone()),
                "and does not clobber the outer one"
            );
        }

        assert!(
            !filesystem_settings_disabled(),
            "the inner guard restored on drop"
        );
        assert_eq!(super::home(), Some(home), "the outer guard still stands");
        drop(outer);
        assert_eq!(
            user_extension_home(),
            None,
            "back to the isolated default a unit test starts from"
        );
    }

    /// User-scope extensions stay invisible until a test says otherwise, so a
    /// suite never loads the developer's real `~/.stella/rules`.
    #[test]
    fn user_extensions_are_invisible_until_a_test_installs_a_home() {
        assert_eq!(user_extension_home(), None);
        let _guard = test_user_home(PathBuf::from("/tmp/stella-paths-extensions"));
        assert_eq!(
            user_extension_home(),
            Some(PathBuf::from("/tmp/stella-paths-extensions")),
            "installing a home is the opt-in"
        );
    }

    /// `from_environment` derives the XDG state home from `HOME` when the
    /// variable itself is unset — the fallback the deck's debug log depends on.
    #[test]
    fn state_home_falls_back_under_the_user_home() {
        let paths = UserPaths::rooted_at(Path::new("/home/dev"));
        assert_eq!(
            paths.state_home,
            Some(PathBuf::from("/home/dev/.local/state"))
        );
    }

    /// The fence that keeps this module the only seam.
    ///
    /// #1139's first acceptance line is "no `stella-cli` production code reads
    /// `HOME` via `std::env` directly" — a property that decays the moment
    /// someone adds a twelfth reader, which is exactly how the eleven
    /// accumulated. So it is checked rather than asserted in prose.
    ///
    /// # Why a test rather than a gate script
    ///
    /// `make gate` already runs the suite, so this costs no new step (the same
    /// argument `src/tests.rs`' command-documentation fence makes). And it
    /// scopes itself correctly for free: a shell grep would have to re-derive
    /// which directory is this crate's source.
    ///
    /// It matches **reads** only. Writes (`set_var`/`remove_var`) are a
    /// different question — see `crate::startup` for the guard on those — and
    /// tests legitimately set `HOME` to steer loaders in sibling crates.
    #[test]
    fn nothing_else_in_this_crate_reads_a_home_out_of_the_environment() {
        const ANCHORS: [&str; 5] = [
            stella_home::HOME_ENV,
            stella_home::USERPROFILE_ENV,
            stella_home::STELLA_HOME_ENV,
            stella_home::STELLA_DATA_DIR_ENV,
            XDG_STATE_HOME_ENV,
        ];
        // Built rather than written out, so this file is not itself a match.
        let forbidden: Vec<String> = ANCHORS
            .iter()
            .flat_map(|name| ["var_os", "var"].map(|read| format!("{read}(\"{name}\")")))
            .collect();
        // `remove_var("HOME")` ends in the same six characters as
        // `var("HOME")`, and it is a WRITE — a different question, guarded
        // elsewhere. Requiring a non-`_` before the match separates the two
        // without pinning the exact `std::env::` spelling a caller used.
        let reads = |body: &str, pattern: &str| {
            body.match_indices(pattern)
                .any(|(at, _)| at == 0 || !body[..at].ends_with('_'))
        };

        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut dirs = vec![src.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
                .flatten()
            {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                if path.extension().is_none_or(|ext| ext != "rs")
                    || path.file_name().is_some_and(|name| name == "paths.rs")
                {
                    continue;
                }
                let body = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
                for pattern in &forbidden {
                    if reads(&body, pattern) {
                        let relative = path.strip_prefix(&src).unwrap_or(&path);
                        offenders.push(format!("{}: {pattern}", relative.display()));
                    }
                }
            }
        }
        offenders.sort();
        assert!(
            offenders.is_empty(),
            "these read a user-global anchor straight out of the process \
             environment instead of asking `crate::paths`, which is what makes \
             a test able to redirect one without mutating process-global state \
             (#1139):\n  {}",
            offenders.join("\n  ")
        );
    }
}
