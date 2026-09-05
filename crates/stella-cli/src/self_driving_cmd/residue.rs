//! The end-of-turn residue gate (`doc:backlog-self-driving` phase B5).
//!
//! A turn can say it is leaving work undone. That statement is residue.
//! This module finds it and files it as an issue. Filing goes through
//! [`super::backlog::file_finding`], so the seen-set dedup and the
//! convention check hold here too.

use stella_autonomy::BacklogConvention;
use stella_protocol::issue::{IssueDraft, IssueLabel, IssueProvider};
use stella_protocol::{CompletionMessage, MessageRole};

use super::backlog::{self, Filed};

/// Phrases that mark work a turn is not doing.
///
/// A fixed list, matched as lowercase substrings. Never a model call.
/// The list stays small on purpose. A missed statement costs one unfiled
/// issue. A false hit costs a wrong issue a human must close. The first
/// error is cheaper, so the list leans that way.
const MARKERS: &[&str] = &[
    "follow-up:",
    "follow up:",
    "as a follow-up",
    "in a follow-up",
    "a follow-up issue",
    "needs a follow-up",
    "left as a follow-up",
    "for a follow-up",
    "deferred to a later",
    "deferring this",
    "left for later",
    "left for a future",
    "remains unhandled",
    "still doesn't handle",
    "still does not handle",
    "not yet implemented",
    "out of scope for this change",
];

/// Markers that count only at the start of a line.
///
/// A line that opens with "todo:" is a statement. The same word later in
/// a sentence is usually a quote — a search command, a lint name.
const LINE_START_MARKERS: &[&str] = &["todo:"];

/// The most filings one turn may cause.
///
/// More hits than this is not eight deferrals. It is a detector misfiring.
/// The cap turns that failure into bounded noise, not a filing storm.
const MAX_STATEMENTS_PER_TURN: usize = 8;

/// Longest title drawn from a statement, in characters.
const TITLE_CHARS: usize = 90;

/// The label every residue filing carries.
///
/// A residue statement names behavior the turn knows is missing. The type
/// axis spells that `bug`. A convention without that member refuses the
/// draft, and the refusal stands. The gate never invents a vocabulary.
const RESIDUE_LABEL: &str = "bug";

/// Statements of leftover work in a turn's prose.
///
/// Line by line, and conservative. Fenced code is skipped: a `TODO` in a
/// quoted diff is the code's debt, not the turn's. Block quotes are
/// skipped: quoted text is someone else's words. A line counts once, no
/// matter how many markers it holds.
pub(super) fn detect_residue(prose: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut in_fence = false;

    for line in prose.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || trimmed.starts_with('>') || trimmed.is_empty() {
            continue;
        }
        if statements.len() >= MAX_STATEMENTS_PER_TURN {
            break;
        }

        let lower = trimmed.to_lowercase();
        let hit = LINE_START_MARKERS.iter().any(|m| lower.starts_with(m))
            || MARKERS.iter().any(|m| lower.contains(m));
        if hit && !statements.iter().any(|s| s == trimmed) {
            statements.push(trimmed.to_owned());
        }
    }

    statements
}

/// What the gate scans, given the operator's switch.
///
/// The switch check lives here, not in the caller. That makes "off files
/// nothing" a property a test can hold, not a habit of whoever wires the
/// hook.
pub(super) fn gate_statements(enabled: bool, prose: &str) -> Vec<String> {
    if !enabled {
        return Vec::new();
    }
    detect_residue(prose)
}

/// One statement's trip through the filing path.
pub(super) struct ResidueOutcome {
    /// The draft's title. The dedup digest is derived from it.
    pub(super) title: String,
    /// What the filing path decided, or why the tracker was out of reach.
    pub(super) result: Result<Filed, String>,
}

/// The issue a statement becomes.
fn residue_draft(statement: &str) -> IssueDraft {
    let mut title: String = statement.trim().chars().take(TITLE_CHARS).collect();
    if title.chars().count() == TITLE_CHARS && statement.trim().chars().count() > TITLE_CHARS {
        title = format!("{}…", title.trim_end());
    }
    IssueDraft {
        title: format!("residue: {title}"),
        body: format!(
            "The end-of-turn residue gate found an explicit leftover-work statement in a \
             turn's transcript:\n\n> {}\n\nFiled automatically so stated follow-up work \
             cannot evaporate when the session ends (`doc:backlog-self-driving` phase B5).",
            statement.trim()
        ),
        labels: vec![IssueLabel {
            name: RESIDUE_LABEL.to_owned(),
        }],
        parent: None,
        assignee: None,
    }
}

/// File every detected statement through [`backlog::file_finding`].
///
/// The seen-set grows as the loop runs. Two equal statements in one turn
/// file once: the first filing's digest blocks the second.
pub(super) async fn file_residue(
    provider: &dyn IssueProvider,
    convention: &BacklogConvention,
    seen: &[String],
    signature: &str,
    statements: &[String],
) -> Vec<ResidueOutcome> {
    let mut session_seen: Vec<String> = seen.to_vec();
    let mut outcomes = Vec::new();

    for statement in statements {
        let draft = residue_draft(statement);
        let title = draft.title.clone();
        let result = backlog::file_finding(provider, convention, &session_seen, &draft, signature)
            .await
            .map_err(|error| error.to_string());
        if let Ok(Filed::New(_)) = &result {
            session_seen.push(stella_autonomy::finding_digest(&title));
        }
        outcomes.push(ResidueOutcome { title, result });
    }

    outcomes
}

/// The end-of-turn hook: scan, file, report. Best-effort throughout.
///
/// Every failure is a notice, never a failed turn. Outcome counts land in
/// the same session stats the `self-driving file` verb feeds. A gate whose
/// drafts all get refused is visible, not silent.
pub(crate) async fn end_of_turn(messages: &[CompletionMessage]) -> Vec<String> {
    let Ok(root) = std::env::current_dir() else {
        return Vec::new();
    };
    let cfg = super::config::load(&root);
    let prose: String = messages
        .iter()
        .filter(|m| matches!(m.role, MessageRole::Assistant))
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let statements = gate_statements(cfg.residue_gate, &prose);
    if statements.is_empty() {
        return Vec::new();
    }

    let Ok(st) = super::state::LoopState::open() else {
        return vec!["residue gate: cannot open the loop's state directory".to_owned()];
    };
    let bound = super::convention::load(&root);
    let outcomes = file_residue(
        &crate::issue_provider::GhIssueProvider,
        &bound.convention,
        &st.seen(),
        &cfg.attribution.issue,
        &statements,
    )
    .await;

    let mut notices = Vec::new();
    for outcome in &outcomes {
        match &outcome.result {
            Ok(Filed::New(key)) => {
                let _ = st.add_seen(&stella_autonomy::finding_digest(&outcome.title));
                st.update_stats(|s| s.record_filing("new"));
                notices.push(format!("residue gate filed #{key}: {}", outcome.title));
            }
            Ok(Filed::Duplicate { .. }) => {
                st.update_stats(|s| s.record_filing("duplicate"));
            }
            Ok(Filed::Refused { .. }) => {
                st.update_stats(|s| s.record_filing("refused"));
                notices.push(format!(
                    "residue gate: the issue convention refused a draft for \"{}\"",
                    outcome.title
                ));
            }
            Err(error) => {
                notices.push(format!("residue gate: could not file ({error})"));
            }
        }
    }
    notices
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use stella_autonomy::{Acceptance, AxisRequirement, ConventionSource, LabelAxis};
    use stella_protocol::issue::{Issue, IssueError, IssueKey};

    use super::*;

    /// A tracker that is not GitHub and not a process. The witnesses below
    /// file against it.
    #[derive(Default)]
    struct FixtureForge {
        filed: std::sync::Mutex<Vec<IssueDraft>>,
    }

    impl FixtureForge {
        fn filed(&self) -> Vec<IssueDraft> {
            self.filed.lock().expect("fixture lock").clone()
        }
    }

    #[async_trait]
    impl IssueProvider for FixtureForge {
        fn id(&self) -> &str {
            "fixture"
        }

        async fn list_open(&self, _limit: usize) -> Result<Vec<Issue>, IssueError> {
            Ok(Vec::new())
        }

        async fn file(&self, draft: &IssueDraft) -> Result<IssueKey, IssueError> {
            let mut filed = self.filed.lock().expect("fixture lock");
            filed.push(draft.clone());
            Ok(IssueKey::from(format!("{}", 2000 + filed.len()).as_str()))
        }

        async fn close(
            &self,
            _key: &IssueKey,
            _receipt: &str,
            _state: &str,
        ) -> Result<(), IssueError> {
            Ok(())
        }

        async fn comment(&self, _key: &IssueKey, _body: &str) -> Result<(), IssueError> {
            Ok(())
        }

        async fn relabel(
            &self,
            _key: &IssueKey,
            _add: &[String],
            _remove: &[String],
        ) -> Result<(), IssueError> {
            Ok(())
        }

        async fn edit(
            &self,
            _key: &IssueKey,
            _title: Option<&str>,
            _body: Option<&str>,
        ) -> Result<(), IssueError> {
            Ok(())
        }
    }

    /// This repository's convention, as `issue-triage.yml` enforces it.
    fn convention() -> BacklogConvention {
        BacklogConvention {
            axes: vec![LabelAxis {
                name: "type".into(),
                members: ["bug", "feature", "chore", "documentation", "epic"]
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect(),
                requirement: AxisRequirement::ExactlyOne,
                source: ConventionSource::Enforced,
            }],
            reserved: vec!["triage".into()],
            acceptance: Acceptance::Bound,
        }
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(f)
    }

    const TRANSCRIPT: &str = "I fixed the retry loop and added the witness.\n\
        The client still doesn't handle the 429 case — follow-up.\n\
        Deferring this: the config reload path needs its own lock audit.\n\
        Everything else is covered by the new tests.";

    /// **The B5 witness.** Two stated leftovers file exactly two issues on
    /// the mock forge. A re-run with those digests seen files zero.
    #[test]
    fn two_residue_statements_file_two_issues_and_a_rerun_files_none() {
        let statements = detect_residue(TRANSCRIPT);
        assert_eq!(
            statements.len(),
            2,
            "expected exactly two residue statements, got {statements:?}"
        );

        let forge = FixtureForge::default();
        let outcomes = block_on(file_residue(
            &forge,
            &convention(),
            &[],
            "Filed by stella.",
            &statements,
        ));
        let digests: Vec<String> = outcomes
            .iter()
            .filter(|o| matches!(o.result, Ok(Filed::New(_))))
            .map(|o| stella_autonomy::finding_digest(&o.title))
            .collect();
        assert_eq!(digests.len(), 2, "both statements must reach the tracker");
        assert_eq!(forge.filed().len(), 2);

        // The re-run: same transcript, first run's digests now seen.
        let rerun_forge = FixtureForge::default();
        let rerun = block_on(file_residue(
            &rerun_forge,
            &convention(),
            &digests,
            "Filed by stella.",
            &detect_residue(TRANSCRIPT),
        ));
        assert!(
            rerun
                .iter()
                .all(|o| matches!(o.result, Ok(Filed::Duplicate { .. }))),
            "every re-run outcome must be a duplicate"
        );
        assert!(
            rerun_forge.filed().is_empty(),
            "a deduplicated statement must never reach the tracker again"
        );
    }

    /// The same statement twice in one turn files once. The first filing's
    /// digest blocks the second before the provider is reached.
    #[test]
    fn a_statement_repeated_within_one_turn_files_once() {
        let forge = FixtureForge::default();
        let statement = "Deferring this: the config reload path needs a lock audit.".to_owned();
        let outcomes = block_on(file_residue(
            &forge,
            &convention(),
            &[],
            "Filed by stella.",
            &[statement.clone(), statement],
        ));
        assert!(matches!(outcomes[0].result, Ok(Filed::New(_))));
        assert!(matches!(outcomes[1].result, Ok(Filed::Duplicate { .. })));
        assert_eq!(forge.filed().len(), 1);
    }

    /// **The off switch files nothing.** The gate hands the filing loop an
    /// empty list, so no provider is ever reached.
    #[test]
    fn the_off_switch_detects_and_files_nothing() {
        assert!(gate_statements(false, TRANSCRIPT).is_empty());

        let forge = FixtureForge::default();
        let outcomes = block_on(file_residue(
            &forge,
            &convention(),
            &[],
            "Filed by stella.",
            &gate_statements(false, TRANSCRIPT),
        ));
        assert!(outcomes.is_empty());
        assert!(forge.filed().is_empty());
    }

    /// Conformance refusal stands. An unaccepted convention refuses every
    /// draft before the tracker is reached.
    #[test]
    fn a_refused_residue_draft_never_reaches_the_tracker() {
        let mut proposed = convention();
        proposed.acceptance = Acceptance::Proposed;

        let forge = FixtureForge::default();
        let outcomes = block_on(file_residue(
            &forge,
            &proposed,
            &[],
            "Filed by stella.",
            &detect_residue(TRANSCRIPT),
        ));
        assert!(
            outcomes
                .iter()
                .all(|o| matches!(o.result, Ok(Filed::Refused { .. }))),
            "an unaccepted convention must refuse every draft"
        );
        assert!(forge.filed().is_empty());
    }

    /// Ordinary completion prose trips nothing. The detector's value is
    /// what it does not match.
    #[test]
    fn ordinary_completion_prose_trips_nothing() {
        let prose = "I fixed the retry loop, added the witness test, and the suite is \
                     green. The change follows up on the earlier refactor and handles \
                     every error class the caller can see.";
        assert!(detect_residue(prose).is_empty());
    }

    /// A `TODO` in fenced code is the code's debt. A block quote is
    /// someone else's words.
    #[test]
    fn code_fences_and_quotes_are_not_residue() {
        let prose = "Here is the relevant snippet:\n\
            ```rust\n// TODO: handle the 429 case\n```\n\
            > TODO: the quoted issue body says this\n\
            All of it is already tracked.";
        assert!(detect_residue(prose).is_empty());
    }

    /// A line that opens with `todo:` is a statement. One that mentions
    /// the word mid-sentence is usually a quote.
    #[test]
    fn todo_counts_only_at_the_start_of_a_line() {
        assert_eq!(
            detect_residue("TODO: wire the config reload lock.").len(),
            1
        );
        assert!(detect_residue("I searched for the string todo: nothing matched.").is_empty());
    }

    /// `residue_gate = "off"` parses. An absent key means on.
    #[test]
    fn the_switch_parses_and_defaults_on() {
        use crate::settings::toml_config::{ResidueGateSwitch, TomlConfig};
        let path = std::path::Path::new("stella.toml");

        let off = TomlConfig::parse("[self_driving]\nresidue_gate = \"off\"\n", path)
            .expect("a valid document");
        assert_eq!(off.self_driving.residue_gate, ResidueGateSwitch::Off);
        assert!(!off.self_driving.residue_gate.enabled());

        let absent = TomlConfig::parse("", path).expect("an empty document");
        assert!(absent.self_driving.residue_gate.enabled());
    }
}
