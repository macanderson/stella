// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The first-session offer to convert markdown slash commands to TOML.
//!
//! # Why this is an offer and not a conversion
//!
//! TOML is how every other piece of workspace config is spelled in stella
//! (`.stella/tools/*.toml`, `.stella/rules/*.toml`, `.stella/domains.toml`),
//! and a slash command written in markdown is the one holdout. Closing that
//! gap is worth a prompt.
//!
//! It is *not* worth doing silently, and [`crate::commands_cmd`]'s module doc
//! says why: `stella init` adopts `.claude/commands/` and `.agents/commands/`
//! into `.stella/commands/` as **symlinks**, so a command edited in either
//! place stays live in both. Converting forks that — the `.toml` is a copy,
//! and the markdown original it came from starts drifting away from it. That
//! trade is the user's to make, so this module asks, and only ever asks once
//! per workspace.
//!
//! # The three ways this stays quiet
//!
//! A prompt nobody wants is worse than no feature, so the offer is suppressed
//! whenever it would be noise, and each suppression is a distinct arm of
//! [`decide`] rather than one fuzzy condition:
//!
//! - **Nothing would convert.** Counted by dry-running the real converter, so
//!   the number in the question is exactly what accepting would write — never
//!   a glob count that includes definitions the parser would reject anyway.
//! - **This workspace was already asked.** The answer is recorded under
//!   `.stella/private/`, and the presence of that marker is the whole
//!   condition: a declined offer never comes back, on any later `stella init`
//!   or `/init`.
//! - **Nobody is present to answer.** `init` runs in CI and in scripts;
//!   there, the offer degrades to a single hint line naming
//!   `stella commands convert`, and no marker is written — so the first
//!   *interactive* session still gets its one real ask.
//!
//! Accepting converts through [`crate::commands_cmd::convert_dir`], the same
//! path `stella commands convert` drives: definitions are parsed by the
//! loader's own parser, the markdown originals are never deleted, and an
//! existing `.toml` is never overwritten.

use std::path::{Path, PathBuf};

use crate::commands_cmd::{Converted, convert_dir};
use crate::interactive::AskUserIo;

/// The marker recording that this workspace has been asked, written beneath
/// `.stella/private/` so it is owner-only and never committed.
const MARKER: &str = "commands-offer.json";

/// What the offer should do this run.
///
/// Every arm that declines to ask names *why*, because the three reasons want
/// different behaviour downstream: a workspace with nothing to convert should
/// say nothing at all, a headless run should leave a hint and stay askable,
/// and an already-answered workspace must stay silent forever.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum OfferDecision {
    /// No convertible markdown definition — say nothing.
    NothingToConvert,
    /// This workspace already answered once — say nothing, ever again.
    AlreadyAsked,
    /// Convertible definitions exist but nobody can answer — leave a hint,
    /// write no marker, so the next interactive session still asks.
    NoHuman { pending: usize },
    /// Ask about `pending` convertible definitions.
    Ask { pending: usize },
}

/// The offer decision, as a pure function of what was observed.
///
/// Ordering is deliberate and is the behaviour, not an implementation
/// detail. `pending == 0` is checked first because it is the only arm that
/// must stay silent *and* leave the workspace askable — a workspace with no
/// commands yet has not declined anything, and writing a marker for it would
/// consume the one ask it is owed before it ever had commands to convert.
/// `already_asked` outranks `human_present` for the mirror reason: an
/// answered workspace is finished, and a later headless run must not
/// resurrect it as a hint.
pub(crate) fn decide(pending: usize, already_asked: bool, human_present: bool) -> OfferDecision {
    if pending == 0 {
        return OfferDecision::NothingToConvert;
    }
    if already_asked {
        return OfferDecision::AlreadyAsked;
    }
    if !human_present {
        return OfferDecision::NoHuman { pending };
    }
    OfferDecision::Ask { pending }
}

/// The command directories the offer covers: this workspace's, then the
/// user-global one when `user_root` names it.
///
/// Both are `.stella/commands/`, which is the point — `sync_extensions` has
/// already adopted `<root>/.claude`, `<root>/.agents`, `~/.claude` and
/// `~/.agents` into exactly these two directories by the time the offer runs,
/// so converting them converts everything without this module needing its own
/// idea of where another agent keeps its commands.
///
/// `user_root` is **passed in, never resolved here.** This function writes to
/// every directory it returns, and a version of it that read the ambient home
/// converted the developer's real `~/.stella/commands/` the first time the
/// unit tests ran against a temp workspace. An injected root is what makes
/// the blast radius a parameter a test can see rather than an environment it
/// happens to inherit — the same discipline `stella-runtime` pins with its
/// `no_ambient_reads` contract test.
pub(crate) fn command_dirs(workspace_root: &Path, user_root: Option<&Path>) -> Vec<PathBuf> {
    let workspace_commands = workspace_root.join(".stella").join("commands");
    let mut dirs = vec![workspace_commands.clone()];
    if let Some(user_commands) = user_root.map(|root| root.join("commands"))
        && user_commands != workspace_commands
    {
        dirs.push(user_commands);
    }
    dirs
}

/// The definitions a conversion of `dir` would actually write, found by
/// dry-running the real converter.
///
/// Deliberately not a glob: `convert_dir` skips a definition the loader
/// cannot parse and one that already has a `.toml`, and a question offering
/// to convert those would be a lie about what accepting does.
fn pending_in(dir: &Path) -> Vec<Converted> {
    convert_dir(dir, true, false)
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry.skipped.is_none())
        .collect()
}

/// Where this workspace's answer is recorded, or `None` when the private
/// state directory cannot be resolved.
fn marker_path(workspace_root: &Path) -> Option<PathBuf> {
    stella_store::workspace_private_state_path(workspace_root, MARKER).ok()
}

/// Whether this workspace has already been asked.
///
/// An unresolvable private directory answers `false`: failing to read the
/// marker must not silently consume the ask, and the worst case of asking
/// twice is a question, while the worst case of never asking is a feature
/// nobody discovers.
pub(crate) fn already_asked(workspace_root: &Path) -> bool {
    marker_path(workspace_root).is_some_and(|path| path.exists())
}

/// Record that this workspace answered, so it is never asked again.
///
/// Best-effort: a marker that cannot be written costs one repeated question
/// on the next init, which is not worth failing init over.
fn record_answer(workspace_root: &Path, accepted: bool, converted: usize) {
    let Some(path) = marker_path(workspace_root) else {
        return;
    };
    let body = serde_json::json!({
        "accepted": accepted,
        "converted": converted,
    });
    let _ = std::fs::write(&path, format!("{body}\n"));
}

/// The question, naming both what accepting does and what it costs.
///
/// The cost half is not decoration: the definitions most likely to be sitting
/// here are symlinks adopted from another agent's directory, and converting
/// one trades its live edit link for typed fields. A user who is not told
/// that cannot make the trade deliberately.
fn question(pending: usize) -> String {
    let noun = if pending == 1 { "command" } else { "commands" };
    format!(
        "{pending} markdown slash {noun} can be converted to TOML, the format the rest of \
         stella's config uses. This writes a `.toml` beside each `.md` and deletes nothing — \
         but a command adopted from `.claude/` or `.agents/` is a symlink, and its TOML copy \
         stops tracking edits to the original. Convert now?"
    )
}

/// Offer to convert this workspace's markdown slash commands to TOML, once.
///
/// Called from [`crate::agent::init_workspace`], so `stella init` and the
/// deck's `/init` share one implementation and one marker. `ask` is `None`
/// when no human can answer; passing the io rather than re-deriving presence
/// here keeps this module honest about the one thing it cannot observe.
pub(crate) async fn offer_conversion(
    workspace_root: &Path,
    user_root: Option<&Path>,
    ask: Option<&dyn AskUserIo>,
    emit: &mut dyn FnMut(String),
) {
    let dirs = command_dirs(workspace_root, user_root);
    let pending: Vec<(PathBuf, Vec<Converted>)> = dirs
        .into_iter()
        .map(|dir| {
            let found = pending_in(&dir);
            (dir, found)
        })
        .filter(|(_, found)| !found.is_empty())
        .collect();
    let total: usize = pending.iter().map(|(_, found)| found.len()).sum();

    match decide(total, already_asked(workspace_root), ask.is_some()) {
        OfferDecision::NothingToConvert | OfferDecision::AlreadyAsked => return,
        OfferDecision::NoHuman { pending } => {
            emit(format!(
                "· {pending} markdown slash command(s) could be converted to TOML — \
                 run `stella commands convert`"
            ));
            return;
        }
        OfferDecision::Ask { .. } => {}
    }

    let options = vec![
        "Convert them to TOML".to_string(),
        "Keep markdown (this is asked once — you can still run `stella commands convert`)"
            .to_string(),
    ];
    // An io that fails mid-prompt is a decline, never an accept: this writes
    // files, and silence must never be read as consent. It is also not
    // recorded, so the next interactive session gets the ask it never had.
    let Ok(raw) = ask
        .expect("Ask implies an io")
        .prompt(&question(total), &options)
        .await
    else {
        emit("! could not ask about TOML conversion — nothing was converted".to_string());
        return;
    };

    let answer = raw.trim();
    let accepted =
        answer == "1" || answer.eq_ignore_ascii_case("yes") || answer.eq_ignore_ascii_case("y");
    if !accepted {
        emit("· keeping markdown commands — `stella commands convert` converts them later".into());
        record_answer(workspace_root, false, 0);
        return;
    }

    let mut converted = 0usize;
    for (dir, _) in &pending {
        match convert_dir(dir, false, false) {
            Ok(done) => {
                for entry in done.iter().filter(|e| e.skipped.is_none()) {
                    converted += 1;
                    emit(format!("✓ wrote {}", entry.target.display()));
                }
                for entry in done.iter().filter(|e| e.skipped.is_some()) {
                    let why = entry.skipped.as_deref().unwrap_or("");
                    emit(format!("! skipped {}: {why}", entry.source.display()));
                }
            }
            Err(error) => emit(format!("! could not convert {}: {error}", dir.display())),
        }
    }
    emit(format!(
        "✓ {converted} command(s) converted to TOML — the markdown originals are untouched"
    ));
    record_answer(workspace_root, true, converted);
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Scripted io: records every question it was asked, answers from a
    /// queue, and panics if asked more often than it was scripted for —
    /// which is how "never asks twice" becomes a test failure rather than a
    /// silently popped `None`.
    struct ScriptedIo {
        answers: Mutex<Vec<String>>,
        asked: Mutex<Vec<String>>,
    }

    impl ScriptedIo {
        fn new(answers: &[&str]) -> Self {
            Self {
                answers: Mutex::new(answers.iter().rev().map(|a| a.to_string()).collect()),
                asked: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl AskUserIo for ScriptedIo {
        async fn prompt(&self, question: &str, _options: &[String]) -> Result<String, String> {
            self.asked.lock().unwrap().push(question.to_string());
            Ok(self
                .answers
                .lock()
                .unwrap()
                .pop()
                .expect("asked more questions than the script answers"))
        }
    }

    /// An io that fails the test if it is ever consulted — the shape "this
    /// workspace must not be asked again" needs an observable, and a silent
    /// no-op io would pass whether or not the suppression worked.
    struct NeverAsk;

    #[async_trait::async_trait]
    impl AskUserIo for NeverAsk {
        async fn prompt(&self, question: &str, _options: &[String]) -> Result<String, String> {
            panic!("this workspace must never be asked again, but was asked: {question}");
        }
    }

    fn command_md(name: &str) -> String {
        format!("---\nname: {name}\ndescription: does {name}\n---\n\nRun {name} now.\n")
    }

    /// Seed a workspace with `n` markdown commands and return its root.
    fn workspace_with_commands(names: &[&str]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join(".stella").join("commands");
        std::fs::create_dir_all(&dir).expect("mkdir");
        for name in names {
            std::fs::write(dir.join(format!("{name}.md")), command_md(name)).expect("write");
        }
        tmp
    }

    /// The offer writes only where it was told to write.
    ///
    /// Regression witness for a defect this module shipped with and its own
    /// tests caught: `command_dirs` resolved the user-global root from the
    /// ambient home, so running the suite against a temp workspace converted
    /// the developer's real `~/.stella/commands/`. `None` must mean "no user
    /// scope", not "go and find one".
    #[test]
    fn no_user_root_means_the_workspace_and_nothing_else() {
        let dirs = command_dirs(Path::new("/ws"), None);
        assert_eq!(dirs, vec![PathBuf::from("/ws/.stella/commands")]);

        let dirs = command_dirs(Path::new("/ws"), Some(Path::new("/home/.stella")));
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/ws/.stella/commands"),
                PathBuf::from("/home/.stella/commands"),
            ]
        );
    }

    /// A workspace that *is* the user root is converted once, not twice —
    /// otherwise the second pass reports every file as "a .toml already
    /// exists" and the transcript accuses the user of a conflict they never
    /// had.
    #[test]
    fn a_workspace_that_is_its_own_user_root_is_listed_once() {
        let dirs = command_dirs(Path::new("/ws"), Some(Path::new("/ws/.stella")));
        assert_eq!(dirs, vec![PathBuf::from("/ws/.stella/commands")]);
    }

    #[test]
    fn nothing_to_convert_outranks_every_other_reason() {
        // A workspace with no commands has declined nothing, so it must not
        // be marked as answered — the arm order is the behaviour.
        assert_eq!(decide(0, false, true), OfferDecision::NothingToConvert);
        assert_eq!(decide(0, true, false), OfferDecision::NothingToConvert);
    }

    #[test]
    fn an_answered_workspace_is_silent_even_headless() {
        assert_eq!(decide(3, true, true), OfferDecision::AlreadyAsked);
        assert_eq!(decide(3, true, false), OfferDecision::AlreadyAsked);
    }

    #[test]
    fn a_headless_run_hints_but_stays_askable() {
        assert_eq!(decide(2, false, false), OfferDecision::NoHuman { pending: 2 });
        assert_eq!(decide(2, false, true), OfferDecision::Ask { pending: 2 });
    }

    /// The witness: accepting the first-session offer converts every markdown
    /// command to TOML, leaves the markdown in place, and — the half that
    /// makes it a *first-session* offer rather than a nag — never asks again.
    /// Fails on old code, where no offer existed at any init.
    #[tokio::test]
    async fn the_offer_converts_once_and_never_asks_again() {
        let tmp = workspace_with_commands(&["ship", "review"]);
        let root = tmp.path();
        let mut lines: Vec<String> = Vec::new();

        let io = ScriptedIo::new(&["1"]);
        offer_conversion(root, None, Some(&io), &mut |line| lines.push(line)).await;

        let commands = root.join(".stella").join("commands");
        assert!(commands.join("ship.toml").is_file(), "ship converted");
        assert!(commands.join("review.toml").is_file(), "review converted");
        assert!(
            commands.join("ship.md").is_file(),
            "the markdown original is never deleted"
        );
        assert_eq!(io.asked.lock().unwrap().len(), 1, "asked exactly once");
        assert!(
            lines.iter().any(|l| l.contains("2 command(s) converted")),
            "the conversion is stated in the transcript: {lines:?}"
        );

        // Second init over the same workspace: the marker must suppress the
        // question outright, not merely find nothing left to do.
        std::fs::write(commands.join("deploy.md"), command_md("deploy")).expect("write");
        offer_conversion(root, None, Some(&NeverAsk), &mut |line| lines.push(line)).await;
        assert!(
            !commands.join("deploy.toml").exists(),
            "an answered workspace converts nothing further on its own"
        );
    }

    /// Declining is recorded just as firmly as accepting: nothing converts,
    /// and the workspace is never asked again.
    #[tokio::test]
    async fn declining_converts_nothing_and_is_remembered() {
        let tmp = workspace_with_commands(&["ship"]);
        let root = tmp.path();
        let mut lines: Vec<String> = Vec::new();

        let io = ScriptedIo::new(&["2"]);
        offer_conversion(root, None, Some(&io), &mut |line| lines.push(line)).await;

        let commands = root.join(".stella").join("commands");
        assert!(!commands.join("ship.toml").exists(), "nothing converted");
        assert!(already_asked(root), "the decline is recorded");

        offer_conversion(root, None, Some(&NeverAsk), &mut |line| lines.push(line)).await;
    }

    /// A headless init leaves a hint and writes no marker, so the first
    /// interactive session still gets its one real ask. Without this, a
    /// single CI run would permanently consume the offer for the repository.
    #[tokio::test]
    async fn a_headless_init_does_not_consume_the_ask() {
        let tmp = workspace_with_commands(&["ship"]);
        let root = tmp.path();
        let mut lines: Vec<String> = Vec::new();

        offer_conversion(root, None, None, &mut |line| lines.push(line)).await;

        assert!(
            !already_asked(root),
            "a run nobody could answer must not count as an answer"
        );
        assert!(
            lines.iter().any(|l| l.contains("stella commands convert")),
            "the headless path names the manual route: {lines:?}"
        );

        let io = ScriptedIo::new(&["1"]);
        offer_conversion(root, None, Some(&io), &mut |line| lines.push(line)).await;
        assert_eq!(
            io.asked.lock().unwrap().len(),
            1,
            "the first interactive session still gets asked"
        );
    }
}
