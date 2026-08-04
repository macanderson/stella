//! Path extraction tests, split out of `tests.rs` when the
//! `turn_path_tokens` cases pushed it past its file-size ceiling.
//! `goal_path_anchors` consults the workspace on disk; `turn_path_tokens`
//! deliberately does not — both dialects are pinned here.

use super::*;

#[test]
fn goal_path_anchors_extracts_only_real_workspace_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/driver.rs"), "fn main() {}").unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "").unwrap();
    let root = dir.path();

    // Named files anchor — including file:line and trailing-punctuation
    // spellings; prose, ghosts, and escapes never do.
    assert_eq!(
        goal_path_anchors("fix the panic in src/driver.rs.", root),
        vec!["src/driver.rs"]
    );
    assert_eq!(
        goal_path_anchors("see src/driver.rs:42 and (src/lib.rs)", root),
        vec!["src/driver.rs", "src/lib.rs"]
    );
    assert!(goal_path_anchors("no paths here at all", root).is_empty());
    assert!(goal_path_anchors("src/ghost.rs does not exist", root).is_empty());
    assert!(
        goal_path_anchors("read ../../etc/passwd and src/../src/driver.rs", root).is_empty(),
        "escape spellings must never anchor"
    );
    // Duplicates collapse.
    assert_eq!(
        goal_path_anchors("src/driver.rs then src/driver.rs again", root),
        vec!["src/driver.rs"]
    );
}

#[test]
fn goal_path_anchors_are_capped() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("m")).unwrap();
    let mut goal = String::new();
    for i in 0..8 {
        let rel = format!("m/f{i}.rs");
        std::fs::write(dir.path().join(&rel), "").unwrap();
        goal.push_str(&rel);
        goal.push(' ');
    }
    let anchors = goal_path_anchors(&goal, dir.path());
    assert_eq!(
        anchors.len(),
        4,
        "anchors fan out into neighborhoods — cap them: {anchors:?}"
    );
}

#[test]
fn turn_path_tokens_takes_path_shaped_tokens_without_a_disk_check() {
    // Bare file names qualify: a record scoped to `deny.toml` is about the
    // topic, and the topic does not require the file to exist from here.
    assert_eq!(
        turn_path_tokens("add an MIT dep, then update deny.toml."),
        vec!["deny.toml"]
    );
    // `/` paths, `file:line` spellings, and `./` prefixes normalize.
    assert_eq!(
        turn_path_tokens("see ./src/api/mod.rs:42 and src/lib.rs"),
        vec!["src/api/mod.rs", "src/lib.rs"]
    );
    // Prose extracts nothing; escapes never qualify; duplicates collapse.
    assert!(turn_path_tokens("no paths here at all").is_empty());
    assert!(turn_path_tokens("read ../../etc/passwd or /etc/passwd").is_empty());
    assert_eq!(
        turn_path_tokens("deny.toml then deny.toml again"),
        vec!["deny.toml"]
    );
}
