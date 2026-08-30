//! The operator verbs around the autonomous foundry: `--draft` (the
//! manual escape hatch — author + validate, adopt nothing), `--rollback`
//! (restore a prior recorded version and re-digest it), and `--status`
//! (per-tool health: breaker state, versions, recent launches).

use std::path::Path;

use colored::Colorize;
use stella_store::Store;
use stella_tools::foundry_gate::PROPOSED_DIR;

use super::author::{self, AuthoredPair};
use super::gaps;

/// `stella tools --draft <gap-id>` — author and validate a staged pair from
/// one ledgered gap, and stop. The same authoring the autonomy pipeline
/// runs, without the adopt half: that IS draft-only mode, invoked by hand.
pub(crate) fn run_tools_draft(gap_id: &str) -> Result<(), String> {
    let root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let pair = draft_in(&root, gap_id)?;
    crate::plain::section_header("Tool drafted — staged, inert");
    println!(
        "  {} {}\n  {} {}",
        "·".green(),
        pair.manifest_path.display(),
        "·".green(),
        pair.script_path.display(),
    );
    println!(
        "\n  {}\n  {}",
        "nothing can call it from here — the staging directory is invisible to discovery:".dimmed(),
        format!("prove and record it: stella tools --adopt {}", pair.name).dimmed()
    );
    Ok(())
}

/// The testable core of [`run_tools_draft`]: no cwd, no printing.
pub(crate) fn draft_in(root: &Path, gap_id: &str) -> Result<AuthoredPair, String> {
    let gap = gaps::find_gap(root, gap_id).ok_or_else(|| {
        format!(
            "no gap `{gap_id}` in the ledger — see .stella/private/{} for the detected ones",
            gaps::GAP_LEDGER_FILE
        )
    })?;
    author::author_pair(root, &gap)
}

/// `stella tools --rollback <name> [--to <version>]`.
pub(crate) fn run_tools_rollback(name: &str, to: Option<i64>) -> Result<(), String> {
    let root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let store = Store::open(&root).map_err(|e| format!("cannot open the workspace store: {e}"))?;
    let restored = rollback_in(&root, &store, name, to)?;
    crate::plain::section_header("Tool rolled back");
    println!(
        "  {} {} {}",
        "·".green(),
        name.bright_magenta(),
        format!("restored to v{restored}, re-digested, and enabled").dimmed()
    );
    Ok(())
}

/// The testable core of [`run_tools_rollback`]: restore one recorded
/// version's exact bytes over the adopted pair, re-pin the ledger to their
/// digests, re-enable, and append the rollback to the version history.
/// Returns the version that was restored.
///
/// Append-only, never destructive: the bytes being replaced are already a
/// version row of their own (every adoption records one), and the rollback
/// itself lands as a new row, so what ran when stays readable forever.
pub(crate) fn rollback_in(
    root: &Path,
    store: &Store,
    name: &str,
    to: Option<i64>,
) -> Result<i64, String> {
    let versions = store
        .foundry_versions(name)
        .map_err(|e| format!("cannot read the version history: {e}"))?;
    let Some(latest) = versions.last() else {
        return Err(format!(
            "`{name}` has no recorded versions — versions are recorded at adoption, so \
             adopt it first"
        ));
    };
    let target = match to {
        Some(version) => version,
        None if versions.len() < 2 => {
            return Err(format!(
                "`{name}` has only v{} on file — nothing earlier to roll back to",
                latest.version
            ));
        }
        None => latest.version - 1,
    };
    let (manifest, script) = store
        .foundry_version_bytes(name, target)
        .map_err(|e| format!("cannot read the version history: {e}"))?
        .ok_or_else(|| {
            format!(
                "`{name}` has no v{target} — on file: {}",
                versions
                    .iter()
                    .map(|v| format!("v{}", v.version))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;

    let (manifest_path, script_path) = super::adopt::adopted_paths(root, name);
    if let Some(parent) = manifest_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let manifest_text = String::from_utf8(manifest.clone())
        .map_err(|_| format!("v{target}'s manifest is not UTF-8 — refusing to restore it"))?;
    super::adopt::write_pair(&manifest_path, &manifest_text, &script_path, &script)?;

    let manifest_digest = stella_tools::foundry_gate::digest(&manifest);
    let script_digest = stella_tools::foundry_gate::digest(&script);
    if !store
        .repin_foundry_tool(name, &manifest_digest, &script_digest)
        .map_err(|e| format!("cannot re-pin the adoption: {e}"))?
    {
        return Err(format!("`{name}` has not been adopted"));
    }
    store
        .record_foundry_version(
            name,
            &manifest,
            &script,
            &manifest_digest,
            &script_digest,
            &format!("rollback to v{target}"),
        )
        .map_err(|e| format!("rolled back, but cannot record the version row: {e}"))?;
    Ok(target)
}

/// `stella tools --status` — per-tool health.
pub(crate) fn run_tools_status() -> Result<(), String> {
    let root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let store = Store::open(&root).map_err(|e| format!("cannot open the workspace store: {e}"))?;
    crate::plain::section_header("Foundry tool status");
    print!("{}", render_status(&root, &store)?);
    Ok(())
}

/// The testable core of [`run_tools_status`]: every adopted tool with its
/// enablement (and the breaker's recorded reason when one spoke), version
/// history, and recent launch record — plus the ledgered gaps still waiting.
pub(crate) fn render_status(root: &Path, store: &Store) -> Result<String, String> {
    use std::fmt::Write as _;

    let mut out = String::new();
    let adopted = store
        .adopted_foundry_tools()
        .map_err(|e| format!("cannot read the adoption ledger: {e}"))?;
    if adopted.is_empty() {
        out.push_str("  no foundry tools adopted in this workspace\n");
    }
    for tool in &adopted {
        let state = if tool.enabled {
            "enabled".to_string()
        } else if tool.disabled_reason.is_empty() {
            "adopted, disabled".to_string()
        } else {
            format!("disabled — {}", tool.disabled_reason)
        };
        let versions = store
            .foundry_versions(&tool.name)
            .map_err(|e| format!("cannot read the version history: {e}"))?;
        let outcomes = store
            .recent_foundry_outcomes(&tool.name, 10)
            .map_err(|e| format!("cannot read the invocation telemetry: {e}"))?;
        let failures = outcomes.iter().filter(|&&ok| !ok).count();
        let _ = writeln!(out, "  {} [{state}] {}", tool.name, tool.signature);
        let _ = writeln!(
            out,
            "    versions: {} on file{}",
            versions.len(),
            versions
                .last()
                .map(|v| format!(" (current v{}, {})", v.version, v.reason))
                .unwrap_or_default()
        );
        let _ = writeln!(
            out,
            "    last {} launch(es): {} ok, {failures} failed",
            outcomes.len(),
            outcomes.len() - failures,
        );
    }

    let gaps = gaps::load_ledger(root);
    let _ = writeln!(
        out,
        "\n  {} gap(s) in the ledger — `stella tools --draft <gap-id>` authors one; \
         staged drafts live under {PROPOSED_DIR}/",
        gaps.len()
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST_V1: &str = "name = \"cat_file\"\n\
        description = \"print one file\"\n\
        command = [\"./.stella/tools/cat_file.sh\"]\n\n\
        [foundry]\n\
        authored_by = \"stella-tool-foundry\"\n\
        signature = \"cat <path>\"\n\
        occurrences = 3\n\n\
        [foundry.witness_input]\n\
        p1 = \"a.txt\"\n\n\
        [input_schema]\n\
        type = \"object\"\n\n\
        [input_schema.properties.p1]\n\
        type = \"string\"\n";
    const SCRIPT_V1: &str = "#!/bin/sh\nset -eu\ncat \"${STELLA_INPUT_P1}\"\n";
    const SCRIPT_V2: &str = "#!/bin/sh\nset -eu\ncat -n \"${STELLA_INPUT_P1}\"\n";

    fn adopt_with_versions(root: &Path, store: &Store) {
        for (script, reason) in [(SCRIPT_V1, "adopt"), (SCRIPT_V2, "adopt")] {
            let manifest_digest = stella_tools::foundry_gate::digest(MANIFEST_V1.as_bytes());
            let script_digest = stella_tools::foundry_gate::digest(script.as_bytes());
            store
                .adopt_foundry_tool(&stella_store::AdoptedTool {
                    name: "cat_file".into(),
                    signature: "cat <path>".into(),
                    manifest_digest: manifest_digest.clone(),
                    script_digest: script_digest.clone(),
                    witness: "proven — output contains `x`".into(),
                    witness_input: "{\"p1\":\"a.txt\"}".into(),
                    witness_expect: "x".into(),
                    enabled: false,
                    adopted_at: String::new(),
                    disabled_reason: String::new(),
                })
                .expect("adopt");
            store
                .record_foundry_version(
                    "cat_file",
                    MANIFEST_V1.as_bytes(),
                    script.as_bytes(),
                    &manifest_digest,
                    &script_digest,
                    reason,
                )
                .expect("version");
        }
        store
            .set_foundry_tool_enabled("cat_file", true)
            .expect("enable");
        let (manifest_path, script_path) = super::super::adopt::adopted_paths(root, "cat_file");
        std::fs::create_dir_all(manifest_path.parent().expect("parent")).expect("mkdir");
        super::super::adopt::write_pair(
            &manifest_path,
            MANIFEST_V1,
            &script_path,
            SCRIPT_V2.as_bytes(),
        )
        .expect("write current");
    }

    /// The rollback witness: restore v1 over a v2 workspace, and the
    /// restored tool's bytes, ledger digests, and enablement all agree —
    /// including a gate pass, so the restored tool actually registers. The
    /// history grows by one append-only row; nothing is deleted.
    #[test]
    fn rollback_round_trips_a_prior_version_and_the_gate_accepts_it() {
        let dir = tempfile::tempdir().expect("tmp");
        let root = dir.path();
        let store = Store::open(root).expect("store");
        adopt_with_versions(root, &store);
        // Trip state on the way in, to prove a rollback clears it.
        store
            .disable_foundry_tool_with_reason("cat_file", "circuit breaker: 3 consecutive")
            .expect("disable");

        let restored = rollback_in(root, &store, "cat_file", None).expect("rollback");
        assert_eq!(restored, 1, "default target is the version before current");

        let (_, script_path) = super::super::adopt::adopted_paths(root, "cat_file");
        assert_eq!(
            std::fs::read_to_string(&script_path).expect("script"),
            SCRIPT_V1,
            "the restored bytes are v1's, exactly"
        );
        let row = store
            .adopted_foundry_tool("cat_file")
            .expect("read")
            .expect("row");
        assert!(row.enabled, "a rollback re-enables");
        assert_eq!(row.disabled_reason, "", "and clears the breaker verdict");
        assert_eq!(
            row.script_digest,
            stella_tools::foundry_gate::digest(SCRIPT_V1.as_bytes()),
            "the ledger re-pins to the restored digests"
        );

        // The gate agrees: adopted, enabled, bytes intact → registers.
        let report = super::super::adopt::gate_discovery(
            stella_tools::custom::discover_in(root, None),
            root,
        );
        assert!(
            report.tools.iter().any(|t| t.name == "cat_file"),
            "the restored tool must register: {:?}",
            report
                .diagnostics
                .iter()
                .map(|d| &d.reason)
                .collect::<Vec<_>>()
        );

        // Append-only: two adoptions plus the rollback = three rows.
        let versions = store.foundry_versions("cat_file").expect("versions");
        assert_eq!(versions.len(), 3);
        assert_eq!(versions[2].reason, "rollback to v1");

        // And --to targets an explicit version.
        let restored = rollback_in(root, &store, "cat_file", Some(2)).expect("rollback --to 2");
        assert_eq!(restored, 2);
        assert_eq!(
            std::fs::read_to_string(&script_path).expect("script"),
            SCRIPT_V2
        );
    }

    /// Rollback refuses what it cannot restore: no history, a single
    /// version, an unknown target — each is a named error, never a guess.
    #[test]
    fn rollback_refuses_what_it_cannot_restore() {
        let dir = tempfile::tempdir().expect("tmp");
        let root = dir.path();
        let store = Store::open(root).expect("store");
        let err = rollback_in(root, &store, "ghost", None).unwrap_err();
        assert!(err.contains("no recorded versions"), "{err}");

        store
            .record_foundry_version("one_v", b"m", b"s", "m", "s", "adopt")
            .expect("v1");
        let err = rollback_in(root, &store, "one_v", None).unwrap_err();
        assert!(err.contains("nothing earlier"), "{err}");
        let err = rollback_in(root, &store, "one_v", Some(9)).unwrap_err();
        assert!(err.contains("no v9"), "{err}");
    }

    /// The status view names the breaker's verdict, the version history, and
    /// the waiting gaps.
    #[test]
    fn status_reports_breaker_state_versions_and_gaps() {
        let dir = tempfile::tempdir().expect("tmp");
        let root = dir.path();
        let store = Store::open(root).expect("store");
        adopt_with_versions(root, &store);
        store
            .disable_foundry_tool_with_reason("cat_file", "circuit breaker: 3 consecutive failures")
            .expect("disable");
        store
            .record_foundry_invocation(&stella_store::FoundryInvocation {
                name: "cat_file".into(),
                script_digest: "s".into(),
                gap_id: String::new(),
                duration_ms: 4,
                ok: false,
                timed_out: false,
                output_bytes: 0,
            })
            .expect("telemetry");

        let status = render_status(root, &store).expect("status");
        assert!(status.contains("disabled — circuit breaker"), "{status}");
        assert!(status.contains("versions: 2 on file"), "{status}");
        assert!(status.contains("1 launch(es): 0 ok, 1 failed"), "{status}");
        assert!(status.contains("gap(s) in the ledger"), "{status}");
    }

    /// Draft refuses an unknown gap id with the ledger's address in the
    /// error, so the operator knows where to look.
    #[test]
    fn draft_names_the_ledger_when_the_gap_is_unknown() {
        let dir = tempfile::tempdir().expect("tmp");
        let err = draft_in(dir.path(), "feedbeef00000000").unwrap_err();
        assert!(err.contains("tool_gaps.jsonl"), "{err}");
    }
}
