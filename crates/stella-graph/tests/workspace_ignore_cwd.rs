//! `WorkspaceIgnore::resolve` must answer walk-relative paths whatever
//! directory the process was launched from.
//!
//! `git ls-files` prints paths relative to the **current directory**, not to
//! the work tree, which is why `ask_git` passes `-C root`. Run from
//! `root/generated` without it, a workspace ignoring `generated/` answers
//! `./` — an entry that matches nothing the walk asks about, so the tree is
//! indexed in full and #3204's fix is silently undone.
//!
//! Reaching that branch means changing the working directory, which is
//! global to a whole test process. So the witness runs in a **subprocess**:
//! the parent builds the fixture and re-executes this same test binary with
//! `--exact` at the `#[ignore]`d child below, which is the only code that
//! calls `set_current_dir`. Nothing else in this binary — or in
//! `stella-graph`'s lib tests, which share a process with each other — can
//! observe the change.

use std::path::{Path, PathBuf};
use std::process::Command;

use stella_graph::workspace_ignore::WorkspaceIgnore;

/// Where the parent leaves the fixture for the child to resolve.
const ROOT_ENV: &str = "STELLA_GRAPH_IGNORE_CWD_ROOT";

/// The child's test name, spelled once so the `--exact` filter and the
/// function cannot drift apart.
const CHILD: &str = "resolves_from_inside_an_ignored_subdirectory";

fn git_init(root: &Path) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["init", "--quiet"])
        .status()
        .expect("git must be runnable in the test environment");
    assert!(status.success(), "git init failed");
}

#[test]
fn resolve_is_independent_of_the_processs_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git_init(root);
    std::fs::write(root.join(".gitignore"), "generated/\n").unwrap();
    std::fs::create_dir_all(root.join("generated/deep")).unwrap();
    std::fs::write(root.join("generated/deep/gen.py"), "x = 1\n").unwrap();
    std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();

    let child = Command::new(std::env::current_exe().expect("the test binary knows its own path"))
        .args(["--exact", CHILD, "--ignored", "--nocapture"])
        .env(ROOT_ENV, root)
        .output()
        .expect("re-executing the test binary must succeed");
    assert!(
        child.status.success(),
        "the resolver answered differently from inside an ignored \
         subdirectory — `-C root` is what makes `ls-files` output \
         walk-relative:\n{}{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr),
    );
}

/// Spawned by the test above; not run directly. It changes the working
/// directory, so it must stay the only test this process runs.
#[test]
#[ignore = "spawned as a subprocess by resolve_is_independent_of_the_processs_working_directory"]
fn resolves_from_inside_an_ignored_subdirectory() {
    let root =
        PathBuf::from(std::env::var_os(ROOT_ENV).expect("the parent test passes the fixture root"));
    std::env::set_current_dir(root.join("generated")).unwrap();

    let ignore = WorkspaceIgnore::resolve(&root);
    assert!(
        ignore.excludes_dir("generated"),
        "the declared tree is still pruned at descent: {ignore:?}"
    );
    assert!(
        ignore.excludes("generated/deep/gen.py"),
        "and anything under it is still covered: {ignore:?}"
    );
    assert!(
        !ignore.excludes("main.rs"),
        "source is still not excluded: {ignore:?}"
    );
}
