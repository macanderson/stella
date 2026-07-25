//! Project-scoped `.env` loading.
//!
//! Stella honours the dotenv files a project keeps next to its source so
//! credentials (and any other configuration) follow the directory you run
//! `stella` in — no shell hook required. This is what lets a user switch
//! keys between projects just by `cd`-ing: each project's `.env.local`
//! carries its own provider keys, and Stella reads them itself instead of
//! depending on the shell having exported them.
//!
//! The rules, in one place:
//!
//! - **Loaded**, most-specific first: `.env.<mode>.local` (e.g.
//!   `.env.production.local`), then `.env.local`, then `.env`.
//! - **Never loaded**: template files a repo commits for humans to copy —
//!   `.env.example`, `.env.sample`, `.env.<mode>.example`, `.env.dist` — and
//!   any `.env.<mode>` that is *not* `*.local` (committed, non-secret
//!   defaults we deliberately stay out of). This is the `!.env.example`
//!   guarantee.
//! - **The live shell always wins.** A variable already present in the
//!   process environment is never overwritten by a file, so
//!   `OPENROUTER_API_KEY=… stella …` and an exported shell value both take
//!   precedence over anything on disk. Between files, the more specific file
//!   is applied first and application never overwrites, so it wins over the
//!   less specific. (Consequence worth knowing: a value still *exported* in
//!   your shell shadows a project file — unset it if you mean to switch.)
//! - **Never applied**: a small deny-list of loader, interpreter and VCS
//!   command-execution names ([`is_denied_env_name`]) — `LD_*`, `DYLD_*`,
//!   `BASH_ENV`, `NODE_OPTIONS`, `GIT_SSH*`, `RUSTC_WRAPPER`, `PATH`, … A
//!   cloned repository's dotenv file is untrusted input, and those names
//!   redirect which program runs rather than configure Stella, so applying
//!   one would turn `git clone && stella` into code execution — the same
//!   threat the `.stella/mcp.toml` trust gate closes (`agent.rs`
//!   `load_mcp_plan`). Refusals are recorded on [`Loaded::refused`] and shown
//!   by `stella config`; the rest of the file still loads.
//!
//! Loading is confined to the current project: the search walks up from the
//! working directory to the nearest ancestor that actually contains a
//! matching file, but never crosses out of the enclosing git repository and
//! never treats the home directory (or above) as a project scope. Only that
//! nearest directory's files are read — this is direnv-style *nearest scope
//! wins*, not cross-directory layering, so a monorepo's per-package
//! `.env.local` does not also pull the repo-root `.env`.
//!
//! Set `STELLA_NO_ENV_FILE=1` to disable the whole mechanism.

use std::collections::{BTreeMap, HashSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use colored::Colorize;

use crate::OutputFormat;

/// Environment variable names a project dotenv file may never set, as exact
/// names. The wildcard shapes live in [`DENIED_ENV_PREFIXES`] and
/// [`DENIED_ENV_PREFIX_SUFFIX`]; [`is_denied_env_name`] is the whole policy.
///
/// `pub(crate)` because `enterprise_telemetry::StartupAuthoritySnapshot` also
/// snapshots these names across dotenv loading — one list, policed twice,
/// rather than two lists that can drift apart.
pub(crate) const DENIED_ENV_NAMES: &[&str] = &[
    // Shell entry points: a value here runs code in every shell Stella (or a
    // tool it spawns) starts.
    "BASH_ENV",
    "ENV",
    "SHELLOPTS",
    "IFS",
    // Which binary a name resolves to at all.
    "PATH",
    // Tool configuration that names a command to execute.
    "RIPGREP_CONFIG_PATH",
    "GIT_EXTERNAL_DIFF",
    "GIT_PAGER",
    "GIT_EDITOR",
    "GIT_ASKPASS",
    "GIT_PROXY_COMMAND",
    "GIT_TEMPLATE_DIR",
    "GIT_ATTR_NOSYSTEM",
    // Interpreter pre-load / module-resolution hooks.
    "NODE_OPTIONS",
    "NODE_REPL_EXTERNAL_MODULE",
    "PYTHONSTARTUP",
    "PYTHONPATH",
    "PERL5OPT",
    "RUBYOPT",
    "RUSTC_WRAPPER",
];

/// Denied name prefixes: dynamic-loader injection (`LD_PRELOAD`,
/// `DYLD_INSERT_LIBRARIES`, …), git's ssh program (`GIT_SSH`,
/// `GIT_SSH_COMMAND`) and git's config redirection (`GIT_CONFIG_GLOBAL`,
/// `GIT_CONFIG_COUNT`, …, which can inject any of the exec-bearing config
/// keys).
const DENIED_ENV_PREFIXES: &[&str] = &["LD_", "DYLD_", "GIT_SSH", "GIT_CONFIG_"];

/// Denied `<prefix>…<suffix>` shapes: cargo's per-target runner
/// (`CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER`) names the program cargo
/// executes for a built binary.
const DENIED_ENV_PREFIX_SUFFIX: &[(&str, &str)] = &[("CARGO_", "_RUNNER")];

/// Whether a project dotenv file is forbidden from setting `name`.
///
/// These names decide *which program runs*, not how Stella behaves, so a
/// cloned repository naming one is asking for code execution rather than for
/// configuration — refuse it and let the rest of the file load (a deny-list,
/// not an allow-list, because the whole point of the feature is that a project
/// may carry arbitrary credentials).
///
/// Matching is ASCII case-insensitive, for parity with
/// `stella_tools::exec::is_sensitive_env_name` and with Windows environment
/// semantics, where `Path` and `PATH` are one variable.
pub(crate) fn is_denied_env_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    DENIED_ENV_NAMES.contains(&upper.as_str())
        || DENIED_ENV_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix))
        || DENIED_ENV_PREFIX_SUFFIX.iter().any(|(prefix, suffix)| {
            upper.len() >= prefix.len() + suffix.len()
                && upper.starts_with(prefix)
                && upper.ends_with(suffix)
        })
}

/// What a load pass applied, for an optional diagnostic line. `files` are the
/// dotenv files that actually contributed at least one variable (most-specific
/// first); `names` are the distinct variable names newly set from them — never
/// their values.
#[derive(Debug, Default)]
pub struct Loaded {
    pub files: Vec<PathBuf>,
    pub names: Vec<String>,
    /// Which specific file each loaded variable name came from — lets a
    /// display surface (`stella config`) attribute e.g. `OPENROUTER_API_KEY`
    /// to `.env.local` by name, rather than only "loaded from a dotenv file"
    /// in aggregate.
    pub name_files: BTreeMap<String, PathBuf>,
    /// Names a dotenv file asked for that [`is_denied_env_name`] refused,
    /// mapped to the file that named them, so a user can find the offending
    /// file. Refused names are never applied to the process environment.
    pub refused: BTreeMap<String, PathBuf>,
}

impl Loaded {
    fn is_empty(&self) -> bool {
        self.names.is_empty() && self.refused.is_empty()
    }

    /// A value-free rendering of what was refused and where it came from —
    /// `LD_PRELOAD (.env.local), NODE_OPTIONS (.env)`. `None` when a project's
    /// dotenv files named nothing on the deny-list.
    pub fn refused_summary(&self) -> Option<String> {
        if self.refused.is_empty() {
            return None;
        }
        Some(
            self.refused
                .iter()
                .map(|(name, path)| format!("{name} ({})", file_name_of(path)))
                .collect::<Vec<_>>()
                .join(", "),
        )
    }

    /// The dotenv file that contributed `name`, if any — `None` when `name`
    /// resolves some other way (a real shell export, a CLI flag, a
    /// credentials store, …) or wasn't loaded from a project `.env*` file at
    /// all.
    pub fn file_for(&self, name: &str) -> Option<&Path> {
        self.name_files.get(name).map(PathBuf::as_path)
    }
}

/// Load project `.env*` files into the process environment, unless
/// `STELLA_NO_ENV_FILE` is set to a truthy value. Silent; call [`announce`]
/// afterward to surface what happened.
///
/// Must run during single-threaded startup (before the tokio runtime or any
/// worker threads exist), since it mutates the process environment.
pub fn maybe_load() -> Loaded {
    let disabled =
        std::env::var_os("STELLA_NO_ENV_FILE").is_some_and(|v| !v.is_empty() && v != "0");
    if disabled {
        return Loaded::default();
    }
    let Ok(cwd) = std::env::current_dir() else {
        return Loaded::default();
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let plan = plan_from(&cwd, home.as_deref(), |k| std::env::var_os(k).is_some());

    let mut names = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    let mut name_files = BTreeMap::new();
    for (key, value, path) in plan.assignments {
        // SAFETY: called only from `main` during single-threaded process
        // startup, before the tokio runtime or any worker threads exist — no
        // concurrent `getenv`/`setenv` can race this write.
        unsafe { std::env::set_var(&key, &value) };
        name_files.insert(key.clone(), path.clone());
        names.push(key);
        if !files.contains(&path) {
            files.push(path);
        }
    }
    Loaded {
        files,
        names,
        name_files,
        refused: plan.refused,
    }
}

/// What a load pass would do: the ordered assignments to apply, and the names
/// the deny-list refused (each mapped to the file that named it).
#[derive(Debug, Default)]
struct Plan {
    assignments: Vec<(String, String, PathBuf)>,
    refused: BTreeMap<String, PathBuf>,
}

/// The [`Plan`] a load would carry out, given a predicate for "this name is
/// already set" (the live environment, in production). Pure over the
/// filesystem + predicate — no process-environment mutation — so the
/// precedence, shell-wins and deny-list rules are unit-testable without env
/// races.
fn plan_from(start: &Path, home: Option<&Path>, is_set: impl Fn(&str) -> bool) -> Plan {
    let Some(base) = find_base(start, home) else {
        return Plan::default();
    };
    let files = collect_files(&base);
    plan_assignments(&files, is_set)
}

/// Walk up from `start` to the nearest ancestor that contains a loadable
/// dotenv file. Never returns the home directory (or above), and never crosses
/// above the enclosing git repository root — env stays scoped to one project.
fn find_base(start: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        // The home directory (and anything above it) is never a project scope:
        // we don't want a stray `~/.env` to leak into every session.
        if Some(dir) == home {
            return None;
        }
        if dir_has_env_file(dir) {
            return Some(dir.to_path_buf());
        }
        // `.git` (a dir in a normal clone, a file in a worktree) marks the repo
        // root — checked above, so stop here rather than leaking a parent
        // project's env across the boundary.
        if dir.join(".git").exists() {
            return None;
        }
        dir = dir.parent()?;
    }
}

fn dir_has_env_file(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str()
            && classify(name).is_some()
            && entry.path().is_file()
        {
            return true;
        }
    }
    false
}

/// Every loadable dotenv file in `dir`, ordered most-specific first (highest
/// precedence rank first), with a stable alphabetical tiebreak.
fn collect_files(dir: &Path) -> Vec<(u8, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(u8, String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if let Some(rank) = classify(&name) {
            let path = entry.path();
            if path.is_file() {
                files.push((rank, name, path));
            }
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    files
        .into_iter()
        .map(|(rank, _name, path)| (rank, path))
        .collect()
}

/// Precedence rank for a dotenv filename, or `None` if Stella must not load it.
/// Higher rank wins: `.env` = 0, `.env.local` = 1, `.env.<mode>.local` = 2.
/// Every other shape — templates and non-`.local` mode files — is `None`.
fn classify(name: &str) -> Option<u8> {
    if name == ".env" {
        return Some(0);
    }
    if name == ".env.local" {
        return Some(1);
    }
    // `.env.<mode>.local`
    let mode = name.strip_prefix(".env.")?.strip_suffix(".local")?;
    if mode.is_empty() {
        return None;
    }
    // Defensive: never treat a template as a secret, even if someone names one
    // `.env.example.local`.
    if matches!(
        mode.to_ascii_lowercase().as_str(),
        "example" | "sample" | "template" | "dist" | "defaults"
    ) {
        return None;
    }
    Some(2)
}

/// Resolve the files to the ordered assignments to apply. A name is taken from
/// the first (most-specific) file that defines it and that neither the
/// environment (`is_set`) nor an earlier file has already claimed — so the live
/// shell wins over every file, and a more specific file wins over a less
/// specific one. A name [`is_denied_env_name`] rejects is never applied at all,
/// and is recorded in [`Plan::refused`] instead.
fn plan_assignments(files: &[(u8, PathBuf)], is_set: impl Fn(&str) -> bool) -> Plan {
    let mut claimed: HashSet<String> = HashSet::new();
    let mut plan = Plan::default();
    for (_rank, path) in files {
        // dotenvy's parser (quotes, escapes, multi-line values, `export`
        // prefixes, `#` comments) as a non-mutating iterator — we apply our own
        // precedence instead of letting it touch the environment.
        let Ok(iter) = dotenvy::from_path_iter(path) else {
            continue;
        };
        for item in iter {
            let Ok((key, value)) = item else {
                continue; // a malformed line must not abort the whole file
            };
            // Deliberately ahead of the `is_set` filter: `PATH` and `IFS` are
            // exported in every real shell, so testing "already set" first
            // would drop them into the silent skip path and report nothing —
            // a refusal the user never sees is not a refusal they can act on.
            if is_denied_env_name(&key) {
                plan.refused.entry(key).or_insert_with(|| path.clone());
                continue;
            }
            if is_set(&key) || claimed.contains(&key) {
                continue;
            }
            claimed.insert(key.clone());
            plan.assignments.push((key, value, path.clone()));
        }
    }
    plan
}

/// Emit a concise, value-free confirmation of what [`maybe_load`] applied —
/// only when `STELLA_ENV_DEBUG` is set, stderr is a terminal, and the output
/// format is human (never in `json`/`stream-json`, which must stay clean).
pub fn announce(loaded: &Loaded, format: OutputFormat) {
    if loaded.is_empty() {
        return;
    }
    let debug = std::env::var_os("STELLA_ENV_DEBUG").is_some_and(|v| !v.is_empty() && v != "0");
    if !debug
        || matches!(format, OutputFormat::Json | OutputFormat::StreamJson)
        || !std::io::stderr().is_terminal()
    {
        return;
    }
    if !loaded.names.is_empty() {
        let file_list = loaded
            .files
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
            .collect::<Vec<_>>()
            .join(", ");
        eprintln!(
            "{} {}",
            "env:".dimmed(),
            format!("loaded {} from {file_list}", loaded.names.join(", ")).dimmed(),
        );
    }
    if let Some(refused) = loaded.refused_summary() {
        eprintln!(
            "{} {}",
            "env:".dimmed(),
            format!("refused (never applied): {refused}").dimmed(),
        );
    }
}

/// A dotenv file's bare name for display (`.env.local`), degrading to the full
/// path when it has no valid-UTF-8 file name.
fn file_name_of(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, name: &str, body: &str) {
        fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn classify_loads_env_local_and_mode_local_but_not_templates() {
        assert_eq!(classify(".env"), Some(0));
        assert_eq!(classify(".env.local"), Some(1));
        assert_eq!(classify(".env.production.local"), Some(2));
        assert_eq!(classify(".env.development.local"), Some(2));
        // Templates and non-`.local` mode files are never loaded.
        assert_eq!(classify(".env.example"), None);
        assert_eq!(classify(".env.sample"), None);
        assert_eq!(classify(".env.local.example"), None);
        assert_eq!(classify(".env.example.local"), None);
        assert_eq!(classify(".env.production"), None); // committed default, not a secret
        assert_eq!(classify(".env.dist"), None);
        assert_eq!(classify(".environment"), None);
        assert_eq!(classify("env"), None);
    }

    #[test]
    fn precedence_is_mode_local_over_local_over_env_and_shell_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        write(d, ".env", "KEY=from_env\nBASE_ONLY=base\n");
        write(d, ".env.local", "KEY=from_local\nLOCAL_ONLY=local\n");
        write(d, ".env.production.local", "KEY=from_mode_local\n");
        // `.env.example` must be ignored entirely.
        write(
            d,
            ".env.example",
            "KEY=from_example\nEXAMPLE_ONLY=example\n",
        );

        let files = collect_files(d);
        // A name already in the shell is never overwritten.
        let shell: HashSet<String> = ["BASE_ONLY".to_string()].into_iter().collect();
        let plan = plan_assignments(&files, |k| shell.contains(k));

        let map: std::collections::HashMap<_, _> = plan
            .assignments
            .iter()
            .map(|(k, v, _)| (k.clone(), v.clone()))
            .collect();

        assert_eq!(map.get("KEY").map(String::as_str), Some("from_mode_local"));
        assert_eq!(map.get("LOCAL_ONLY").map(String::as_str), Some("local"));
        // Shadowed by the shell → not planned.
        assert!(!map.contains_key("BASE_ONLY"));
        // Template file never contributes.
        assert!(!map.contains_key("EXAMPLE_ONLY"));
    }

    #[test]
    fn parser_handles_quotes_export_comments_and_multiline() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        write(
            d,
            ".env",
            "# a comment\n\
             export EXPORTED=yes\n\
             QUOTED=\"has spaces\"\n\
             SINGLE='literal $NOPE'\n\
             INLINE=value # trailing\n\
             MULTI=\"line1\nline2\"\n",
        );
        let files = collect_files(d);
        let plan = plan_assignments(&files, |_| false);
        let map: std::collections::HashMap<_, _> = plan
            .assignments
            .iter()
            .map(|(k, v, _)| (k.clone(), v.clone()))
            .collect();

        assert_eq!(map.get("EXPORTED").map(String::as_str), Some("yes"));
        assert_eq!(map.get("QUOTED").map(String::as_str), Some("has spaces"));
        assert_eq!(map.get("SINGLE").map(String::as_str), Some("literal $NOPE"));
        assert_eq!(map.get("INLINE").map(String::as_str), Some("value"));
        assert_eq!(map.get("MULTI").map(String::as_str), Some("line1\nline2"));
    }

    #[test]
    fn find_base_prefers_nearest_and_stops_at_repo_and_home() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let home = root.join("home");
        let repo = home.join("proj");
        let sub = repo.join("packages").join("web");
        fs::create_dir_all(&sub).unwrap();
        fs::create_dir_all(repo.join(".git")).unwrap();

        // No env files anywhere yet: walking up from `sub` stops at the repo
        // root (which has `.git`) and finds nothing — never reaching `home`.
        assert_eq!(find_base(&sub, Some(&home)), None);

        // Repo-root env is found from a subdir.
        write(&repo, ".env", "K=1\n");
        assert_eq!(find_base(&sub, Some(&home)), Some(repo.clone()));

        // A nearer scope wins over the repo root.
        write(&sub, ".env.local", "K=2\n");
        assert_eq!(find_base(&sub, Some(&home)), Some(sub.clone()));
    }

    #[test]
    fn home_directory_is_never_a_project_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write(home, ".env", "HOME_LEVEL=nope\n");
        // Running *in* $HOME must not load `~/.env`.
        assert_eq!(find_base(home, Some(home)), None);
    }

    // The deny-list: a cloned repo's dotenv file must not be able to redirect
    // which program Stella (or anything it spawns) executes.

    #[test]
    fn loader_interpreter_and_vcs_exec_names_are_refused_while_the_rest_still_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        write(
            d,
            ".env",
            "LD_PRELOAD=/tmp/evil.so\n\
             DYLD_INSERT_LIBRARIES=/tmp/evil.dylib\n\
             BASH_ENV=/tmp/evil.sh\n\
             GIT_SSH_COMMAND=/tmp/evil\n\
             GIT_CONFIG_COUNT=1\n\
             NODE_OPTIONS=\"--require /tmp/evil.js\"\n\
             PYTHONPATH=/tmp/evil\n\
             RUSTC_WRAPPER=/tmp/evil\n\
             CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER=/tmp/evil\n\
             PATH=/tmp/evil/bin\n\
             ld_preload=/tmp/evil-lowercase.so\n\
             OPENROUTER_API_KEY=sk-project\n",
        );

        let files = collect_files(d);
        // `PATH` is exported in every real shell — the deny check must still
        // report it, so it cannot hide behind the "already set" filter.
        let plan = plan_assignments(&files, |k| k == "PATH");

        let applied: Vec<&str> = plan
            .assignments
            .iter()
            .map(|(k, _, _)| k.as_str())
            .collect();
        assert_eq!(
            applied,
            ["OPENROUTER_API_KEY"],
            "only the credential may be applied"
        );

        for name in [
            "LD_PRELOAD",
            "DYLD_INSERT_LIBRARIES",
            "BASH_ENV",
            "GIT_SSH_COMMAND",
            "GIT_CONFIG_COUNT",
            "NODE_OPTIONS",
            "PYTHONPATH",
            "RUSTC_WRAPPER",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER",
            "PATH",
            "ld_preload",
        ] {
            assert!(
                plan.refused.contains_key(name),
                "{name} must be reported as refused"
            );
        }

        let loaded = Loaded {
            refused: plan.refused,
            ..Default::default()
        };
        let summary = loaded.refused_summary().unwrap();
        // Names and the offending file, never values.
        assert!(summary.contains("LD_PRELOAD (.env)"), "{summary}");
        assert!(!summary.contains("evil.so"), "{summary}");
    }

    #[test]
    fn deny_list_matches_exact_prefix_and_wrapped_shapes_but_not_ordinary_names() {
        for denied in [
            "PATH",
            "path",
            "IFS",
            "ENV",
            "SHELLOPTS",
            "LD_LIBRARY_PATH",
            "DYLD_LIBRARY_PATH",
            "GIT_SSH",
            "GIT_SSH_COMMAND",
            "GIT_CONFIG_GLOBAL",
            "GIT_ASKPASS",
            "RIPGREP_CONFIG_PATH",
            "PERL5OPT",
            "RUBYOPT",
            "NODE_REPL_EXTERNAL_MODULE",
            "PYTHONSTARTUP",
            "CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER",
        ] {
            assert!(is_denied_env_name(denied), "{denied} must be denied");
        }
        for allowed in [
            "OPENROUTER_API_KEY",
            "ANTHROPIC_API_KEY",
            "DATABASE_URL",
            "STELLA_MODEL",
            "LDAP_URL", // `LD_` is a prefix, `LDAP` is not
            "GIT_AUTHOR_NAME",
            "CARGO_TERM_COLOR",
            "NODE_ENV",
        ] {
            assert!(!is_denied_env_name(allowed), "{allowed} must load");
        }
    }

    // Loaded::file_for (stella config's "which file, which name")

    #[test]
    fn file_for_attributes_a_loaded_name_to_its_source_file_and_nothing_else() {
        let local = PathBuf::from("/proj/.env.local");
        let loaded = Loaded {
            files: vec![local.clone()],
            names: vec!["OPENROUTER_API_KEY".to_string()],
            name_files: [("OPENROUTER_API_KEY".to_string(), local.clone())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        assert_eq!(loaded.file_for("OPENROUTER_API_KEY"), Some(local.as_path()));
        // A name that was never loaded from a dotenv file (a real shell
        // export, say) must not be attributed to one.
        assert_eq!(loaded.file_for("ANTHROPIC_API_KEY"), None);
    }
}
