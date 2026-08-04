//! `~/.stella` — the single user-level stella home, on every platform.
//!
//! Everything user-global (the `usage.db` telemetry hub, `catalog.db`, the
//! session registry, notifications, the enterprise spool, settings, skills,
//! agents, rules, tools, credentials) lives directly under `~/.stella`, the
//! same way Claude Code uses `~/.claude`. No platform data-dir guessing:
//! macOS, Linux, and Windows all resolve to the home directory.
//!
//! Overrides: `STELLA_HOME` moves the whole home; the narrower
//! `STELLA_DATA_DIR` / `STELLA_CONFIG_DIR` overrides still win where they
//! always did (tests, sandboxes, org-managed layouts).
//!
//! # Why this is its own crate
//!
//! It used to be two implementations. `stella_store::usage::data_dir` was
//! canonical and `stella_observatory::global::data_dir` was a deliberate copy,
//! because the observatory must not link `stella-store` — `Store::open` runs
//! migrations, migrations are writes, and an observer must never mutate what
//! it observes. The copy carried a comment asking future readers to keep the
//! two in sync by hand, which is a divergence waiting to happen (#1139). A
//! dependency-free leaf crate is the one shape both sides can depend on.
//!
//! # Reading the environment, and not reading it
//!
//! Every resolver comes in two halves: a `resolve_*` function that is a pure
//! total function of the anchors it is given, and a thin wrapper that supplies
//! those anchors from the process environment. Callers that already hold
//! resolved paths (a test, an injected port, a host that was told where its
//! home is) use the pure half and never touch `std::env` — which is the point,
//! since concurrent `getenv`/`setenv` is undefined behaviour on POSIX and the
//! process environment is the one piece of global state a test cannot own.

use std::ffi::OsString;
use std::path::PathBuf;

/// The OS user home. `USERPROFILE` is the Windows spelling.
pub const HOME_ENV: &str = "HOME";
/// The Windows spelling of [`HOME_ENV`], consulted only as a fallback.
pub const USERPROFILE_ENV: &str = "USERPROFILE";
/// Moves the whole stella home away from `~/.stella`.
pub const STELLA_HOME_ENV: &str = "STELLA_HOME";
/// Moves the user-tier data dir alone. Narrower than [`STELLA_HOME_ENV`], and
/// wins over it.
pub const STELLA_DATA_DIR_ENV: &str = "STELLA_DATA_DIR";
/// Moves the user-tier config dir alone. Narrower than [`STELLA_HOME_ENV`].
pub const STELLA_CONFIG_DIR_ENV: &str = "STELLA_CONFIG_DIR";

/// The directory name stella keeps under the user's home.
const DOT_STELLA: &str = ".stella";

/// Every override that can point resolution away from the `~/.stella`
/// defaults, in one list so a caller asking "is any of this redirected?"
/// cannot answer with a stale subset.
pub const OVERRIDE_ENV_VARS: [&str; 3] =
    [STELLA_HOME_ENV, STELLA_DATA_DIR_ENV, STELLA_CONFIG_DIR_ENV];

/// The user's home directory: `HOME`, else `USERPROFILE` (Windows).
#[must_use]
pub fn home_dir() -> Option<PathBuf> {
    resolve_home_dir(read(HOME_ENV), read(USERPROFILE_ENV))
}

/// [`home_dir`] over anchors the caller supplies.
#[must_use]
pub fn resolve_home_dir(home: Option<OsString>, userprofile: Option<OsString>) -> Option<PathBuf> {
    home.or(userprofile).map(PathBuf::from)
}

/// The stella home: `$STELLA_HOME`, else `~/.stella`. `None` only when no
/// home directory is discoverable at all.
#[must_use]
pub fn stella_home() -> Option<PathBuf> {
    resolve_stella_home(read(STELLA_HOME_ENV), home_dir())
}

/// [`stella_home`] over anchors the caller supplies.
#[must_use]
pub fn resolve_stella_home(
    stella_home: Option<OsString>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    match stella_home {
        Some(dir) => Some(PathBuf::from(dir)),
        None => home.map(|home| home.join(DOT_STELLA)),
    }
}

/// The user-tier stella data dir (usage rollup, session registry,
/// notifications, enterprise spool, catalog): `$STELLA_DATA_DIR`, else
/// [`stella_home`], else the current directory.
///
/// The current-directory fallback is deliberate and load-bearing: a machine
/// with no discoverable home must still get a usable path rather than a
/// panic, because every caller of this is on a best-effort telemetry or
/// registry path that has no business failing a turn.
#[must_use]
pub fn data_dir() -> PathBuf {
    resolve_data_dir(read(STELLA_DATA_DIR_ENV), stella_home())
}

/// [`data_dir`] over anchors the caller supplies.
#[must_use]
pub fn resolve_data_dir(data_dir: Option<OsString>, stella_home: Option<PathBuf>) -> PathBuf {
    match data_dir {
        Some(dir) => PathBuf::from(dir),
        None => stella_home.unwrap_or_else(|| PathBuf::from(".")),
    }
}

/// Whether any of [`OVERRIDE_ENV_VARS`] is set — i.e. whether resolution is
/// pointing somewhere other than the `~/.stella` defaults.
///
/// The one-time legacy-layout migration consults this: moving real user data
/// while an override redirects resolution would move it out from under
/// whatever set the override.
#[must_use]
pub fn any_override_set() -> bool {
    OVERRIDE_ENV_VARS.iter().any(|var| read(var).is_some())
}

/// The single point in this crate that touches the process environment.
fn read(var: &str) -> Option<OsString> {
    std::env::var_os(var)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every assertion below drives the pure halves, so the whole module is
    /// safe under the parallel test runner: no `set_var`, no lock, no restore
    /// guard, and nothing another test could observe.
    fn os(value: &str) -> Option<OsString> {
        Some(OsString::from(value))
    }

    #[test]
    fn home_falls_back_to_the_windows_spelling() {
        assert_eq!(
            resolve_home_dir(os("/home/dev"), os("C:\\Users\\dev")),
            Some(PathBuf::from("/home/dev")),
            "HOME wins when both are set"
        );
        assert_eq!(
            resolve_home_dir(None, os("C:\\Users\\dev")),
            Some(PathBuf::from("C:\\Users\\dev")),
            "USERPROFILE stands in when HOME is absent"
        );
        assert_eq!(resolve_home_dir(None, None), None, "neither is not a path");
    }

    #[test]
    fn stella_home_is_dot_stella_under_the_user_home() {
        assert_eq!(
            resolve_stella_home(None, Some(PathBuf::from("/home/dev"))),
            Some(PathBuf::from("/home/dev/.stella")),
            "no platform data-dir guessing on any OS"
        );
        assert_eq!(
            resolve_stella_home(os("/srv/stella"), Some(PathBuf::from("/home/dev"))),
            Some(PathBuf::from("/srv/stella")),
            "STELLA_HOME moves the whole home"
        );
        assert_eq!(
            resolve_stella_home(None, None),
            None,
            "no home directory is discoverable"
        );
    }

    #[test]
    fn the_narrow_data_dir_override_wins_over_the_whole_home() {
        let home = Some(PathBuf::from("/srv/stella"));
        assert_eq!(
            resolve_data_dir(os("/var/lib/stella"), home.clone()),
            PathBuf::from("/var/lib/stella"),
            "STELLA_DATA_DIR is narrower than STELLA_HOME and takes precedence"
        );
        assert_eq!(
            resolve_data_dir(None, home),
            PathBuf::from("/srv/stella"),
            "otherwise the data dir IS the stella home"
        );
        assert_eq!(
            resolve_data_dir(None, None),
            PathBuf::from("."),
            "a homeless machine still gets a usable path"
        );
    }

    /// Witness for #1139's stated risk: `stella-store` and `stella-observatory`
    /// resolved this independently, so the two could answer differently for
    /// the same environment. They now share this crate, and the precedence
    /// order they must agree on is pinned here rather than in a comment
    /// asking each copy to please stay equal.
    #[test]
    fn precedence_is_data_dir_then_stella_home_then_user_home() {
        let home = resolve_home_dir(os("/home/dev"), None);
        let resolved = |data: Option<OsString>, stella: Option<OsString>| {
            resolve_data_dir(data, resolve_stella_home(stella, home.clone()))
        };
        assert_eq!(
            resolved(os("/data"), os("/stella")),
            PathBuf::from("/data"),
            "STELLA_DATA_DIR"
        );
        assert_eq!(
            resolved(None, os("/stella")),
            PathBuf::from("/stella"),
            "then STELLA_HOME"
        );
        assert_eq!(
            resolved(None, None),
            PathBuf::from("/home/dev/.stella"),
            "then ~/.stella"
        );
    }

    #[test]
    fn the_override_list_names_every_redirecting_variable() {
        assert!(
            OVERRIDE_ENV_VARS.contains(&STELLA_HOME_ENV)
                && OVERRIDE_ENV_VARS.contains(&STELLA_DATA_DIR_ENV)
                && OVERRIDE_ENV_VARS.contains(&STELLA_CONFIG_DIR_ENV),
            "a migration guard reading a stale subset would move data out from \
             under whichever override it forgot"
        );
    }
}
