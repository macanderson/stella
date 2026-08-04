//! End to end over the real commands: mine receipts, author, adopt, enable —
//! and confirm the tool is unreachable at every step until the last one.

use stella_core::GapDetectionConfig;
use stella_store::{ToolCallRow, ToolCallState};

use super::*;
use crate::tool_foundry::run_tools_author_in;

/// A workspace with a real on-disk store, seeded with the `bash` receipts the
/// detector mines. Returns the root and the store.
fn workspace(commands: &[&str]) -> (tempfile::TempDir, Store) {
    let ws = tempfile::tempdir().expect("tmp");
    let store = Store::open(ws.path()).expect("store");
    let id = store
        .begin_execution("run", "p", "zai", "glm-5.2")
        .expect("execution");
    let rows: Vec<ToolCallRow> = commands
        .iter()
        .enumerate()
        .map(|(i, command)| ToolCallRow {
            call_id: format!("c{i}"),
            name: "bash".into(),
            surface: "native".into(),
            args_json: serde_json::json!({ "command": command }).to_string(),
            args_digest: format!("d{i}"),
            reason: String::new(),
            state: ToolCallState::Ok,
            error: String::new(),
            bytes_out: 0,
            duration_ms: 1,
        })
        .collect();
    store.record_tool_calls(id, &rows).expect("record");
    (ws, store)
}

/// The motivating capability, staged: reading a file the receipts named.
/// Returns the tool's name.
fn author_cat(root: &Path, store: &Store) -> String {
    std::fs::write(root.join("a.txt"), "alpha-contents\n").unwrap();
    std::fs::write(root.join("b.txt"), "beta-contents\n").unwrap();
    std::fs::write(root.join("c.txt"), "gamma-contents\n").unwrap();
    run_tools_author_in(root, store, Some("cat"), GapDetectionConfig::default())
        .expect("authoring succeeds");
    "cat".to_string()
}

/// What a session would actually be offered, given what is on disk and what
/// the ledger says.
fn session_tools(root: &Path) -> Vec<String> {
    let report = custom::discover_in(root, None);
    gate_discovery(report, root)
        .tools
        .into_iter()
        .map(|tool| tool.name)
        .collect()
}

/// The full protocol, in order — and the tool is unreachable until the very
/// last step. This is #830's guardrail as a single readable trace.
#[test]
fn a_tool_is_unreachable_until_it_is_both_proven_and_approved() {
    let (ws, store) = workspace(&["cat a.txt", "cat b.txt", "cat c.txt"]);
    let root = ws.path();
    let _home = crate::paths::test_user_home(root.to_path_buf());
    let name = author_cat(root, &store);

    // 1. authored → staged only, invisible to discovery.
    assert!(
        session_tools(root).is_empty(),
        "a staged tool is not a tool"
    );

    // 2. adopted → its witness flipped, and it still cannot be called.
    let record = adopt_in(root, &store, &name).expect("the witness must flip");
    assert!(!record.enabled, "adoption is evidence, not permission");
    assert!(
        record.witness.starts_with("proven"),
        "witness: {}",
        record.witness
    );
    assert!(
        ["alpha", "beta", "gamma"]
            .iter()
            .any(|f| record.witness.contains(&format!("{f}-contents"))),
        "the proof must name the value it asserted, from one of the observed files: {}",
        record.witness
    );
    let (manifest, script) = adopted_paths(root, &name);
    assert!(manifest.exists() && script.exists(), "the pair is in place");
    assert!(
        session_tools(root).is_empty(),
        "an adopted tool is withheld until a human enables it"
    );

    // 3. enabled → now, and only now, the model can reach it.
    set_enabled_in(root, &store, &name, true).expect("enable");
    assert_eq!(session_tools(root), vec![name.clone()]);

    // ...and disabling takes it away again without discarding the proof.
    set_enabled_in(root, &store, &name, false).expect("disable");
    assert!(session_tools(root).is_empty());
    assert!(
        store
            .adopted_foundry_tool(&name)
            .unwrap()
            .expect("still adopted")
            .witness
            .starts_with("proven"),
        "disabling keeps the proof on file"
    );
}

/// The gate's reason reaches the operator. A withheld tool is not a missing
/// tool, and the difference has to be readable.
#[test]
fn a_withheld_tool_is_explained_not_silently_dropped() {
    let (ws, store) = workspace(&["cat a.txt", "cat b.txt", "cat c.txt"]);
    let root = ws.path();
    let _home = crate::paths::test_user_home(root.to_path_buf());
    let name = author_cat(root, &store);
    adopt_in(root, &store, &name).expect("adopt");

    let gated = gate_discovery(custom::discover_in(root, None), root);
    assert!(gated.tools.is_empty());
    assert_eq!(gated.diagnostics.len(), 1);
    let reason = &gated.diagnostics[0].reason;
    assert!(reason.contains(&name), "{reason}");
    assert!(reason.contains("stella tools --enable"), "{reason}");
}

/// A witness that does not flip adopts nothing, and leaves no debris: the
/// relocated pair is removed and the staged one is untouched for review.
#[test]
fn a_failed_witness_adopts_nothing_and_leaves_the_staged_pair() {
    let (ws, store) = workspace(&["cat a.txt", "cat b.txt", "cat c.txt"]);
    let root = ws.path();
    let _home = crate::paths::test_user_home(root.to_path_buf());
    let name = author_cat(root, &store);
    // Never create the files the receipts named, so `cat` fails.
    for f in ["a.txt", "b.txt", "c.txt"] {
        std::fs::remove_file(root.join(f)).unwrap();
    }

    let err = adopt_in(root, &store, &name).expect_err("a broken tool must not adopt");
    assert!(err.contains("witness failed"), "{err}");

    let (manifest, script) = adopted_paths(root, &name);
    assert!(!manifest.exists(), "no manifest left behind");
    assert!(!script.exists(), "no script left behind");
    assert!(
        store.adopted_foundry_tool(&name).unwrap().is_none(),
        "nothing recorded"
    );
    assert!(
        root.join(".stella/tools/proposed")
            .join(format!("{name}.toml"))
            .exists(),
        "the staged pair survives for review"
    );
}

/// Tamper exclusion at the moment someone asks a direct question. Rewriting
/// the script and then enabling it is the attack this refuses by name.
#[test]
fn enabling_a_tool_whose_script_changed_is_refused() {
    let (ws, store) = workspace(&["cat a.txt", "cat b.txt", "cat c.txt"]);
    let root = ws.path();
    let _home = crate::paths::test_user_home(root.to_path_buf());
    let name = author_cat(root, &store);
    adopt_in(root, &store, &name).expect("adopt");

    let (_, script) = adopted_paths(root, &name);
    std::fs::write(&script, "#!/bin/sh\necho pwned\n").unwrap();

    let err = set_enabled_in(root, &store, &name, true).expect_err("must refuse");
    assert!(err.contains("changed after adoption"), "{err}");
    assert!(session_tools(root).is_empty());
}

/// Re-adopting a tool whose bytes changed proves the NEW bytes and revokes the
/// old approval — the edit does not ride in on the previous decision.
#[test]
fn re_adoption_proves_the_new_bytes_and_needs_a_new_approval() {
    let (ws, store) = workspace(&["cat a.txt", "cat b.txt", "cat c.txt"]);
    let root = ws.path();
    let _home = crate::paths::test_user_home(root.to_path_buf());
    let name = author_cat(root, &store);
    adopt_in(root, &store, &name).expect("adopt");
    set_enabled_in(root, &store, &name, true).expect("enable");
    assert_eq!(session_tools(root), vec![name.clone()]);

    // A reviewer edits the STAGED script and re-adopts.
    let staged_script = root
        .join(".stella/tools/proposed")
        .join(format!("{name}.sh"));
    std::fs::write(
        &staged_script,
        "#!/bin/sh\nset -eu\ncat \"$STELLA_INPUT_P1\" | tr a-z A-Z\n",
    )
    .unwrap();

    let record = adopt_in(root, &store, &name).expect("re-adopt");
    assert!(!record.enabled, "new bytes need a new approval");
    assert!(
        ["ALPHA", "BETA", "GAMMA"]
            .iter()
            .any(|f| record.witness.contains(&format!("{f}-CONTENTS"))),
        "the new proof is about the new behavior: {}",
        record.witness
    );
    assert!(session_tools(root).is_empty(), "and it is off again");
}

/// A foundry manifest that appears in `.stella/tools/` without going through
/// adoption is refused by the adopt command and withheld by the gate — the two
/// halves of closing the hand-move hole.
#[test]
fn a_hand_moved_manifest_is_refused_and_withheld() {
    let (ws, store) = workspace(&["cat a.txt", "cat b.txt", "cat c.txt"]);
    let root = ws.path();
    let _home = crate::paths::test_user_home(root.to_path_buf());
    let name = author_cat(root, &store);

    // Someone follows the old instructions and moves the manifest up.
    let staged = root.join(".stella/tools/proposed");
    let (manifest, script) = adopted_paths(root, &name);
    std::fs::copy(staged.join(format!("{name}.sh")), &script).unwrap();
    std::fs::write(
        &manifest,
        std::fs::read_to_string(staged.join(format!("{name}.toml")))
            .unwrap()
            .replace(
                &format!("./.stella/tools/proposed/{name}.sh"),
                &format!("./.stella/tools/{name}.sh"),
            ),
    )
    .unwrap();

    assert!(
        session_tools(root).is_empty(),
        "moving a file grants nothing"
    );
    let gated = gate_discovery(custom::discover_in(root, None), root);
    assert!(
        gated.diagnostics[0].reason.contains("never adopted"),
        "{}",
        gated.diagnostics[0].reason
    );
    let err = adopt_in(root, &store, &name).expect_err("refuses to clobber");
    assert!(err.contains("never adopted"), "{err}");
}

/// A hand-written manifest passes straight through: this protocol governs what
/// Stella authored, and a developer's own tool was never in scope.
#[test]
fn a_hand_written_tool_is_unaffected_by_the_gate() {
    let ws = tempfile::tempdir().expect("tmp");
    let root = ws.path();
    std::fs::create_dir_all(root.join(ADOPTED_DIR)).unwrap();
    std::fs::write(
        root.join(ADOPTED_DIR).join("deploy.toml"),
        "name = \"deploy\"\ndescription = \"ship it\"\ncommand = [\"./deploy.sh\"]\n",
    )
    .unwrap();
    assert_eq!(session_tools(root), vec!["deploy".to_string()]);
}

/// The #830 metric, rendered: what was adopted, what it proved, and whether
/// anything ever used it.
#[test]
fn the_report_shows_reuse_and_names_false_starts() {
    let (ws, store) = workspace(&["cat a.txt", "cat b.txt", "cat c.txt"]);
    let root = ws.path();
    let _home = crate::paths::test_user_home(root.to_path_buf());

    assert!(
        render_report(&store).unwrap().contains("no self-authored"),
        "an empty ledger reports itself"
    );

    let name = author_cat(root, &store);
    adopt_in(root, &store, &name).expect("adopt");

    let report = render_report(&store).unwrap();
    assert!(report.contains(&name), "{report}");
    assert!(report.contains("adopted, disabled"), "{report}");
    assert!(report.contains("proven"), "{report}");
    assert!(
        report.contains("1 adopted · 0 reused · 1 never used"),
        "the cost column must be reported alongside the win: {report}"
    );
}

/// Adoption only applies to foundry-authored manifests.
#[test]
fn adopting_a_manifest_without_provenance_is_refused() {
    let ws = tempfile::tempdir().expect("tmp");
    let root = ws.path();
    let store = Store::open(root).expect("store");
    let staged = root.join(".stella/tools/proposed");
    std::fs::create_dir_all(&staged).unwrap();
    std::fs::write(staged.join("plain.sh"), "#!/bin/sh\necho hi\n").unwrap();
    std::fs::write(
        staged.join("plain.toml"),
        "name = \"plain\"\ndescription = \"d\"\ncommand = [\"./.stella/tools/proposed/plain.sh\"]\n",
    )
    .unwrap();

    let err = adopt_in(root, &store, "plain").expect_err("no provenance, no adoption");
    assert!(err.contains("[foundry] provenance"), "{err}");
}

/// Adopting something that was never staged says so, rather than failing on a
/// missing file three steps later.
#[test]
fn adopting_an_unstaged_name_points_at_the_author_command() {
    let ws = tempfile::tempdir().expect("tmp");
    let store = Store::open(ws.path()).expect("store");
    let err = adopt_in(ws.path(), &store, "nothing_here").expect_err("nothing to adopt");
    assert!(err.contains("stella tools --author nothing_here"), "{err}");
}

/// Enabling a name with no adoption record is refused with the step that would
/// fix it.
#[test]
fn enabling_an_unadopted_name_points_at_the_adopt_command() {
    let ws = tempfile::tempdir().expect("tmp");
    let store = Store::open(ws.path()).expect("store");
    let err = set_enabled_in(ws.path(), &store, "ghost", true).expect_err("not adopted");
    assert!(err.contains("stella tools --adopt ghost"), "{err}");
}

/// The relocation is exact: one command path, rewritten, and nothing else in
/// the manifest disturbed.
#[test]
fn relocation_rewrites_exactly_the_command() {
    let text = "# comment mentioning .stella/tools/proposed\n\
                name = \"jq\"\n\
                command = [\"./.stella/tools/proposed/jq.sh\"]\n";
    let moved = relocate_command(text, "jq").expect("relocates");
    assert!(
        moved.contains("command = [\"./.stella/tools/jq.sh\"]"),
        "{moved}"
    );
    assert!(
        moved.contains("# comment mentioning .stella/tools/proposed"),
        "prose is untouched: {moved}"
    );
    assert!(relocate_command("command = [\"./other.sh\"]", "jq").is_err());
}
