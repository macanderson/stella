use std::path::Path;
use std::sync::{Arc, Barrier};

use super::*;

/// Racers all calling [`ensure_workspace_generated_ignore`] on one fresh
/// `.stella` must all succeed (#1410).
///
/// This is the fast path into the window that `Store::open`'s
/// `concurrent_first_open_of_a_fresh_workspace_both_succeed` reaches only by
/// luck: that test pays for SQLite migrations on every racer, so it lands in
/// the few microseconds that matter perhaps once in a thousand runs. Calling
/// the racing function directly buys thousands of attempts per second.
fn race_generated_ignore(racers: usize, rounds: usize) -> Vec<String> {
    let mut failures = Vec::new();
    for _ in 0..rounds {
        let workspace = tempfile::tempdir().expect("tempdir");
        let dot = workspace.path().join(".stella");
        std::fs::create_dir_all(&dot).expect("mkdir .stella");

        let barrier = Arc::new(Barrier::new(racers));
        let handles: Vec<_> = (0..racers)
            .map(|_| {
                let dot = dot.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    ensure_workspace_generated_ignore(&dot)
                })
            })
            .collect();

        for handle in handles {
            if let Err(error) = handle.join().expect("racer panicked") {
                failures.push(error.0);
            }
        }

        // Whoever won, the invariant the function exists for must hold.
        let settled = std::fs::read(dot.join(".gitignore")).expect("read settled ignore");
        if !settled
            .split(|byte| *byte == b'\n')
            .any(|line| line == b"private/")
        {
            failures.push(format!(
                "settled ignore lacks `private/`: {:?}",
                String::from_utf8_lossy(&settled)
            ));
        }
    }
    failures
}

#[test]
fn concurrent_first_open_never_reports_a_planted_ignore() {
    const RACERS: usize = 16;
    const ROUNDS: usize = 150;
    let failures = race_generated_ignore(RACERS, ROUNDS);
    assert!(
        failures.is_empty(),
        "{} of {} racers failed; first few: {:?}",
        failures.len(),
        RACERS * ROUNDS,
        &failures[..failures.len().min(3)]
    );
}

/// The window itself, driven rather than raced.
///
/// A zero-length `.gitignore` is exactly what a winner leaves visible between
/// its `O_EXCL` create and its `write_all`. A loser that reads it must not
/// conclude the file is real content missing `private/` and rename over it:
/// that rename unlinks the winner's inode, and any third racer holding it open
/// then sees `nlink() == 0` and reports a file nothing attacked.
#[test]
fn a_zero_length_ignore_is_not_treated_as_content_to_append_to() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let dot = workspace.path().join(".stella");
    std::fs::create_dir_all(&dot).expect("mkdir .stella");
    let path = dot.join(".gitignore");

    // The winner's mid-write state: created, not yet written.
    std::fs::write(&path, b"").expect("create empty");
    let before = inode_of(&path);

    // A winner finishing its write while the loser is in the settle window.
    let writer_path = path.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(20));
        // Write in place, exactly as `create_generated_ignore_exclusively`
        // does — the inode must survive.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&writer_path)
            .expect("reopen")
            .write_all(WORKSPACE_GENERATED_IGNORE)
            .expect("write");
    });

    ensure_workspace_generated_ignore(&dot).expect("loser must not fail");
    writer.join().expect("writer panicked");

    assert_eq!(
        inode_of(&path),
        before,
        "the loser replaced the winner's inode instead of waiting for it"
    );
    let settled = std::fs::read(&path).expect("read");
    assert!(
        settled
            .split(|byte| *byte == b'\n')
            .any(|line| line == b"private/"),
        "settled ignore lacks `private/`"
    );
}

/// A genuinely empty `.gitignore` — no winner ever arrives — must still be
/// repaired, or the settle window would turn into a permanent skip.
#[test]
fn an_empty_ignore_with_no_writer_is_still_given_the_private_entry() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let dot = workspace.path().join(".stella");
    std::fs::create_dir_all(&dot).expect("mkdir .stella");
    let path = dot.join(".gitignore");
    std::fs::write(&path, b"").expect("create empty");

    ensure_workspace_generated_ignore(&dot).expect("must repair, not skip");

    let settled = std::fs::read(&path).expect("read");
    assert!(
        settled
            .split(|byte| *byte == b'\n')
            .any(|line| line == b"private/"),
        "an empty ignore nobody is writing must still gain `private/`, got {:?}",
        String::from_utf8_lossy(&settled)
    );
}

use std::io::Write as _;

#[cfg(unix)]
fn inode_of(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::symlink_metadata(path).expect("stat").ino()
}

#[cfg(not(unix))]
fn inode_of(_path: &Path) -> u64 {
    0
}
