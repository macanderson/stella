//! The creation-task exception to the import/loader refusal (#2668).
//!
//! [`super::instrument::import_failure_signature`] states the general rule: a
//! previous-code run that dies while *loading* the test — an import, not an
//! assertion — is not evidence of a flip, because a build artifact present in
//! the working tree and absent from the fresh shadow checkout produces the
//! same text (#2067).
//!
//! That rule has a blind spot on **creation** tasks, which are most of what a
//! coding agent is asked to do: the deliverable does not exist at the
//! baseline *by construction*, so every witness for it either passes there
//! (`VACUOUS`) or dies on import (`WITNESS_INCONCLUSIVE_BUILD_ERROR`). The
//! gate can then never say yes, however correct the work is. Measured on
//! 2026-08-10: 30-70% of a trial's wall budget spent re-entering it, and on
//! `pypi-server__HRUntZQ` an agent twice ran `stop_process` on the graded
//! server — destroying the deliverable to manufacture a failing baseline.
//!
//! # What distinguishes the two, mechanically
//!
//! The failure text cannot tell them apart, and a self-report ("this is a
//! creation task") is not evidence. Git can: **is the file the loader named
//! absent at the baseline commit and present now?** A genuine deliverable
//! always is. A stale artifact never is *in the sense that matters*, and the
//! two ways one could sneak in are both closed here:
//!
//! - gitignored artifacts (`target/`, `node_modules/`, a `.so` beside a
//!   `.gitignore`) never appear, because [`added_since_baseline`] asks git
//!   with `--exclude-standard`;
//! - an artifact that is merely *un*ignored is rejected by name — see
//!   [`is_build_product`], which encodes #2067's own list. A witness must be
//!   flipped by source the change adds, never by a build product.
//!
//! Every uncertainty resolves toward the existing refusal: an unparseable
//! message, a git call that fails, a name that matches nothing, or a match
//! that is a build product all return `None`, and the caller's
//! `WITNESS_INCONCLUSIVE_BUILD_ERROR` stands exactly as it did before.

use std::path::Path;

/// Compiled outputs that must never decide a verdict, by extension. This is
/// #2067's hazard stated as data: the false positive it fixed was a compiled
/// `.so` present in the working tree and absent from a fresh checkout, which
/// would otherwise read here as "a file the change adds".
const BUILD_PRODUCT_EXTENSIONS: &[&str] = &[
    "so", "dylib", "dll", "pyd", "a", "o", "obj", "class", "jar", "wasm", "node",
];

/// Directories whose contents are built or vendored rather than authored.
/// Over-inclusive on purpose (`build`, `dist` and `out` are occasionally
/// source directories): a wrong entry costs a creation-task witness the
/// exception and leaves the old refusal, while a missing entry would let an
/// artifact confirm a proof.
const BUILD_PRODUCT_DIRECTORIES: &[&str] = &[
    "node_modules",
    "target",
    "build",
    "dist",
    "out",
    "__pycache__",
    "site-packages",
    ".venv",
    "venv",
    ".tox",
];

/// Is `path` a build product rather than authored source?
fn is_build_product(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let extension_is_built = Path::new(&lower)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| BUILD_PRODUCT_EXTENSIONS.contains(&e));
    extension_is_built
        || lower
            .split('/')
            .any(|segment| BUILD_PRODUCT_DIRECTORIES.contains(&segment))
}

/// The module the previous-code run could not find, normalised to a
/// slash-separated path with no extension — the form [`module_keys`] answers
/// in.
///
/// The two dialects are parsed separately rather than by one shared rule,
/// because their punctuation means opposite things: a Python dot is a path
/// separator (`pkg.mod` is `pkg/mod`), while a Node dot introduces a file
/// extension (`./utils.js` is `utils`). One normalisation for both is wrong
/// for both. Families whose text names no single identifier — the
/// shared-library and pytest-collection signatures — are deliberately absent:
/// they name a build product or a directory, and neither is a file a change
/// adds.
fn missing_module(output: &str) -> Option<String> {
    if let Some(raw) = quoted_after(output, "no module named") {
        return python_module_path(&raw);
    }
    quoted_after(output, "cannot find module").and_then(|raw| node_module_path(&raw))
}

/// The first `'…'`- or `"…"`-quoted token after (case-insensitively)
/// `marker`. Both dialects quote the name they could not resolve, and
/// requiring the quotes is what keeps this a parser rather than a guess: an
/// unquoted variant yields `None`, which withholds the exception instead of
/// matching on whatever word happened to follow.
fn quoted_after(output: &str, marker: &str) -> Option<String> {
    let marker_at = output.to_ascii_lowercase().find(marker)?;
    let rest = &output[marker_at + marker.len()..];
    let open = rest.find(['\'', '"'])?;
    let quote = rest[open..].chars().next()?;
    let after_open = &rest[open + quote.len_utf8()..];
    let close = after_open.find(quote)?;
    let name = after_open[..close].trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// `pkg.mod` → `pkg/mod`.
fn python_module_path(raw: &str) -> Option<String> {
    let joined = raw
        .split('.')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    (!joined.is_empty()).then_some(joined)
}

/// `../lib/utils.js` → `lib/utils`.
fn node_module_path(raw: &str) -> Option<String> {
    let mut segments: Vec<&str> = raw
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != "." && *segment != "..")
        .collect();
    let last = segments.pop()?;
    let stem = Path::new(last).file_stem().and_then(|s| s.to_str())?;
    segments.push(stem);
    let joined = segments.join("/");
    (!joined.is_empty()).then_some(joined)
}

/// The module path(s) an added file answers to: itself without its extension,
/// plus — for a package initialiser — the directory it initialises, so a bare
/// `import pkg` matches the `pkg/__init__.py` the change added.
fn module_keys(path: &str) -> Vec<String> {
    let lower = path.to_ascii_lowercase();
    let as_path = Path::new(&lower);
    let Some(stem) = as_path.file_stem().and_then(|s| s.to_str()) else {
        return Vec::new();
    };
    let parent = as_path
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .trim_end_matches('/');
    let without_extension = if parent.is_empty() {
        stem.to_string()
    } else {
        format!("{parent}/{stem}")
    };
    let mut keys = vec![without_extension];
    if (stem == "__init__" || stem == "index") && !parent.is_empty() {
        keys.push(parent.to_string());
    }
    keys
}

/// The first of `added` that `module` names, or `None`.
///
/// A key matches when it *ends at a path boundary* with the module path, so
/// `pkg/mod` matches `src/pkg/mod.py` but not `src/other/mod.py` — a leaf-only
/// comparison would credit the second and name the wrong file as evidence.
fn matches_added_file<'a>(module: &str, added: &'a [String]) -> Option<&'a str> {
    let needle = module.to_ascii_lowercase();
    let suffix = format!("/{needle}");
    added
        .iter()
        .filter(|path| !is_build_product(path))
        .find(|path| {
            module_keys(path)
                .iter()
                .any(|key| *key == needle || key.ends_with(&suffix))
        })
        .map(String::as_str)
}

/// Repository-relative paths that exist now — committed, staged, or merely
/// untracked — and did not exist at `baseline_sha`.
///
/// Both halves are needed and neither is redundant: inside a pipeline
/// candidate workspace the agent's files are committed by the time the
/// witness runs, while in a bare task workspace (the shape #2668 was measured
/// on) a brand-new module is still untracked. Paths are forced
/// toplevel-relative on both halves — `--full-name`, and `diff.relative` set
/// off against a repository that configures it — so the two agree on a base
/// when `root` is a subdirectory.
///
/// Best-effort: a git failure contributes nothing, which can only withhold
/// the exception, never grant it.
async fn added_since_baseline(root: &Path, baseline_sha: &str) -> Vec<String> {
    let mut paths = Vec::new();

    let mut diff = super::git_in(root);
    diff.args([
        "-c",
        "diff.relative=false",
        "diff",
        "--name-status",
        "--no-renames",
        baseline_sha,
        "--",
    ]);
    if let crate::exec::Captured::Done(out) = crate::exec::run_captured(diff, 30).await
        && out.status.success()
    {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(path) = line.strip_prefix("A\t") {
                let path = path.trim();
                if !path.is_empty() {
                    paths.push(path.to_string());
                }
            }
        }
    }

    let mut untracked = super::git_in(root);
    untracked.args(["ls-files", "--others", "--exclude-standard", "--full-name"]);
    if let crate::exec::Captured::Done(out) = crate::exec::run_captured(untracked, 30).await
        && out.status.success()
    {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let path = line.trim();
            if !path.is_empty() {
                paths.push(path.to_string());
            }
        }
    }

    paths
}

/// One observed flip whose previous-code half died on an import — everything
/// [`confirm`] needs to decide whether that death was the deliverable's
/// absence, and to say so in the verdict.
///
/// A struct rather than six positional parameters because four of them are
/// strings, which is the shape that silently swaps at a call site.
pub(super) struct ImportFlip<'a> {
    /// What [`super::instrument::import_failure_signature`] recognised, for the prose.
    pub label: &'a str,
    pub test_cmd: &'a str,
    pub baseline: &'a super::WitnessBaseline,
    pub old_exit: i32,
    pub old_output: &'a str,
    /// The caller's note when the previous-code run came from the turn's memo.
    pub reuse_note: &'a str,
}

/// The exception itself: when the previous-code run's import failure names a
/// source file this change adds, return the CONFIRMED verdict text — the
/// baseline's inability to load the deliverable IS the genuine FAIL half of a
/// flip. `None` means the exception does not apply and the caller's
/// [`super::Verdict::InconclusiveBuildError`] refusal stands unchanged.
pub(super) async fn confirm(root: &Path, flip: &ImportFlip<'_>) -> Option<String> {
    let module = missing_module(flip.old_output)?;
    let added = added_since_baseline(root, &flip.baseline.sha).await;
    let deliverable = matches_added_file(&module, &added)?;

    let ImportFlip {
        label,
        test_cmd,
        baseline,
        old_exit,
        reuse_note,
        ..
    } = flip;
    let (short, provenance) = (baseline.short(), &baseline.provenance);
    let old_tail = super::tail(flip.old_output);
    Some(format!(
        "WITNESS CONFIRMED — creation-task proof: `{deliverable}` does not exist at baseline \
         {short} ({provenance}) and does exist now, so {label} on the previous code is the \
         pre-change state itself, not a stale build artifact.\n\
         - new code:      `{test_cmd}` exit 0 (PASS)\n\
         - previous code: baseline {short} → exit {old_exit}, `{deliverable}` \
         absent{reuse_note}\n\
         --- previous-code output tail ---\n{old_tail}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Tool;
    use stella_protocol::tool::ToolOutput;

    #[test]
    fn a_python_module_not_found_names_a_slash_path() {
        assert_eq!(
            missing_module("ModuleNotFoundError: No module named 'portfolio_optimized_c'")
                .as_deref(),
            Some("portfolio_optimized_c")
        );
        assert_eq!(
            missing_module("ModuleNotFoundError: No module named 'pkg.mod'").as_deref(),
            Some("pkg/mod")
        );
    }

    /// A Node dot introduces an extension where a Python dot separates path
    /// segments — parsing both with one rule gets both wrong.
    #[test]
    fn a_node_cannot_find_module_drops_its_extension_and_relative_prefix() {
        assert_eq!(
            missing_module("Error: Cannot find module '../lib/utils.js'").as_deref(),
            Some("lib/utils")
        );
    }

    #[test]
    fn signatures_that_name_no_single_file_are_not_parsed() {
        assert_eq!(
            missing_module("./app: error while loading shared libraries: libz.so.1"),
            None
        );
        assert_eq!(missing_module("2 errors during collection"), None);
    }

    #[test]
    fn a_module_matches_the_source_file_that_defines_it() {
        let added = vec!["src/portfolio_optimized_c.py".to_string()];
        assert_eq!(
            matches_added_file("portfolio_optimized_c", &added),
            Some("src/portfolio_optimized_c.py")
        );
    }

    /// A leaf-only comparison would credit `src/other/mod.py` for `pkg.mod`
    /// and then name it as the evidence — a wrong file in a proof.
    #[test]
    fn a_dotted_module_matches_only_at_a_path_boundary() {
        let added = vec!["src/other/mod.py".to_string(), "src/pkg/mod.py".to_string()];
        assert_eq!(
            matches_added_file("pkg/mod", &added),
            Some("src/pkg/mod.py")
        );
        assert_eq!(matches_added_file("pkg/mod", &added[..1]), None);
    }

    #[test]
    fn a_new_package_matches_the_initialiser_that_creates_it() {
        let added = vec!["pkg/__init__.py".to_string()];
        assert_eq!(matches_added_file("pkg", &added), Some("pkg/__init__.py"));
    }

    /// #2067's false positive, re-checked from this side: an unignored build
    /// product is "new relative to the baseline" in the literal sense and
    /// must still never confirm a witness.
    #[test]
    fn a_build_product_never_matches_however_new_it_is() {
        for artifact in [
            "portfolio_optimized_c.so",
            "lib/portfolio_optimized_c.dylib",
            "build/portfolio_optimized_c.py",
            "node_modules/portfolio_optimized_c/index.js",
        ] {
            let added = vec![artifact.to_string()];
            assert_eq!(
                matches_added_file("portfolio_optimized_c", &added),
                None,
                "{artifact} must not confirm a witness"
            );
        }
    }

    #[test]
    fn nothing_matches_when_the_change_adds_nothing() {
        assert_eq!(matches_added_file("portfolio_optimized_c", &[]), None);
    }

    /// The #2668 witness, end to end: on a creation task the deliverable is
    /// absent at baseline by construction, so the previous-code run's import
    /// failure names a file this change adds. That is the FAIL half of a real
    /// flip and must be CONFIRMED, not refused as
    /// `WITNESS_INCONCLUSIVE_BUILD_ERROR` — the catch-22 that left
    /// `proven=0` on every Terminal-Bench trial ever recorded.
    ///
    /// The witness script stands in for the interpreter, as every sibling
    /// test in this module's parent does, so the proof depends on git rather
    /// than on a Python installation.
    #[tokio::test]
    async fn a_baseline_absent_deliverable_confirms_instead_of_refusing() {
        let root = super::super::tests::scaffold("creation").await;
        std::fs::write(
            root.join("new_module.py"),
            "def greet():\n    return 'hi'\n",
        )
        .unwrap();
        std::fs::write(
            root.join("witness.sh"),
            "if [ -f new_module.py ]; then exit 0; fi\n\
             echo \"ModuleNotFoundError: No module named 'new_module'\"; exit 1\n",
        )
        .unwrap();

        let out = super::super::VerifyDone::default()
            .execute(
                &serde_json::json!({
                    "test_cmd": "bash witness.sh",
                    "test_files": ["witness.sh"],
                    "timeout_secs": 60
                }),
                &root,
            )
            .await;
        match &out {
            ToolOutput::Ok { content } => {
                // `starts_with`, not `contains`: the halt this verdict must
                // fire is gated on the prefix. Both consumers spell it that
                // way — `WITNESS_CONFIRMED_MARKER` in
                // `stella-core/src/driver/confident_zero.rs` and in
                // `stella-pipeline/src/pipeline/execute_stage.rs` — so a
                // creation proof that merely mentioned the phrase would be a
                // verdict nothing acts on.
                assert!(content.starts_with("WITNESS CONFIRMED"), "{content}");
                assert!(content.contains("creation-task proof"), "{content}");
                assert!(content.contains("new_module.py"), "{content}");
            }
            other => panic!("a creation-task deliverable must confirm, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// The other side of the same seam, and the reason the exception is a git
    /// question rather than a text one: an identical failure message whose
    /// named module is a build product the baseline also lacks stays refused.
    #[tokio::test]
    async fn an_unignored_build_product_still_refuses() {
        let root = super::super::tests::scaffold("artifact").await;
        std::fs::write(root.join("portfolio_optimized_c.so"), "\0binary\n").unwrap();
        std::fs::write(
            root.join("witness.sh"),
            "if [ -f portfolio_optimized_c.so ]; then exit 0; fi\n\
             echo \"ModuleNotFoundError: No module named 'portfolio_optimized_c'\"; exit 1\n",
        )
        .unwrap();

        let out = super::super::VerifyDone::default()
            .execute(
                &serde_json::json!({
                    "test_cmd": "bash witness.sh",
                    "test_files": ["witness.sh"],
                    "timeout_secs": 60
                }),
                &root,
            )
            .await;
        match &out {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("WITNESS_INCONCLUSIVE_BUILD_ERROR"),
                    "{message}"
                );
            }
            other => panic!("a build product must not confirm a witness, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }
}
