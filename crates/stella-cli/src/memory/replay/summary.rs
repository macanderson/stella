//! What a replay built, and the honest way to report it.
//!
//! `doc:trace-replay-learning-harness` §6. The summary is the harness's whole
//! output, so two properties matter more than completeness:
//!
//! 1. **It must be reproducible.** Assertion 8 compares two replays of one trace
//!    byte for byte, so nothing minted from a process-global source may appear
//!    here. That rules out `edge.public_id` specifically — `EDGE_SEQ` is a
//!    process-global counter, so the same logical edge gets different ids in two
//!    replays *inside one process*. Content, never identity.
//! 2. **It must not flatter.** An artifact count that rises because the fixture
//!    repeated one lesson forty times measures the **fixture**. So the corpus's
//!    own recurrence profile is printed *beside* the artifact counts, never
//!    instead of them, and [`ReplaySummary::render`] cannot emit one without the
//!    other. Reporting a learner's yield without the shape of what it was fed is
//!    the kind of small dishonesty that costs more than it buys.

use std::collections::BTreeSet;
use std::path::Path;

use stella_core::tool_foundry::{GapDetectionConfig, ShellInvocation, detect_tool_gaps};

use super::trace::{ScriptedReflection, Trace};
use crate::memory::ReflectionReport;

/// What one replay built, plus the corpus shape it was built from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ReplaySummary {
    /// The fixture's own one-line description. A number that cannot name its
    /// corpus is not reportable (§11's "the corpus proves the corpus").
    pub description: String,
    // ---- what the corpus supplied -----------------------------------------
    pub sessions: usize,
    pub turns: usize,
    pub distinct_tasks: usize,
    /// Turns whose scripted reflection was a well-formed lessons array.
    pub turns_with_lessons: usize,
    /// Turns scripted as an unreadable response.
    pub turns_unreadable: usize,
    /// Turns scripted as a failed model call.
    pub turns_model_error: usize,
    /// Turns whose source carried no reflection at all — counted, never folded
    /// into "nothing to learn". An adapter over a corpus with no reflection JSON
    /// (§7.2) produces these, and a metric that silently counted them as empty
    /// reflections would report the learner as idle when it was never asked.
    pub turns_not_recorded: usize,
    /// Lessons the corpus offered, before any filtering.
    pub lessons_offered: usize,
    // ---- what the learners built ------------------------------------------
    /// Lessons that actually reached the context store.
    pub lessons_recorded: usize,
    /// Distinct memory texts in the store at the end.
    pub memories: usize,
    /// `SKILL.md` files on disk at the end.
    pub skills: Vec<String>,
    /// Published rule records on disk at the end.
    pub rules: Vec<String>,
    /// Tool proposals the foundry would mint from the whole shell history.
    pub tools: Vec<String>,
    // ---- when ---------------------------------------------------------------
    /// Turn ordinal (1-based) at which each artifact class first appeared.
    ///
    /// All four are measured — probed per turn, `None` when the class never
    /// appeared and rendered as an em dash. `first_skill_turn` used to be set
    /// to the total turn count whenever any skill existed, which printed
    /// `skill 10` beside two measured values and read as a measurement
    /// (#2359); `first_rule_turn` did not exist at all.
    pub first_memory_turn: Option<usize>,
    pub first_skill_turn: Option<usize>,
    pub first_rule_turn: Option<usize>,
    pub first_tool_turn: Option<usize>,
    /// Every turn's reported model error, in order — the starvation record.
    ///
    /// A turn that recorded nothing *and* said why is a different event from a
    /// turn that recorded nothing quietly, and assertion 7 is the difference.
    pub model_errors: Vec<String>,
}

impl ReplaySummary {
    pub(crate) fn new(trace: &Trace) -> Self {
        let mut summary = ReplaySummary {
            description: trace.description.clone(),
            sessions: trace.sessions.len(),
            distinct_tasks: trace.distinct_tasks(),
            ..Default::default()
        };
        for (_, turn) in trace.turns() {
            match &turn.reflection {
                ScriptedReflection::Lessons { lessons, .. } => {
                    summary.turns_with_lessons += 1;
                    summary.lessons_offered += lessons.len();
                }
                ScriptedReflection::Unreadable { .. } => summary.turns_unreadable += 1,
                ScriptedReflection::ModelError { .. } => summary.turns_model_error += 1,
                ScriptedReflection::NotRecorded => summary.turns_not_recorded += 1,
            }
        }
        summary
    }

    /// Fold one replayed turn into the summary.
    ///
    /// Skills and rules are probed on disk here rather than tallied at their
    /// write sites, for the reason [`observe_final_state`] gives: what is on
    /// disk is what a later session would load, and a counter incremented at
    /// the write site keeps claiming a file a no-clobber refusal never
    /// created. Two directory reads per turn against a workspace of a handful
    /// of files — the cost #2350 declined to pay when nothing asserted on the
    /// number, and the number was a placeholder for exactly as long (#2359).
    ///
    /// [`observe_final_state`]: ReplaySummary::observe_final_state
    pub(crate) fn observe_turn(
        &mut self,
        root: &Path,
        report: &ReflectionReport,
        tool_proposals: usize,
    ) {
        self.turns += 1;
        self.lessons_recorded += report.recorded;
        if report.recorded > 0 && self.first_memory_turn.is_none() {
            self.first_memory_turn = Some(self.turns);
        }
        if tool_proposals > 0 && self.first_tool_turn.is_none() {
            self.first_tool_turn = Some(self.turns);
        }
        if self.first_skill_turn.is_none()
            && !crate::memory::skill_paths_on_disk(&crate::memory::workspace_skills_dir(root))
                .is_empty()
        {
            self.first_skill_turn = Some(self.turns);
        }
        if self.first_rule_turn.is_none() && !published_rules(root).is_empty() {
            self.first_rule_turn = Some(self.turns);
        }
        if let Some(error) = &report.model_error {
            self.model_errors.push(error.clone());
        }
    }

    /// Read the workspace and the foundry once the last turn has run.
    ///
    /// Deliberately a filesystem read rather than an accumulated tally: what is
    /// *on disk* is what a later session would actually load, and a counter
    /// incremented at the write site would keep claiming a file that a
    /// no-clobber refusal never created.
    pub(crate) fn observe_final_state(
        &mut self,
        root: &Path,
        shell_history: &[ShellInvocation],
        foundry: GapDetectionConfig,
    ) {
        self.skills = relative_names(
            root,
            &crate::memory::skill_paths_on_disk(&crate::memory::workspace_skills_dir(root)),
        );
        self.rules = published_rules(root);
        self.tools = detect_tool_gaps(shell_history, foundry)
            .into_iter()
            .map(|proposal| proposal.name)
            .collect();
        self.memories = memory_texts(root).len();
    }

    /// The §6 report, as `make replay-learning` prints it.
    ///
    /// Deterministic by construction: every list is sorted, and no field here is
    /// minted from a process-global source.
    pub(crate) fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("── trace-replay learning harness ──────────────────────\n");
        if !self.description.is_empty() {
            out.push_str(&format!("corpus: {}\n", self.description));
        }
        out.push_str("\nwhat the corpus supplied\n");
        out.push_str(&format!(
            "  {} session(s), {} turn(s), {} distinct task(s)\n",
            self.sessions, self.turns, self.distinct_tasks
        ));
        out.push_str(&format!(
            "  reflections: {} with lessons, {} unreadable, {} model error(s), \
             {} not recorded by the source\n",
            self.turns_with_lessons,
            self.turns_unreadable,
            self.turns_model_error,
            self.turns_not_recorded
        ));
        out.push_str(&format!(
            "  {} lesson(s) offered across {} turn(s) — recurrence {:.2} lesson/turn\n",
            self.lessons_offered,
            self.turns,
            recurrence(self.lessons_offered, self.turns),
        ));

        out.push_str("\nwhat the learners built\n");
        out.push_str(&format!(
            "  memories: {} distinct in store ({} lesson(s) recorded of {} offered)\n",
            self.memories, self.lessons_recorded, self.lessons_offered
        ));
        out.push_str(&format!("  skills:   {}\n", render_list(&self.skills)));
        out.push_str(&format!("  rules:    {}\n", render_list(&self.rules)));
        out.push_str(&format!("  tools:    {}\n", render_list(&self.tools)));
        // All four classes §6 names, each measured. An em dash where a class
        // never appeared: "never" and "on the last turn" are different
        // statements, and the old skill placeholder said the second when it
        // meant neither.
        out.push_str(&format!(
            "  turns-to-first: memory {}, skill {}, rule {}, tool {}\n",
            render_turn(self.first_memory_turn),
            render_turn(self.first_skill_turn),
            render_turn(self.first_rule_turn),
            render_turn(self.first_tool_turn),
        ));

        // The honesty guard, and the reason this is one function rather than
        // two: a caller cannot print the yield above without printing the shape
        // it came from.
        out.push_str("\nread the two together\n");
        out.push_str(&format!(
            "  {} artifact(s) from {} lesson(s) over {} distinct task(s). A count that rises\n  \
             because one lesson repeats is a fact about this fixture, not about the learner.\n",
            self.skills.len() + self.rules.len() + self.tools.len(),
            self.lessons_offered,
            self.distinct_tasks,
        ));
        if !self.model_errors.is_empty() {
            out.push_str(&format!(
                "\n{} turn(s) taught the lifecycle nothing and said so:\n",
                self.model_errors.len()
            ));
            for error in &self.model_errors {
                out.push_str(&format!("  · {}\n", first_line(error)));
            }
        }
        out
    }
}

fn recurrence(lessons: usize, turns: usize) -> f64 {
    if turns == 0 {
        0.0
    } else {
        lessons as f64 / turns as f64
    }
}

fn render_turn(turn: Option<usize>) -> String {
    turn.map(|t| t.to_string()).unwrap_or_else(|| "—".into())
}

fn render_list(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        format!("{} — {}", items.len(), items.join(", "))
    }
}

/// The first line of a message, so a multi-line model error does not derail the
/// report's shape.
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or(text)
}

/// Paths as workspace-relative strings, sorted.
///
/// The temp root differs on every run, so an absolute path would defeat
/// assertion 8 on its own — the summary must name what was built, never where
/// this particular replay put it.
fn relative_names(root: &Path, paths: &[String]) -> Vec<String> {
    let root = root.to_string_lossy();
    let mut names: Vec<String> = paths
        .iter()
        .map(|path| {
            path.strip_prefix(root.as_ref())
                .unwrap_or(path)
                .trim_start_matches('/')
                .replace('\\', "/")
        })
        .collect();
    names.sort();
    names
}

/// Every published rule record's file stem, sorted.
pub(crate) fn published_rules(root: &Path) -> Vec<String> {
    let dir = crate::memory::rules_mining::workspace_rules_dir(root);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "toml" || extension == "md")
        })
        // `governance.toml` and `promotions.jsonl` are the plane's own
        // bookkeeping, not mined records.
        .filter(|path| {
            path.file_stem()
                .is_some_and(|stem| stem != "governance" && stem != "promotions")
        })
        .filter_map(|path| Some(path.file_stem()?.to_string_lossy().into_owned()))
        .collect();
    names.sort();
    names
}

/// Every memory text in the workspace's context store, sorted and deduplicated.
///
/// Opened read-only through a fresh `ContextStore` rather than borrowed from a
/// live `SessionMemory`, because the summary is taken after the last session has
/// been dropped — which is also when a real later session would see it.
pub(crate) fn memory_texts(root: &Path) -> Vec<String> {
    with_store(root, |store| store.memory_nodes().ok())
}

/// Open the workspace's context store read-only and project its nodes to
/// deduplicated, sorted text.
///
/// Opened fresh rather than borrowed from a live `SessionMemory`, because the
/// summary is taken after the last session has been dropped — which is also when
/// a real later session would see it.
fn with_store<F>(root: &Path, read: F) -> Vec<String>
where
    F: FnOnce(&stella_context::ContextStore) -> Option<Vec<stella_context::NodeRow>>,
{
    let Ok(path) = crate::memory::context_db_path(root) else {
        return Vec::new();
    };
    let Ok(store) = stella_context::ContextStore::open_with(
        &path,
        std::sync::Arc::new(stella_context::HashEmbedder::default()),
        std::sync::Arc::new(stella_context::SystemClock),
    ) else {
        return Vec::new();
    };
    let Some(nodes) = read(&store) else {
        return Vec::new();
    };
    nodes
        .into_iter()
        .map(|node| node.content)
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect()
}
