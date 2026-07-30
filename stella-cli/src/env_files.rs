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
//! - **Never applied, whatever the file says**: names whose value is executed
//!   by something Stella later spawns — the dynamic loader (`LD_*`, `DYLD_*`),
//!   command lookup (`PATH`, `SHELL`), interpreter startup hooks
//!   (`NODE_OPTIONS`, `PYTHONSTARTUP`, `BASH_ENV`, …), the git/pager escapes
//!   (`GIT_SSH_COMMAND`, `LESSOPEN`, …), the config-file redirections that
//!   reach the same escapes indirectly (`GIT_CONFIG_*`, `RIPGREP_CONFIG_PATH`,
//!   `CARGO_TARGET_<TRIPLE>_RUNNER`), and the *roots* those config files hang
//!   off — `HOME`, `XDG_CONFIG_HOME`, `STELLA_HOME` and friends, which move
//!   `~/.gitconfig` and Stella's own trusted user scope somewhere the
//!   repository controls. A dotenv file is attacker controlled the moment you
//!   clone an unfamiliar repository, and applying any of these would make
//!   `git clone && stella` arbitrary code execution on the first subprocess
//!   (#553). Refused names are reported, never applied; if you genuinely want
//!   one, export it in your shell — which still wins.
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

use std::collections::HashSet;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use colored::Colorize;

use crate::OutputFormat;

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
    pub name_files: std::collections::BTreeMap<String, PathBuf>,
    /// Names present in a dotenv file that Stella refused to apply because they
    /// redirect a loader, interpreter, or spawned command (#553). Names only —
    /// the values are never read past the parser.
    pub refused: Vec<String>,
}

impl Loaded {
    fn is_empty(&self) -> bool {
        self.names.is_empty()
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
    let (plan, refused) = plan_from(&cwd, home.as_deref(), |k| std::env::var_os(k).is_some());

    let mut names = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    let mut name_files = std::collections::BTreeMap::new();
    for (key, value, path) in plan {
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
        refused,
    }
}

/// The ordered list of assignments a load would apply, given a predicate for
/// "this name is already set" (the live environment, in production). Pure over
/// the filesystem + predicate — no process-environment mutation — so the
/// precedence and shell-wins rules are unit-testable without env races.
fn plan_from(
    start: &Path,
    home: Option<&Path>,
    is_set: impl Fn(&str) -> bool,
) -> (Vec<(String, String, PathBuf)>, Vec<String>) {
    let Some(base) = find_base(start, home) else {
        return (Vec::new(), Vec::new());
    };
    let files = collect_files(&base);
    plan_assignments(&files, is_set)
}

/// Walk up from `start` to the nearest ancestor that contains a loadable
/// dotenv file. Never returns the home directory (or above), and never crosses
/// above the enclosing git repository root — env stays scoped to one project.
fn find_base(start: &Path, home: Option<&Path>) -> Option<PathBuf> {
    // `start` comes from `current_dir()`, which is `getcwd` — already free of
    // symlinks. `$HOME` is not: `/home` symlinked onto another volume is a
    // routine layout, and there the raw string compare below never matched, so
    // the "`~/.env` never leaks into every session" guarantee silently did not
    // hold for those users. Resolve it once so the two are comparable; an
    // unresolvable `$HOME` (deleted, or a bare name) falls back to the literal,
    // which is exactly the previous behaviour.
    let home = home.map(|home| std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf()));
    let mut dir = start;
    loop {
        // The home directory (and anything above it) is never a project scope:
        // we don't want a stray `~/.env` to leak into every session.
        if home.as_deref() == Some(dir) {
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

/// Variable names a project dotenv file may never set, because the value is
/// consumed as *code* by something Stella later spawns. A `.env` is attacker
/// controlled the moment you `git clone` an unfamiliar repository, and these
/// names turn "open this project" into arbitrary execution on the first
/// subprocess — no tool call, no approval prompt, no bash opt-in (#553).
///
/// Deliberately a deny-list of execution vectors rather than a trust gate: even
/// in a project you trust, a dotenv file is the wrong place to redirect the
/// dynamic loader. The escape hatch already exists and is safer — the live
/// shell always wins over a file, so `LD_PRELOAD=… stella …` still works when a
/// human means it.
const DENIED_EXACT: &[&str] = &[
    // Where a subprocess is looked up, and which shell/pager/editor runs it.
    "PATH",
    "SHELL",
    "IFS",
    "BASH_ENV",
    "ENV",
    "ZDOTDIR",
    "EDITOR",
    "VISUAL",
    "PAGER",
    "MANPAGER",
    // `bash` reads SHELLOPTS at startup to turn shell options on for every
    // shell Stella (or a tool it spawns) starts.
    "SHELLOPTS",
    // `less` runs LESSOPEN as a command; `git` shells out through these.
    // (`GIT_SSH*` is covered by the prefix list below.)
    "LESSOPEN",
    "LESSCLOSE",
    "GIT_EXTERNAL_DIFF",
    "GIT_PAGER",
    "GIT_EDITOR",
    "GIT_ASKPASS",
    "GIT_PROXY_COMMAND",
    // A template directory is copied into a new repository's `.git`, hooks
    // included — and hooks run on the first commit. `GIT_ATTR_NOSYSTEM`
    // switches off the system gitattributes file a host may be relying on.
    "GIT_TEMPLATE_DIR",
    "GIT_ATTR_NOSYSTEM",
    // `git <verb>` dispatches to a `git-<verb>` binary found in
    // GIT_EXEC_PATH (which git also prepends to PATH for its own children),
    // so pointing it at a repository directory makes `git status` run the
    // repository's code. GIT_DIR/GIT_WORK_TREE/GIT_COMMON_DIR relocate the
    // `.git` git operates on, hooks and per-repo config included.
    "GIT_EXEC_PATH",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    // `GIT_ALLOW_PROTOCOL=ext` re-enables the `ext::` transport, whose URL
    // *is* a command line git runs on clone/submodule update.
    "GIT_ALLOW_PROTOCOL",
    // Helper programs ssh and anything opening a URL will exec.
    "SSH_ASKPASS",
    "BROWSER",
    // Stella shells out to `rg` (stella-tools/src/grep.rs), and a ripgrep
    // config file may carry `--pre=<command>`, which rg then executes per file.
    "RIPGREP_CONFIG_PATH",
    // Interpreters that execute a path or flag string at startup, and the
    // module-search roots that decide WHICH file an `import`/`require`
    // resolves to — shadowing a stdlib module is the same hijack as naming
    // the startup script outright.
    "NODE_OPTIONS",
    "NODE_REPL_EXTERNAL_MODULE",
    "NODE_PATH",
    "PYTHONSTARTUP",
    "PYTHONPATH",
    "PYTHONHOME",
    // `PYTHONUSERBASE` moves the per-user site directory, and site startup
    // imports `sitecustomize`/`usercustomize` from it before main runs.
    "PYTHONUSERBASE",
    // `PYTHONWARNINGS`' filter grammar carries a module field that the
    // warnings machinery imports at interpreter startup.
    "PYTHONWARNINGS",
    "PERL5OPT",
    "PERL5LIB",
    "RUBYOPT",
    "RUBYLIB",
    "GEM_HOME",
    "GEM_PATH",
    "JAVA_TOOL_OPTIONS",
    "_JAVA_OPTIONS",
    "CLASSPATH",
    "DOTNET_STARTUP_HOOKS",
    // Build toolchains that accept a wrapper binary.
    "RUSTC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO",
    "CARGO_BUILD_RUSTC_WRAPPER",
    // glibc loads shared conversion modules from GCONV_PATH — the same
    // "point the loader at a directory you control" shape as `LD_*`, just
    // outside that namespace.
    "GCONV_PATH",
    // The ROOTS every per-user config file hangs off. Denying
    // `GIT_CONFIG_GLOBAL` while allowing `HOME` blocks the leaf and leaves
    // the trunk: git reads `$HOME/.gitconfig` (then
    // `$XDG_CONFIG_HOME/git/config`) for `core.pager`, `core.sshCommand`, and
    // aliases with a `!` shell body, and ssh reads `$HOME/.ssh/config` for
    // `ProxyCommand`. Worse for Stella specifically: the USER settings scope
    // is `$HOME/.stella/settings.json` (settings.rs) and is *trusted* — it may
    // declare lifecycle hooks and context providers that the project scope is
    // gated away from — so a repository that could move `HOME` would walk
    // straight through the trust boundary `.stella/settings.json` is held to.
    // `STELLA_HOME`/`STELLA_CONFIG_DIR`/`STELLA_DATA_DIR` are the same trunk
    // for `~/.stella` (stella-store's `home.rs`).
    "HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    // Windows spellings of the same root: git-for-windows falls back to
    // HOMEDRIVE+HOMEPATH then USERPROFILE, and the legacy data dir is
    // %APPDATA%.
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "STELLA_HOME",
    "STELLA_CONFIG_DIR",
    "STELLA_DATA_DIR",
    // Stella's own privileged controls. `STELLA_MANAGED_SETTINGS` names the
    // org-managed settings file (the authority CEILING); `STELLA_NO_SETTINGS`
    // switches the whole scope chain off; `STELLA_TRUST_PROJECT` /
    // `STELLA_PROJECT_HOOKS` are the very trust gate that decides whether
    // this repository's hooks may run; `STELLA_ENGINE_CONFIG_JSON` is the
    // trusted-launcher seam that replaces the merged engine posture
    // wholesale (config.rs), replacement judge prompt included. A repository
    // granting itself any of these is the whole threat model.
    "STELLA_MANAGED_SETTINGS",
    "STELLA_NO_SETTINGS",
    "STELLA_TRUST_PROJECT",
    "STELLA_PROJECT_HOOKS",
    "STELLA_ENGINE_CONFIG_JSON",
    // The skill-manager verbs are literal command lines Stella shells out to.
    "STELLA_SKILLS_SEARCH_CMD",
    "STELLA_SKILLS_INSTALL_CMD",
    "STELLA_SKILLS_USE_CMD",
];

/// Prefixes denied wholesale, because naming the individual members would rot
/// as the upstream tool grows new ones:
///
/// - `LD_*` / `DYLD_*` — every name in these namespaces is a dynamic-loader
///   control, so an allow-list of the dangerous ones would go stale as libc
///   adds more.
/// - `GIT_SSH*` — `GIT_SSH` and `GIT_SSH_COMMAND` name the program git execs,
///   and `GIT_SSH_VARIANT` steers how its arguments are built.
/// - `GIT_CONFIG*` — the root of the vector [`DENIED_EXACT`]'s `GIT_PAGER` /
///   `GIT_ASKPASS` entries close. `GIT_CONFIG_COUNT` + `GIT_CONFIG_KEY_0` +
///   `GIT_CONFIG_VALUE_0` injects *any* config key into every git Stella runs
///   (`core.pager`, `core.sshCommand`, an alias with a `!` shell body), and
///   `GIT_CONFIG_GLOBAL` / `GIT_CONFIG_SYSTEM` point git at a file the cloned
///   repository controls. Blocking the leaves while leaving the root open is
///   no block at all.
const DENIED_PREFIXES: &[&str] = &["LD_", "DYLD_", "GIT_SSH", "GIT_CONFIG"];

/// Denied `<prefix>…<suffix>` shapes, for names whose middle is a wildcard.
/// `CARGO_TARGET_<TRIPLE>_RUNNER` (e.g.
/// `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER`) names the program cargo
/// executes to *run* a built binary — the same class of hijack as the
/// `RUSTC_WRAPPER` family already in [`DENIED_EXACT`], but the triple in the
/// middle means it cannot be spelled as an exact name or a prefix.
const DENIED_PREFIX_SUFFIX: &[(&str, &str)] = &[("CARGO_", "_RUNNER")];

/// Whether a dotenv file must not be allowed to set `name`.
///
/// Compared case-insensitively. The loader and most interpreters only honour the
/// canonical upper-case spelling, so a lower-case variant is already inert — but
/// refusing it too costs nothing and removes a class of near-miss reasoning.
fn is_execution_hijack(name: &str) -> bool {
    let upper = name.trim().to_ascii_uppercase();
    DENIED_EXACT.contains(&upper.as_str())
        || DENIED_PREFIXES.iter().any(|p| upper.starts_with(p))
        || DENIED_PREFIX_SUFFIX.iter().any(|(prefix, suffix)| {
            // The length guard keeps prefix and suffix from overlapping, so
            // `CARGO_RUNNER` is not read as having an empty triple.
            upper.len() >= prefix.len() + suffix.len()
                && upper.starts_with(prefix)
                && upper.ends_with(suffix)
        })
}

/// Resolve the files to the ordered assignments to apply. A name is taken from
/// the first (most-specific) file that defines it and that neither the
/// environment (`is_set`) nor an earlier file has already claimed — so the live
/// shell wins over every file, and a more specific file wins over a less
/// specific one.
///
/// Returns the assignments and, separately, the names refused by
/// [`is_execution_hijack`] so a caller can surface that they were ignored.
fn plan_assignments(
    files: &[(u8, PathBuf)],
    is_set: impl Fn(&str) -> bool,
) -> (Vec<(String, String, PathBuf)>, Vec<String>) {
    let mut claimed: HashSet<String> = HashSet::new();
    let mut out: Vec<(String, String, PathBuf)> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
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
            // Checked before `claimed`/`is_set` so the name is reported once
            // even when several files define it.
            if is_execution_hijack(&key) {
                if !refused.contains(&key) {
                    refused.push(key);
                }
                continue;
            }
            if is_set(&key) || claimed.contains(&key) {
                continue;
            }
            claimed.insert(key.clone());
            out.push((key, value, path.clone()));
        }
    }
    (out, refused)
}

/// Emit a concise, value-free confirmation of what [`maybe_load`] applied —
/// only when `STELLA_ENV_DEBUG` is set, stderr is a terminal, and the output
/// format is human (never in `json`/`stream-json`, which must stay clean).
pub fn announce(loaded: &Loaded, format: OutputFormat) {
    if loaded.is_empty() && loaded.refused.is_empty() {
        return;
    }
    // Machine-readable formats stay clean, and a non-terminal stderr is being
    // captured by something that did not ask for chatter.
    if matches!(format, OutputFormat::Json | OutputFormat::StreamJson)
        || !std::io::stderr().is_terminal()
    {
        return;
    }
    // A refusal is reported unconditionally: the project asked to redirect a
    // loader or interpreter and Stella declined. That is a security-relevant
    // fact about the repository you just opened, not a debug detail, so it is
    // deliberately NOT behind STELLA_ENV_DEBUG (#553).
    if !loaded.refused.is_empty() {
        eprintln!(
            "{} {}",
            "env:".yellow().bold(),
            format!(
                "ignored {} from this project's dotenv file — {} redirect a loader, \
                 interpreter, or spawned command. Export it in your shell if you meant it.",
                loaded.refused.join(", "),
                if loaded.refused.len() == 1 {
                    "it can"
                } else {
                    "they can"
                },
            )
            .yellow(),
        );
    }
    let debug = std::env::var_os("STELLA_ENV_DEBUG").is_some_and(|v| !v.is_empty() && v != "0");
    if !debug || loaded.is_empty() {
        return;
    }
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
        let (plan, _refused) = plan_assignments(&files, |k| shell.contains(k));

        let map: std::collections::HashMap<_, _> = plan
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
        let (plan, _refused) = plan_assignments(&files, |_| false);
        let map: std::collections::HashMap<_, _> = plan
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

    /// `$HOME` reached through a symlink — `/home` mounted elsewhere is a
    /// routine layout. `start` is `getcwd`, which is already resolved, so the
    /// raw string compare never matched and `~/.env` leaked into every session
    /// for exactly those users.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_home_is_still_never_a_project_scope() {
        let tmp = tempfile::tempdir().unwrap();
        // The real home, and a symlink standing in for it in `$HOME`.
        let real = std::fs::canonicalize(tmp.path()).unwrap().join("real-home");
        fs::create_dir_all(&real).unwrap();
        write(&real, ".env", "HOME_LEVEL=nope\n");
        let link = std::fs::canonicalize(tmp.path()).unwrap().join("home-link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        // `start` is the resolved path (what `current_dir` hands back); `$HOME`
        // is the symlink. These are the same directory and must be treated so.
        assert_eq!(find_base(&real, Some(&link)), None);
        // A subdirectory of home is still a perfectly good project scope.
        let project = real.join("proj");
        fs::create_dir_all(&project).unwrap();
        write(&project, ".env.local", "K=1\n");
        assert_eq!(find_base(&project, Some(&link)), Some(project));
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
            refused: Vec::new(),
        };
        assert_eq!(loaded.file_for("OPENROUTER_API_KEY"), Some(local.as_path()));
        // A name that was never loaded from a dotenv file (a real shell
        // export, say) must not be attributed to one.
        assert_eq!(loaded.file_for("ANTHROPIC_API_KEY"), None);
    }

    // Execution-hijack deny-list (#553)

    /// The witness: a cloned repository's dotenv file must not be able to
    /// redirect the dynamic loader, an interpreter, or the command lookup path.
    /// Before the deny-list these names were applied verbatim to Stella's own
    /// process environment at startup, so `git clone && stella` was arbitrary
    /// code execution on the first subprocess Stella spawned.
    #[test]
    fn dotenv_cannot_set_loader_interpreter_or_command_lookup_variables() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        write(
            d,
            ".env",
            "LD_PRELOAD=/tmp/evil.so\n\
             DYLD_INSERT_LIBRARIES=/tmp/evil.dylib\n\
             PATH=/tmp/evil/bin\n\
             NODE_OPTIONS=\"--require /tmp/evil.js\"\n\
             GIT_SSH_COMMAND=/tmp/evil.sh\n\
             BASH_ENV=/tmp/evil.sh\n\
             PYTHONSTARTUP=/tmp/evil.py\n\
             RUSTC_WRAPPER=/tmp/evil\n\
             LESSOPEN=\"|/tmp/evil.sh %s\"\n\
             ld_preload=/tmp/evil.so\n\
             OPENROUTER_API_KEY=sk-legit\n",
        );

        let files = collect_files(d);
        let (plan, refused) = plan_assignments(&files, |_| false);
        let planned: std::collections::HashMap<_, _> = plan
            .iter()
            .map(|(k, v, _)| (k.as_str(), v.as_str()))
            .collect();

        for hijack in [
            "LD_PRELOAD",
            "DYLD_INSERT_LIBRARIES",
            "PATH",
            "NODE_OPTIONS",
            "GIT_SSH_COMMAND",
            "BASH_ENV",
            "PYTHONSTARTUP",
            "RUSTC_WRAPPER",
            "LESSOPEN",
            "ld_preload",
        ] {
            assert!(
                !planned.contains_key(hijack),
                "{hijack} must never be applied from a project dotenv file"
            );
            assert!(
                refused.iter().any(|r| r == hijack),
                "{hijack} should be reported as refused"
            );
        }

        // The legitimate reason this mechanism exists still works.
        assert_eq!(planned.get("OPENROUTER_API_KEY"), Some(&"sk-legit"));
    }

    /// The witness for the second wave: a dotenv file must not reach the same
    /// execution vectors *indirectly*, by pointing a tool at a config file (or
    /// a config key) that names the command. Denying `GIT_PAGER` while leaving
    /// `GIT_CONFIG_*` open blocks the leaf and not the root — the same pager
    /// can be set with `GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.pager`.
    #[test]
    fn dotenv_cannot_redirect_a_spawned_tool_via_its_config() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        write(
            d,
            ".env",
            // git config injected straight from the environment…
            "GIT_CONFIG_COUNT=1\n\
             GIT_CONFIG_KEY_0=core.pager\n\
             GIT_CONFIG_VALUE_0=/tmp/evil.sh\n\
             # …or by pointing git at a file the repo ships.\n\
             GIT_CONFIG_GLOBAL=/tmp/evil.gitconfig\n\
             GIT_SSH_VARIANT=/tmp/evil\n\
             GIT_TEMPLATE_DIR=/tmp/evil-hooks\n\
             SHELLOPTS=xtrace\n\
             RIPGREP_CONFIG_PATH=/tmp/evil.rgrc\n\
             CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER=/tmp/evil\n\
             git_config_count=1\n\
             CARGO_TARGET_DIR=/tmp/target\n\
             OPENROUTER_API_KEY=sk-legit\n",
        );

        let files = collect_files(d);
        let (plan, refused) = plan_assignments(&files, |_| false);
        let planned: std::collections::HashMap<_, _> = plan
            .iter()
            .map(|(k, v, _)| (k.as_str(), v.as_str()))
            .collect();

        for hijack in [
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_0",
            "GIT_CONFIG_GLOBAL",
            "GIT_SSH_VARIANT",
            "GIT_TEMPLATE_DIR",
            "SHELLOPTS",
            "RIPGREP_CONFIG_PATH",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER",
            "git_config_count",
        ] {
            assert!(
                !planned.contains_key(hijack),
                "{hijack} must never be applied from a project dotenv file"
            );
            assert!(
                refused.iter().any(|r| r == hijack),
                "{hijack} should be reported as refused"
            );
        }

        // Neighbours that only *configure* cargo still load — the deny-list
        // must not swallow the namespace it borders on.
        assert_eq!(planned.get("CARGO_TARGET_DIR"), Some(&"/tmp/target"));
        assert_eq!(planned.get("OPENROUTER_API_KEY"), Some(&"sk-legit"));
    }

    /// The third wave: the names that reach the same escapes by moving the
    /// ROOT a config file is looked up under, rather than naming the file.
    /// `GIT_CONFIG_GLOBAL` was denied while `HOME`/`XDG_CONFIG_HOME` — which
    /// select the very same `core.pager`/`core.sshCommand` — were not, and
    /// `GIT_EXEC_PATH` (the directory `git <verb>` dispatches out of, and
    /// unset in an ordinary shell, so nothing shadowed it) was open outright.
    #[test]
    fn dotenv_cannot_move_the_root_a_config_or_dispatch_path_hangs_off() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        write(
            d,
            ".env",
            "GIT_EXEC_PATH=/tmp/evil-git-libexec\n\
             GIT_DIR=/tmp/evil.git\n\
             GIT_ALLOW_PROTOCOL=ext\n\
             HOME=/tmp/evil-home\n\
             XDG_CONFIG_HOME=/tmp/evil-config\n\
             STELLA_HOME=/tmp/evil-stella\n\
             STELLA_TRUST_PROJECT=1\n\
             STELLA_ENGINE_CONFIG_JSON={}\n\
             NODE_PATH=/tmp/evil-node-modules\n\
             PYTHONHOME=/tmp/evil-python\n\
             GCONV_PATH=/tmp/evil-gconv\n\
             OPENROUTER_API_KEY=sk-legit\n",
        );

        let files = collect_files(d);
        // `|_| false` deliberately models the environment where these are
        // NOT already exported — which is the ordinary case for every name
        // here except HOME, and is exactly when the hole was reachable.
        let (plan, refused) = plan_assignments(&files, |_| false);
        let planned: std::collections::HashMap<_, _> = plan
            .iter()
            .map(|(k, v, _)| (k.as_str(), v.as_str()))
            .collect();

        for hijack in [
            "GIT_EXEC_PATH",
            "GIT_DIR",
            "GIT_ALLOW_PROTOCOL",
            "HOME",
            "XDG_CONFIG_HOME",
            "STELLA_HOME",
            "STELLA_TRUST_PROJECT",
            "STELLA_ENGINE_CONFIG_JSON",
            "NODE_PATH",
            "PYTHONHOME",
            "GCONV_PATH",
        ] {
            assert!(
                !planned.contains_key(hijack),
                "{hijack} must never be applied from a project dotenv file"
            );
            assert!(
                refused.iter().any(|r| r == hijack),
                "{hijack} should be reported as refused"
            );
        }
        assert_eq!(planned.get("OPENROUTER_API_KEY"), Some(&"sk-legit"));
    }

    #[test]
    fn deny_list_matches_namespaces_and_case_but_spares_ordinary_names() {
        // Loader namespaces are refused wholesale, in any case.
        assert!(is_execution_hijack("LD_AUDIT"));
        assert!(is_execution_hijack("LD_LIBRARY_PATH"));
        assert!(is_execution_hijack("DYLD_FALLBACK_LIBRARY_PATH"));
        assert!(is_execution_hijack("dyld_insert_libraries"));
        assert!(is_execution_hijack("  PATH  ")); // whitespace is not an escape

        // git's ssh and config namespaces, root and leaves alike.
        assert!(is_execution_hijack("GIT_SSH"));
        assert!(is_execution_hijack("GIT_SSH_COMMAND"));
        assert!(is_execution_hijack("GIT_SSH_VARIANT"));
        assert!(is_execution_hijack("GIT_CONFIG"));
        assert!(is_execution_hijack("GIT_CONFIG_GLOBAL"));
        assert!(is_execution_hijack("GIT_CONFIG_KEY_17"));
        assert!(is_execution_hijack("git_config_value_17"));

        // The `<prefix>…<suffix>` shape, for every target triple.
        assert!(is_execution_hijack(
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER"
        ));
        assert!(is_execution_hijack("CARGO_TARGET_WASM32_WASIP1_RUNNER"));
        assert!(is_execution_hijack(
            "cargo_target_aarch64_apple_darwin_runner"
        ));

        // Names a project legitimately sets must still load — an over-broad
        // deny-list would quietly break the feature this module exists for.
        assert!(!is_execution_hijack("OPENROUTER_API_KEY"));
        assert!(!is_execution_hijack("ANTHROPIC_API_KEY"));
        assert!(!is_execution_hijack("DATABASE_URL"));
        assert!(!is_execution_hijack("GIT_AUTHOR_NAME"));
        assert!(!is_execution_hijack("PATHOLOGY_API")); // prefix-of-PATH, not PATH
        assert!(!is_execution_hijack("NODE_ENV"));
        // Bordering names in the same namespaces are configuration, not
        // execution: cargo's output directory and job count, git's committer.
        // Only the `_RUNNER` suffix and the `GIT_CONFIG`/`GIT_SSH` roots bite.
        assert!(!is_execution_hijack("CARGO_TARGET_DIR"));
        assert!(!is_execution_hijack("CARGO_BUILD_JOBS"));
        assert!(!is_execution_hijack("GIT_COMMITTER_EMAIL"));
        // The config-ROOT entries are exact names, not prefixes: a project
        // may still set its own `HOME…`/`XDG_…`/`STELLA_…`-shaped variables,
        // and the documented per-project `STELLA_MODEL` steering — the reason
        // a project dotenv is worth reading at all — is untouched.
        assert!(!is_execution_hijack("HOMEBREW_PREFIX"));
        assert!(!is_execution_hijack("HOMEPAGE_URL"));
        assert!(!is_execution_hijack("STELLA_MODEL"));
        assert!(!is_execution_hijack("XDG_RUNTIME_DIR"));
        assert!(!is_execution_hijack("NODE_VERSION"));
    }

    #[test]
    fn a_refused_name_is_reported_once_even_when_several_files_define_it() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        write(d, ".env", "LD_PRELOAD=/tmp/a.so\n");
        write(d, ".env.local", "LD_PRELOAD=/tmp/b.so\n");

        let files = collect_files(d);
        let (plan, refused) = plan_assignments(&files, |_| false);
        assert!(plan.is_empty());
        assert_eq!(refused, vec!["LD_PRELOAD".to_string()]);
    }
}
