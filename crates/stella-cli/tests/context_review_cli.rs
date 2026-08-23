//! End-to-end witness for `stella context review` (#4261): the probe verdict a
//! reviewer reads is a statement about the tree in front of them.
//!
//! Through the real binary rather than `review::run_review`, because the defect
//! is what reaches a person's terminal: the recorded verdict was rendered
//! verbatim, in the same colour a fresh one would use, with nothing to say it
//! had not been re-asked. Only stdout can settle that.
//!
//! `env_clear` with `STELLA_HOME` pointed at the temp tree, so the run cannot
//! read or write the developer's real `~/.stella`, and `PATH` is empty so no
//! ambient tool can join in.

use std::path::Path;
use std::process::{Command, Output};

fn review(workspace: &Path, home: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stella"))
        .current_dir(workspace)
        .env_clear()
        .env("STELLA_HOME", home)
        .env("PATH", "")
        .env("NO_COLOR", "1")
        .args(["context", "review"])
        .output()
        .expect("spawn stella")
}

/// One proposal as `stella ingest` wrote it months ago: a `path_exists` probe
/// that passed at the time, recorded verbatim in the file.
fn write_stale_proposal(root: &Path, probe_path: &str) {
    let dir = root.join(".stella").join("proposals");
    std::fs::create_dir_all(&dir).expect("proposals dir");
    std::fs::write(
        dir.join("agents.toml"),
        format!(
            r#"
schema = "context-record/v0.1"
set_id = "stella"
ingest_run_id = "ing_01"

[defaults]
sharing_scope = "repository"
origin = "imported"
status = "active"

[defaults.provenance]
source_kind = "document"
source_uri = "AGENTS.md"

[[proposal]]
candidate_id = "verify-done-shadow-worktree-59ada0e6"
proposal_kind = "knowledge"
status = "eligible"
confidence = 85
observed_at = "2026-08-09T00:42:44Z"

[proposal.record]
lineage_id = "ctx.stella.verify-done-shadow-worktree"
kind = "fact"
statement = "The verify_done tool runs the suite in a detached shadow worktree."

[proposal.record.steering]
force = "info"
precedence = 100

[proposal.record.truth]
basis = "measured"

[proposal.record.truth.probe]
kind = "path_exists"
path = "{probe_path}"

[proposal.refutation]
verdict = "supported"
checked_at = "2026-08-09T00:42:44Z"
probe_kind = "path_exists"
detail = "path `{probe_path}` exists."
"#
        ),
    )
    .expect("proposal file writes");
}

/// **Witness (#4261).** A proposal whose probe target the tree no longer holds
/// is not rendered `supported`.
///
/// Fails on base, which printed the stored refutation verbatim — the verdict
/// `stella ingest` recorded at extraction, re-rendered forever with no re-run
/// and no staleness marker. Four proposals in this repository's own queue read
/// green that way, one of them citing a file PR #3244 deleted.
#[test]
fn a_deleted_probe_target_is_not_still_reported_supported() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("home");
    let root = tmp.path();
    write_stale_proposal(root, "crates/stella-tools/src/verify.rs");

    let out = review(root, home.path());
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "review exits clean: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The control: the proposal is on the page at all.
    assert!(
        stdout.contains("verify-done-shadow-worktree"),
        "the proposal is listed: {stdout}"
    );
    assert!(
        !stdout.contains("probe: supported"),
        "a claim the tree now refutes must not render as supported: {stdout}"
    );
    assert!(
        stdout.contains("refuted"),
        "the re-run probe reports what the tree says now: {stdout}"
    );
    assert!(
        stdout.contains("recorded supported at 2026-08-09T00:42:44Z"),
        "and says the verdict on file disagrees, so a reviewer can see the \
         tree moved: {stdout}"
    );
}

/// A `command_succeeds` probe on a proposal is not run by review, and the
/// verdict on file is what a reviewer sees.
///
/// **This closed no live hole.** `ingest_cmd::probe::evaluate` abstains on
/// every gated kind rather than executing it, so nothing ran before either.
/// What the explicit refusal in `reprobe` buys is that review does not
/// *depend* on that — a
/// proposal's `origin` may sit in the file's `[defaults]` header rather than on
/// the record, so the sweep's own `honored_probe` rule cannot be evaluated
/// correctly from the record alone on this side — and that the stored verdict
/// survives: re-running would have replaced a real recorded verdict with a
/// fresh `unfalsifiable`, which reads as "the tree now refutes less" when
/// nothing was checked at all.
///
/// The canary is what makes the first half falsifiable: the probe's command
/// writes a file, and the assertion is that the file is not there.
#[test]
fn review_never_runs_a_gated_probe_from_a_proposal() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("home");
    let root = tmp.path();
    let canary = root.join("the-probe-ran");
    let dir = root.join(".stella").join("proposals");
    std::fs::create_dir_all(&dir).expect("proposals dir");
    std::fs::write(
        dir.join("gated.toml"),
        format!(
            r#"
schema = "context-record/v0.1"
set_id = "stella"
ingest_run_id = "ing_01"

[defaults]
sharing_scope = "repository"
origin = "imported"
status = "active"

[[proposal]]
candidate_id = "gated-probe-deadbeef"
proposal_kind = "knowledge"
status = "eligible"
confidence = 50
observed_at = "2026-08-09T00:42:44Z"

[proposal.record]
lineage_id = "ctx.stella.gated"
kind = "fact"
statement = "The build script is runnable."

[proposal.record.steering]
force = "info"
precedence = 100

[proposal.record.truth]
basis = "decree"
verified_by = "someone@example.test"

[proposal.record.truth.probe]
kind = "command_succeeds"
command = "touch {}"
"#,
            canary.display()
        ),
    )
    .expect("proposal file writes");

    let out = review(root, home.path());
    assert!(
        out.status.success(),
        "review exits clean: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !canary.exists(),
        "review must not execute a proposal's command probe"
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        stdout.contains("gated-probe-deadbeef"),
        "the control: the proposal is still listed: {stdout}"
    );
    assert!(
        !stdout.contains("no probe kind could judge this claim"),
        "and review does not overwrite what is on file with a fresh abstention: {stdout}"
    );
}

/// The other side: a probe the tree still satisfies renders supported, so the
/// assertion above is about staleness and not about a command that has stopped
/// saying `supported` at all.
#[test]
fn a_probe_target_that_still_exists_renders_supported() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("home");
    let root = tmp.path();
    write_stale_proposal(root, "AGENTS.md");
    std::fs::write(root.join("AGENTS.md"), "# guidance\n").expect("AGENTS.md writes");

    let stdout = String::from_utf8_lossy(&review(root, home.path()).stdout).into_owned();
    assert!(
        stdout.contains("probe: supported"),
        "a claim the tree still supports reads supported: {stdout}"
    );
    assert!(
        !stdout.contains("the tree has moved"),
        "and carries no supersession note when nothing moved: {stdout}"
    );
}
