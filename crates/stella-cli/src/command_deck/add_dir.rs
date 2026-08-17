// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `/add-dir` — grant this session a directory it may write to.
//!
//! The interactive half of the write scope
//! ([`stella_core::workspace_scope`]). The other two ways in are
//! `--allow-dir` on the command line and `[workspace] allowed_dirs` in
//! `stella.toml`; this is the one for the moment you discover mid-session that
//! the work needs a directory nobody thought to list.
//!
//! # Why it asks
//!
//! A grant has two very different lifetimes and the difference is invisible
//! afterwards, so the command asks rather than guessing:
//!
//! - **This session only.** Nothing is written to disk. The next `stella` run
//!   starts from the committed configuration again.
//! - **Every future session.** The directory is appended to `stella.toml`'s
//!   `[workspace] allowed_dirs`, which is a *committed, reviewed* file — so
//!   the answer also decides whether a teammate inherits the grant.
//!
//! Defaulting either way is wrong. Defaulting to persist writes to a
//! source-controlled file on a keystroke; defaulting to session-only silently
//! discards a grant the operator will have to re-type every run and will
//! eventually stop trusting.
//!
//! # What it cannot do
//!
//! It cannot re-admit a tree the scope hides — the origin project of a
//! worktree session, or any session's own worktrees. `SessionScope` filters
//! every grant through the same denial, so this command refuses that case up
//! front with the reason, rather than appearing to succeed and leaving the
//! operator to discover the tools still refuse the path.

use std::path::{Path, PathBuf};

use stella_core::workspace_scope::SessionScope;

/// A parsed `/add-dir` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AddDirCommand {
    /// `/add-dir` with no argument — print how to use it, and what is
    /// currently granted.
    Usage,
    /// `/add-dir <path>` — grant `path` for this session, then ask whether it
    /// should outlive it.
    Grant(String),
    /// `/add-dir persist` — the answer "every future session", applied to the
    /// grant just made.
    AnswerPersist,
    /// `/add-dir session` — the answer "this session only". Nothing to do but
    /// confirm and forget the pending question.
    AnswerSessionOnly,
}

/// Parse `/add-dir …`, or `None` when the line is some other command.
///
/// Accepts `/add-dir` and `/adddir`: the hyphen is easy to drop and refusing
/// the near-miss as "unknown command" teaches nothing.
///
/// `persist` and `session` are answers rather than paths. They are reserved
/// words here, which is safe because they are not path-shaped — a directory
/// literally named `persist` is reachable as `./persist`, and the usage line
/// says so.
pub(crate) fn parse(line: &str) -> Option<AddDirCommand> {
    let trimmed = line.trim();
    let rest = trimmed
        .strip_prefix("/add-dir")
        .or_else(|| trimmed.strip_prefix("/adddir"))?;
    // `/add-dirty` must not parse as `/add-dir` with the argument `ty`.
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    match rest.trim() {
        "" => Some(AddDirCommand::Usage),
        "persist" | "p" => Some(AddDirCommand::AnswerPersist),
        "session" | "s" => Some(AddDirCommand::AnswerSessionOnly),
        argument => Some(AddDirCommand::Grant(argument.to_string())),
    }
}

/// The grant awaiting an answer to the persistence question.
///
/// Process-global because a Stella process *is* one session, and threading a
/// single `Option<PathBuf>` through the deck's command dispatch would touch
/// every caller for one command's benefit. Cleared as soon as it is answered,
/// so a later `/add-dir persist` with nothing pending gets told so rather than
/// re-persisting whatever was last granted.
fn pending() -> &'static std::sync::Mutex<Option<PathBuf>> {
    static PENDING: std::sync::OnceLock<std::sync::Mutex<Option<PathBuf>>> =
        std::sync::OnceLock::new();
    PENDING.get_or_init(|| std::sync::Mutex::new(None))
}

/// Remember `granted` as the subject of the persistence question.
pub(crate) fn set_pending(granted: PathBuf) {
    *pending().lock().unwrap_or_else(|p| p.into_inner()) = Some(granted);
}

/// Take the pending grant, if the question is still open.
pub(crate) fn take_pending() -> Option<PathBuf> {
    pending().lock().unwrap_or_else(|p| p.into_inner()).take()
}

/// Handle one `/add-dir …` line, returning what to say — or `None` when the
/// line is not this command.
///
/// The whole handler lives here rather than in the deck's dispatch `match`
/// because `command_deck.rs` is a grandfathered god file closed to growth
/// (AGENTS.md § "God files"), and because a command's behaviour belongs beside
/// its parser: the bare form, the grant, and the two answers are one thing to
/// read.
pub(crate) fn handle(
    line: &str,
    cfg: &crate::config::Config,
    registry: &stella_tools::ToolRegistry,
) -> Option<String> {
    Some(match parse(line)? {
        AddDirCommand::Usage => usage(&registry.write_scope()),
        AddDirCommand::Grant(requested) => {
            match resolve_grant(&registry.write_scope(), &cfg.workspace_root, &requested) {
                Err(refusal) => refusal.to_string(),
                Ok(granted) => {
                    // The grant takes effect immediately, before the question
                    // is answered: the operator asked for it now, and making
                    // them answer first would leave the session-only case
                    // doing nothing at all.
                    let mut dirs: Vec<PathBuf> = registry.write_scope().roots().to_vec();
                    dirs.push(granted.clone());
                    registry.allow_write_dirs(dirs);
                    set_pending(granted.clone());
                    format!(
                        "granted `{}` — writable now, for this session.\n\n{}",
                        granted.display(),
                        persistence_question(&granted)
                    )
                }
            }
        }
        AddDirCommand::AnswerPersist => match take_pending() {
            None => "nothing to persist — run `/add-dir <path>` first.".to_string(),
            Some(granted) => match persist(
                &cfg.workspace_root,
                &cfg.workspace_root.join("stella.toml"),
                &granted,
            ) {
                Ok(PersistOutcome::Written(entry)) => format!(
                    "added `{entry}` to stella.toml's [workspace] allowed_dirs. That file \
                     is committed, so teammates inherit the grant."
                ),
                Ok(PersistOutcome::AlreadyPresent(entry)) => {
                    format!("`{entry}` was already listed in stella.toml; the file is unchanged.")
                }
                Err(error) => format!("could not update stella.toml: {error}"),
            },
        },
        AddDirCommand::AnswerSessionOnly => match take_pending() {
            None => "nothing pending — run `/add-dir <path>` first.".to_string(),
            Some(granted) => format!(
                "kept `{}` for this session only; nothing was written to disk.",
                granted.display()
            ),
        },
    })
}

/// Why a grant was refused, so the caller can say something useful.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GrantError {
    /// The path is not a directory (or does not exist).
    NotADirectory(String),
    /// The scope hides this path — a worktree, or the origin project of a
    /// worktree session. The second field is the reason, phrased for whichever
    /// session hit it.
    Denied(String, String),
    /// Already covered by the workspace or an existing grant. The second field
    /// names the root that already covers it.
    Redundant(String, String),
}

/// Rendered straight into the deck transcript, so each variant is a whole
/// sentence rather than a fragment the caller has to frame.
///
/// Hand-written rather than derived: `stella-cli` is a binary and carries no
/// `thiserror`, and this is three arms.
impl std::fmt::Display for GrantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GrantError::NotADirectory(path) => write!(
                f,
                "`{path}` is not a directory. `/add-dir` grants a directory the agent \
                 may write inside; create it first, or name its parent."
            ),
            GrantError::Denied(path, why) => write!(
                f,
                "`{path}` is inside a tree this session deliberately cannot see, so \
                 granting it would not make it writable. {why}"
            ),
            GrantError::Redundant(path, root) => {
                write!(f, "`{path}` is already writable — it is inside {root}.")
            }
        }
    }
}

/// Resolve and check one requested directory against `scope`.
///
/// Returns the absolute path to grant. The three refusals are separated
/// because they want different sentences: a typo, a genuinely hidden tree, and
/// a grant that would change nothing are three different things to be told.
pub(crate) fn resolve_grant(
    scope: &SessionScope,
    workspace_root: &Path,
    requested: &str,
) -> Result<PathBuf, GrantError> {
    let expanded = expand_home(requested);
    let candidate = if expanded.is_absolute() {
        expanded
    } else {
        workspace_root.join(expanded)
    };
    let canonical = candidate
        .canonicalize()
        .map_err(|_| GrantError::NotADirectory(requested.to_string()))?;
    if !canonical.is_dir() {
        return Err(GrantError::NotADirectory(requested.to_string()));
    }

    // Already writable? Then the grant is a no-op and saying "added" would be
    // a small lie that costs an operator a debugging session later.
    if let Some(root) = scope
        .roots()
        .iter()
        .find(|root| canonical.starts_with(root))
    {
        return Err(GrantError::Redundant(
            requested.to_string(),
            root.display().to_string(),
        ));
    }

    // The scope hides some trees from this session entirely, and a grant
    // cannot re-admit them (`SessionScope::with_additional` drops it). Refuse
    // here so the operator learns why, instead of watching the tools keep
    // refusing a directory the deck said it had added.
    let widened = scope.clone().with_additional([canonical.clone()]);
    if !widened.may_write(&canonical.join("probe")).is_allowed() {
        let why = if scope.in_worktree() {
            format!(
                "This session entered a worktree, so the project it was cut from \
                 (`{}`) stays invisible until the session leaves it.",
                scope.origin().display()
            )
        } else {
            "It holds other sessions' git worktrees, which are separate checkouts \
             of this repository at other revisions."
                .to_string()
        };
        return Err(GrantError::Denied(requested.to_string(), why));
    }

    Ok(canonical)
}

/// `~` and `~/…` against the user's home, left alone when there is no home.
///
/// Through [`crate::paths::home`] rather than `$HOME` directly: that is the
/// single resolver a test can redirect without mutating process-global
/// environment state (#1139), and a guard in `paths.rs` fails the build for
/// anything in this crate that reaches past it.
fn expand_home(raw: &str) -> PathBuf {
    let Some(rest) = raw.strip_prefix('~') else {
        return PathBuf::from(raw);
    };
    let Some(home) = crate::paths::home() else {
        return PathBuf::from(raw);
    };
    match rest.strip_prefix('/') {
        Some(tail) => home.join(tail),
        None if rest.is_empty() => home,
        // `~other-user` is not ours to resolve.
        None => PathBuf::from(raw),
    }
}

/// Append `granted` to the project `stella.toml`'s `[workspace] allowed_dirs`.
///
/// Stored **relative to the workspace root when it is inside it**, absolute
/// otherwise. `stella.toml` is committed and read by teammates on other
/// machines, so an absolute path that happens to work here (`/Users/mac/…`)
/// would be meaningless on theirs — while a relative one resolves against each
/// checkout's own root, which is what `write_dirs::resolve` already does.
///
/// Reads the current list, appends, and writes the section back through
/// [`crate::settings::toml_io::save_section`], which preserves every comment
/// and key order elsewhere in the file — an operator's `stella.toml` is a
/// reviewed document, not a serialisation target.
///
/// A directory already in the list is not appended twice; the function reports
/// that rather than silently doing nothing, because "already there" and
/// "written" are different answers to the operator's question.
pub(crate) fn persist(
    workspace_root: &Path,
    config_path: &Path,
    granted: &Path,
) -> Result<PersistOutcome, String> {
    #[derive(serde::Serialize, serde::Deserialize, Default)]
    struct WorkspaceSection {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        allowed_dirs: Vec<String>,
    }

    let entry = granted
        .strip_prefix(workspace_root)
        .map(|relative| relative.to_string_lossy().into_owned())
        .unwrap_or_else(|_| granted.to_string_lossy().into_owned());

    let existing = std::fs::read_to_string(config_path).unwrap_or_default();
    let document: toml::Value =
        toml::from_str(&existing).unwrap_or(toml::Value::Table(toml::map::Map::new()));
    let mut allowed: Vec<String> = document
        .get("workspace")
        .and_then(|section| section.get("allowed_dirs"))
        .and_then(|list| list.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    if allowed.iter().any(|existing| existing == &entry) {
        return Ok(PersistOutcome::AlreadyPresent(entry));
    }
    allowed.push(entry.clone());

    crate::settings::toml_io::save_section(
        config_path,
        "workspace",
        Some(&WorkspaceSection {
            allowed_dirs: allowed,
        }),
        // Project scope, not the user tier: this file is committed, so it gets
        // ordinary permissions rather than the owner-only atomic write
        // reserved for credential-bearing user config.
        false,
    )?;
    Ok(PersistOutcome::Written(entry))
}

/// What [`persist`] did, so the deck can say the true thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PersistOutcome {
    /// Appended to `allowed_dirs`.
    Written(String),
    /// The entry was already listed; the file is unchanged.
    AlreadyPresent(String),
}

/// The prompt text, and the usage line, kept beside each other so the two
/// stay consistent.
pub(crate) fn persistence_question(path: &Path) -> String {
    format!(
        "Grant `{}`?\n  [s] this session only — nothing is written to disk\n  \
         [p] every future session — appends to stella.toml's [workspace] allowed_dirs, \
         which is committed and reviewed\n  [esc] cancel",
        path.display()
    )
}

/// Usage, including what is granted right now — an operator asking "how do I
/// add one" usually also wants to know what is already there.
pub(crate) fn usage(scope: &SessionScope) -> String {
    let mut out = String::from(
        "usage: `/add-dir <path>` — let this session write inside a directory outside \
         the workspace. You are asked whether the grant is for this session only or \
         for every future session.\n\nWritable now:",
    );
    for root in scope.roots() {
        out.push_str(&format!("\n  {}", root.display()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_spellings_and_the_bare_form() {
        assert_eq!(parse("/add-dir"), Some(AddDirCommand::Usage));
        assert_eq!(parse("  /adddir  "), Some(AddDirCommand::Usage));
        assert_eq!(
            parse("/add-dir ../shared"),
            Some(AddDirCommand::Grant("../shared".into()))
        );
        assert_eq!(parse("/model zai/glm-5.2"), None);
        assert_eq!(
            parse("/add-dirty"),
            None,
            "a longer command must not parse as this one with an argument"
        );
    }

    #[test]
    fn a_path_that_is_not_a_directory_is_refused_by_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let scope = SessionScope::new(dir.path().canonicalize().expect("canon"));
        let error = resolve_grant(&scope, dir.path(), "no-such-place").unwrap_err();
        assert!(matches!(error, GrantError::NotADirectory(_)), "{error}");
    }

    #[test]
    fn a_directory_already_inside_the_workspace_is_reported_as_redundant() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        let scope = SessionScope::new(dir.path().canonicalize().expect("canon"));
        let error = resolve_grant(&scope, dir.path(), "src").unwrap_err();
        assert!(matches!(error, GrantError::Redundant(..)), "{error}");
    }

    #[test]
    fn an_outside_directory_resolves_to_its_canonical_path() {
        let workspace = tempfile::tempdir().expect("workspace");
        let shared = tempfile::tempdir().expect("shared");
        let scope = SessionScope::new(workspace.path().canonicalize().expect("canon"));
        let granted = resolve_grant(&scope, workspace.path(), &shared.path().to_string_lossy())
            .expect("an outside directory is grantable");
        assert_eq!(granted, shared.path().canonicalize().expect("canon"));
    }

    /// The rule the command exists to enforce at the surface: a worktree
    /// session cannot grant itself the project it was cut from, and the
    /// refusal explains why rather than failing silently later.
    #[test]
    fn a_worktree_session_cannot_grant_its_origin_project() {
        let project = tempfile::tempdir().expect("project");
        let worktree = project.path().join(".stella/worktrees/feature-x");
        std::fs::create_dir_all(&worktree).expect("mkdir");
        let scope = SessionScope::new(worktree.canonicalize().expect("canon"));

        let error = resolve_grant(&scope, &worktree, &project.path().to_string_lossy())
            .expect_err("the origin project must not be grantable");
        let GrantError::Denied(_, why) = &error else {
            panic!("expected a denial, got {error}");
        };
        assert!(why.contains("worktree"), "{why}");
    }

    #[test]
    fn the_answers_parse_in_long_and_short_form() {
        for line in ["/add-dir persist", "/add-dir p"] {
            assert_eq!(parse(line), Some(AddDirCommand::AnswerPersist), "{line}");
        }
        for line in ["/add-dir session", "/add-dir s"] {
            assert_eq!(
                parse(line),
                Some(AddDirCommand::AnswerSessionOnly),
                "{line}"
            );
        }
        assert_eq!(
            parse("/add-dir ./persist"),
            Some(AddDirCommand::Grant("./persist".into())),
            "a directory really named `persist` stays reachable as a path"
        );
    }

    /// Persisting writes a **workspace-relative** entry when the directory is
    /// inside the tree, because `stella.toml` is committed and an absolute
    /// path from one machine is meaningless on a teammate's.
    #[test]
    fn persisting_writes_a_relative_entry_and_is_idempotent() {
        let workspace = tempfile::tempdir().expect("workspace");
        let root = workspace.path().canonicalize().expect("canon");
        let shared = root.join("vendor/shared");
        std::fs::create_dir_all(&shared).expect("mkdir");
        let config = root.join("stella.toml");
        std::fs::write(&config, "# keep me\n[meta]\nschema_version = 1\n").expect("seed");

        let first = persist(&root, &config, &shared).expect("write");
        assert_eq!(first, PersistOutcome::Written("vendor/shared".into()));

        let text = std::fs::read_to_string(&config).expect("read");
        assert!(text.contains("vendor/shared"), "{text}");
        assert!(
            text.contains("# keep me"),
            "an operator's comments must survive the write: {text}"
        );

        let second = persist(&root, &config, &shared).expect("second write");
        assert_eq!(
            second,
            PersistOutcome::AlreadyPresent("vendor/shared".into()),
            "a repeat must not append a duplicate"
        );
    }

    /// A directory outside the workspace has no relative spelling, so it is
    /// stored absolute — the honest answer, even though it is machine-specific.
    #[test]
    fn a_directory_outside_the_workspace_persists_as_an_absolute_path() {
        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        let root = workspace.path().canonicalize().expect("canon");
        let target = outside.path().canonicalize().expect("canon");
        let config = root.join("stella.toml");

        let outcome = persist(&root, &config, &target).expect("write");
        assert_eq!(
            outcome,
            PersistOutcome::Written(target.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn the_usage_line_lists_what_is_already_writable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let scope = SessionScope::new(dir.path().canonicalize().expect("canon"));
        let text = usage(&scope);
        assert!(text.contains("/add-dir <path>"));
        assert!(
            text.contains(
                &dir.path()
                    .canonicalize()
                    .expect("canon")
                    .display()
                    .to_string()
            )
        );
    }

    #[test]
    fn the_question_names_both_lifetimes_and_where_persisting_writes() {
        let question = persistence_question(Path::new("/work/shared"));
        assert!(question.contains("this session only"));
        assert!(question.contains("every future session"));
        assert!(
            question.contains("stella.toml"),
            "the operator must know persisting edits a committed file: {question}"
        );
    }
}
