//! The #938 witness: confinement has to survive a filesystem that changes
//! between the check and the open.
//!
//! `resolve_within_root` answers about a *string*. For a path whose tail does
//! not exist yet it walks up to the deepest existing ancestor and re-appends
//! the missing components unvalidated, so the interior directories of a
//! brand-new file were never checked. Everything that runs afterwards —
//! `create_dir_all(parent)`, then the open — resolves every one of those names
//! again from scratch. Plant a symlink at one of them in between and the bytes
//! land outside the workspace; `O_NOFOLLOW` on the final component does not
//! help, because the final component is not the one that moved.
//!
//! `RootHandle` opens the root once and walks descriptors from it, so a name
//! is resolved exactly once, by the kernel, at the moment it is used.
//!
//! Four shapes of evidence here:
//!
//! 1. [`a_root_re_pointed_after_the_handle_is_opened_cannot_redirect_a_write`]
//!    — the gate. Deterministic, no threads, and red on string resolution: it
//!    changes what the *name* means after the confinement decision was taken
//!    and asserts the write follows the descriptor rather than the name.
//! 2. A bounded soak driving the real `write_file` tool against a symlink
//!    being flipped underneath it — the acceptance criterion's own wording.
//!    It asserts the negative (nothing lands outside) rather than asserting
//!    that an escape was observed, because a race test that must win a race
//!    is a flaky test. On the pre-fix sequence it went red within a dozen
//!    iterations.
//! 3. The static cases: a planted interior symlink, at the first component and
//!    below a real directory, refused as a *confinement* verdict. The old
//!    string resolver was already right about these; they are here so a
//!    rewrite of the walk cannot lose them.
//! 4. A proptest over component sequences: every successful open names an
//!    inode that is reachable inside the canonical root, and nothing ever
//!    appears on the far side of an outward symlink.

#![cfg(unix)]

use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use stella_tools::registry::Tool;
use stella_tools::rootfd::RootHandle;

fn scratch(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "stella-confinement-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("create scratch dir");
    base
}

/// The deterministic witness, and the shortest statement of the fix: the
/// confinement is bound to an *inode*, not to a name.
///
/// Once the workspace root's own name is re-pointed — `mv workspace
/// workspace.real && ln -s /tmp/evil workspace`, one line in a `build.rs` —
/// every later `canonicalize(root)` answers with the attacker's directory,
/// and a string resolver then approves everything under it, because it really
/// is "inside the root" by the only test a string can apply. A held
/// descriptor keeps naming the directory that was opened, so the write lands
/// in the workspace it was given and `outside/` stays empty.
///
/// Red on string resolution, green on descriptors, with no race to win.
#[test]
fn a_root_re_pointed_after_the_handle_is_opened_cannot_redirect_a_write() {
    let base = scratch("root-swap");
    let root = base.join("root");
    let real = base.join("root-real");
    let outside = base.join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    let handle = RootHandle::open(&root).expect("open root");

    // The name `root` now means something else entirely.
    std::fs::rename(&root, &real).unwrap();
    std::os::unix::fs::symlink(&outside, &root).unwrap();

    let target = handle
        .open_write("landed.txt", false)
        .expect("the handle still names the workspace it opened");
    drop(target);

    assert!(
        real.join("landed.txt").exists(),
        "the write must land in the directory the handle holds"
    );
    assert!(
        !outside.join("landed.txt").exists(),
        "the write must not follow the root's new name out of the workspace"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// The walk refuses a symlink planted at an interior component, and refuses it
/// as a *confinement* verdict rather than as a stray IO error.
///
/// Note what this does and does not prove: with the link already in place, the
/// old string resolver refuses it too — statically it was correct, and the
/// issue says so. What it could not do is stay correct across the window
/// between its answer and the open, which is what the test above and the soak
/// below cover. This one pins the static half so a future rewrite of the walk
/// cannot lose it.
#[test]
fn escape_through_an_intermediate_component_planted_after_resolution() {
    let base = scratch("plant");
    let root = base.join("root");
    let outside = base.join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    // The confinement decision is taken here, while `root/` is still empty.
    let handle = RootHandle::open(&root).expect("open root");

    // The string resolver cannot see what happens next, and this is why: it
    // says yes to a path whose interior does not exist, and it says yes
    // *before* the plant. Everything downstream of it re-resolves the name.
    let string_verdict = stella_tools::resolve_within_root(&root, "sub/evil.txt");
    assert!(
        string_verdict.is_some(),
        "the string resolver approves a tail it has not validated — the premise of this bug"
    );

    // The window. A concurrent `bash` tool, or a `build.rs` in the repository
    // under audit, replaces the interior component with a link out of the tree.
    std::os::unix::fs::symlink(&outside, root.join("sub")).unwrap();

    let error = handle
        .open_write("sub/evil.txt", true)
        .expect_err("the walk must refuse the planted component");
    assert!(
        error.is_escape(),
        "must read as a refusal, not an IO error: {error}"
    );
    assert!(
        !outside.join("evil.txt").exists(),
        "nothing may be created outside the workspace root"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// The same plant, one level deeper and mid-path, so the refusal is not an
/// artifact of the interior component being the first one.
#[test]
fn a_plant_below_a_real_directory_is_refused_at_the_component_that_moved() {
    let base = scratch("deep-plant");
    let root = base.join("root");
    let outside = base.join("outside");
    std::fs::create_dir_all(root.join("src/generated")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    let handle = RootHandle::open(&root).expect("open root");
    assert!(handle.open_write("src/generated/ok.rs", false).is_ok());

    std::fs::remove_dir_all(root.join("src/generated")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("src/generated")).unwrap();

    let error = handle
        .open_write("src/generated/evil.rs", true)
        .expect_err("refused");
    assert!(error.is_escape(), "{error}");
    assert!(!outside.join("evil.rs").exists());

    let _ = std::fs::remove_dir_all(&base);
}

/// The acceptance criterion's other half: a symlink swap raced against a write
/// loop cannot escape. Bounded, and it asserts the negative — a test that has
/// to win a race to pass is a test that fails for no reason on a loaded CI box.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_symlink_swap_racing_a_write_loop_never_lands_outside_the_root() {
    const ITERATIONS: usize = 400;

    let base = scratch("race");
    let root = base.join("root");
    let outside = base.join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let flipper = {
        let (root, outside, stop) = (root.clone(), outside.clone(), Arc::clone(&stop));
        std::thread::spawn(move || {
            let sub = root.join("sub");
            while !stop.load(Ordering::Relaxed) {
                // Real directory, then a link out of the tree, over and over.
                // Both teardowns are attempted because only one of them applies
                // to whichever shape `sub` currently has.
                let _ = std::fs::remove_dir_all(&sub);
                let _ = std::fs::remove_file(&sub);
                let _ = std::os::unix::fs::symlink(&outside, &sub);
                let _ = std::fs::remove_file(&sub);
                let _ = std::fs::create_dir(&sub);
            }
        })
    };

    let tool = stella_tools::write::WriteFile::default();
    let mut wrote = 0usize;
    let mut refused = 0usize;
    for i in 0..ITERATIONS {
        let out = tool
            .execute(
                &serde_json::json!({"path": "sub/evil.txt", "content": "x"}),
                &root,
            )
            .await;
        if out.is_error() {
            refused += 1;
        } else {
            wrote += 1;
        }
        assert!(
            !outside.join("evil.txt").exists(),
            "iteration {i}: a write escaped the workspace root ({out:?})"
        );
    }
    stop.store(true, Ordering::Relaxed);
    flipper.join().unwrap();

    assert!(
        !outside.join("evil.txt").exists(),
        "nothing may be left outside the root after the loop"
    );
    // Not an assertion about the race — an assertion that the loop actually
    // exercised the write path rather than failing for some unrelated reason.
    assert!(
        wrote + refused == ITERATIONS && wrote > 0,
        "{wrote} wrote / {refused} refused of {ITERATIONS}"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// The four adversarial cases `lib.rs` pins for the string resolver, asserted
/// against the tools now that they walk descriptors: the assertions port even
/// though the call shape does not.
#[tokio::test]
async fn the_file_tools_refuse_the_same_escapes_through_the_new_path() {
    let base = scratch("tools");
    let root = base.join("root");
    let outside = base.join("outside");
    std::fs::create_dir_all(root.join("real")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "not yours").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();
    std::os::unix::fs::symlink(base.join("nowhere"), root.join("dangling")).unwrap();
    std::os::unix::fs::symlink(root.join("real"), root.join("alias")).unwrap();

    for path in ["../escaped.txt", "out/evil.txt", "dangling"] {
        let out = stella_tools::write::WriteFile::default()
            .execute(&serde_json::json!({"path": path, "content": "x"}), &root)
            .await;
        assert!(out.is_error(), "write_file must refuse `{path}`: {out:?}");
    }
    assert!(!outside.join("evil.txt").exists());
    assert!(!base.join("escaped.txt").exists());
    assert!(!base.join("nowhere").exists());

    // Reads are the information-disclosure half of the same bug.
    let out = stella_tools::read::ReadFile::default()
        .execute(&serde_json::json!({"path": "out/secret.txt"}), &root)
        .await;
    assert!(
        out.is_error(),
        "read_file must refuse an outward symlink: {out:?}"
    );

    // And a delete must not be laundered through one either.
    let out = stella_tools::delete::DeleteFile
        .execute(&serde_json::json!({"path": "out/secret.txt"}), &root)
        .await;
    assert!(out.is_error(), "delete_file must refuse: {out:?}");
    assert!(
        outside.join("secret.txt").exists(),
        "the file must still be there"
    );

    // The allow case that constrains the design: a tree that symlinks
    // internally still works, and the write lands on the real file.
    let out = stella_tools::write::WriteFile::default()
        .execute(
            &serde_json::json!({"path": "alias/new.txt", "content": "hello"}),
            &root,
        )
        .await;
    assert!(
        !out.is_error(),
        "an in-root symlink must still be writable: {out:?}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("real/new.txt")).unwrap(),
        "hello"
    );

    let _ = std::fs::remove_dir_all(&base);
}

// ---------------------------------------------------------------------------
// The property.
// ---------------------------------------------------------------------------

/// The component alphabet the issue asks for: ordinary names, `..`, `.`, a
/// symlink pointing inside the root, one pointing outside it, a dangling one,
/// and an absolute path.
const ALPHABET: &[&str] = &[
    "dir",
    "deep",
    "file.txt",
    "leaf",
    "..",
    ".",
    "abslink",  // -> <root>/dir, absolute
    "rellink",  // -> dir, relative
    "outlink",  // -> <outside>, absolute
    "dangling", // -> <base>/nowhere, absolute and nonexistent
    "reldangle",
    "/etc",
];

/// Build the fixture the alphabet names. Returns `(root, outside)`.
fn fixture(base: &Path) -> (PathBuf, PathBuf) {
    let root = base.join("root");
    let outside = base.join("outside");
    std::fs::create_dir_all(root.join("dir/deep")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(root.join("file.txt"), "x").unwrap();
    std::os::unix::fs::symlink(root.join("dir"), root.join("abslink")).unwrap();
    std::os::unix::fs::symlink("dir", root.join("rellink")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("outlink")).unwrap();
    std::os::unix::fs::symlink(base.join("nowhere"), root.join("dangling")).unwrap();
    std::os::unix::fs::symlink("nowhere-either", root.join("reldangle")).unwrap();
    (root, outside)
}

/// Every `(dev, ino)` reachable inside `root` without crossing a symlink.
///
/// This is the portable form of "the result is inside the canonical root":
/// the descriptor a successful open produced must name an inode this walk
/// finds. Comparing paths would only re-run the string reasoning the fix
/// exists to replace, and `/proc/self/fd` does not exist on macOS.
fn inodes_under(root: &Path, found: &mut Vec<(u64, u64)>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.path().symlink_metadata() else {
            continue;
        };
        found.push((meta.dev(), meta.ino()));
        if meta.file_type().is_dir() {
            inodes_under(&entry.path(), found);
        }
    }
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig {
        // Each case materializes a small tree, so the budget is spent on
        // filesystem work rather than on shrinking a pure function.
        cases: 96,
        ..proptest::prelude::ProptestConfig::default()
    })]

    /// The invariant the issue asks for, in the form the descriptor walk can
    /// state it: an open either fails, or it names an inode inside the root.
    /// A path that escapes cannot do either quietly — nothing may appear in
    /// `outside/` at all.
    #[test]
    fn every_successful_open_names_an_inode_inside_the_root(
        components in proptest::collection::vec(
            proptest::sample::select(ALPHABET),
            1..=5,
        )
    ) {
        let base = std::env::temp_dir().join(format!(
            "stella-confinement-prop-{}-{:?}-{:x}",
            std::process::id(),
            std::thread::current().id(),
            // Cases run on the same thread, so the pid/thread pair is not
            // unique on its own.
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let (root, outside) = fixture(&base);
        let handle = RootHandle::open(&root).unwrap();

        let relative = components.join("/");
        let outcome = handle.open_write(&relative, true);

        if let Ok(target) = outcome {
            let meta = target.file.metadata().unwrap();
            let mut inside = Vec::new();
            inodes_under(&root.canonicalize().unwrap(), &mut inside);
            proptest::prop_assert!(
                inside.contains(&(meta.dev(), meta.ino())),
                "`{relative}` opened an inode that is not inside the root",
            );
        }

        // Whatever the verdict, the walk may not have created anything on the
        // far side of an outward symlink — including a directory, which
        // `create_dir_all` used to be free to make.
        let leaked: Vec<_> = std::fs::read_dir(&outside)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        proptest::prop_assert!(
            leaked.is_empty(),
            "`{relative}` left {leaked:?} outside the workspace root",
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
