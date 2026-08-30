//! The autonomous foundry: detect → author → validate → adopt →
//! enable, with the standing controls that replace the stage→adopt→enable
//! human ceremony.
//!
//! The pipeline runs on the end-of-turn seam, fed by the gap ledger
//! ([`super::gaps`]). What replaced the human is not trust — it is five
//! mechanisms, each enforced where it cannot be skipped:
//!
//! 1. **Network denied by default** for every foundry-built tool, at spawn,
//!    by the OS (`stella_tools::netdeny`). Where the platform offers no real
//!    mechanism, autonomy **degrades to draft-only** — files written, nothing
//!    adopted — rather than faking the control.
//! 2. **The witness still gates adoption**: [`super::adopt::adopt_in_async`]
//!    is the same prove-then-record path the human `--adopt` runs, and the
//!    witness's two executions of the candidate happen through the
//!    network-denied spawn path — they are the sandboxed dry-run.
//! 3. **Per-call re-digest stays**: `foundry_gate::recheck_before_launch`
//!    holds every launch to the adopted bytes, exactly as before.
//! 4. **Versioned rollback**: every adoption appends its bytes to the store's
//!    version history; `stella tools --rollback` restores and re-digests.
//! 5. **Telemetry + circuit breaker**: every launch is recorded, and repeated
//!    failure disables the tool with a recorded, user-visible reason.
//!
//! `foundry.autonomy` in settings is the kill switch (`auto` / `draft-only` /
//! `off`), and `stella tools --disable <name>` turns off one tool.

use std::path::Path;

use stella_store::Store;
use stella_tools::foundry_gate::PROPOSED_DIR;

use super::gaps::GapRecord;
use super::{adopt, author};
use crate::settings::{FoundryAutonomy, FoundryConfig};

/// How many new gaps one turn may carry through the pipeline. The witness
/// executes each candidate twice, so this bounds end-of-turn latency; the
/// rest stay ledgered for `stella tools --draft`.
const MAX_AUTONOMOUS_PER_TURN: usize = 2;

/// Run the autonomy pipeline over this turn's newly-ledgered gaps. Returns
/// the user-visible notices; never fails the turn — every failure is a
/// notice naming the gap it stranded.
pub(crate) async fn run_autonomy(
    root: &Path,
    store: Option<&Store>,
    new_gaps: &[GapRecord],
    config: &FoundryConfig,
) -> Vec<String> {
    let mut notices = Vec::new();
    if config.autonomy == FoundryAutonomy::Off {
        return notices;
    }
    let Some(store) = store else {
        return notices;
    };

    let netdeny_live = stella_tools::netdeny::available();
    let draft_only = config.autonomy == FoundryAutonomy::DraftOnly || !netdeny_live;
    if config.autonomy == FoundryAutonomy::Auto && !netdeny_live {
        notices.push(
            "foundry autonomy degraded to draft-only: this platform offers no working \
             network isolation (macOS sandbox-exec / Linux unshare -rn), and the network \
             denial is never faked"
                .to_string(),
        );
    }

    for gap in new_gaps.iter().take(MAX_AUTONOMOUS_PER_TURN) {
        // Author + validate (manifest re-parse and script lint are inside).
        let pair = match author::author_pair(root, gap) {
            Ok(pair) => pair,
            Err(why) => {
                notices.push(format!("gap {} not authored: {why}", gap.gap_id));
                continue;
            }
        };
        if draft_only {
            notices.push(format!(
                "drafted `{}` under {PROPOSED_DIR}/ (draft-only) — review and adopt with \
                 `stella tools --adopt {}`",
                pair.name, pair.name
            ));
            continue;
        }
        // Adopt: the witness proves the tool over the real gated surface, and
        // its executions run network-denied. A failed witness leaves the
        // draft staged for review — authored work is never silently thrown
        // away, and nothing unproven registers.
        match adopt::adopt_in_async(root, store, &pair.name, "adopt (autonomous)").await {
            Ok(record) => match adopt::set_enabled_in(root, store, &pair.name, true) {
                Ok(()) => notices.push(format!(
                    "auto-adopted tool `{}` from gap {} — {}; network denied at spawn; \
                     disable with `stella tools --disable {}`",
                    pair.name, gap.gap_id, record.witness, pair.name
                )),
                Err(why) => notices.push(format!(
                    "adopted `{}` but could not enable it: {why}",
                    pair.name
                )),
            },
            Err(why) => notices.push(format!(
                "drafted `{}` but did not adopt it: {why}",
                pair.name
            )),
        }
    }

    let overflow = new_gaps.len().saturating_sub(MAX_AUTONOMOUS_PER_TURN);
    if overflow > 0 {
        notices.push(format!(
            "{overflow} more gap(s) stay ledgered — author one with `stella tools --draft \
             <gap-id>`"
        ));
    }
    notices
}

#[cfg(test)]
mod tests {
    use stella_core::ports::ToolExecutor;
    use stella_store::{Store, ToolCallRow, ToolCallState};
    use stella_tools::custom::{self, CustomToolSet};

    use crate::settings::{FoundryAutonomy, FoundryConfig};
    use crate::tool_foundry::{adopt, end_of_turn_with};

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

    /// A helper script whose output does two jobs at once: its first line
    /// records whether a TCP connect got out (the network-denial evidence
    /// the witness pins), and its second is real work over the input file
    /// (so the witness has a non-echo value to assert on).
    fn write_workspace_fixture(root: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let helper = root.join("sub.sh");
        std::fs::write(
            &helper,
            "#!/bin/sh\n\
             if bash -c 'exec 3<>/dev/tcp/1.1.1.1/53' 2>/dev/null; then\n\
               echo 'NET:REACHED'\n\
             else\n\
               echo 'NET:DENIED'\n\
             fi\n\
             tr 'a-z' 'A-Z' < \"$1\"\n",
        )
        .expect("helper");
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).expect("mode");
        std::fs::write(root.join("a.json"), "alpha-payload\n").expect("a.json");
        std::fs::write(root.join("b.json"), "beta-payload\n").expect("b.json");
    }

    /// The autonomy end-to-end witness, through the LIVE end-of-turn path: a
    /// synthetic repeated-command session is detected, authored, validated,
    /// witness-proven under network denial, auto-adopted, auto-enabled, and
    /// then executed through the gated discovery surface — with the tool's
    /// own attempted TCP connect observably denied. Where the platform has
    /// no isolation mechanism, the same call must degrade to draft-only and
    /// adopt nothing — the control is exercised on every machine, in
    /// whichever direction is true there.
    #[tokio::test]
    async fn a_synthetic_gap_is_autonomously_adopted_and_its_network_call_is_denied() {
        let dir = tempfile::tempdir().expect("tmp");
        let root = dir.path();
        write_workspace_fixture(root);
        let store = Store::open(root).expect("store");
        record_bash_history(
            &store,
            &[
                "sh sub.sh a.json",
                "sh sub.sh b.json",
                "sh sub.sh a.json",
                "sh sub.sh b.json",
                "sh sub.sh a.json",
                "sh sub.sh b.json",
            ],
        );

        let config = FoundryConfig::default();
        assert_eq!(
            config.autonomy,
            FoundryAutonomy::Auto,
            "auto is the default"
        );
        let notices = end_of_turn_with(root, Some(&store), &config).await;
        assert!(!notices.is_empty(), "the pipeline reports what it did");

        if !stella_tools::netdeny::available() {
            // The degraded arm IS the control on this machine: files staged,
            // nothing adopted, and the notice says why.
            assert!(
                notices.iter().any(|n| n.contains("draft-only")),
                "degradation must be user-visible: {notices:?}"
            );
            assert!(
                store.adopted_foundry_tools().expect("read").is_empty(),
                "without real isolation nothing may be auto-adopted"
            );
            return;
        }

        // Adopted, enabled, witnessed — and the witness pinned the denial:
        // the tool's first output line is NET:DENIED, recorded from two real
        // sandboxed executions.
        let adopted = store.adopted_foundry_tools().expect("read");
        assert_eq!(adopted.len(), 1, "one gap, one adoption: {notices:?}");
        let record = &adopted[0];
        assert!(record.enabled, "autonomy enables what it proves");
        assert!(
            record.witness.contains("NET:DENIED"),
            "the witness executions must have run network-denied: {}",
            record.witness
        );

        // One version row, bytes on file — the rollback substrate.
        let versions = store.foundry_versions(&record.name).expect("versions");
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].reason, "adopt (autonomous)");

        // Executed through the real gated discovery surface, and the network
        // attempt is denied at run time too.
        let report = adopt::gate_discovery(custom::discover_in(root, None), root);
        let tool = report
            .tools
            .iter()
            .find(|t| t.name == record.name)
            .expect("the adopted tool registers")
            .clone();
        let builtins = adopt::Builtins;
        let surface = CustomToolSet::new(&builtins, vec![tool], root.to_path_buf());
        let out = surface
            .execute(
                &record.name,
                &serde_json::json!({ "p1": "sub.sh", "p2": "b.json" }),
            )
            .await;
        let stella_protocol::tool::ToolOutput::Ok { content, .. } = out else {
            panic!("the adopted tool must execute");
        };
        assert!(
            content.contains("NET:DENIED"),
            "denied at run time: {content}"
        );
        assert!(
            content.contains("BETA-PAYLOAD"),
            "and it does real work: {content}"
        );

        // Telemetry: the witness's two runs and this one are all on file.
        let outcomes = store
            .recent_foundry_outcomes(&record.name, 10)
            .expect("outcomes");
        assert!(outcomes.len() >= 3, "every launch writes a row");
        assert!(outcomes.iter().all(|&ok| ok));
    }

    /// The kill switch: `autonomy = "off"` ledgers the gap and authors
    /// nothing; `"draft-only"` authors the pair and adopts nothing.
    #[tokio::test]
    async fn the_kill_switch_stops_the_pipeline_where_it_says() {
        let dir = tempfile::tempdir().expect("tmp");
        let root = dir.path();
        write_workspace_fixture(root);
        let store = Store::open(root).expect("store");
        record_bash_history(
            &store,
            &[
                "sh sub.sh a.json",
                "sh sub.sh b.json",
                "sh sub.sh a.json",
                "sh sub.sh b.json",
                "sh sub.sh a.json",
                "sh sub.sh b.json",
            ],
        );

        let off = FoundryConfig {
            autonomy: FoundryAutonomy::Off,
            ..FoundryConfig::default()
        };
        let notices = end_of_turn_with(root, Some(&store), &off).await;
        assert!(
            notices.iter().any(|n| n.contains("tool gap detected")),
            "off still detects and ledgers: {notices:?}"
        );
        assert!(
            !root.join(".stella/tools/proposed").exists(),
            "off authors nothing"
        );
        assert!(store.adopted_foundry_tools().expect("read").is_empty());

        // Fresh workspace for the draft-only arm (the gap is ledgered now).
        let dir2 = tempfile::tempdir().expect("tmp");
        let root2 = dir2.path();
        write_workspace_fixture(root2);
        let store2 = Store::open(root2).expect("store");
        record_bash_history(
            &store2,
            &[
                "sh sub.sh a.json",
                "sh sub.sh b.json",
                "sh sub.sh a.json",
                "sh sub.sh b.json",
                "sh sub.sh a.json",
                "sh sub.sh b.json",
            ],
        );
        let draft_only = FoundryConfig {
            autonomy: FoundryAutonomy::DraftOnly,
            ..FoundryConfig::default()
        };
        let notices = end_of_turn_with(root2, Some(&store2), &draft_only).await;
        assert!(
            notices.iter().any(|n| n.contains("draft-only")),
            "{notices:?}"
        );
        let staged: Vec<_> = std::fs::read_dir(root2.join(".stella/tools/proposed"))
            .expect("staging dir exists")
            .collect();
        assert_eq!(staged.len(), 2, "manifest + script staged");
        assert!(
            store2.adopted_foundry_tools().expect("read").is_empty(),
            "draft-only adopts nothing"
        );
    }
}
