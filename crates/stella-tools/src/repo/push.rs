// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `repo_push` — what a push carries, and what to do when the repository's
//! own hooks are slower than the push.
//!
//! Split out of [`super`] (which is at its size ceiling) because the tool grew
//! two concerns beyond "run git push":
//!
//! # 1. A push has contents, and the caller could not see them
//!
//! The old tool answered `{}` and returned git's terse `To <url>` line. So the
//! transcript row for the single most consequential action of a session — the
//! moment work leaves the machine — said less than a `read_file`. [`PushPlan`]
//! is what the push would carry (branch, upstream, the commits, the files and
//! their line counts), derived from the same reads the hook decision below
//! needs anyway, so the richer answer costs nothing extra.
//!
//! # 2. A pre-push hook can be slower than any timeout worth having
//!
//! This repository's own `pre-push` hook runs `make gate` — the whole test
//! suite. A push through the tool therefore *always* hit the 300s ceiling and
//! reported `repo_push failed`, and the agent's only way through was to fall
//! back to `bash` and hand-write `git push --no-verify`. That is the shape of
//! a tool that has been defeated: the capability still exists, but only
//! outside the surface that is supposed to govern it, where no policy sees it.
//!
//! So the tool takes `skip_hooks` — and **decides for itself whether the
//! caller may use it**, from the diff rather than from the caller's say-so:
//!
//! - A push whose commits change **no source file** may skip hooks. The hooks
//!   a repository installs on push exist to gate code; a push that moves only
//!   documentation, data, fixtures or deletions of generated artifacts has
//!   nothing for them to gate, and paying a full test suite for it is the
//!   waste this exists to end. This is the case the request came from: a
//!   commit that deleted 42,000 benchmark result files.
//! - A push that **does** change source may not, whatever it passes. Silently
//!   letting an agent skip the gate on real code is the one outcome worth more
//!   than the time it saves, and `skip_hooks: true` there is refused with the
//!   reason rather than downgraded to a warning nobody reads.
//!
//! The refusal is not a dead end: it names how long the hook ran and what the
//! push actually contained, which is what lets the caller decide between
//! waiting, narrowing the push, and fixing the hook.

use std::path::Path;

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use stella_protocol::tool::{ToolOutput, ToolSchema};

use super::{CommitRef, DiffFileStat, RepoBackend, RepoError, valid_branch_name};
use crate::registry::Tool;

/// Commits named in a push summary at most. A push of five hundred commits is
/// not one whose subjects anyone reads; the count above them is the answer.
pub(super) const MAX_PUSH_COMMITS: usize = 20;

/// Files named in a push summary at most, for the same reason.
pub(super) const MAX_PUSH_FILES: usize = 40;

/// Everything a push would carry, read before it runs.
///
/// Derived once and used twice — for the summary the caller reads and for the
/// hook decision — so the "may this skip hooks" answer and the "what is in
/// this push" answer can never describe different pushes.
#[derive(Debug, Clone, Default)]
pub struct PushPlan {
    /// Commits this push would publish, newest first, capped at
    /// `MAX_PUSH_COMMITS`.
    pub commits: Vec<CommitRef>,
    /// How many commits there are in total (`commits` may be capped).
    pub commit_count: usize,
    /// Per-file line stats across the whole push, capped at
    /// `MAX_PUSH_FILES`.
    pub files: Vec<DiffFileStat>,
    /// How many files there are in total (`files` may be capped).
    pub file_count: usize,
    /// Whether the remote already has this branch. A first push is worth
    /// saying: it is the one that creates a ref, and the one whose "0 commits
    /// behind" reads differently.
    pub new_branch: bool,
    /// The remote URL, when there is one.
    pub remote: Option<String>,
}

impl PushPlan {
    /// Total lines added and removed across every file (binary files count
    /// zero on both sides — they have no lines).
    pub fn totals(&self) -> (u64, u64) {
        self.files.iter().fold((0, 0), |(a, r), f| {
            (a + f.added.unwrap_or(0), r + f.removed.unwrap_or(0))
        })
    }

    /// Whether any path in this push is a **source file** — the thing a
    /// push-time hook exists to gate. See `path_is_source` for the rules and
    /// why they are inclusive by default.
    pub fn changes_source(&self) -> bool {
        self.files.iter().any(|f| path_is_source(&f.path))
    }

    /// The source paths this push carries, capped for a refusal message —
    /// naming three is what turns "this push changes source" from an assertion
    /// into something the caller can check.
    pub fn source_paths(&self) -> Vec<&str> {
        self.files
            .iter()
            .filter(|f| path_is_source(&f.path))
            .map(|f| f.path.as_str())
            .take(3)
            .collect()
    }

    /// The human summary that rides a successful push.
    pub fn render(&self, branch: &str, skipped_hooks: bool) -> String {
        let (added, removed) = self.totals();
        let mut out = format!("pushed `{branch}`");
        if self.new_branch {
            out.push_str(" (new remote branch)");
        }
        if let Some(remote) = &self.remote {
            out.push_str(&format!(" → {remote}"));
        }
        out.push('\n');
        out.push_str(&format!(
            "{} commit{}, {} file{} (+{added} −{removed})\n",
            self.commit_count,
            plural(self.commit_count),
            self.file_count,
            plural(self.file_count),
        ));
        if skipped_hooks {
            out.push_str("repository hooks were SKIPPED — this push changes no source file\n");
        }
        for commit in &self.commits {
            out.push_str(&format!("  {} {}\n", commit.commit, commit.subject));
        }
        if self.commit_count > self.commits.len() {
            out.push_str(&format!(
                "  … {} more commit(s)\n",
                self.commit_count - self.commits.len()
            ));
        }
        for file in &self.files {
            match (file.added, file.removed) {
                (Some(a), Some(r)) => out.push_str(&format!("  {} +{a} -{r}\n", file.path)),
                _ => out.push_str(&format!("  {} (binary)\n", file.path)),
            }
        }
        if self.file_count > self.files.len() {
            out.push_str(&format!(
                "  … {} more file(s)\n",
                self.file_count - self.files.len()
            ));
        }
        out
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Extensions whose files a push-time hook has nothing to gate, wherever they
/// sit: prose, tabular data, line-oriented logs, images, fonts, media and
/// archives.
///
/// Deliberately short. Every entry is a promise that a repository's own test
/// suite has no opinion about this file, and the cost of being wrong is a
/// skipped gate — so a format that is *sometimes* configuration does not
/// belong here. Those go to [`STRUCTURED_EXTENSIONS`], which answers by
/// position instead.
const NON_SOURCE_EXTENSIONS: &[&str] = &[
    "md", "markdown", "txt", "rst", "adoc", "csv", "tsv", "jsonl", "ndjson", "log", "png", "jpg",
    "jpeg", "gif", "svg", "webp", "ico", "pdf", "zip", "gz", "tar", "bz2", "zst", "woff", "woff2",
    "ttf", "otf", "mp4", "mov", "webm", "wav", "mp3",
];

/// Formats that are configuration where a build reads them and data
/// everywhere else.
///
/// `package.json` is the thing a gate exists for; `bench/run-4711/result.json`
/// is an artifact, and 42,000 of them are the push that motivated this module.
/// The extension cannot tell them apart, so [`path_is_source`] asks *where the
/// file sits* — see [`SHALLOW_IS_CONFIG_DEPTH`].
const STRUCTURED_EXTENSIONS: &[&str] = &["json", "yaml", "yml", "toml", "ini", "cfg", "xml"];

/// How deep a [`STRUCTURED_EXTENSIONS`] file may sit and still be read as
/// configuration: at the repository root or one directory beneath it.
///
/// That is where a build looks — `package.json`, `deny.toml`,
/// `web/tsconfig.json` — and it is not where anything generates thousands of
/// files. Deeper than this, a structured file is configuration only if
/// [`MANIFEST_NAMES`] or a dot-directory says so.
const SHALLOW_IS_CONFIG_DEPTH: usize = 1;

/// Basenames that are configuration at any depth. A monorepo's per-crate
/// `Cargo.toml` and per-package `package.json` sit well below
/// [`SHALLOW_IS_CONFIG_DEPTH`], and every one of them is exactly what a gate
/// is for.
const MANIFEST_NAMES: &[&str] = &[
    "cargo.toml",
    "cargo.lock",
    "rust-toolchain.toml",
    "deny.toml",
    "clippy.toml",
    "rustfmt.toml",
    "package.json",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lockb",
    "tsconfig.json",
    "jsconfig.json",
    "pyproject.toml",
    "poetry.lock",
    "uv.lock",
    "setup.cfg",
    "setup.py",
    "requirements.txt",
    "go.mod",
    "go.sum",
    "gemfile",
    "gemfile.lock",
    "pom.xml",
    "build.gradle",
    "makefile",
    "dockerfile",
    "justfile",
];

/// Whether one path counts as source for the hook decision.
///
/// **Inclusive by default** — a path none of the rules below recognises as
/// inert is source. That direction is the whole design: a wrong "not source"
/// answer takes real code past the repository's own gate, while a wrong
/// "source" answer costs one test run.
///
/// In order:
/// 1. A **manifest basename** is configuration at any depth.
/// 2. Anything under a **dot-directory** (`.github/`, `.cargo/`, `.stella/`)
///    is configuration — that is what a leading dot means in a repository.
/// 3. A path with **no extension**, or a bare dotfile (`.gitignore`, `.env`),
///    is source: `Makefile`, `Dockerfile` and an extensionless shell script
///    all land here.
/// 4. A [`NON_SOURCE_EXTENSIONS`] file is inert wherever it sits.
/// 5. A [`STRUCTURED_EXTENSIONS`] file is configuration when it sits shallow
///    enough for a build to read it, and data when it does not.
/// 6. Everything else is source.
fn path_is_source(path: &str) -> bool {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let name = segments.last().copied().unwrap_or(path);
    if MANIFEST_NAMES.contains(&name.to_ascii_lowercase().as_str()) {
        return true;
    }
    // Every segment but the basename: a leading dot on the FILE is rule 3's
    // business, and `.gitignore` at the root must not read as a directory.
    if segments
        .iter()
        .rev()
        .skip(1)
        .any(|segment| segment.starts_with('.'))
    {
        return true;
    }
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return true;
    };
    if stem.is_empty() {
        return true;
    }
    let ext = ext.to_ascii_lowercase();
    if NON_SOURCE_EXTENSIONS.contains(&ext.as_str()) {
        return false;
    }
    if STRUCTURED_EXTENSIONS.contains(&ext.as_str()) {
        return segments.len().saturating_sub(1) <= SHALLOW_IS_CONFIG_DEPTH;
    }
    true
}

/// `repo_push` — never the default branch, never forced; see [`super`]'s
/// module doc for the structural rules and this module's for the hook policy.
pub struct RepoPush(pub Arc<dyn RepoBackend>);

#[async_trait]
impl Tool for RepoPush {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "repo_push".into(),
            description: "Push the current (or named) branch to the primary remote and report \
                          what went: the branch, the remote, every commit published, and the \
                          files with their line counts. STRUCTURALLY refuses the repository's \
                          default branch — publish work on a feature branch. Force-push does \
                          not exist here. Set `skip_hooks` when a repository's pre-push hook \
                          is slower than the push is worth (a full test suite on a docs-only \
                          change); the tool checks the push's own diff and REFUSES to skip \
                          them when it carries any source file, so this can never take a code \
                          change past the repository's gate."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "branch": {
                        "type": "string",
                        "description": "Branch to push (default: the current branch)"
                    },
                    "skip_hooks": {
                        "type": "boolean",
                        "description": "Run the push without the repository's pre-push hooks. \
                                        Granted only when the push changes no source file — \
                                        otherwise the call is refused and says which files \
                                        made it source. Use it when a hook would run a full \
                                        test suite over a change that has no code in it."
                    }
                }
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, root: &Path) -> ToolOutput {
        let named = input.get("branch").and_then(|v| v.as_str());
        if let Some(branch) = named
            && !valid_branch_name(branch)
        {
            return ToolOutput::Error { class: None,
                message: format!(
                    "`{branch}` is not a valid branch name (must not start with `-` or \
                     contain whitespace)"
                ),
            };
        }
        let branch = match named {
            Some(b) => b.to_string(),
            None => match self.0.current_branch(root).await {
                Ok(Some(b)) => b,
                Ok(None) => {
                    return ToolOutput::Error { class: None,
                        message: "the checkout is detached (no current branch) — pass \
                                  `branch` explicitly"
                            .into(),
                    };
                }
                Err(e) => {
                    return ToolOutput::Error { class: None,
                        message: e.to_string(),
                    };
                }
            },
        };
        // The structural rule: resolve the default branch and refuse it.
        // An UNRESOLVABLE default fails closed — pushing blind could be
        // pushing the default.
        match self.0.default_branch(root).await {
            Ok(Some(default)) if default == branch => {
                return ToolOutput::Error { class: None,
                    message: format!(
                        "repo_push refuses to push `{branch}`: it is the repository's \
                         default branch. Publish work on a feature branch instead — this \
                         rule is structural and has no override"
                    ),
                };
            }
            Ok(Some(_)) => {}
            Ok(None) => {
                return ToolOutput::Error { class: None,
                    message: "cannot determine the repository's default branch (remote \
                              HEAD) — refusing to push rather than risk pushing it"
                        .into(),
                };
            }
            Err(e) => {
                return ToolOutput::Error { class: None,
                    message: e.to_string(),
                };
            }
        }

        // Read what the push carries BEFORE running it: the summary needs it,
        // and so does the hook decision. A plan that cannot be read degrades to
        // the empty one — which reports nothing and, because
        // `changes_source()` is then false, would be the one way to talk the
        // tool into skipping hooks on an unknown push. So the skip decision
        // requires a plan that actually saw commits.
        let plan = self.0.push_plan(root, &branch).await.unwrap_or_default();

        let asked_to_skip = input
            .get("skip_hooks")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let skip_hooks = if asked_to_skip {
            match hook_skip_permitted(&plan) {
                Ok(()) => true,
                Err(reason) => return ToolOutput::Error { class: None, message: reason },
            }
        } else {
            false
        };

        match self.0.push_branch(root, &branch, skip_hooks).await {
            Ok(out) => ToolOutput::Ok {
                content: format!("{}{}", plan.render(&branch, skip_hooks), out.trim()),
            },
            Err(e) => ToolOutput::Error { class: None,
                message: push_failure(&e, &plan, skip_hooks),
            },
        }
    }

    // See `RepoStatusTool::command_for_gate`. Without a named branch the
    // refspec resolves from the checkout mid-execute; the gate then sees the
    // line minus the refspec — still enough for a policy denying pushes.
    async fn command_for_gate(&self, input: &Value, _root: &Path) -> Option<String> {
        let skip = if input
            .get("skip_hooks")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            " --no-verify"
        } else {
            ""
        };
        Some(match input.get("branch").and_then(|v| v.as_str()) {
            Some(branch) => format!(
                "git push{skip} --set-upstream origin refs/heads/{branch}:refs/heads/{branch}"
            ),
            None => format!("git push{skip} --set-upstream origin"),
        })
    }
}

/// Whether this push may run without the repository's hooks.
///
/// `Err` carries the whole refusal message, because a refusal that only says
/// "no" makes the caller guess which of the two reasons it hit.
fn hook_skip_permitted(plan: &PushPlan) -> Result<(), String> {
    if plan.commit_count == 0 {
        return Err(
            "refusing `skip_hooks`: this push's contents could not be read, so there is no \
             evidence it is free of source changes. Push without `skip_hooks`, or name the \
             branch explicitly if it has no upstream yet."
                .to_string(),
        );
    }
    if plan.changes_source() {
        let named = plan.source_paths();
        return Err(format!(
            "refusing `skip_hooks`: this push changes {} source file(s) — including {} — and \
             the repository's pre-push hooks exist to gate exactly that. `skip_hooks` is for a \
             push with no code in it (docs, data, fixtures, generated artifacts). If the hook \
             is too slow for a real code change, that is a problem with the hook, not with \
             this push.",
            plan.files
                .iter()
                .filter(|f| path_is_source(&f.path))
                .count(),
            named.join(", "),
        ));
    }
    Ok(())
}

/// The failure message, with the one piece of advice that is actually
/// actionable when a hook ate the timeout.
///
/// A bare `repo_push failed: … timed out after 300s` is where the agent that
/// motivated this module gave up on the tool and re-implemented the push in
/// `bash`. Naming the likely cause and the exact way through keeps the
/// capability inside the surface that governs it.
fn push_failure(error: &RepoError, plan: &PushPlan, skipped_hooks: bool) -> String {
    let base = error.to_string();
    let timed_out = base.contains("timed out") || base.contains("timeout");
    if !timed_out || skipped_hooks {
        return base;
    }
    let (added, removed) = plan.totals();
    let mut out = format!(
        "{base}\n\nThe push itself is rarely what takes that long: a repository `pre-push` \
         hook runs before git contacts the remote, and a hook that runs the test suite can \
         outlast any timeout worth having. This push carries {} commit(s) and {} file(s) \
         (+{added} −{removed}).",
        plan.commit_count, plan.file_count
    );
    if plan.commit_count > 0 && !plan.changes_source() {
        out.push_str(
            "\n\nNone of those files is source, so this push has nothing for a code gate to \
             check: retry with `skip_hooks: true`.",
        );
    } else {
        out.push_str(
            "\n\nThis push does change source, so skipping the hooks is not available for it \
             — the gate is the thing that would have caught a break. Narrow the push, or fix \
             the hook so it is affordable.",
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(path: &str) -> DiffFileStat {
        DiffFileStat {
            path: path.into(),
            added: Some(1),
            removed: Some(0),
        }
    }

    fn plan_of(paths: &[&str]) -> PushPlan {
        PushPlan {
            commit_count: 1,
            commits: vec![CommitRef {
                commit: "abc1234".into(),
                subject: "chore: drop the match results".into(),
            }],
            file_count: paths.len(),
            files: paths.iter().map(|p| stat(p)).collect(),
            ..PushPlan::default()
        }
    }

    /// The case this module was built for: a push that deletes tens of
    /// thousands of benchmark result files has nothing for a code gate to
    /// check, and paying a full test suite for it is what made every push of
    /// that shape time out.
    #[test]
    fn a_push_with_no_code_in_it_may_skip_the_hooks() {
        let plan = plan_of(&[
            "arenabench/arenabench-cloud/f89p3/000/result.json",
            "arenabench/arenabench-cloud/f89p3/000/trace.jsonl",
            "docs/bench.md",
            "website/public/og.png",
        ]);
        assert!(!plan.changes_source(), "{plan:?}");
        assert!(hook_skip_permitted(&plan).is_ok());
    }

    /// And the half that matters more. Letting an agent take real code past
    /// the repository's own gate is worth more than the time it saves, so the
    /// request is refused rather than downgraded to a warning.
    #[test]
    fn a_push_that_changes_source_may_never_skip_the_hooks() {
        for path in [
            "crates/stella-tools/src/repo.rs",
            "scripts/check-gate-parity.sh",
            "Cargo.toml",
            "Cargo.lock",
            ".github/workflows/ci.yml",
            "deny.toml",
            "Makefile",
            "Dockerfile",
            ".gitignore",
            "package.json",
            "app/page.tsx",
            "tests/fixtures/case.py",
        ] {
            let plan = plan_of(&[path]);
            assert!(plan.changes_source(), "`{path}` must count as source");
            let refusal = hook_skip_permitted(&plan).expect_err(path);
            assert!(
                refusal.contains(path),
                "the refusal must name what made it source: {refusal}"
            );
        }
    }

    /// An unreadable plan is not an excuse. `changes_source()` is false for
    /// an empty file list, so without this the one way to talk the tool into
    /// skipping hooks would be to make it unable to see the push.
    #[test]
    fn a_push_whose_contents_could_not_be_read_may_not_skip_the_hooks() {
        let refusal = hook_skip_permitted(&PushPlan::default()).expect_err("must refuse");
        assert!(refusal.contains("could not be read"), "{refusal}");
    }

    /// A timeout has to say what to do next. The agent that motivated this
    /// module read `repo_push failed: … timed out after 5m00s`, concluded the
    /// tool was unusable, and hand-wrote `git push --no-verify` through
    /// `bash` — outside every policy that governs pushing.
    #[test]
    fn a_timeout_names_the_hook_and_the_way_through() {
        let timeout = RepoError::Failed {
            action: "repo_push",
            detail: "`git push --set-upstream origin refs/heads/x:refs/heads/x` timed out \
                     after 300s"
                .into(),
        };
        let docs_only = push_failure(&timeout, &plan_of(&["docs/a.md"]), false);
        assert!(docs_only.contains("pre-push"), "{docs_only}");
        assert!(docs_only.contains("skip_hooks: true"), "{docs_only}");

        let with_code = push_failure(&timeout, &plan_of(&["src/a.rs"]), false);
        assert!(
            !with_code.contains("skip_hooks: true"),
            "a code push must not be told to skip the gate: {with_code}"
        );
        assert!(with_code.contains("fix the hook"), "{with_code}");

        // A failure that is not a timeout keeps its own words, and so does a
        // push that ALREADY skipped the hooks — there is no hook left to blame.
        let other = RepoError::Failed {
            action: "repo_push",
            detail: "exit 1: rejected (non-fast-forward)".into(),
        };
        assert_eq!(
            push_failure(&other, &plan_of(&["docs/a.md"]), false),
            other.to_string()
        );
        assert_eq!(
            push_failure(&timeout, &plan_of(&["docs/a.md"]), true),
            timeout.to_string()
        );
    }

    /// The summary the caller reads. The old tool answered git's `To <url>`
    /// line and nothing else, so a transcript row for the moment work leaves
    /// the machine said less than a `read_file` did.
    #[test]
    fn the_summary_names_the_branch_the_commits_and_the_files() {
        let mut plan = plan_of(&["docs/a.md", "docs/b.md"]);
        plan.new_branch = true;
        plan.remote = Some("git@github.com:macanderson/stella.git".into());
        let rendered = plan.render("chore/drop-results", true);
        assert!(rendered.contains("chore/drop-results"), "{rendered}");
        assert!(rendered.contains("new remote branch"), "{rendered}");
        assert!(rendered.contains("github.com"), "{rendered}");
        assert!(rendered.contains("1 commit, 2 files"), "{rendered}");
        assert!(rendered.contains("+2 −0"), "{rendered}");
        assert!(
            rendered.contains("chore: drop the match results"),
            "{rendered}"
        );
        assert!(rendered.contains("docs/a.md +1 -0"), "{rendered}");
        assert!(rendered.contains("hooks were SKIPPED"), "{rendered}");
        // …and says nothing about hooks when it ran them.
        assert!(!plan.render("b", false).contains("SKIPPED"));
    }

    /// The default direction of the source test: a path nobody classified is
    /// source. A wrong "not source" skips the repository's gate on real code;
    /// a wrong "source" costs a test run.
    #[test]
    fn an_unrecognised_extension_counts_as_source() {
        assert!(path_is_source("weird/thing.qqq"));
        assert!(path_is_source("no_extension_at_all"));
        assert!(path_is_source("src/.hidden"));
        assert!(!path_is_source("README.MD"), "the test folds case");
    }

    /// The awkward middle: one extension, two meanings. `package.json` is the
    /// thing a gate exists for; `bench/run-4711/000/result.json` is an
    /// artifact, and forty thousand of them are the push this module was
    /// built for. Position is what tells them apart — with a manifest-name
    /// list and the dot-directory rule catching the cases position gets
    /// wrong.
    #[test]
    fn a_structured_file_is_configuration_where_a_build_reads_it_and_data_elsewhere() {
        for config in [
            "package.json",
            "deny.toml",
            "web/tsconfig.json",
            "crates/stella-tools/Cargo.toml",
            "packages/ui/package.json",
            ".github/workflows/ci.yml",
            ".cargo/config.toml",
            ".stella/rules/context.toml",
            "docker-compose.yml",
        ] {
            assert!(
                path_is_source(config),
                "`{config}` is configuration a gate exists for"
            );
        }
        for data in [
            "arenabench/arenabench-cloud/f89p3/000/result.json",
            "bench/matches/run-4711/summary.json",
            "tests/fixtures/deep/nested/case.yaml",
        ] {
            assert!(
                !path_is_source(data),
                "`{data}` sits where nothing builds from it"
            );
        }
    }
}
