//! What a shell running inside an isolated candidate may not touch (#2875).
//!
//! [`crate::rootfd`] confines the *file tools* by held descriptors, and says so
//! in its own header: it "is not a sandbox for the subprocesses they spawn —
//! `bash` can still write wherever the user's own account can". That concession
//! was paid for twice on the 2026-08-11 Terminal-Bench panel, in both of the
//! two ways it can be:
//!
//! * **Through the filesystem.** On `log-summary-date-ranges` the worker's
//!   first script hardcoded the task statement's `/app/summary.csv`, ran once,
//!   and wrote a wrong intermediate straight into the graded tree. The worker
//!   then computed the right answer *inside* its candidate, the run aborted
//!   before adoption, and the grader read the leaked wrong file. Stella solved
//!   the task and scored zero. `write_file` and `apply_edits` refuse that path
//!   (`escapes workspace root`); the model simply reissued it as a heredoc.
//! * **Through the git ref namespace.** On `code-from-image` the worker ran
//!   `git update-ref refs/worktree/stella/witness-baseline <its own commit>` —
//!   the exact ref `verify_done` pins its baseline to — then
//!   `git reflog expire --expire=now --all && git gc --prune=now`. Nothing in
//!   that trace shows a fabricated flip, but a benchmark the subject can edit
//!   the scorer's state inside is not a measurement. The seal's own error text
//!   already conceded the hole: "a candidate worktree isolates the working tree
//!   but not the ref namespace."
//!
//! # The posture, and why it is not "refuse everything outside the root"
//!
//! Some tasks genuinely require system-level effects — `apt-get install nginx`,
//! writing `/etc/nginx/sites-enabled`, populating `/var/www/html`. A blanket
//! refusal of everything outside the workspace fails those outright, so what is
//! protected is deliberately narrow and named:
//!
//! 1. **The graded tree** — the real workspace root the candidate's work is
//!    delivered into by adoption. Writing there bypasses the delivery channel
//!    and corrupts the artifact the run is judged against, which is a different
//!    thing from doing the task.
//! 2. **Stella's own git refs** ([`STELLA_REF_NAMESPACE`]) and, inside a
//!    candidate, the git operations that destroy the objects those refs pin.
//!    No task's work lives there; only the scorer's does.
//!
//! Everything else stays exactly as writable as it was. `/etc`, `/usr`, `/var`,
//! `/tmp` and the package manager are untouched by this module — protection (1)
//! arms only when a host declares a *distinct* graded tree, which today means a
//! best-of-N candidate workspace, and protection (2) costs nothing anywhere
//! because nothing legitimate names those refs.
//!
//! # What is enforced, and what is not
//!
//! This is a **literal audit of the command text**, performed before the spawn.
//! It is honest about its own ceiling: a shell can compute a path at runtime,
//! and `p=/ap; p=$p$'p'; printf x > "$p/f"` defeats every string check that
//! will ever be written here. What it does buy:
//!
//! * an **accidental** escape — the dominant real shape, a path copied out of
//!   the task statement — becomes impossible rather than silent, and
//! * a **deliberate** one has to be obfuscated, which a trace reader can see.
//!
//! Total confinement of a shell is an OS boundary (a container, a seccomp
//! profile), not a parser; `STELLA_BASH_SANDBOX` was removed in #1300 precisely
//! because it claimed a session-wide bound it did not have, and this module
//! does not re-make that claim. `crates/stella-cli/src/candidate_ws/escape.rs`
//! remains the post-hoc backstop for what gets through.
//!
//! Two mechanisms were considered and rejected rather than shipped weak.
//! `GIT_CEILING_DIRECTORIES` stops git's *upward* discovery, and a candidate
//! worktree's `.git` file points into the shared common directory from the
//! first component — there is no ascent to stop, so the variable would be
//! decoration. A per-candidate `GIT_DIR`/`GIT_COMMON_DIR` would sever the
//! worktree from the object store it is a worktree of. The mechanism that
//! *would* be total for refs is a `reference-transaction` hook in the shared
//! repository, which git enforces itself for every ref update from any
//! worktree; it mutates the operator's repository, so it belongs to the
//! candidate's setup rather than to a tool, and is tracked separately.
//!
//! # Deliberately out of scope
//!
//! Refs the graded tree merely *shares* (`refs/heads/*`): a candidate creating,
//! moving or deleting a branch is ordinary task work, and the observed loss
//! there (`query-optimize`) is a seal that refuses where it could repair, not
//! an escape to prevent.

use std::path::{Component, Path, PathBuf};

/// Stella's private per-worktree git ref namespace. `verify_done` pins the
/// baseline commit of every witness run under it, so a command that writes
/// here is editing the scorer's state, whatever it meant to do.
///
/// Kept as a prefix rather than a list of exact refs: a new Stella-owned ref
/// under this namespace is protected the day it is introduced, with nothing
/// to remember to add here.
pub const STELLA_REF_NAMESPACE: &str = "refs/worktree/stella/";

/// git subcommands that delete unreachable objects or the reflogs that keep
/// them reachable. A candidate shares its object store with the graded tree,
/// so any of these can drop the baseline commit the verification and the
/// adoption both resolve against.
const HISTORY_DESTROYING: &[&str] = &["gc", "prune", "repack"];

/// Characters that continue a path literal. Everything else terminates one —
/// quotes and shell operators most importantly, which is what lets an embedded
/// `open('/app/summary.csv', 'w')` yield exactly `/app/summary.csv`.
fn is_path_char(c: char) -> bool {
    !(c.is_whitespace()
        || matches!(
            c,
            '\'' | '"'
                | '`'
                | ';'
                | '&'
                | '|'
                | '<'
                | '>'
                | '('
                | ')'
                | '$'
                | '{'
                | '}'
                | ','
                | '='
                | ':'
                | '*'
                | '?'
                | '['
                | ']'
                | '!'
                | '#'
                | '\\'
        ))
}

/// Resolve `rel` against `base` **lexically** — `..` pops a component instead
/// of asking the filesystem. Not a security primitive on its own (a symlink
/// makes the lexical answer and the real one differ); it exists so a relative
/// `../../app/x` is classified the same way the absolute spelling would be.
fn lexical_join(base: &Path, rel: &Path) -> PathBuf {
    let mut out = base.to_path_buf();
    for component in rel.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(name) => out.push(name),
            Component::RootDir => out = PathBuf::from("/"),
            Component::Prefix(prefix) => out = PathBuf::from(prefix.as_os_str()),
        }
    }
    out
}

/// Why a shell command was refused before it ran.
///
/// A typed value rather than a message, because the two halves have different
/// audiences: the variant is what a caller would branch on (and what a future
/// diagnostic record would carry), while [`Display`](std::fmt::Display) is the
/// remediation the model reads. Every message names the substitute that *does*
/// work — a refusal that only says "no" teaches the model to try a spelling
/// that evades the check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The command names a path inside the graded tree.
    GradedTree {
        /// The literal as written, for the message.
        literal: String,
        /// The graded tree it falls under.
        graded_root: PathBuf,
        /// The same file inside this shell's own workspace, when the literal
        /// is under the graded root by a path that can be rebased.
        substitute: Option<PathBuf>,
    },
    /// The command names a ref in [`STELLA_REF_NAMESPACE`].
    StellaRef { reference: String },
    /// The command runs a git operation that destroys shared objects.
    HistoryDestroying { subcommand: String },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GradedTree {
                literal,
                graded_root,
                substitute,
            } => {
                write!(
                    f,
                    "refused before running: `{literal}` is inside the graded tree `{}`, which \
                     this shell may not touch. This shell's workspace is a snapshot of that \
                     tree, and work done here IS delivered back to it when the run finishes; \
                     work written directly into it is not part of that delivery and corrupts \
                     the tree the result is judged against.",
                    graded_root.display()
                )?;
                if let Some(substitute) = substitute {
                    write!(f, " Use `{}` instead.", substitute.display())?;
                }
                write!(
                    f,
                    " Naming the graded tree at all — even inside a script body, an `echo`, or \
                     a comment — trips this, so rewrite the path rather than quoting it \
                     differently. Nothing else outside this workspace is restricted: system \
                     paths such as /etc, /usr and /var are still writable."
                )
            }
            Self::StellaRef { reference } => write!(
                f,
                "refused before running: `{reference}` is in `{STELLA_REF_NAMESPACE}`, Stella's \
                 own git ref namespace rather than the task's. `verify_done` pins the baseline \
                 of every witness run there, so a command that moves, deletes, or rewrites it \
                 can fabricate a fail→pass flip — it is refused whatever it intended. Ordinary \
                 work belongs under refs/heads/ and refs/tags/."
            ),
            Self::HistoryDestroying { subcommand } => write!(
                f,
                "refused before running: `git {subcommand}` destroys objects and reflogs in an \
                 object store this workspace SHARES with the graded tree, including the \
                 baseline commit this run's verification and its adoption both resolve \
                 against. There is nothing to reclaim here — the workspace is discarded whole \
                 when the run ends."
            ),
        }
    }
}

impl std::error::Error for Refusal {}

/// The territory a `bash` invocation may not name.
///
/// Default is [`unconfined`](Self::unconfined), which still enforces the two
/// protections that have no legitimate use anywhere — see the module header
/// for why the graded-tree half is opt-in and the ref half is not.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellConfinement {
    /// Every spelling of the real workspace root this shell's work is
    /// *delivered into*, when this shell is running somewhere else. Empty in
    /// an ordinary session, where the workspace root and the graded tree are
    /// the same directory and there is nothing to keep the shell out of.
    ///
    /// A list rather than one path because a text audit compares *names*, and
    /// one directory has several: `/var/folders/…` and `/private/var/folders/…`
    /// are the same tree on macOS, `/app` may be a symlink in a container, and
    /// a task statement quotes whichever the operator configured. The first
    /// entry is the canonical one and is what a refusal reports; the rest are
    /// aliases that must refuse identically.
    graded_roots: Vec<PathBuf>,
}

impl ShellConfinement {
    /// An ordinary session: the workspace root is the tree the work is judged
    /// on, so a write into it is the work, not an escape.
    pub fn unconfined() -> Self {
        Self {
            graded_roots: Vec::new(),
        }
    }

    /// Arm the graded-tree protection for a shell running in an isolated
    /// workspace whose output reaches `graded_root` only by adoption.
    ///
    /// The canonical spelling is registered alongside the given one when the
    /// two differ, so a host that passes either gets both covered without
    /// having to know which it holds.
    ///
    /// A root with no components of its own (`/`, or a relative path) does not
    /// arm: "everything is protected" is the blanket refusal this module
    /// exists not to be, and a relative root cannot be compared against the
    /// absolute literals a command contains.
    pub fn graded_tree(graded_root: impl Into<PathBuf>) -> Self {
        let graded_root = graded_root.into();
        let canonical = graded_root.canonicalize().ok();
        Self {
            graded_roots: Vec::new(),
        }
        .and_alias(graded_root)
        .and_alias_opt(canonical)
    }

    /// Register another name for the same graded tree — the spelling a host
    /// held *before* it canonicalized, or the workspace root inside it. A
    /// path that would not arm on its own is ignored rather than stored.
    pub fn and_alias(mut self, alias: impl Into<PathBuf>) -> Self {
        let alias = alias.into();
        let arms = alias.is_absolute()
            && alias
                .components()
                .any(|c| matches!(c, Component::Normal(_)))
            && alias.to_str().is_some();
        if arms && !self.graded_roots.contains(&alias) {
            self.graded_roots.push(alias);
        }
        self
    }

    fn and_alias_opt(self, alias: Option<PathBuf>) -> Self {
        match alias {
            Some(alias) => self.and_alias(alias),
            None => self,
        }
    }

    /// The graded tree this shell is kept out of, if any — the first
    /// registered spelling, which is what a refusal reports.
    pub fn graded_root(&self) -> Option<&Path> {
        self.graded_roots.first().map(PathBuf::as_path)
    }

    /// Refuse `command` if its text names protected territory, given the
    /// workspace root the shell will run in.
    ///
    /// The two ref protections run whatever the posture; the graded-tree scan
    /// runs only when armed, and never when the graded tree *is* this shell's
    /// own root (a host that armed it redundantly gets the ordinary session
    /// behaviour rather than a shell that cannot name its own files).
    pub fn audit(&self, command: &str, session_root: &Path) -> Result<(), Refusal> {
        if let Some(reference) = stella_ref_named(command) {
            return Err(Refusal::StellaRef { reference });
        }
        if self.graded_roots.is_empty()
            || self
                .graded_roots
                .iter()
                .any(|graded| session_root == graded)
        {
            return Ok(());
        }
        if let Some(subcommand) = history_destroying_git(command) {
            return Err(Refusal::HistoryDestroying { subcommand });
        }
        // Every spelling refuses identically; the message reports the first
        // registered one, so two refusals of the same tree read the same.
        let reported = self.graded_root().unwrap_or(Path::new("/"));
        for graded in &self.graded_roots {
            if let Some(literal) = graded_literal(command, graded, session_root) {
                let substitute = Path::new(&literal)
                    .strip_prefix(graded)
                    .ok()
                    .map(|rel| session_root.join(rel));
                return Err(Refusal::GradedTree {
                    literal,
                    graded_root: reported.to_path_buf(),
                    substitute,
                });
            }
        }
        // A relative spelling of the same escape: `cd ../..` out of a
        // workspace that happens to sit beside the graded tree. Only words
        // that climb are resolved — a plain relative path cannot leave the
        // root it is joined to.
        for word in super::shell_words(command) {
            let path = Path::new(&word);
            if path.is_absolute() || !word.contains("..") {
                continue;
            }
            let resolved = lexical_join(session_root, path);
            if resolved.starts_with(session_root) {
                continue;
            }
            for graded in &self.graded_roots {
                if resolved.starts_with(graded) {
                    let substitute = resolved
                        .strip_prefix(graded)
                        .ok()
                        .map(|rel| session_root.join(rel));
                    return Err(Refusal::GradedTree {
                        literal: word,
                        graded_root: reported.to_path_buf(),
                        substitute,
                    });
                }
            }
        }
        Ok(())
    }
}

/// The first ref in [`STELLA_REF_NAMESPACE`] the command names, read straight
/// out of the text: the namespace is distinctive enough that no boundary test
/// is needed, and every way of reaching it — plumbing, porcelain, a config
/// file written by the command — spells it the same.
fn stella_ref_named(command: &str) -> Option<String> {
    let at = command.find(STELLA_REF_NAMESPACE)?;
    let rest = &command[at..];
    let end = rest.find(|c: char| !is_path_char(c)).unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

/// The first path literal under `graded_root` the command names.
///
/// Matched at path boundaries in both directions, so `/application` does not
/// match a graded `/app` and `/tmp/app` does not match one either. A literal
/// that is also under `session_root` is this shell's own file — that only
/// arises if a host nests the two, and it must not be refused.
fn graded_literal(command: &str, graded_root: &Path, session_root: &Path) -> Option<String> {
    let needle = graded_root.to_str()?;
    let mut from = 0usize;
    while let Some(offset) = command[from..].find(needle) {
        let at = from + offset;
        let end = at + needle.len();
        let before_ok = !command[..at].chars().next_back().is_some_and(is_path_char);
        let after = command[end..].chars().next();
        let after_ok = after == Some('/') || !after.is_some_and(is_path_char);
        if before_ok && after_ok {
            let rest = &command[at..];
            let len = rest.find(|c: char| !is_path_char(c)).unwrap_or(rest.len());
            let literal = &rest[..len];
            if !Path::new(literal).starts_with(session_root) {
                return Some(literal.to_string());
            }
        }
        from = end;
    }
    None
}

/// The object-destroying git subcommand the command runs, if any.
///
/// Scans for a `git` word and reads the first word after it that is not a
/// global flag, which is where every spelling — `git gc`, `git -C dir gc`,
/// `git -c gc.auto=0 gc` — puts the subcommand. `reflog` counts only with a
/// destructive verb after it, because `git reflog` alone is a read.
fn history_destroying_git(command: &str) -> Option<String> {
    let words = super::shell_words(command);
    for (i, word) in words.iter().enumerate() {
        if word != "git" && !word.ends_with("/git") {
            continue;
        }
        let mut rest = words[i + 1..].iter();
        let mut subcommand = None;
        while let Some(next) = rest.next() {
            if matches!(next.as_str(), ";" | "&" | "&&" | "|" | "||") {
                break;
            }
            if matches!(next.as_str(), "-C" | "-c" | "--git-dir" | "--work-tree") {
                // These take a separate argument; skip it too.
                rest.next();
                continue;
            }
            if next.starts_with('-') {
                continue;
            }
            subcommand = Some(next.clone());
            break;
        }
        let Some(subcommand) = subcommand else {
            continue;
        };
        if HISTORY_DESTROYING.contains(&subcommand.as_str()) {
            return Some(subcommand);
        }
        if subcommand == "reflog" && rest.any(|w| matches!(w.as_str(), "expire" | "delete")) {
            return Some("reflog expire".to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate() -> (ShellConfinement, PathBuf) {
        (
            ShellConfinement::graded_tree("/app"),
            PathBuf::from("/tmp/stella_candidate_63_0"),
        )
    }

    /// The namespace constant and the ref `verify_done` actually pins must not
    /// drift apart: this guard is worthless if it protects a prefix nothing
    /// uses.
    #[test]
    fn the_protected_namespace_covers_the_witness_baseline_ref() {
        assert!(
            crate::verify::WITNESS_BASELINE_WORKTREE_REF.starts_with(STELLA_REF_NAMESPACE),
            "{} must live under {STELLA_REF_NAMESPACE}",
            crate::verify::WITNESS_BASELINE_WORKTREE_REF
        );
    }

    /// The `code-from-image` shape, in every spelling that reached it.
    #[test]
    fn writing_stellas_own_ref_is_refused_confined_or_not() {
        let root = Path::new("/tmp/ws");
        for command in [
            "git update-ref refs/worktree/stella/witness-baseline 93f67c6",
            "git symbolic-ref refs/worktree/stella/witness-baseline refs/heads/main",
            "git update-ref -d refs/worktree/stella/witness-baseline",
            "echo 93f67c6 > .git/refs/worktree/stella/witness-baseline",
        ] {
            let refusal = ShellConfinement::unconfined()
                .audit(command, root)
                .expect_err(command);
            assert!(
                matches!(refusal, Refusal::StellaRef { .. }),
                "{command}: {refusal:?}"
            );
        }
        // An ordinary branch is the task's business, not Stella's.
        assert!(
            ShellConfinement::unconfined()
                .audit("git update-ref refs/heads/sol-branch 930948dd", root)
                .is_ok()
        );
    }

    #[test]
    fn object_destroying_git_is_refused_inside_a_candidate_only() {
        let (confined, root) = candidate();
        for command in [
            "git gc --prune=now",
            "git -C . gc",
            "git reflog expire --expire=now --all",
            "git prune",
        ] {
            assert!(
                matches!(
                    confined.audit(command, &root),
                    Err(Refusal::HistoryDestroying { .. })
                ),
                "{command}"
            );
            // Unconfined, this is the user's own repository to maintain.
            assert!(
                ShellConfinement::unconfined().audit(command, &root).is_ok(),
                "{command}"
            );
        }
        // Reads are untouched.
        assert!(confined.audit("git reflog", &root).is_ok());
        assert!(confined.audit("git log --oneline", &root).is_ok());
    }

    #[test]
    fn a_graded_tree_path_is_refused_in_every_position_it_was_observed_in() {
        let (confined, root) = candidate();
        for command in [
            "echo today,ERROR,414 > /app/summary.csv",
            "rm -rf /app/pyknotid",
            "cd /app && make",
            "cp -a povray-2.2 /app/povray-2.2",
            // The heredoc body that word-splitting alone cannot see.
            "python3 - <<'EOF'\nwith open('/app/summary.csv', 'w') as f:\n    f.write('x')\nEOF",
        ] {
            let refusal = confined.audit(command, &root).expect_err(command);
            assert!(
                matches!(refusal, Refusal::GradedTree { .. }),
                "{command}: {refusal:?}"
            );
        }
    }

    /// The legitimate case the posture is built around: a task that installs
    /// and configures system software is doing the job, not escaping.
    #[test]
    fn system_paths_outside_the_graded_tree_stay_writable() {
        let (confined, root) = candidate();
        for command in [
            "apt-get install -y nginx",
            "cp nginx.conf /etc/nginx/sites-enabled/default",
            "mkdir -p /var/www/html && echo hi > /var/www/html/index.html",
            "install -m 755 server /usr/local/bin/server",
            "echo x > /tmp/scratch.txt",
            "cd /tmp/stella_candidate_63_0/sub && ls",
        ] {
            assert!(confined.audit(command, &root).is_ok(), "{command}");
        }
    }

    /// Boundary discipline in both directions — a refusal on a path that only
    /// looks like the graded tree would be the false accusation #2058 spent a
    /// benchmark cycle on.
    #[test]
    fn a_path_that_merely_resembles_the_graded_tree_is_not_refused() {
        let (confined, root) = candidate();
        for command in [
            "ls /application/config",
            "cat /tmp/app/notes",
            "echo /appendix",
            "grep -rn app .",
        ] {
            assert!(confined.audit(command, &root).is_ok(), "{command}");
        }
    }

    #[test]
    fn a_relative_climb_into_the_graded_tree_is_refused() {
        let confined = ShellConfinement::graded_tree("/srv/graded");
        let root = Path::new("/srv/graded_candidate/ws");
        let refusal = confined
            .audit("cp answer.txt ../../graded/answer.txt", root)
            .expect_err("a climb into the graded tree");
        assert!(matches!(refusal, Refusal::GradedTree { .. }), "{refusal:?}");
        // A climb that stays outside the graded tree is ordinary.
        assert!(confined.audit("ls ../..", root).is_ok());
    }

    #[test]
    fn the_refusal_names_the_substitute_inside_this_workspace() {
        let (confined, root) = candidate();
        let refusal = confined
            .audit("echo 370 > /app/summary.csv", &root)
            .expect_err("graded write");
        let Refusal::GradedTree {
            literal,
            substitute,
            ..
        } = &refusal
        else {
            panic!("{refusal:?}");
        };
        assert_eq!(literal, "/app/summary.csv");
        assert_eq!(
            substitute.as_deref(),
            Some(Path::new("/tmp/stella_candidate_63_0/summary.csv"))
        );
        let message = refusal.to_string();
        assert!(
            message.contains("/tmp/stella_candidate_63_0/summary.csv"),
            "{message}"
        );
        assert!(message.contains("delivered back to it"), "{message}");
    }

    /// One directory has several names, and a text audit compares names. The
    /// macOS `/var` → `/private/var` symlink is the case that caught this in
    /// development: the guard knew only the canonical spelling, and the
    /// end-to-end candidate test wrote the graded tree straight through it.
    #[test]
    fn every_registered_spelling_of_the_graded_tree_refuses_identically() {
        let confined = ShellConfinement::graded_tree("/private/var/graded")
            .and_alias("/var/graded")
            .and_alias("/private/var/graded");
        let root = Path::new("/tmp/ws");
        for command in [
            "echo x > /private/var/graded/out.txt",
            "echo x > /var/graded/out.txt",
        ] {
            let refusal = confined.audit(command, root).expect_err(command);
            let Refusal::GradedTree {
                graded_root,
                substitute,
                ..
            } = &refusal
            else {
                panic!("{command}: {refusal:?}");
            };
            // Reported against one root whichever alias matched, so two
            // refusals of the same tree read the same.
            assert_eq!(graded_root, Path::new("/private/var/graded"), "{command}");
            assert_eq!(substitute.as_deref(), Some(Path::new("/tmp/ws/out.txt")));
        }
        // Aliases are deduplicated, and one that would protect everything is
        // dropped rather than stored as a root that matches every path.
        let noisy = ShellConfinement::graded_tree("/graded")
            .and_alias("/graded")
            .and_alias("/")
            .and_alias("relative");
        assert!(noisy.audit("echo x > /etc/hosts", root).is_ok());
    }

    /// A blanket root would make every absolute path a refusal — the failure
    /// mode this module is defined against.
    #[test]
    fn a_root_that_would_protect_everything_does_not_arm() {
        assert_eq!(ShellConfinement::graded_tree("/").graded_root(), None);
        assert_eq!(
            ShellConfinement::graded_tree("relative").graded_root(),
            None
        );
        assert_eq!(
            ShellConfinement::graded_tree("/app").graded_root(),
            Some(Path::new("/app"))
        );
    }

    /// A host that arms the protection against the shell's own root gets the
    /// ordinary session, not a shell that cannot name its own files.
    #[test]
    fn a_graded_root_equal_to_the_session_root_is_inert() {
        let confined = ShellConfinement::graded_tree("/app");
        assert!(
            confined
                .audit("echo x > /app/out.txt", Path::new("/app"))
                .is_ok()
        );
    }
}
