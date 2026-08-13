//! The #2931 witness: the graded tree survives a path the command text never
//! spells.
//!
//! `bash::confine` audits the command string, and its header always said what
//! that is worth: "a shell can compute a path at runtime, and
//! `p=/ap; p=$p$'p'` defeats every string check that will ever be written
//! here." On `video-processing__pBadsUh` a worker did exactly that — built the
//! graded tree out of `chr(47) + 'app'`, read and wrote through it five times
//! while the same path spelled literally was refused in the same trial — and
//! the run's entire output, 126 steps and 53.8 minutes, was then thrown away
//! by the post-hoc sealed-bytes detector.
//!
//! So the assertions here are deliberately in two halves, and the first one
//! matters as much as the second:
//!
//! 1. the text audit **still permits** the obfuscated command, because no
//!    pattern added to it could ever not permit some spelling — this pins the
//!    fact that the string layer is not, and cannot be, the enforcement; and
//! 2. the write **does not land** anyway, because `bash::contain` runs the
//!    shell behind an OS write ban the spelling cannot reach.
//!
//! A test that only proved a literal `/app/x` is refused would be the bug
//! restated: that already passed on `main` while the escape was happening.

use std::path::{Path, PathBuf};

use serde_json::json;
use stella_tools::bash::{Bash, ShellConfinement, contain};
use stella_tools::registry::Tool;

/// A throwaway pair of trees: one standing in for the graded tree the run is
/// judged against, one for the isolated candidate the shell actually runs in.
struct Trees {
    dir: PathBuf,
    graded: PathBuf,
    candidate: PathBuf,
}

impl Trees {
    fn create(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "stella_contain_witness_{}_{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let graded = dir.join("graded");
        let candidate = dir.join("candidate");
        // `.git` stands in for the object store a real candidate worktree
        // shares with its graded tree: the ban must not reach it, or every
        // `git commit` the model runs inside its own workspace breaks.
        std::fs::create_dir_all(graded.join(".git")).expect("graded tree");
        std::fs::create_dir_all(&candidate).expect("candidate tree");
        std::fs::write(graded.join("output.toml"), b"original").expect("sentinel");
        // Canonical because both OS mechanisms name a real path to the
        // kernel, and the platform temp dir is a symlink on macOS.
        let graded = graded.canonicalize().expect("canonical graded");
        let candidate = candidate.canonicalize().expect("canonical candidate");
        Self {
            dir,
            graded,
            candidate,
        }
    }

    fn sentinel(&self) -> Vec<u8> {
        std::fs::read(self.graded.join("output.toml")).expect("sentinel readable")
    }
}

impl Drop for Trees {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The `chr(47)` shape, in shell: the separators are computed by `printf` and
/// the tree's own name is assembled from two variables, so the graded path
/// exists only in the running process's memory and never in the command text.
fn obfuscated_write(graded: &Path, leaf: &str) -> String {
    let parent = graded.parent().expect("graded parent");
    let name = graded
        .file_name()
        .expect("graded name")
        .to_str()
        .expect("utf-8");
    format!(
        "s=$(printf '\\57'); b='{}'; n='{name}'; l='{leaf}'; printf pwned > \
         \"${{b}}${{s}}${{n}}${{s}}${{l}}\"",
        parent.display()
    )
}

async fn run(bash: &Bash, command: &str, root: &Path) -> String {
    match bash.execute(&json!({ "command": command }), root).await {
        stella_protocol::tool::ToolOutput::Ok { content, .. } => content,
        stella_protocol::tool::ToolOutput::Error { message, .. } => message,
    }
}

/// Skip loudly rather than pass quietly on a host with no boundary — unless
/// `STELLA_REQUIRE_CONTAINMENT=1` says this host is known to support one, in
/// which case an unenforced posture is a hard failure instead of a skip
/// (#2998).
///
/// The env gate exists because the two hosts that call this need opposite
/// answers to the same non-enforcement: a developer's laptop or a restricted
/// container has a real, permanent reason to be [`contain::Posture::Advisory`]
/// and must not fail a local `cargo test`, while a CI runner that is *known*
/// to support the mechanism (declares the flag) going [`Advisory`] is a
/// regression in the recipe that must turn the job red, not print a line
/// nobody reads. Without the gate this test always chose "pass quietly" —
/// which is exactly the failure mode `contain.rs`'s own header argues against
/// for the string audit it replaces: a guard that reports success when it did
/// nothing.
///
/// The panic message keeps the three outcomes distinguishable: `Enforced`
/// (returns `true`, nothing printed), a required host reporting
/// `UnsupportedPlatform` (the flag was set on the wrong kind of runner — a
/// config mistake, not a recipe regression), and a required host reporting
/// `ProbeFailed` (the mechanism exists here and the functional probe watched
/// it fail to hold — a real regression in `contain.rs`'s recipe).
fn enforced_or_skip() -> bool {
    let posture = contain::posture();
    if posture.is_enforced() {
        return true;
    }
    let required = std::env::var("STELLA_REQUIRE_CONTAINMENT").as_deref() == Ok("1");
    if required {
        panic!(
            "STELLA_REQUIRE_CONTAINMENT=1 but this host is not Enforced ({posture}). \
             UnsupportedPlatform means the flag is set on a runner this module has no \
             recipe for — fix the workflow, not the recipe. ProbeFailed means the \
             mechanism is present but the functional probe watched it fail to hold — \
             a regression in contain.rs, not a config mistake."
        );
    }
    eprintln!("SKIPPED: no OS write boundary on this host ({posture})");
    false
}

/// The witness. Fails on `main`: the audit permits the obfuscated command
/// (half 1 passes there too) and the write then lands in the graded tree.
#[tokio::test]
async fn an_obfuscated_path_cannot_write_the_graded_tree() {
    if !enforced_or_skip() {
        return;
    }
    let trees = Trees::create("obfuscated");
    let confinement = ShellConfinement::graded_tree(trees.graded.clone());
    let command = obfuscated_write(&trees.graded, "output.toml");

    // Half 1: the string layer does not catch this, and adding patterns to it
    // never could. Asserted, not assumed — if a future change makes the audit
    // refuse *this* spelling, the escape simply moves to the next one and the
    // enforcement below is still the only thing holding.
    assert!(
        confinement.audit(&command, &trees.candidate).is_ok(),
        "the text audit is expected to permit an obfuscated path: {command}"
    );

    // Half 2: it does not land.
    let bash = Bash::new(None).confined_to(confinement);
    let output = run(&bash, &command, &trees.candidate).await;
    assert_eq!(
        trees.sentinel(),
        b"original",
        "the graded tree was written through an obfuscated path; shell said: {output}"
    );
}

/// The same escape spelled literally is refused by the text audit *before* the
/// spawn, and the message names the substitute and the delivery mapping the
/// #2931 worker was missing.
#[tokio::test]
async fn the_literal_spelling_is_still_refused_with_the_delivery_mapping() {
    let trees = Trees::create("literal");
    let confinement = ShellConfinement::graded_tree(trees.graded.clone());
    let target = trees.graded.join("output.toml");
    let command = format!("printf pwned > {}", target.display());

    let refusal = confinement
        .audit(&command, &trees.candidate)
        .expect_err("a literal graded write");
    let message = refusal.to_string();
    assert!(
        message.contains(&trees.candidate.join("output.toml").display().to_string()),
        "{message}"
    );
    assert!(message.contains("is delivered to"), "{message}");
    assert_eq!(trees.sentinel(), b"original");
}

/// The boundary is a write ban on one tree, not a cage. Everything the
/// candidate legitimately does has to keep working, or the fix costs more than
/// the escape did: its own workspace stays writable, the graded tree stays
/// readable (#2928), and the shared git object store stays writable or every
/// `git commit` inside a candidate fails.
#[tokio::test]
async fn the_boundary_leaves_reads_the_candidate_and_the_object_store_alone() {
    if !enforced_or_skip() {
        return;
    }
    let trees = Trees::create("permitted");
    let confinement = ShellConfinement::graded_tree(trees.graded.clone());
    let bash = Bash::new(None).confined_to(confinement);

    let own = trees.candidate.join("work.txt");
    let output = run(
        &bash,
        &format!("printf mine > {}", own.display()),
        &trees.candidate,
    )
    .await;
    assert_eq!(
        std::fs::read(&own).ok().as_deref(),
        Some(b"mine".as_slice()),
        "the candidate cannot write its own workspace: {output}"
    );

    let read = run(
        &bash,
        &format!("cat {}", trees.graded.join("output.toml").display()),
        &trees.candidate,
    )
    .await;
    assert!(read.contains("original"), "graded read blocked: {read}");

    let object = trees.graded.join(".git").join("object");
    let output = run(
        &bash,
        &obfuscated_write(&trees.graded, ".git/object"),
        &trees.candidate,
    )
    .await;
    assert!(
        object.exists(),
        "the shared object store is not writable, which breaks candidate commits: {output}"
    );
}
