//! Whole-file replacement against a belief the agent only ever glimpsed.
//!
//! The sibling tests in the parent module cover the *other* half of the
//! no-clobber guarantee: someone else moved the bytes since you looked. This
//! module covers the half that needs no second agent at all — the file is
//! exactly as you left it, and replacing it whole still destroys every line
//! outside the window you read.
//!
//! The three claims, in the order the guard has to hold them:
//!
//! 1. a whole-file replacement against a partial belief is refused;
//! 2. a *targeted* edit against that same belief is not — `read_file` windows
//!    at 2000 lines, so refusing those would make every file above that
//!    ceiling unwritable;
//! 3. a complete read still upgrades the belief, so the ordinary
//!    read-then-write cycle never sees this guard at all.

use super::*;

/// `n` numbered lines — long enough that a windowed read is genuinely a
/// window onto part of it.
fn numbered_lines(n: usize) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

/// **Witness.** A file this session has only ranged-read must not be
/// replaceable whole.
///
/// No second agent, no concurrent edit, nothing stale: the agent saw 3 lines
/// of 40 and then overwrites all 40. The 37 it never looked at are destroyed
/// by its own hand, and the hash comparison cannot see it — the bytes on disk
/// are exactly the bytes the belief recorded, so the only thing that can flag
/// this call is the belief's *coverage*.
///
/// Before this guard, a file the agent had glimpsed was less protected than
/// one it had never touched: the glimpse recorded a matching digest, which the
/// clobber check reads as consent.
#[tokio::test]
async fn a_partial_read_cannot_authorize_a_blind_whole_file_overwrite() {
    let (dir, agent) = telemetry_fixture();
    let file = dir.path().join("notes.txt");
    let original = numbered_lines(40);
    std::fs::write(&file, &original).unwrap();

    // The agent looks at the header. Lines 4-40 are never rendered.
    let peek = agent
        .execute(
            "read_file",
            &serde_json::json!({
                "path": "notes.txt", "offset": 1, "limit": 3, "reason": "checking the header",
            }),
        )
        .await;
    assert!(!peek.is_error(), "the ranged read succeeds: {peek:?}");

    // And then replaces the file with content formed against 3 of its 40 lines.
    let wrote = agent
        .execute(
            "write_file",
            &serde_json::json!({
                "path": "notes.txt",
                "content": "line 1 — rewritten header\n",
                "reason": "tidying the header",
            }),
        )
        .await;

    match &wrote {
        ToolOutput::Error { message } => {
            assert!(
                message.contains("notes.txt"),
                "the refusal names the file so the model can act on it: {message}"
            );
            assert!(
                message.contains("edit_file"),
                "the refusal names the tool that CAN do this safely: {message}"
            );
            assert!(
                message.contains("nothing was written"),
                "the refusal says the edit was not half-applied: {message}"
            );
        }
        other => panic!("a whole-file write over 37 unseen lines must be refused, got {other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        original,
        "the unseen lines are still there — this is the whole point"
    );
}

/// **Pin.** A targeted edit against a partial belief still lands.
///
/// This is the reason the naive "refuse every mutation against a partial
/// belief" version is wrong. `read_file` renders at most 2000 lines, so a file
/// above that ceiling can NEVER be read whole and its belief can never be
/// anything but partial — refusing targeted edits there would make every large
/// file in the tree permanently unwritable. `edit_file` and `apply_edits`
/// replace a matched region and leave the rest of the bytes exactly where they
/// were, so there is no unseen remainder for them to destroy.
#[tokio::test]
async fn a_targeted_edit_against_a_partial_belief_still_lands() {
    let (dir, agent) = telemetry_fixture();
    let file = dir.path().join("huge.txt");
    // Past `read_file`'s ceiling: every read of this file is a window, so the
    // belief is partial and stays that way.
    std::fs::write(&file, numbered_lines(2_100)).unwrap();

    let read = agent
        .execute(
            "read_file",
            &serde_json::json!({ "path": "huge.txt", "reason": "reading what fits" }),
        )
        .await;
    assert!(!read.is_error(), "the read succeeds: {read:?}");

    let edited = agent
        .execute(
            "edit_file",
            &serde_json::json!({
                "path": "huge.txt",
                "old_string": "line 7\n",
                "new_string": "line 7 — edited in place\n",
                "reason": "changing a region I actually read",
            }),
        )
        .await;
    assert!(
        !edited.is_error(),
        "a targeted edit destroys no unseen lines and must be allowed: {edited:?}"
    );

    let batched = agent
        .execute(
            "apply_edits",
            &serde_json::json!({
                "edits": [{
                    "path": "huge.txt",
                    "old_string": "line 9\n",
                    "new_string": "line 9 — edited in a batch\n",
                }],
                "reason": "the transactional path is not a second policy",
            }),
        )
        .await;
    assert!(
        !batched.is_error(),
        "apply_edits is the same shape of change and must be allowed too: {batched:?}"
    );

    let on_disk = std::fs::read_to_string(&file).unwrap();
    assert!(
        on_disk.contains("line 7 — edited in place"),
        "{on_disk:.80}"
    );
    assert!(
        on_disk.contains("line 9 — edited in a batch"),
        "{on_disk:.80}"
    );
    assert!(
        on_disk.contains("line 2100\n"),
        "the tail the agent never saw is untouched — which is why this is safe"
    );
}

/// **Pin.** A complete read still upgrades a partial belief to full, so the
/// ordinary read-then-write cycle is untouched.
///
/// The recovery the refusal asks for has to actually work: an agent told "you
/// have only seen part of this file" reads it whole and writes. If the upgrade
/// did not happen, the guard would refuse the write, then refuse it again
/// after the re-read, and the file would be permanently unwritable — the
/// deadlock that makes a guard get deleted rather than fixed.
#[tokio::test]
async fn a_complete_read_upgrades_a_partial_belief_and_the_write_lands() {
    let (dir, agent) = telemetry_fixture();
    let file = dir.path().join("small.txt");
    std::fs::write(&file, numbered_lines(40)).unwrap();

    // A glimpse first, so the belief starts out partial.
    let peek = agent
        .execute(
            "read_file",
            &serde_json::json!({
                "path": "small.txt", "offset": 1, "limit": 3, "reason": "checking the header",
            }),
        )
        .await;
    assert!(!peek.is_error(), "{peek:?}");

    // Under the read ceiling, so this shows the model every line.
    let whole = agent
        .execute(
            "read_file",
            &serde_json::json!({ "path": "small.txt", "reason": "reading it in full" }),
        )
        .await;
    assert!(!whole.is_error(), "{whole:?}");

    let wrote = agent
        .execute(
            "write_file",
            &serde_json::json!({
                "path": "small.txt",
                "content": "rewritten against every line I read\n",
                "reason": "the ordinary cycle",
            }),
        )
        .await;
    assert!(
        !wrote.is_error(),
        "a full read earns the whole-file belief a whole-file write needs: {wrote:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "rewritten against every line I read\n"
    );
}
