//! `~/.stella` — the single user-level stella home, on every platform.
//!
//! Everything user-global (the `usage.db` telemetry hub, `catalog.db`, the
//! session registry, notifications, the enterprise spool, settings, skills,
//! agents, rules, tools, credentials) lives directly under `~/.stella`, the
//! same way Claude Code uses `~/.claude`. No platform data-dir guessing:
//! macOS, Linux, and Windows all resolve to the home directory.
//!
//! Overrides: `STELLA_HOME` moves the whole home; the narrower
//! `STELLA_DATA_DIR` moves the data tier alone and wins over it where it
//! always did (tests, sandboxes, org-managed layouts). Those two are the
//! entire list — see [`OVERRIDE_ENV_VARS`], which is exact in both
//! directions on purpose.
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

// Invariant #5: library code never panics on runtime data. This crate is at
// zero production `unwrap`/`expect`, and this keeps it there — `make lint` runs
// clippy with `-D warnings`, so a new one fails the gate instead of arriving
// unremarked. `not(test)` scopes the lint exactly as the invariant does: the
// rule is about runtime data, and `unwrap` in a test is fine.
#![cfg_attr(not(test), warn(clippy::unwrap_used, clippy::expect_used))]

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

/// The directory name stella keeps under the user's home.
const DOT_STELLA: &str = ".stella";

/// The self-driving loop's state directory under the stella home.
const SELF_DRIVING_DIR: &str = "self-driving";

/// Installed plugins, in both tiers: `~/.stella/plugins` and
/// `<workspace>/.stella/plugins`.
const PLUGINS_DIR: &str = "plugins";

/// What [`SELF_DRIVING_DIR`] was called before #1757 renamed the loop. A
/// spelling on disk, not a name — see [`legacy_self_driving_roots`].
const LEGACY_SELF_DRIVING_DIR: &str = "fullauto";

/// The original home, before the state moved under the stella home.
const LEGACY_DOT_SELF_DRIVING_DIR: &str = ".fullauto";

/// Every override that can point resolution away from the `~/.stella`
/// defaults, in one list so a caller asking "is any of this redirected?"
/// cannot answer with a stale subset.
///
/// Exact in **both** directions, and both directions are required. A redirecting
/// variable missing from the list lets [`any_override_set`] answer `false`
/// while resolution points elsewhere, which is how real user data gets moved
/// out from under an override. A name on the list that redirects *nothing*
/// is the opposite failure and is not merely cosmetic: it makes
/// [`any_override_set`] answer `true` for a process whose paths all resolve
/// to the defaults, silently declining work that should have run.
/// `STELLA_CONFIG_DIR` sat here for its whole life without a resolver
/// reading it and did exactly that to the legacy-layout migration (#2442).
/// The soundness half is witnessed by
/// `every_override_on_the_list_actually_redirects_a_resolved_path`.
pub const OVERRIDE_ENV_VARS: [&str; 2] = [STELLA_HOME_ENV, STELLA_DATA_DIR_ENV];

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
/// The current-directory fallback is deliberate and required: a machine
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

/// Where the user's `systemd --user` units live: `$XDG_CONFIG_HOME`, else
/// `~/.config`, per the XDG base directory spec.
pub const XDG_CONFIG_HOME_ENV: &str = "XDG_CONFIG_HOME";

/// The per-user launchd agent directory on macOS: `~/Library/LaunchAgents`.
///
/// Not a stella path — launchd only reads this one location, so no stella
/// override can move it — but it is anchored on the user home, and this crate
/// is the one place that resolves the home (see the module docs on reading
/// the environment). `None` when no home directory is discoverable, which the
/// caller must treat as "cannot install", never as a fallback location.
#[must_use]
pub fn launch_agents_dir() -> Option<PathBuf> {
    resolve_launch_agents_dir(home_dir())
}

/// [`launch_agents_dir`] over anchors the caller supplies.
#[must_use]
pub fn resolve_launch_agents_dir(home: Option<PathBuf>) -> Option<PathBuf> {
    home.map(|home| home.join("Library").join("LaunchAgents"))
}

/// The per-user systemd unit directory on Linux:
/// `$XDG_CONFIG_HOME/systemd/user`, else `~/.config/systemd/user`.
///
/// Same contract as [`launch_agents_dir`]: this is where `systemd --user`
/// looks, stella overrides cannot move it, and `None` means "cannot install".
#[must_use]
pub fn systemd_user_unit_dir() -> Option<PathBuf> {
    resolve_systemd_user_unit_dir(read(XDG_CONFIG_HOME_ENV), home_dir())
}

/// [`systemd_user_unit_dir`] over anchors the caller supplies.
#[must_use]
pub fn resolve_systemd_user_unit_dir(
    xdg_config_home: Option<OsString>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    let config = match xdg_config_home {
        Some(dir) => PathBuf::from(dir),
        None => home?.join(".config"),
    };
    Some(config.join("systemd").join("user"))
}

/// The self-driving loop's state root: `<stella home>/self-driving`, one
/// directory per repository slug beneath it.
///
/// Resolved here rather than in either crate that uses it, because two
/// processes share it and disagree destructively: `stella-cli` is the single
/// writer and `stella-observatory` is a read-only reader, so a root spelled
/// differently in one of them shows an operator an empty dashboard for a
/// machine that has a full ledger.
#[must_use]
pub fn self_driving_root() -> Option<PathBuf> {
    resolve_self_driving_root(stella_home())
}

/// [`self_driving_root`] over anchors the caller supplies.
#[must_use]
pub fn resolve_self_driving_root(stella_home: Option<PathBuf>) -> Option<PathBuf> {
    stella_home.map(|home| home.join(SELF_DRIVING_DIR))
}

/// The environment variable that moves a workspace's **state** — and only its
/// state — somewhere other than the directory the process is standing in.
///
/// # Why this exists, and why it is not in [`OVERRIDE_ENV_VARS`]
///
/// That list redirects the *user tier* (`~/.stella`). This one redirects the
/// *workspace tier* (`<workspace>/.stella`), which is a different question with
/// a different answer, so conflating them would let a user-home override
/// silently relocate a repository's memories.
///
/// It exists because an agent's **code** and an agent's **learning** do not
/// belong in the same place when the code is checked out somewhere disposable.
/// A turn that runs in a throwaway git worktree writes its reflections, its
/// promoted skills, its telemetry and its code graph into `<worktree>/.stella`,
/// and then the worktree is removed — so the session's entire record of what it
/// learned is deleted by the cleanup that follows every unit of work.
///
/// That is not hypothetical. A self-driving run against `oxagen-platform`
/// executed 22 turns across 16 merged pull requests and finished with **no
/// reflections file and no memories directory at all**: every turn had written
/// them faithfully into a directory built to be destroyed.
///
/// Setting this to the real repository root makes the worktree's code
/// disposable and its learning durable, which is the split that was always
/// intended.
pub const WORKSPACE_STATE_ROOT_ENV: &str = "STELLA_WORKSPACE_STATE_ROOT";

/// Where a workspace's `.stella` lives, honouring [`WORKSPACE_STATE_ROOT_ENV`].
#[must_use]
pub fn workspace_state_root(workspace_root: &std::path::Path) -> PathBuf {
    resolve_workspace_state_root(std::env::var_os(WORKSPACE_STATE_ROOT_ENV), workspace_root)
}

/// [`workspace_state_root`] over the value the caller supplies.
///
/// An unset or blank override resolves to `workspace_root` unchanged, so the
/// ordinary case — a session standing in the repository it is working on — is
/// exactly what it was before this existed. A **relative** override is refused
/// the same way, because the whole point is to name a directory that outlives
/// the process's own working directory, and a relative path does not.
#[must_use]
pub fn resolve_workspace_state_root(
    override_var: Option<OsString>,
    workspace_root: &std::path::Path,
) -> PathBuf {
    let candidate = override_var
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty() && path.is_absolute());

    candidate.unwrap_or_else(|| workspace_root.to_path_buf())
}

/// Where user-scope plugins are installed: `<stella home>/plugins`, one
/// directory per plugin beneath it. `None` only when no home is discoverable,
/// which a caller must read as "there is no user tier here" — never as a
/// reason to fall back to the working directory, because that would install
/// third-party code into whatever repository happened to be open.
///
/// Nothing calls this wrapper yet. `stella-cli`'s plugin loader calls the
/// pure half, [`resolve_user_plugins_dir`], with its own redirectable root
/// instead. The expected first caller is the wrapper socket's other
/// drivers — `stella-serve` or an embedded host — which need the same
/// roster without depending on `stella-cli` to spell the path
/// (`doc:wrapper-socket` §6). If that never lands, this belongs back in
/// `stella-cli`.
#[must_use]
pub fn user_plugins_dir() -> Option<PathBuf> {
    resolve_user_plugins_dir(stella_home())
}

/// [`user_plugins_dir`] over the anchor the caller supplies.
#[must_use]
pub fn resolve_user_plugins_dir(stella_home: Option<PathBuf>) -> Option<PathBuf> {
    stella_home.map(|home| home.join(PLUGINS_DIR))
}

/// Where project-scope plugins are installed:
/// `<workspace root>/.stella/plugins`.
///
/// Total, and env-free even in the wrapper sense — a workspace root is an
/// argument, never something this crate discovers — so there is no
/// `project_plugins_dir()` half to pair with it. It lives here anyway, beside
/// the user tier, because the two directories are read as one roster and a
/// loader that spelled `.stella/plugins` itself is exactly the second
/// implementation this crate exists to prevent (#1139).
#[must_use]
pub fn resolve_project_plugins_dir(workspace_root: &std::path::Path) -> PathBuf {
    workspace_root.join(DOT_STELLA).join(PLUGINS_DIR)
}

/// Roots earlier builds wrote this state to, newest first.
///
/// The `fullauto` spelling is deliberate and must never be renamed with the
/// rest of the loop (#1757): these name bytes already on users' disks, not a
/// concept. Rewriting them points the migration at a directory that has never
/// existed, and losing the seen-set re-files every finding ever triaged.
///
/// 1. `<stella home>/fullauto` — before the loop was renamed `self-driving`.
/// 2. `~/.fullauto` — the original home, before the state moved under the
///    stella home.
///
/// The writer migrates the first of these that exists; the reader scans them
/// so a loop that has not been migrated yet is still visible.
#[must_use]
pub fn legacy_self_driving_roots() -> Vec<PathBuf> {
    resolve_legacy_self_driving_roots(stella_home(), home_dir())
}

/// [`legacy_self_driving_roots`] over anchors the caller supplies.
#[must_use]
pub fn resolve_legacy_self_driving_roots(
    stella_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Vec<PathBuf> {
    let mut roots = Vec::with_capacity(2);
    if let Some(home) = stella_home {
        roots.push(home.join(LEGACY_SELF_DRIVING_DIR));
    }
    if let Some(home) = home {
        roots.push(home.join(LEGACY_DOT_SELF_DRIVING_DIR));
    }
    roots
}

/// Whether any of [`OVERRIDE_ENV_VARS`] is set — i.e. whether resolution is
/// pointing somewhere other than the `~/.stella` defaults.
///
/// The one-time legacy-layout migration consults this: moving real user data
/// while an override redirects resolution would move it out from under
/// whatever set the override.
///
/// The `i.e.` is the whole contract, and it was false for as long as
/// `STELLA_CONFIG_DIR` was on the list (#2442): that name reached no
/// resolver, so a process that set it declined the migration while every
/// path it opened still resolved to the defaults. Keeping the list exact is
/// what makes this predicate mean what it says.
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

    /// A turn that runs in a throwaway git worktree must not take the
    /// session's learning with it when the worktree is removed.
    ///
    /// Twenty-two self-driving turns against a repository finished with no
    /// reflections, no memories and no promoted skills, because each one wrote
    /// them faithfully into `<worktree>/.stella/private` — a directory built to
    /// be destroyed. The override is what separates *where the code is* from
    /// *where the learning goes*.
    #[test]
    fn the_state_root_follows_the_override_not_the_working_directory() {
        let worktree = PathBuf::from("/tmp/throwaway-worktree");
        assert_eq!(
            resolve_workspace_state_root(os("/repo"), &worktree),
            PathBuf::from("/repo"),
            "an absolute override displaces the working directory"
        );
        assert_eq!(
            resolve_workspace_state_root(None, &worktree),
            worktree,
            "an ordinary session keeps state beside the code it is working on"
        );
    }

    /// The override names a durable home, so anything that cannot be one is
    /// declined rather than half-honoured.
    ///
    /// A relative path would resolve against the *worktree's* working
    /// directory, which is the exact failure this override exists to prevent —
    /// it would look configured and still be destroyed. An empty value is what
    /// an unset-but-exported variable looks like from a shell.
    #[test]
    fn an_unusable_override_falls_back_instead_of_resolving_somewhere_worse() {
        let worktree = PathBuf::from("/tmp/throwaway-worktree");
        for rejected in ["", "../repo", "relative/repo"] {
            assert_eq!(
                resolve_workspace_state_root(os(rejected), &worktree),
                worktree,
                "{rejected:?} is not a durable root and must not be honoured"
            );
        }
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

    /// Witness for #1587: the service installer's paths come from these
    /// resolvers, not from `HOME` reads scattered through the CLI.
    #[test]
    fn service_directories_anchor_on_the_home_the_caller_supplies() {
        let home = Some(PathBuf::from("/home/dev"));
        assert_eq!(
            resolve_launch_agents_dir(home.clone()),
            Some(PathBuf::from("/home/dev/Library/LaunchAgents")),
            "launchd reads exactly one per-user location"
        );
        assert_eq!(
            resolve_launch_agents_dir(None),
            None,
            "no home means cannot install, never a fallback location"
        );
        assert_eq!(
            resolve_systemd_user_unit_dir(None, home.clone()),
            Some(PathBuf::from("/home/dev/.config/systemd/user")),
            "the XDG default is ~/.config"
        );
        assert_eq!(
            resolve_systemd_user_unit_dir(os("/etc/xdg-alt"), home),
            Some(PathBuf::from("/etc/xdg-alt/systemd/user")),
            "XDG_CONFIG_HOME moves the config root"
        );
        assert_eq!(resolve_systemd_user_unit_dir(None, None), None);
    }

    #[test]
    fn the_self_driving_root_follows_the_stella_home() {
        assert_eq!(
            resolve_self_driving_root(Some(PathBuf::from("/home/dev/.stella"))),
            Some(PathBuf::from("/home/dev/.stella/self-driving"))
        );
        assert_eq!(resolve_self_driving_root(None), None);
    }

    /// The two plugin tiers a loader unions. The project tier is spelled from
    /// the same `.stella` constant every other project path uses, so a
    /// workspace whose directory name changed cannot leave one resolver
    /// pointing at the old spelling.
    #[test]
    fn the_plugin_tiers_are_the_stella_home_and_the_workspace_dot_stella() {
        assert_eq!(
            resolve_user_plugins_dir(Some(PathBuf::from("/home/dev/.stella"))),
            Some(PathBuf::from("/home/dev/.stella/plugins"))
        );
        assert_eq!(
            resolve_user_plugins_dir(None),
            None,
            "no home means there is no user tier — never a fallback into the \
             working directory, which would install third-party code into \
             whatever repository happened to be open"
        );
        assert_eq!(
            resolve_project_plugins_dir(std::path::Path::new("/ws")),
            PathBuf::from("/ws/.stella/plugins"),
            "the project tier is under the same .stella every other \
             per-workspace path lives in"
        );
    }

    #[test]
    fn the_legacy_roots_are_ordered_newest_first() {
        let roots = resolve_legacy_self_driving_roots(
            Some(PathBuf::from("/home/dev/.stella")),
            Some(PathBuf::from("/home/dev")),
        );
        assert_eq!(
            roots,
            vec![
                PathBuf::from("/home/dev/.stella/fullauto"),
                PathBuf::from("/home/dev/.fullauto"),
            ],
            "the writer migrates the FIRST root that exists, so the newest \
             layout must come first — reversed, a machine carrying both would \
             adopt the older ledger and strand the newer one"
        );
    }

    #[test]
    fn the_legacy_roots_keep_the_pre_rename_spelling() {
        let roots =
            resolve_legacy_self_driving_roots(Some(PathBuf::from("/s")), Some(PathBuf::from("/h")));
        assert!(
            roots.iter().all(|r| {
                let name = r.file_name().unwrap().to_string_lossy();
                name == "fullauto" || name == ".fullauto"
            }),
            "these name bytes already on users' disks, not a concept (#1757). \
             Renaming them with the rest of the loop points the migration at a \
             directory that has never existed and silently orphans a live \
             ledger; losing the seen-set re-files every finding ever triaged."
        );
    }

    #[test]
    fn a_machine_with_no_home_at_all_yields_no_legacy_roots() {
        assert!(resolve_legacy_self_driving_roots(None, None).is_empty());
    }

    #[test]
    fn the_current_root_is_never_also_a_legacy_root() {
        let stella = PathBuf::from("/home/dev/.stella");
        let current = resolve_self_driving_root(Some(stella.clone())).unwrap();
        assert!(
            !resolve_legacy_self_driving_roots(Some(stella), Some(PathBuf::from("/home/dev")))
                .contains(&current),
            "the reader unions these; an overlap would list every loop twice"
        );
    }

    #[test]
    fn the_override_list_names_every_redirecting_variable() {
        assert!(
            OVERRIDE_ENV_VARS.contains(&STELLA_HOME_ENV)
                && OVERRIDE_ENV_VARS.contains(&STELLA_DATA_DIR_ENV),
            "a migration guard reading a stale subset would move data out from \
             under whichever override it forgot"
        );
    }

    /// Witness for #2442, and the direction the completeness test above cannot
    /// see: a name on [`OVERRIDE_ENV_VARS`] must actually move a path some
    /// resolver returns.
    ///
    /// `STELLA_CONFIG_DIR` was declared, documented as narrowing
    /// `STELLA_HOME`, and listed there — but nothing ever read it to resolve a
    /// path. Setting it therefore moved no file the CLI opens while still
    /// making [`any_override_set`] answer `true`, which declined the
    /// legacy-layout migration for a process sitting squarely on the defaults.
    ///
    /// Each entry is paired here with the resolution it claims to redirect,
    /// driven through the pure halves. An entry with no pairing fails
    /// outright: the unmatched arm *is* the assertion, not a gap in the cases.
    #[test]
    fn every_override_on_the_list_actually_redirects_a_resolved_path() {
        const ELSEWHERE: &str = "/elsewhere";
        let home = resolve_home_dir(os("/home/dev"), None);
        let default_root = resolve_stella_home(None, home.clone());

        for var in OVERRIDE_ENV_VARS {
            let (tier, redirected, unset) = match var {
                // The whole home — the root itself, and every tier under it.
                STELLA_HOME_ENV => (
                    "the stella root",
                    resolve_stella_home(os(ELSEWHERE), home.clone()),
                    default_root.clone(),
                ),
                // The data tier alone, over a root that has not moved.
                STELLA_DATA_DIR_ENV => (
                    "the data dir",
                    Some(resolve_data_dir(os(ELSEWHERE), default_root.clone())),
                    Some(resolve_data_dir(None, default_root.clone())),
                ),
                unread => panic!(
                    "{unread} is listed as a redirecting override, but no \
                     resolver in this crate reads it. Wire it to one, or take \
                     it off OVERRIDE_ENV_VARS — a name on that list makes \
                     `any_override_set` answer true, so an inert one declines \
                     the legacy-layout migration for a process whose paths all \
                     resolve to the defaults (#2442)."
                ),
            };
            assert_eq!(
                redirected,
                Some(PathBuf::from(ELSEWHERE)),
                "{var} is on OVERRIDE_ENV_VARS, so it must move {tier}, which \
                 resolves to {unset:?} when it is unset"
            );
        }
    }
}
