//! D-3's acceptance, through the shipping install path
//! (`doc:pipeline-as-plugins` §10 D3, run playbook §3 D-3).
//!
//! The authority question is settled and is not re-asked here: packaging the
//! self-driving loop as a plugin **relocates** the authority it already holds
//! as a shell script running as you, and grants nothing new. What D-3 puts on
//! us is narrower — that grant must be **expressible** in the manifest
//! vocabulary and **showable to a human before install**. `stella-plugin`'s
//! `install_consent.rs` holds the expressible half against a fixture. This
//! file holds the showable half against the real thing: the real manifest at
//! `plugins/stella-selfdriving/plugin.toml`, rendered by the real
//! `stella plugin install`, on stdout, before anything is copied anywhere.
//!
//! # The half that would rot on its own
//!
//! A consent document is a claim about a program. Nothing in a manifest keeps
//! the two honest, so [`the_declared_grant_and_the_loop_name_the_same_powers`]
//! enumerates the powers §10 lists and requires each to appear on **both**
//! sides: in the rendered consent text a user reads, and in the file that
//! actually does it. A power the loop drops must leave the grant; a power the
//! grant drops must leave the loop.
//!
//! Each power names its own file, because the loop is three programs and they
//! do not hold the same powers. The shell driver holds `brew`, the rig and
//! the daemon; the binary's `self-driving` verbs hold the `gh` reads that
//! rank the queue; the slash commands hold the `gh` writes that file a
//! finding. Pointing every row at `scripts/self-driving.sh` was this test's
//! first shape and it was wrong on the very first row — the driver stopped
//! calling `gh issue list` when #1548 moved the defect queue into the binary.
//!
//! **What that check cannot do**, said plainly because a guard trusted past
//! its reach is worse than none: the table below is an enumeration, so it
//! catches drift in a power somebody already thought of. A driver that grew
//! an entirely new capability nobody listed would pass it. Closing that needs
//! the loader binding these declarations to a gate under
//! `Principal::Plugin`, where an undeclared call is refused rather than
//! merely unlisted — `doc:pipeline-as-plugins` §A4, and not this slice.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod common;
use common::SealsEmbedderBackend;

fn repo_root() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `<repo>/crates/stella-cli`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the crate sits two levels under the repository root")
        .to_path_buf()
}

/// Run the real install path with no terminal attached, which is what a test
/// process is. Nothing is installed: the binary prints the declaration and
/// then refuses, because there is nobody to answer.
fn install_attempt() -> Output {
    let root = repo_root();
    Command::new(env!("CARGO_BIN_EXE_stella"))
        .without_embedder_backend()
        .current_dir(&root)
        .arg("plugin")
        .arg("install")
        .arg(root.join("plugins/stella-selfdriving"))
        .output()
        .expect("spawn stella")
}

fn consent_text() -> String {
    let out = install_attempt();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// **Showable before install, and only before.** The declaration reaches
/// stdout in full, and with no human present the install refuses rather than
/// assuming an answer — the two halves of "before" that a prompt printed
/// *after* the copy would fail.
#[test]
fn the_whole_grant_is_printed_and_nothing_is_installed_without_an_answer() {
    let out = install_attempt();
    let text = String::from_utf8_lossy(&out.stdout);
    let err = String::from_utf8_lossy(&out.stderr);

    assert!(
        text.contains("Install `stella-selfdriving`?"),
        "the prompt never rendered:\n{text}\n{err}"
    );
    assert!(
        text.contains("The widest thing it asks for is graded DESTRUCTIVE"),
        "the worst grade must be the headline, not a thing to find:\n{text}"
    );
    assert!(
        text.contains("Nothing above is granted until you accept."),
        "{text}"
    );
    assert!(
        !out.status.success(),
        "with no terminal attached the install must refuse, not proceed:\n{text}"
    );
    assert!(
        err.contains("no terminal is attached"),
        "the refusal must say why, so a script author knows to pass --yes:\n{err}"
    );
    assert!(
        !text.contains("installed `stella-selfdriving`"),
        "something was copied before anyone answered:\n{text}"
    );
}

/// The plugin is a caller of its own, and the prompt says so. Without that
/// sentence the text reads as "may I do these on your behalf", which is a
/// different and much smaller question than the one being asked.
#[test]
fn the_prompt_says_the_loop_is_a_principal_of_its_own() {
    let text = consent_text();
    assert!(text.contains("attributed to it, not to you"), "{text}");
    assert!(
        text.contains("The gate enforces the tool and the grade; it does not verify the claim."),
        "a claimed limit must never read as an enforced one:\n{text}"
    );
}

/// It never runs inside a turn — that is §10's whole granularity argument,
/// and it is the sentence that tells a reader why a `none` grade sits above
/// a DESTRUCTIVE capability list without contradiction.
///
/// **And it does not call this package a content bundle** (#3537). That clause
/// used to be part of the `none` grade itself, so the prompt said "It is a
/// content bundle (skills, commands, agents, tools)" about a package that
/// ships none of those four and asks for `bash`, `write_file`, an EC2 rig
/// billed by the day, and a rewrite of a line in `~/.zshrc`. Since #3637 added
/// `driver_say` it also contradicted the "DRIVES Stella" sentence two lines
/// below it.
#[test]
fn the_prompt_shows_a_host_that_takes_no_say_in_the_turn() {
    let text = consent_text();
    assert!(
        text.contains("none — it never runs inside a turn"),
        "{text}"
    );
    assert!(text.contains("runs at no hook point"), "{text}");
    assert!(
        !text.contains("content bundle"),
        "this package ships no skills, tools or records and drives Stella from outside — \
         calling it a content bundle steers a reader away from the grant they are \
         accepting:\n{text}"
    );
    assert!(
        text.contains("DRIVES Stella"),
        "the vocabulary that does describe it must still be there:\n{text}"
    );
}

/// Every power `doc:pipeline-as-plugins` §10 names, on both sides: the words
/// a user reads at install, and the code that does it.
///
/// Each row is `(what §10 calls it, the consent text must contain, the file
/// that holds it, what that file must contain)`.
#[test]
fn the_declared_grant_and_the_loop_name_the_same_powers() {
    let text = consent_text();
    let root = repo_root();

    let powers = [
        // The power did not change; where it lives did (#3599 B1). It used to
        // be a `gh` argv built inline in `self_driving_cmd.rs`; it is now the
        // GitHub adapter behind `IssueProvider`, which is the only place in the
        // tree that reads issues from GitHub.
        //
        // The anchor is the impl rather than the argv on purpose. An argv
        // string fires spuriously the first time someone adds a `--json` field,
        // which teaches a reader that this guard cries wolf; the impl
        // disappears only if the capability really does.
        (
            "reading the GitHub defect queue",
            "`gh` — read the defect queue",
            "crates/stella-cli/src/issue_provider.rs",
            "impl IssueProvider for GhIssueProvider",
        ),
        (
            "filing findings as GitHub issues",
            "file findings as issues",
            "scripts/self-driving/commands/self-driving/tickets.md",
            "gh issue create",
        ),
        (
            "brew installs from a tap, never the homebrew-core name",
            "macanderson/stella/stella",
            "scripts/self-driving.sh",
            r#"readonly BREW_FORMULA="macanderson/stella/stella""#,
        ),
        (
            "starting and stopping a paid EC2 rig",
            "aws ec2 start-instances",
            "scripts/self-driving.sh",
            "aws ec2 start-instances",
        ),
        (
            "rewriting a line in ~/.zshrc",
            "your login shell's rc file (SELF_DRIVING_RC, ~/.zshrc by default)",
            "scripts/self-driving.sh",
            "${SELF_DRIVING_RC:-$HOME/.zshrc}",
        ),
        (
            "daemonising itself",
            "under nohup",
            "scripts/self-driving.sh",
            r#"nohup "$0" daemon loop"#,
        ),
        (
            "installing commands into another agent product's directory",
            "~/.claude/commands/self-driving*",
            "scripts/self-driving.sh",
            "$HOME/.claude/commands",
        ),
        (
            "spending money unattended, once per cycle",
            "under SELF_DRIVING_BUDGET_USD (default $10) per cycle",
            "scripts/self-driving.sh",
            "${SELF_DRIVING_BUDGET_USD:-10}",
        ),
        (
            "durable cross-run state under the stella home",
            "~/.stella/self-driving/<repo>/",
            "crates/stella-home/src/lib.rs",
            r#"const SELF_DRIVING_DIR: &str = "self-driving";"#,
        ),
    ];

    for (power, in_consent, holder, in_holder) in powers {
        assert!(
            text.contains(in_consent),
            "the loop holds `{power}` and the install prompt never says so — \
             looked for {in_consent:?} in:\n{text}"
        );
        let source = std::fs::read_to_string(root.join(holder))
            .unwrap_or_else(|e| panic!("{holder} is where `{power}` lives: {e}"));
        assert!(
            source.contains(in_holder),
            "the grant declares `{power}` but {holder} no longer does it — looked for \
             {in_holder:?}. Narrow the grant rather than leaving a capability declared \
             that nothing needs, or repoint this row if the power moved."
        );
    }
}

/// **The driver grant is shown before install, family by family.**
///
/// The `[driver]` block is the second consent this manifest carries and it is
/// a materially different one from the `[loop]` grade above: a grade says how
/// much say a plugin has *inside* a turn, and this says what it may ask Stella
/// to do to your repository between them. A user who reads only the first has
/// not been told about the second, which is the failure
/// `doc:backlog-self-driving` §6.1 calls "the grant must actually bind".
///
/// It also asserts the two deliberate absences. Each is a decision recorded in
/// the manifest, and a later widening should have to delete an assertion that
/// says why rather than quietly adding a line to a list.
#[test]
fn the_prompt_shows_the_driver_grant_and_its_deliberate_limits() {
    let text = consent_text();

    assert!(
        text.contains("DRIVES Stella"),
        "the install prompt must say the plugin drives Stella, not just that it takes no say in a \
         turn: {text}"
    );
    // Read by family, because "pushes branches, opens pull requests, reads CI,
    // and merges" is the sentence a human weighs, and `deliver_merge` on its
    // own is not.
    for family in [
        "reads and writes your issue tracker",
        "runs Stella against an issue in an isolated worktree",
        "pushes branches, opens pull requests, reads CI, and merges",
        "runs audit tooling over this workspace and files what it finds",
        "proposes new tools, context records and skills",
    ] {
        assert!(
            text.contains(family),
            "missing driver family `{family}`: {text}"
        );
    }
    assert!(
        text.contains("this plugin holds no credential"),
        "the prompt must say every driver capability is performed BY Stella: {text}"
    );

    // `curate_accept` is withheld because this repository is governance mode
    // `regulated` (§7), and no release verb is on the channel at all (§6.4).
    assert!(
        !text.contains("curate_accept"),
        "the loop may propose an authority and grant itself none: {text}"
    );
    // Narrowly `release_`, not `release`: the `bash` capability above legitimately
    // says "the release install", and asserting on the bare word would fail on
    // a sentence that has nothing to do with the channel.
    assert!(
        !text.contains("release_"),
        "a release verb is not on the driver channel at B0-B6: {text}"
    );
}
