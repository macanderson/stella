//! The tool-gap ledger — the live half of `detect_tool_gaps`, the
//! reconnect ADR 0023 records.
//!
//! The detector itself stays a pure function in `stella-core`; this module is
//! its production caller. At the end of a turn, the recent `bash` history is
//! read back out of the store's `tool_calls` projection, mined, and every
//! proposal at or above the configured thresholds is appended — once — to
//! `.stella/private/tool_gaps.jsonl`. The ledger is what `stella tools
//! --draft <gap-id>` and the autonomy pipeline author from, and the
//! `gap_id` rides every downstream artifact (the manifest's `[foundry]`
//! table, the invocation telemetry) as detection lineage.
//!
//! Append-only, deduplicated by `gap_id` — the FNV-1a hash of the proposal's
//! signature, so the same shape detected across many turns is one row, and a
//! row survives the shell history that produced it rolling out of the
//! detector's window.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::tool_foundry::detect::{
    GapDetectionConfig, ProposedTool, ShellInvocation, detect_tool_gaps,
};
use serde::{Deserialize, Serialize};

/// The ledger's file name under `.stella/private/`.
pub(crate) const GAP_LEDGER_FILE: &str = "tool_gaps.jsonl";

/// How much shell history one scan reads back. Bounded so the end-of-turn
/// pass costs one indexed query, not a table walk; 500 finished commands is
/// several sessions' worth on any realistic workspace.
const SHELL_HISTORY_WINDOW: usize = 500;

/// One ledgered gap — a [`ProposedTool`] plus its stable identity and when it
/// was first detected. Its own serde type because `stella-core`
/// derives no serde (the detector is pure over owned data); the ledger is
/// this crate's artifact, so the wire shape lives here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct GapRecord {
    /// FNV-1a of the signature, in hex — the handle `stella tools --draft`
    /// takes and the lineage every downstream artifact carries.
    pub gap_id: String,
    /// The detector's synthesized candidate tool name.
    pub name: String,
    /// The normalized command skeleton, e.g. `jq <str> <path>`.
    pub signature: String,
    /// The skeleton with numbered holes, e.g. `jq {p1} {p2}`.
    pub command_template: String,
    /// The holes, in command order.
    pub parameters: Vec<GapParameter>,
    /// Matching invocations observed at detection time.
    pub occurrences: usize,
    /// Distinct argument sets observed at detection time.
    pub distinct_arguments: usize,
    /// A few example command lines.
    pub examples: Vec<String>,
    /// Unix seconds when the gap was first ledgered.
    pub detected_at: u64,
}

/// One parameter hole of a ledgered gap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct GapParameter {
    /// `p1`, `p2`, … in command order.
    pub name: String,
    /// The hole's kind: `str`, `path`, or `num`.
    pub kind: String,
    /// Distinct concrete values seen at this position.
    pub examples: Vec<String>,
}

/// A gap's stable identity: the FNV-1a hash of its signature, in hex. The
/// signature *is* the cluster key, so two detections of the same shape get
/// the same id whatever the counts around them did.
pub(crate) fn gap_id(signature: &str) -> String {
    format!("{:016x}", super::fnv1a(signature))
}

/// The ledger's path, when the workspace has a private state directory.
pub(crate) fn ledger_path(root: &Path) -> Option<PathBuf> {
    stella_store::workspace_private_state_path(root, GAP_LEDGER_FILE).ok()
}

fn record_from(proposal: &ProposedTool, detected_at: u64) -> GapRecord {
    GapRecord {
        gap_id: gap_id(&proposal.signature),
        name: proposal.name.clone(),
        signature: proposal.signature.clone(),
        command_template: proposal.command_template.clone(),
        parameters: proposal
            .parameters
            .iter()
            .map(|p| GapParameter {
                name: p.name.clone(),
                kind: match p.kind {
                    crate::tool_foundry::detect::ParamKind::Str => "str".to_string(),
                    crate::tool_foundry::detect::ParamKind::Path => "path".to_string(),
                    crate::tool_foundry::detect::ParamKind::Number => "num".to_string(),
                },
                examples: p.examples.clone(),
            })
            .collect(),
        occurrences: proposal.occurrences,
        distinct_arguments: proposal.distinct_arguments,
        examples: proposal.examples.clone(),
        detected_at,
    }
}

/// Every row in the ledger, oldest first. A malformed line is skipped rather
/// than fatal — the ledger is advisory state, and one corrupt line must not
/// take the readable rows with it.
pub(crate) fn load_ledger(root: &Path) -> Vec<GapRecord> {
    let Some(path) = ledger_path(root) else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<GapRecord>(line).ok())
        .collect()
}

/// One ledgered gap by id. Accepts the full id only — a prefix would make
/// the handle ambiguous the day two gaps share one.
pub(crate) fn find_gap(root: &Path, id: &str) -> Option<GapRecord> {
    load_ledger(root).into_iter().find(|gap| gap.gap_id == id)
}

/// The live detection pass: mine the store's recent shell history,
/// append every **novel** proposal to the ledger, and say so in one line.
///
/// Returns the newly-ledgered gaps (for the autonomy pipeline) and the
/// notice, `None` when nothing new was found. Best-effort throughout — a
/// failed ledger write drops that row's notice rather than the turn.
pub(crate) fn scan_and_ledger(
    root: &Path,
    store: Option<&stella_store::Store>,
    detection: GapDetectionConfig,
) -> (Vec<GapRecord>, Option<String>) {
    let Some(store) = store else {
        return (Vec::new(), None);
    };
    let history: Vec<ShellInvocation> = store
        .recent_shell_invocations(SHELL_HISTORY_WINDOW)
        .unwrap_or_default()
        .into_iter()
        .map(|(command, succeeded)| ShellInvocation { command, succeeded })
        .collect();
    if history.is_empty() {
        return (Vec::new(), None);
    }

    let proposals = detect_tool_gaps(&history, detection);
    if proposals.is_empty() {
        return (Vec::new(), None);
    }

    let known: BTreeSet<String> = load_ledger(root)
        .into_iter()
        .map(|gap| gap.gap_id)
        .collect();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut ledgered = Vec::new();
    for proposal in &proposals {
        let record = record_from(proposal, now);
        if known.contains(&record.gap_id) {
            continue;
        }
        let Ok(line) = serde_json::to_string(&record) else {
            continue;
        };
        if stella_store::append_workspace_private_line(root, GAP_LEDGER_FILE, &line).is_ok() {
            ledgered.push(record);
        }
    }
    if ledgered.is_empty() {
        return (Vec::new(), None);
    }

    let notice = if ledgered.len() == 1 {
        let gap = &ledgered[0];
        format!(
            "tool gap detected: `{}` ({}x across {} argument sets) — draft it with \
             `stella tools --draft {}`",
            gap.signature, gap.occurrences, gap.distinct_arguments, gap.gap_id
        )
    } else {
        format!(
            "{} tool gaps detected and ledgered — see .stella/private/{GAP_LEDGER_FILE}",
            ledgered.len()
        )
    };
    (ledgered, Some(notice))
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_store::{Store, ToolCallRow, ToolCallState};

    fn record_bash_history(store: &Store, commands: &[&str]) {
        let id = store
            .begin_execution("run", "p", "zai", "glm-5.2")
            .expect("execution");
        let rows: Vec<ToolCallRow> = commands
            .iter()
            .enumerate()
            .map(|(i, command)| ToolCallRow {
                error_class: None,
                call_id: format!("c{i}"),
                name: "bash".into(),
                surface: "native".into(),
                args_json: serde_json::json!({ "command": command }).to_string(),
                args_digest: "d".into(),
                reason: String::new(),
                state: ToolCallState::Ok,
                error: String::new(),
                bytes_out: 0,
                duration_ms: 1,
                sub_agent_id: None,
            })
            .collect();
        store.record_tool_calls(id, &rows).expect("record");
    }

    /// A history that clears the shipped thresholds: one shape, six uses,
    /// two distinct argument sets, 3.0x reuse.
    fn qualifying_history() -> Vec<&'static str> {
        vec![
            "jq '.a' a.json",
            "jq '.b' b.json",
            "jq '.a' a.json",
            "jq '.b' b.json",
            "jq '.a' a.json",
            "jq '.b' b.json",
        ]
    }

    /// The gap-ledger witness, through the live hook path: a synthetic
    /// repeated-command session yields exactly one ledger row, the scan is
    /// fed from the store's own `tool_calls` projection (not a hand-built
    /// history), and a second pass over the same history appends nothing.
    #[test]
    fn a_repeated_command_session_yields_exactly_one_ledger_row() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = Store::open(dir.path()).expect("store");
        record_bash_history(&store, &qualifying_history());

        let (new_gaps, notice) =
            scan_and_ledger(dir.path(), Some(&store), GapDetectionConfig::default());
        assert_eq!(new_gaps.len(), 1, "one shape, one gap");
        let gap = &new_gaps[0];
        assert_eq!(gap.signature, "jq <str> <path>");
        assert_eq!(gap.command_template, "jq {p1} {p2}");
        assert_eq!(gap.occurrences, 6);
        assert_eq!(gap.distinct_arguments, 2);
        assert_eq!(gap.gap_id, gap_id("jq <str> <path>"));
        let notice = notice.expect("a novel gap surfaces a notice");
        assert!(
            notice.contains(&gap.gap_id),
            "the notice carries the handle"
        );

        let ledger = load_ledger(dir.path());
        assert_eq!(ledger.len(), 1, "exactly one row landed");
        assert_eq!(ledger[0], *gap);

        // The same history again: known gap, no new row, no notice.
        let (again, notice) =
            scan_and_ledger(dir.path(), Some(&store), GapDetectionConfig::default());
        assert!(again.is_empty(), "a known gap is not re-ledgered");
        assert!(notice.is_none());
        assert_eq!(load_ledger(dir.path()).len(), 1);
    }

    /// Below-threshold history ledgers nothing — the hook is silent on the
    /// overwhelming majority of turns.
    #[test]
    fn below_threshold_history_ledgers_nothing() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = Store::open(dir.path()).expect("store");
        record_bash_history(&store, &["jq '.a' a.json", "jq '.b' b.json"]);
        let (gaps, notice) =
            scan_and_ledger(dir.path(), Some(&store), GapDetectionConfig::default());
        assert!(gaps.is_empty());
        assert!(notice.is_none());
        assert!(load_ledger(dir.path()).is_empty());
    }

    /// The configured thresholds are honored — the same history that the
    /// shipped floor rejects is ledgered when the workspace lowers the
    /// floor — the whole point of exposing the thresholds.
    #[test]
    fn lowered_thresholds_ledger_what_the_default_rejects() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = Store::open(dir.path()).expect("store");
        record_bash_history(
            &store,
            &["jq '.a' a.json", "jq '.b' b.json", "jq '.c' c.json"],
        );
        let (default_gaps, _) =
            scan_and_ledger(dir.path(), Some(&store), GapDetectionConfig::default());
        assert!(default_gaps.is_empty(), "1.0x reuse is below the 3.0 floor");

        let relaxed = GapDetectionConfig {
            min_reuse_ratio: 1.0,
            ..GapDetectionConfig::default()
        };
        let (gaps, _) = scan_and_ledger(dir.path(), Some(&store), relaxed);
        assert_eq!(gaps.len(), 1);
    }

    /// `find_gap` answers by exact id, and only exact id.
    #[test]
    fn find_gap_is_exact() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = Store::open(dir.path()).expect("store");
        record_bash_history(&store, &qualifying_history());
        let (gaps, _) = scan_and_ledger(dir.path(), Some(&store), GapDetectionConfig::default());
        let id = gaps[0].gap_id.clone();
        assert_eq!(find_gap(dir.path(), &id).expect("found").gap_id, id);
        assert!(find_gap(dir.path(), &id[..8]).is_none(), "no prefix match");
    }
}
