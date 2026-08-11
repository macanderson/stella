//! Tool scratch — the one list of directories that *running* a test writes
//! into, and which therefore belong to no author and to no deliverable.
//!
//! # Why this is a module and not a constant beside its first caller
//!
//! Three places in the run have to answer the same question, and each of them
//! answered it differently until this module existed:
//!
//! - **Adoption** (`stella_cli::candidate_ws::adopt`) must not carry a
//!   `__pycache__/x.pyc` into the user's tree. It had the list.
//! - **The witness artifact validator** ([`crate::witness::validate_witness_artifact`])
//!   must not read the same `.pyc` as "the author created a second file". It
//!   had no list, so a run whose *own baseline oracle* compiled the authored
//!   Python test was rejected for an integrity violation it committed against
//!   itself — and the executed work, which a grader independently scored 9 of
//!   11 passing, was discarded with it.
//! - **The post-verification seal check** (`sealed_unchanged_inner`) must not
//!   read the same `.pyc` as "the worktree changed after verification". It had
//!   no list either, for the same reason and with the same result.
//!
//! One copy, because the failure mode is precisely divergence: the second and
//! third callers were each written by someone who did not know the first
//! existed.
//!
//! # What is on the list, and what deliberately is not
//!
//! Only scratch written by *running* code. Build outputs (`target`, `dist`,
//! `build`) are deliberately absent: producing one of those is frequently the
//! whole job, and the same reasoning keeps them out of
//! `stella_tools::shell_touch::SKIP_DIRS`. Coverage data (`.gcda`) is likewise
//! absent — it is a *file suffix* rather than a directory, and whether a gcov
//! task's coverage data is scratch or deliverable is a judgement this list
//! cannot make (#2877 tracks it).

/// Directory names that hold tool scratch, matched at **any depth**.
///
/// Deliberately short and deliberately not build outputs — see the module
/// docs for the criterion. Consumed by adoption's git pathspecs (as
/// `:(exclude,glob)**/<dir>/**`) and by [`is_tool_scratch`] for the two
/// callers that work over plain paths.
pub const TOOL_SCRATCH_DIRS: &[&str] = &[
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".tox",
];

/// Whether a repository-relative path lies inside a [`TOOL_SCRATCH_DIRS`]
/// directory at any depth.
///
/// Backslashes normalize to `/` so a Windows-shaped path is judged by the
/// same rule; the comparison is over whole path components, so a file merely
/// *named* `.tox` — or a directory named `mypy_cache_notes` — is not scratch.
pub fn is_tool_scratch(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let mut components = normalized.split('/');
    // The final component is the file itself: a path IS scratch when one of
    // its parent directories is, never when its own name happens to collide.
    let Some(_file) = components.next_back() else {
        return false;
    };
    components.any(|component| TOOL_SCRATCH_DIRS.contains(&component))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pipelines_own_oracle_artifact_is_scratch() {
        assert!(is_tool_scratch(
            "__pycache__/test_numpy_compatibility.cpython-313-pytest-9.1.1.pyc"
        ));
        assert!(is_tool_scratch(
            "pkg/tests/__pycache__/test_x.cpython-312.pyc"
        ));
        assert!(is_tool_scratch(".pytest_cache/v/cache/lastfailed"));
        assert!(is_tool_scratch(".tox/py311/log/build.log"));
    }

    #[test]
    fn a_deliverable_is_never_scratch() {
        assert!(!is_tool_scratch("test_numpy_compatibility.py"));
        assert!(!is_tool_scratch("src/lib.rs"));
        // Build outputs are off the list on purpose: producing one is often
        // the whole task.
        assert!(!is_tool_scratch("target/release/stella"));
        assert!(!is_tool_scratch("dist/app.js"));
        assert!(!is_tool_scratch("build/sqlite3"));
    }

    #[test]
    fn only_whole_components_match() {
        assert!(!is_tool_scratch("notes/mypy_cache_notes.md"));
        assert!(
            !is_tool_scratch("scripts/.tox"),
            "its own name is not a parent"
        );
        assert!(is_tool_scratch("scripts/.tox/run"));
    }

    #[test]
    fn windows_separators_are_judged_by_the_same_rule() {
        assert!(is_tool_scratch(r"pkg\__pycache__\test_x.pyc"));
    }
}
