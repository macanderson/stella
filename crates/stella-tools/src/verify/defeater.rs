// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Why a `verify_done` call could not produce a flip — when the answer is a
//! fact about the **setup**, not about the test.
//!
//! # The failure this module exists to end
//!
//! `verify_done`'s only negative-evidence verdict used to be `VACUOUS TEST`,
//! and its message offered three causes: the behaviour already existed, the
//! test doesn't exercise it, the change isn't wired in. On the 2026-08-11
//! Terminal-Bench panel none of the three was ever the real one, and the
//! closing advice — "strengthen the test" — is advice that cannot terminate
//! when the test was never the problem. Pipeline arms made 5-7 calls with 5-6
//! `VACUOUS` returns each; `polyglot-c-py` spent 91% of its wall clock, and
//! `query-optimize` 81% of its steps, *after the work was already correct*
//! (#2871).
//!
//! Two mechanisms produced almost all of it, and both are decidable:
//!
//! - **The command leaves the tree the run was aimed at.**
//!   [`run_against_baseline`](super::run_against_baseline) sets `current_dir`
//!   on the shadow invocation, but a `cd` to an absolute path inside `test_cmd`
//!   walks straight back out of it. `query-optimize` step 67 ran
//!   `cd /tmp/stella_candidate_360_0 && bash witness_test.sh` and got
//!   `VACUOUS`; step 91 ran the identical test **without the `cd`** and got
//!   `WITNESS CONFIRMED`. Seventy steps, one prefix.
//! - **`test_files` layering.** [`stage_witness_files`](super::stage_witness_files)
//!   copies every named file onto the baseline, which is the whole point for a
//!   test file and a defeater for anything else: name the change's deliverable
//!   there and the previous-code run is handed the new code, so it cannot fail.
//!
//! And one fact is simply invisible: when the baseline commit is git's
//! [`EMPTY_TREE_OID`], there is no old code for a *behavioural* assertion to
//! fail against, so a whole family of witnesses cannot discriminate however
//! hard the author strengthens them. `pypi-server` reasoned its way to that
//! and kept calling anyway; the bare loop burned ~13 minutes and 5,466
//! reasoning deltas on the same paradox. See [`empty_baseline_note`] for what
//! that fact does and does not license — it is not the blanket "no flip is
//! constructible" the forensics reported, and the test written to prove the
//! stronger claim disproved it.
//!
//! # The discipline every message here follows
//!
//! Say the mechanism, name the observation it was read off, and state the
//! repair. Never accuse the test of a fault the call did not observe: a
//! refusal that misattributes is worse than no refusal, because the author
//! acts on it. This is the same contract
//! [`resolve_witness_baseline`](super::resolve_witness_baseline)'s
//! `WITNESS BASELINE UNRESOLVED` already keeps.
//!
//! Every string below is a pure function of the call's inputs and
//! observations — no timings, no counters. `ToolOutput` is what `stella-core`'s
//! loop detector keys on (see [`phases`](super::phases)), and a message that
//! varies call to call would blind it for the one tool an agent stuck in a
//! prove-it loop calls over and over.

use std::path::Path;

/// Git's empty tree: the tree object every repository has and no repository
/// stores. A baseline commit pointing at it has no files, which is the exact
/// greenfield shape [`empty_baseline_note`] names.
pub(super) const EMPTY_TREE_OID: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Where a `cd` in `test_cmd` sends the run — the absolute destination, as the
/// author wrote it, so the refusal can quote it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CwdEscape {
    /// The literal operand (`/tmp/stella_candidate_360_0`), or `$HOME` for a
    /// bare `cd`, which is where the shell sends it.
    pub(super) target: String,
}

/// One shell token, plus whether it sat in command position.
#[derive(Debug, PartialEq, Eq)]
enum Token {
    /// A word, with its quotes already stripped.
    Word(String),
    /// A control operator: `;`, `&&`, `||`, `|`, `&`, `(`, `)`, `{`, `}`, or a
    /// newline. What follows one is a fresh command.
    Sep,
}

/// Split `command` into words and control operators, honouring single and
/// double quotes so `cd '/tmp/x'` is seen for what it is.
///
/// Deliberately not a shell parser: it does not expand, substitute, or resolve
/// anything, because [`cwd_escape`] only ever asks a question that quoting can
/// hide and expansion cannot answer. A `$VAR` destination stays a word and is
/// never flagged — the tool cannot know where it points, and a refusal that
/// guesses is exactly the failure this module exists to end.
fn tokenize(command: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut chars = command.chars().peekable();
    // Set whenever a quote opened, so `cd ''` is an empty *word* rather than
    // no word at all — otherwise a quoted-empty operand would read as a bare
    // `cd`, which is a different (and wrong) accusation.
    let mut quoted = false;
    while let Some(ch) = chars.next() {
        match ch {
            '\'' | '"' => {
                let close = ch;
                quoted = true;
                for inner in chars.by_ref() {
                    if inner == close {
                        break;
                    }
                    word.push(inner);
                }
            }
            ';' | '|' | '&' | '(' | ')' | '{' | '}' | '\n' => {
                if !word.is_empty() || quoted {
                    tokens.push(Token::Word(std::mem::take(&mut word)));
                    quoted = false;
                }
                // `&&` and `||` are one operator, but a second `Sep` is
                // harmless: `Sep` only ever means "a command starts next".
                tokens.push(Token::Sep);
            }
            c if c.is_whitespace() => {
                if !word.is_empty() || quoted {
                    tokens.push(Token::Word(std::mem::take(&mut word)));
                    quoted = false;
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() || quoted {
        tokens.push(Token::Word(word));
    }
    tokens
}

/// Whether `target` names a location outside the directory the run was aimed
/// at: an absolute path, or a `~` the shell expands to one.
fn leaves_the_tree(target: &str) -> bool {
    target.starts_with('/') || target == "~" || target.starts_with("~/")
}

/// Find a `cd` (or `pushd`) in `test_cmd` that walks the run out of the
/// directory it was aimed at.
///
/// Only *command position* counts — the start of the string or just after a
/// control operator — so `echo cd /tmp` and `grep -q cd /etc/foo` are not
/// accusations. A bare `cd` counts: with no operand the shell goes to `$HOME`,
/// which is as far outside the shadow as any absolute path.
///
/// **Every** command in the chain is examined. A harmless `cd` does not end
/// the scan, because a chain is only as confined as its last hop:
/// `cd src && cd /tmp && pytest` ends outside the tree exactly as
/// `cd /tmp && pytest` does. Concluding from the first `cd` was this
/// function's own bug, caught in review on #2880 before it shipped.
pub(super) fn cwd_escape(test_cmd: &str) -> Option<CwdEscape> {
    let tokens = tokenize(test_cmd);
    let mut command_position = true;
    let mut index = 0;
    'scan: while index < tokens.len() {
        match &tokens[index] {
            Token::Sep => {
                command_position = true;
                index += 1;
            }
            Token::Word(word) => {
                if !command_position {
                    index += 1;
                    continue;
                }
                command_position = false;
                if word != "cd" && word != "pushd" {
                    index += 1;
                    continue;
                }
                // The first operand that is not an option flag. `cd -` (the
                // previous directory) and `cd -P /x` both land here correctly:
                // the flag is skipped, `-` alone is not a path we can resolve
                // and is not flagged.
                let mut operand = index + 1;
                while let Some(Token::Word(candidate)) = tokens.get(operand) {
                    if candidate.starts_with('-') && candidate.len() > 1 {
                        operand += 1;
                        continue;
                    }
                    if leaves_the_tree(candidate) {
                        return Some(CwdEscape {
                            target: candidate.clone(),
                        });
                    }
                    // This hop stays inside whichever tree the run was aimed
                    // at. That is a fact about this `cd` alone, so the scan
                    // resumes at the next command rather than clearing the
                    // whole chain.
                    index = operand + 1;
                    continue 'scan;
                }
                // No operand at all: `cd` alone, or `cd` followed by `&&`.
                return Some(CwdEscape {
                    target: "$HOME".to_string(),
                });
            }
        }
    }
    None
}

/// [`cwd_escape`] and its refusal, as the one call `verify_done` makes —
/// returned before either half runs, because the verdict this would otherwise
/// produce is a lie and both runs are expensive.
pub(super) fn cwd_refusal(test_cmd: &str) -> Option<String> {
    cwd_escape(test_cmd).map(|escape| cwd_escape_message(&escape))
}

/// The refusal text for an escape, split out so every branch of it is
/// reachable from a unit test that names the target directly.
fn cwd_escape_message(escape: &CwdEscape) -> String {
    format!(
        "WITNESS COMMAND ESCAPES THE TEST TREE — `test_cmd` changes directory to `{target}`, \
         which is outside the tree each half of this proof is aimed at. The working-tree half \
         runs in your workspace root; the previous-code half runs in a fresh checkout of the \
         baseline. A `cd` to an absolute path defeats both: the two halves would execute in the \
         SAME directory, the previous-code run would execute your NEW code, and this call would \
         answer `VACUOUS TEST` about a test that was never the problem.\n\
         This is a fact about the command, NOT about your test — do not weaken, strengthen, or \
         rewrite the test in response.\n\
         Fix: delete the `cd`. The correct directory is ALREADY the working directory for both \
         halves, so spell every path in `test_cmd` relative to it — \
         `cd {target} && bash witness_test.sh` becomes `bash witness_test.sh`.",
        target = escape.target
    )
}

/// What an empty baseline tree adds to a [`vacuous_message`] — the greenfield
/// fact, and the one witness shape that still discriminates there.
///
/// # Why this is a note and not a refusal
///
/// The forensics reported greenfield as *structurally* unwinnable: with the
/// baseline at [`EMPTY_TREE_OID`] there is no old code, so "fails on the old
/// code" is unreachable. The first version of this module refused such a call
/// outright on the strength of that claim.
///
/// The claim is false, and the witness test written to prove the refusal
/// disproved it instead: on a repository whose only commit is empty,
/// `test -f server.py` fails on the baseline, passes on the candidate, and
/// `verify_done` answers `WITNESS CONFIRMED` — a real flip, by the letter and
/// by the spirit. An **existence** assertion discriminates an empty tree from
/// a populated one perfectly well; it is only a *behavioural* assertion that
/// cannot, because the behaviour it would call has nowhere to live.
///
/// So refusing would have thrown away confirmations the tool already grants,
/// which is the exact inversion #2871 exists to stop: nothing that runs after
/// the work is done may destroy the work. What is left is a fact the author
/// cannot see and keeps paying for — `pypi-server` re-derived it at step 36
/// and kept calling anyway — so it is stated where it changes what they do
/// next, and nowhere else. The other half of the greenfield catch-22 — a
/// behavioural witness on a new module dying as
/// `WITNESS_INCONCLUSIVE_BUILD_ERROR` — is [`super::creation`]'s, and what
/// remains open on it is #2668.
pub(super) fn empty_baseline_note() -> &'static str {
    "\nTHE BASELINE TREE IS EMPTY — this baseline is git's empty tree \
     (4b825dc642cb6eb9a060e54bf8d69288fbee4904) and contains no files at all, which makes this \
     a greenfield change: every file the witness could touch is one your work created. No \
     assertion about the *behaviour* of that code can fail on the baseline, because the code \
     is not there to misbehave. The only witness that discriminates an empty tree is one about \
     what now EXISTS — assert the presence, and then the content, of what your change created, \
     and layer nothing but the test itself."
}

/// Whether `baseline_sha`'s tree is [`EMPTY_TREE_OID`].
///
/// `false` on any git failure: a question this cannot answer must never
/// manufacture a refusal, and the ordinary verdict path below is the honest
/// fallback.
pub(super) async fn baseline_is_empty_tree(root: &Path, baseline_sha: &str) -> bool {
    let mut cmd = super::git_in(root);
    cmd.args(["rev-parse", "--verify", "--quiet"]);
    cmd.arg(format!("{baseline_sha}^{{tree}}"));
    match crate::exec::run_captured(cmd, 30).await {
        crate::exec::Captured::Done(out) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).trim() == EMPTY_TREE_OID
        }
        _ => false,
    }
}

/// How one staged witness file compares to the same path at the baseline —
/// the observation that decides which defeater [`vacuous_message`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BaselineStatus {
    /// The path does not exist at the baseline commit. Expected of a witness
    /// test; also true of a newly created deliverable.
    Absent,
    /// The path exists at the baseline and the staged copy is different — so
    /// layering it put the working tree's version on the previous-code side.
    Differs,
    /// The path exists at the baseline and the staged copy is byte-identical,
    /// so layering it changed nothing.
    Identical,
    /// Git could not answer. Reported as such rather than folded into a
    /// neighbour: an unknown that reads as a fact is how a message starts
    /// misleading its reader.
    Unknown,
}

impl BaselineStatus {
    /// The phrase this status contributes to the observation list.
    const fn describe(self) -> &'static str {
        match self {
            Self::Absent => "absent at the baseline",
            Self::Differs => "EXISTS at the baseline and the staged copy DIFFERS",
            Self::Identical => "identical to the baseline copy",
            Self::Unknown => "could not be compared against the baseline",
        }
    }
}

/// Resolve `<baseline>:<path>` to a blob oid, or `None` when the path does not
/// exist there.
async fn baseline_blob(root: &Path, baseline_sha: &str, relpath: &Path) -> Option<String> {
    let mut cmd = super::git_in(root);
    cmd.args(["rev-parse", "--verify", "--quiet"]);
    cmd.arg(format!("{baseline_sha}:{}", relpath.display()));
    match crate::exec::run_captured(cmd, 30).await {
        crate::exec::Captured::Done(out) if out.status.success() => {
            let oid = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (!oid.is_empty()).then_some(oid)
        }
        _ => None,
    }
}

/// Hash the working-tree file exactly as git would store it at `relpath` —
/// `--path` so any clean filter configured for that path is applied and a
/// line-ending normalization cannot fake a difference.
async fn working_blob(root: &Path, src: &Path, relpath: &Path) -> Option<String> {
    let mut cmd = super::git_in(root);
    cmd.arg("hash-object");
    cmd.arg("--path");
    cmd.arg(relpath);
    cmd.arg("--");
    cmd.arg(src);
    match crate::exec::run_captured(cmd, 30).await {
        crate::exec::Captured::Done(out) if out.status.success() => {
            let oid = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (!oid.is_empty()).then_some(oid)
        }
        _ => None,
    }
}

/// Compare every staged witness file against the baseline.
///
/// Two `git` calls per file, on a terminal path that has already paid for a
/// full shadow checkout and two test runs — the cost is noise, and the answer
/// is the difference between a message that names the defeater and one that
/// guesses at three.
pub(super) async fn staged_baseline_status(
    root: &Path,
    baseline_sha: &str,
    resolved: &[(String, std::path::PathBuf, std::path::PathBuf)],
) -> Vec<(String, BaselineStatus)> {
    let mut out = Vec::with_capacity(resolved.len());
    for (display, src, relpath) in resolved {
        let status = match baseline_blob(root, baseline_sha, relpath).await {
            None => BaselineStatus::Absent,
            Some(base) => match working_blob(root, src, relpath).await {
                Some(now) if now == base => BaselineStatus::Identical,
                Some(_) => BaselineStatus::Differs,
                None => BaselineStatus::Unknown,
            },
        };
        out.push((display.clone(), status));
    }
    out
}

/// Gather the observations a `VACUOUS TEST` verdict rests on, then render it.
///
/// The whole arm lives here rather than at the call site because the two git
/// questions and the prose that reads them are one decision: split across the
/// boundary, a later reader of `verify.rs` sees two lookups whose purpose is
/// only legible in a message they would have to go and find.
pub(super) async fn vacuous_verdict(
    root: &Path,
    baseline: &super::WitnessBaseline,
    resolved: &[(String, std::path::PathBuf, std::path::PathBuf)],
    reuse_note: &str,
    output_tail: &str,
) -> String {
    let statuses = staged_baseline_status(root, &baseline.sha, resolved).await;
    let empty = baseline_is_empty_tree(root, &baseline.sha).await;
    vacuous_message(
        baseline.short(),
        &baseline.provenance,
        &statuses,
        empty,
        reuse_note,
        output_tail,
    )
}

/// The `VACUOUS TEST` verdict, built from what the call observed.
///
/// The old message offered three causes and closed with "strengthen the test".
/// This one reports the staged-file comparison first, then names exactly one
/// diagnosis — chosen by that comparison — and only reaches for "strengthen
/// the test" in the single case the phrase actually describes.
///
/// Split from [`vacuous_verdict`] so every branch of the prose is reachable
/// from a plain unit test with no git repository behind it.
fn vacuous_message(
    short: &str,
    provenance: &str,
    statuses: &[(String, BaselineStatus)],
    empty_baseline: bool,
    reuse_note: &str,
    output_tail: &str,
) -> String {
    let mut message = format!(
        "VACUOUS TEST — the witness test ALSO PASSES on the previous code (baseline {short}, \
         {provenance}), so it does not witness your change.\n\
         What this call observed about the file(s) it layered onto the baseline:\n"
    );
    for (path, status) in statuses {
        message.push_str(&format!("  - `{path}` — {}\n", status.describe()));
    }
    message.push_str(&diagnosis(statuses));
    if empty_baseline {
        message.push_str(empty_baseline_note());
    }
    message.push_str(reuse_note);
    message.push_str("\n--- previous-code output tail ---\n");
    message.push_str(output_tail);
    message
}

/// Which single defeater the staged-file comparison supports.
///
/// Ordered by how decisive the observation is, not by how common the cause is:
/// a `Differs` is proof that the previous-code run was handed part of the
/// change, and proof outranks the two readings that remain when there is none.
fn diagnosis(statuses: &[(String, BaselineStatus)]) -> String {
    let differing: Vec<&str> = statuses
        .iter()
        .filter(|(_, status)| *status == BaselineStatus::Differs)
        .map(|(path, _)| path.as_str())
        .collect();
    if !differing.is_empty() {
        let named = differing
            .iter()
            .map(|path| format!("`{path}`"))
            .collect::<Vec<_>>()
            .join(", ");
        return format!(
            "LAYERING DEFEATER — every file named in `test_files` is copied onto the baseline \
             before the previous-code run, in its NEW form. {named} exists at the baseline and \
             your copy differs from it, so your change to that file was handed to the \
             previous-code run: it could not fail, whatever the test asserts.\n\
             This is a fact about the call, NOT about your test — do not weaken or strengthen \
             the test in response. Name ONLY the witness test file(s) in `test_files`; the \
             implementation must stay out of it, and the test must reach the implementation \
             through the workspace rather than by being layered beside it."
        );
    }
    if statuses
        .iter()
        .all(|(_, status)| *status == BaselineStatus::Identical)
    {
        return "The layered file(s) are byte-identical to the baseline, so layering them \
                changed nothing and the previous-code run saw the true previous code. The test \
                genuinely does not discriminate: strengthen it so it fails without your change."
            .to_string();
    }
    "No layered file overwrote a baseline file, so the layering channel carried no modified \
     implementation. Two readings remain, and the previous-code output tail below tells them \
     apart — it is what the baseline actually did:\n\
     - one of the layered files IS this change's deliverable rather than a test. Then the \
     previous-code run was handed your implementation and could not fail: drop that file from \
     `test_files` and have the test assert its behaviour instead of shipping it alongside;\n\
     - the test genuinely does not discriminate. Then strengthen it so it fails without your \
     change."
        .to_string()
}

#[cfg(test)]
mod tests {
    use stella_protocol::tool::ToolOutput;

    use super::*;
    use crate::registry::Tool;
    use crate::verify::VerifyDone;
    // The git-repo fixture the tool's own tests build, shared for the same
    // reason `memo.rs` and `phases.rs` share it.
    use crate::verify::tests::{scaffold, scratch_git};

    /// #2871(a), end to end: the escape is refused by the tool, not merely
    /// detected by [`cwd_escape`]. On `main` this call reaches the shadow run
    /// and answers `VACUOUS` — an accusation against a test that is correct,
    /// which is what cost `query-optimize` ~70 steps.
    #[tokio::test]
    async fn an_absolute_cd_is_refused_instead_of_answering_vacuous() {
        let root = scaffold("cwdescape").await;
        std::fs::write(root.join("impl.txt"), "new behavior\n").unwrap();
        std::fs::write(root.join("witness.sh"), "grep -q 'new behavior' impl.txt\n").unwrap();

        let out = VerifyDone::default()
            .execute(
                &serde_json::json!({
                    "test_cmd": format!("cd {} && bash witness.sh", root.display()),
                    "test_files": ["witness.sh"],
                    "timeout_secs": 60
                }),
                &root,
            )
            .await;
        match &out {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("WITNESS COMMAND ESCAPES THE TEST TREE"),
                    "{message}"
                );
                // The word `VACUOUS` appears — as the verdict this refusal
                // replaced — but never as this call's own verdict, and never
                // with the advice that made it unterminating.
                assert!(
                    !message.contains("Strengthen the test"),
                    "an escaped cwd must not accuse the test: {message}"
                );
            }
            other => panic!("expected the escape refusal, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// Build a repository whose only commit is empty — the greenfield shape,
    /// where the whole deliverable is new and the baseline tree is
    /// [`EMPTY_TREE_OID`].
    async fn greenfield(tag: &str) -> std::path::PathBuf {
        let root = scaffold(tag).await;
        scratch_git(&root, &["checkout", "-q", "--orphan", "fresh"]);
        scratch_git(&root, &["rm", "-r", "--cached", "-q", "."]);
        scratch_git(&root, &["commit", "-q", "--allow-empty", "-m", "init"]);
        std::fs::remove_file(root.join("impl.txt")).unwrap();
        root
    }

    /// #2871(c), end to end: a `VACUOUS` on an empty baseline states the
    /// greenfield fact, so the author stops hunting for a behavioural
    /// difference that cannot exist. `pypi-server` re-derived that fact itself
    /// at step 36 and kept calling anyway; the bare arm burned ~13 minutes and
    /// 5,466 reasoning deltas on it.
    #[tokio::test]
    async fn a_vacuous_on_an_empty_baseline_states_the_greenfield_fact() {
        let root = greenfield("greenfield").await;
        std::fs::write(root.join("server.py"), "print('serving')\n").unwrap();
        std::fs::write(root.join("witness.sh"), "test -f server.py\n").unwrap();

        let out = VerifyDone::default()
            .execute(
                &serde_json::json!({
                    // `server.py` is the deliverable: layering it into the
                    // empty shadow is what makes the baseline pass.
                    "test_cmd": "bash witness.sh",
                    "test_files": ["witness.sh", "server.py"],
                    "timeout_secs": 60
                }),
                &root,
            )
            .await;
        match &out {
            ToolOutput::Error { message } => {
                assert!(message.contains("VACUOUS TEST"), "{message}");
                assert!(message.contains("THE BASELINE TREE IS EMPTY"), "{message}");
                assert!(message.contains("what now EXISTS"), "{message}");
            }
            other => panic!("expected the greenfield note on a vacuous verdict, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// The other half of #2871(c), and the reason it is a note rather than the
    /// refusal it was first written as: an **existence** assertion does flip
    /// against an empty tree, and that confirmation must survive. A refusal
    /// keyed on the baseline tree alone would have destroyed it — the exact
    /// inversion the whole issue exists to stop.
    #[tokio::test]
    async fn an_existence_witness_still_flips_against_an_empty_baseline() {
        let root = greenfield("greenfield_flip").await;
        std::fs::write(root.join("server.py"), "print('serving')\n").unwrap();
        std::fs::write(root.join("witness.sh"), "test -f server.py\n").unwrap();

        let out = VerifyDone::default()
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
                assert!(content.contains("WITNESS CONFIRMED"), "{content}")
            }
            other => panic!("a greenfield flip must still be credited, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// #2871(b), end to end: naming the deliverable in `test_files` layers it
    /// onto the baseline, and the verdict must say so. On `main` the same call
    /// answers "either the behavior already existed, the test doesn't exercise
    /// the new behavior, or your change isn't wired in. Strengthen the test" —
    /// three causes, none of them this one, and advice that cannot terminate.
    #[tokio::test]
    async fn a_layered_deliverable_is_named_as_the_defeater() {
        let root = scaffold("layering").await;
        std::fs::write(root.join("impl.txt"), "new behavior\n").unwrap();
        std::fs::write(root.join("witness.sh"), "grep -q 'new behavior' impl.txt\n").unwrap();

        let out = VerifyDone::default()
            .execute(
                &serde_json::json!({
                    // `impl.txt` is the deliverable, not a test — layering it
                    // hands the change to the previous-code run.
                    "test_cmd": "bash witness.sh",
                    "test_files": ["witness.sh", "impl.txt"],
                    "timeout_secs": 60
                }),
                &root,
            )
            .await;
        match &out {
            ToolOutput::Error { message } => {
                assert!(message.contains("VACUOUS TEST"), "{message}");
                assert!(message.contains("LAYERING DEFEATER"), "{message}");
                assert!(message.contains("`impl.txt`"), "{message}");
                assert!(
                    message.contains("`witness.sh` — absent at the baseline"),
                    "the observation list must cover every staged file: {message}"
                );
                assert!(
                    !message.contains("Strengthen the test"),
                    "the test is not the fault here: {message}"
                );
            }
            other => panic!("expected a named layering defeater, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_absolute_cd_in_command_position_is_an_escape() {
        // The exact command `f89p2/query-optimize` step 67 ran (#2871).
        let escape = cwd_escape("cd /tmp/stella_candidate_360_0 && bash witness_test.sh")
            .expect("an absolute cd escapes the tree the run was aimed at");
        assert_eq!(escape.target, "/tmp/stella_candidate_360_0");
        // And step 91, one prefix shorter, is clean.
        assert_eq!(cwd_escape("bash witness_test.sh"), None);
    }

    #[test]
    fn quotes_and_later_command_positions_are_seen_through() {
        for command in [
            "cd '/app' && pytest",
            "cd \"/app\" && pytest",
            "make build; cd /app && ./run.sh",
            "make build | cd /app",
            "pytest\ncd ~/work && pytest",
            "cd ~",
            "pushd /app && pytest",
            "cd -P /app && pytest",
        ] {
            assert!(
                cwd_escape(command).is_some(),
                "`{command}` escapes the tree and was not caught"
            );
        }
    }

    /// A chain is only as confined as its last hop, so a harmless `cd` must
    /// not end the scan. Caught in review on #2880: the first version returned
    /// `None` the moment it saw a relative destination, which cleared every
    /// absolute `cd` that came after it.
    #[test]
    fn a_harmless_cd_does_not_clear_an_absolute_one_later_in_the_chain() {
        assert_eq!(
            cwd_escape("cd src && cd /tmp && pytest").unwrap().target,
            "/tmp"
        );
        assert_eq!(
            cwd_escape("cd ./build; make; cd /app && ./run.sh")
                .unwrap()
                .target,
            "/app"
        );
        // The extra operand of a harmless `cd` is not itself a command, so it
        // cannot be mistaken for one when the scan resumes.
        assert_eq!(cwd_escape("cd src cd && pytest"), None);
    }

    /// A bare `cd` goes to `$HOME`, which is as far outside the shadow as any
    /// absolute path — and the message has to say so rather than quote an
    /// operand that was never written.
    #[test]
    fn a_bare_cd_is_an_escape_to_home() {
        assert_eq!(cwd_escape("cd && pytest").unwrap().target, "$HOME");
        assert_eq!(cwd_escape("cd").unwrap().target, "$HOME");
    }

    /// The refusal must be narrow: over-refusing sends an author to rewrite a
    /// command that was correct, which is the same harm in the other
    /// direction.
    #[test]
    fn relative_moves_and_non_command_mentions_are_not_escapes() {
        for command in [
            "cd src && cargo test",
            "cd ./tests && pytest",
            "cd \"$(git rev-parse --show-toplevel)\" && pytest",
            "cd $WORKDIR && pytest",
            "echo cd /tmp",
            "grep -q cd /etc/config",
            "python -c \"import os; os.chdir('/app')\"",
            "bash witness_test.sh",
            "",
        ] {
            assert_eq!(
                cwd_escape(command),
                None,
                "`{command}` is not an escape and must not be refused"
            );
        }
    }

    #[test]
    fn the_escape_refusal_names_the_target_and_the_repair() {
        let message = cwd_escape_message(&CwdEscape {
            target: "/tmp/stella_candidate_360_0".to_string(),
        });
        assert!(
            message.contains("WITNESS COMMAND ESCAPES THE TEST TREE"),
            "{message}"
        );
        assert!(message.contains("/tmp/stella_candidate_360_0"), "{message}");
        assert!(message.contains("NOT about your test"), "{message}");
        assert!(
            message.contains("becomes `bash witness_test.sh`"),
            "{message}"
        );
    }

    /// The greenfield note must state the fact and the one witness shape that
    /// still works — never an instruction to strengthen a test, and never a
    /// claim that no flip exists (one does; see
    /// `an_existence_witness_still_flips_against_an_empty_baseline`).
    #[test]
    fn the_greenfield_note_names_the_witness_shape_that_still_discriminates() {
        let note = empty_baseline_note();
        assert!(note.contains("THE BASELINE TREE IS EMPTY"), "{note}");
        assert!(note.contains(EMPTY_TREE_OID), "{note}");
        assert!(note.contains("what now EXISTS"), "{note}");
        assert!(!note.contains("trengthen"), "{note}");
    }

    /// The note rides a `VACUOUS` verdict and nothing else, so a confirmed
    /// greenfield flip is never editorialised.
    #[test]
    fn the_greenfield_note_is_opt_in_per_call() {
        let statuses = vec![("witness.sh".to_string(), BaselineStatus::Absent)];
        let with = vacuous_message("4b825dc6", "HEAD", &statuses, true, "", "tail");
        let without = vacuous_message("4b825dc6", "HEAD", &statuses, false, "", "tail");
        assert!(with.contains("THE BASELINE TREE IS EMPTY"), "{with}");
        assert!(!without.contains("THE BASELINE TREE IS EMPTY"), "{without}");
    }

    /// The layering defeater outranks everything else, because a `Differs` is
    /// proof that the previous-code run was handed part of the change.
    #[test]
    fn a_differing_staged_file_is_named_as_the_layering_defeater() {
        let statuses = vec![
            ("witness.sh".to_string(), BaselineStatus::Absent),
            ("src/solver.py".to_string(), BaselineStatus::Differs),
        ];
        let message = vacuous_message("abc12345", "HEAD", &statuses, false, "", "tail");
        assert!(message.contains("LAYERING DEFEATER"), "{message}");
        assert!(message.contains("`src/solver.py`"), "{message}");
        assert!(
            message.contains("do not weaken or strengthen the test"),
            "the test is not the fault here, and the message must say so: {message}"
        );
    }

    /// "Strengthen the test" survives in exactly the case it describes: every
    /// layered file matched the baseline, so nothing about the setup can
    /// explain the pass.
    #[test]
    fn identical_staged_files_are_the_one_case_that_blames_the_test() {
        let statuses = vec![("witness.sh".to_string(), BaselineStatus::Identical)];
        let message = vacuous_message("abc12345", "HEAD", &statuses, false, "", "tail");
        assert!(message.contains("does not discriminate"), "{message}");
        assert!(message.contains("strengthen it"), "{message}");
        assert!(!message.contains("LAYERING DEFEATER"), "{message}");
    }

    /// All-new staged files leave two readings open. Both are stated, and the
    /// deliverable-in-`test_files` one is stated first — it is the one the old
    /// message never mentioned at all.
    #[test]
    fn all_new_staged_files_state_both_readings_and_how_to_tell_them_apart() {
        let statuses = vec![
            ("witness.sh".to_string(), BaselineStatus::Absent),
            ("server.py".to_string(), BaselineStatus::Absent),
        ];
        let message = vacuous_message("abc12345", "HEAD", &statuses, false, "", "the tail");
        assert!(
            message.contains("IS this change's deliverable"),
            "{message}"
        );
        assert!(message.contains("does not discriminate"), "{message}");
        assert!(message.contains("the tail"), "{message}");
    }

    /// Every message names each staged file and what was observed about it —
    /// the half the old verdict had none of.
    #[test]
    fn the_observation_list_names_every_staged_file() {
        let statuses = vec![
            ("a.sh".to_string(), BaselineStatus::Absent),
            ("b.sh".to_string(), BaselineStatus::Identical),
            ("c.sh".to_string(), BaselineStatus::Unknown),
        ];
        let message = vacuous_message("abc12345", "HEAD", &statuses, false, "", "tail");
        for (path, status) in &statuses {
            assert!(
                message.contains(&format!("`{path}` — {}", status.describe())),
                "{message}"
            );
        }
    }
}
