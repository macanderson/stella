//! The capability witness's own proof: without the tool the call fails, with
//! it the call answers — and every way that flip can be counterfeited is
//! refused.

use std::path::{Path, PathBuf};

use serde_json::json;
use stella_core::{GapDetectionConfig, ShellInvocation, detect_tool_gaps};
use stella_protocol::tool::ToolSchema;

use super::*;
use crate::foundry_author::{self, PROPOSED_DIR};

/// The session's executor chain as the witness meets it: nothing advertised,
/// nothing answered. `advertises` lets one test put the candidate's name on
/// the existing surface, and `answers` lets another have it answer without
/// advertising — the two ways a "new" capability can turn out to be old.
struct Chain {
    advertises: Vec<String>,
    answers: Option<String>,
}

impl Chain {
    fn empty() -> Self {
        Self {
            advertises: Vec::new(),
            answers: None,
        }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for Chain {
    fn schemas(&self) -> Vec<ToolSchema> {
        self.advertises
            .iter()
            .map(|name| ToolSchema {
                name: name.clone(),
                description: "existing".into(),
                input_schema: json!({ "type": "object" }),
                read_only: false,
                speculation_safe: false,
            })
            .collect()
    }

    async fn execute(&self, name: &str, _: &Value) -> ToolOutput {
        match &self.answers {
            Some(content) => ToolOutput::Ok {
                content: content.clone(),
            },
            None => ToolOutput::Error {
                message: format!("unknown tool `{name}`"),
            },
        }
    }
}

/// Stage a hand-built script tool in `root` and return it, with `witness_input`
/// as its `[foundry]` witness. Used for the shapes the detector cannot mint on
/// demand (a silent tool, a flaky one).
fn staged(root: &Path, name: &str, body: &str, witness_input: Value) -> CustomTool {
    let file = format!("{name}.sh");
    let path = root.join(&file);
    std::fs::write(&path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    CustomTool {
        name: name.to_string(),
        description: "staged candidate".into(),
        command: vec![format!("./{file}")],
        timeout_ms: 10_000,
        input_schema: json!({ "type": "object", "properties": { "p1": { "type": "string" } } }),
        env: Default::default(),
        source: root.join(format!("{name}.toml")),
        foundry: Some(crate::foundry_gate::FoundryProvenance {
            authored_by: crate::foundry_gate::AUTHORED_BY.into(),
            signature: format!("{name} <str>"),
            occurrences: 3,
            witness_input,
            approved: None,
        }),
    }
}

/// Author a real tool from real detector output and write its staged pair
/// into `root`, exactly as `stella tools --author` does. Returns the parsed
/// tool the witness will be run against.
fn authored_in(root: &Path, commands: &[&str]) -> CustomTool {
    let history: Vec<ShellInvocation> = commands.iter().map(|c| ShellInvocation::ok(*c)).collect();
    let proposals = detect_tool_gaps(&history, GapDetectionConfig::default());
    assert_eq!(proposals.len(), 1, "expected exactly one proposal");
    let authored = foundry_author::author(&proposals[0]).expect("authorship succeeds");

    let staged_dir = root.join(PROPOSED_DIR);
    std::fs::create_dir_all(&staged_dir).unwrap();
    let manifest_path = staged_dir.join(&authored.manifest_filename);
    std::fs::write(&manifest_path, &authored.manifest_toml).unwrap();
    let script_path = staged_dir.join(&authored.script_filename);
    std::fs::write(&script_path, &authored.script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    crate::custom::parse_manifest(&authored.manifest_toml, &manifest_path)
        .expect("authored manifest parses")
}

/// **The headline.** A real detector proposal, really authored, really run:
/// the same call fails on the session's existing tool surface and answers once
/// the authored tool is in it. That fail→pass flip is the whole proof #830
/// asks for, and it is measured here rather than asserted.
#[tokio::test]
async fn the_witness_fails_without_the_tool_and_passes_with_it() {
    let ws = tempfile::tempdir().unwrap();
    let root = ws.path();
    // The capability: read a file's contents. The three receipts below are
    // what a session's `bash` log would hold.
    std::fs::write(root.join("a.txt"), "alpha-contents\n").unwrap();
    std::fs::write(root.join("b.txt"), "beta-contents\n").unwrap();
    std::fs::write(root.join("c.txt"), "gamma-contents\n").unwrap();
    let tool = authored_in(root, &["cat a.txt", "cat b.txt", "cat c.txt"]);

    let chain = Chain::empty();
    let input = witness_input(&tool).expect("the manifest carries a runnable witness input");

    // World A — the session as it is. The capability is not reachable.
    let before = chain.execute(&tool.name, &input).await;
    assert!(
        matches!(before, ToolOutput::Error { .. }),
        "the capability must be missing before the tool exists"
    );

    // World B — the same chain plus one tool. Now it answers, with the
    // contents of the file the receipts named.
    let with_tool = CustomToolSet::new(&chain, vec![tool.clone()], root.to_path_buf());
    let after = with_tool.execute(&tool.name, &input).await;
    match &after {
        ToolOutput::Ok { content } => assert!(
            content.contains("alpha-contents"),
            "the tool must produce the capability's answer: {content}"
        ),
        ToolOutput::Error { message } => panic!("the tool must answer: {message}"),
    }

    // And that is exactly what `prove` reports, end to end.
    let verdict = prove(&chain, &tool, root).await;
    let WitnessVerdict::Proven(case) = &verdict else {
        panic!("expected a proven witness, got {verdict:?}");
    };
    assert_eq!(case.expect, "alpha-contents");
    assert_eq!(case.input, input);
    assert!(verdict.summary().starts_with("proven"), "{verdict:?}");
}

/// The counterfeit the flip alone cannot catch: a name the session already
/// advertises. The call would "fail without and pass with" only because the
/// custom set shadows the built-in — so advertisement is checked before
/// anything runs.
#[tokio::test]
async fn a_name_the_session_already_advertises_earns_no_flip() {
    let ws = tempfile::tempdir().unwrap();
    let tool = staged(
        ws.path(),
        "already",
        "#!/bin/sh\necho fresh\n",
        json!({ "p1": "x" }),
    );
    let chain = Chain {
        advertises: vec!["already".into()],
        answers: None,
    };
    let verdict = prove(&chain, &tool, ws.path()).await;
    assert!(
        matches!(&verdict, WitnessVerdict::NoFlip { how } if how.contains("already an advertised tool")),
        "{verdict:?}"
    );
}

/// The other half of the same counterfeit: an unadvertised name the chain
/// answers anyway (an MCP set that routes by prefix, say). Answering is
/// answering — there is no capability gap to close.
#[tokio::test]
async fn a_capability_the_chain_already_answers_earns_no_flip() {
    let ws = tempfile::tempdir().unwrap();
    let tool = staged(
        ws.path(),
        "answered",
        "#!/bin/sh\necho fresh\n",
        json!({ "p1": "x" }),
    );
    let chain = Chain {
        advertises: Vec::new(),
        answers: Some("the existing surface already did this".into()),
    };
    let verdict = prove(&chain, &tool, ws.path()).await;
    assert!(
        matches!(&verdict, WitnessVerdict::NoFlip { how } if how.contains("already answers")),
        "{verdict:?}"
    );
}

/// A tool that does not work is refuted, not adopted.
#[tokio::test]
async fn a_failing_tool_is_refuted() {
    let ws = tempfile::tempdir().unwrap();
    let tool = staged(
        ws.path(),
        "broken",
        "#!/bin/sh\necho 'it broke' >&2\nexit 3\n",
        json!({ "p1": "x" }),
    );
    let verdict = prove(&Chain::empty(), &tool, ws.path()).await;
    assert!(
        matches!(&verdict, WitnessVerdict::Refuted { .. }),
        "{verdict:?}"
    );
    assert!(verdict.summary().starts_with("refuted"), "{verdict:?}");
}

/// The `density`-shaped refusal: exit 0 and nothing to show for it. The flip
/// happens, and proves only that a process ran.
#[tokio::test]
async fn a_silent_tool_is_vacuous() {
    let ws = tempfile::tempdir().unwrap();
    let tool = staged(
        ws.path(),
        "silent",
        "#!/bin/sh\nexit 0\n",
        json!({ "p1": "x" }),
    );
    let verdict = prove(&Chain::empty(), &tool, ws.path()).await;
    assert_eq!(
        verdict,
        WitnessVerdict::Vacuous(VacuousWitness::EmptyOutput),
        "a tool with no output has nothing to assert on"
    );
    assert!(!verdict.is_proven());
}

/// The tautology: the tool's whole output is its own argument. That holds for
/// any implementation able to spell its input, so its flip constrains nothing.
#[tokio::test]
async fn a_tool_that_only_echoes_its_input_is_vacuous() {
    let ws = tempfile::tempdir().unwrap();
    let tool = staged(
        ws.path(),
        "parrot",
        "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$STELLA_INPUT_P1\"\n",
        json!({ "p1": "just-the-argument" }),
    );
    let verdict = prove(&Chain::empty(), &tool, ws.path()).await;
    assert!(
        matches!(
            &verdict,
            WitnessVerdict::Vacuous(VacuousWitness::EchoesItsInput { value })
                if value == "just-the-argument"
        ),
        "{verdict:?}"
    );
}

/// ...but only on equality. A tool whose output *contains* its argument —
/// every grep-shaped tool there is — is doing real work and must pass.
#[tokio::test]
async fn a_tool_whose_output_merely_contains_its_input_is_not_vacuous() {
    let ws = tempfile::tempdir().unwrap();
    let tool = staged(
        ws.path(),
        "grepish",
        "#!/bin/sh\nset -eu\nprintf 'src/lib.rs:42: %s here\\n' \"$STELLA_INPUT_P1\"\n",
        json!({ "p1": "needle" }),
    );
    let verdict = prove(&Chain::empty(), &tool, ws.path()).await;
    let WitnessVerdict::Proven(case) = &verdict else {
        panic!("expected proven, got {verdict:?}");
    };
    assert_eq!(case.expect, "src/lib.rs:42: needle here");
}

/// A recorded expectation has to be a standing claim. A tool that answers
/// differently every run can be observed once but never re-verified, so its
/// witness is not a proof the gate can keep checking.
#[tokio::test]
async fn a_tool_that_disagrees_with_itself_is_unstable() {
    let ws = tempfile::tempdir().unwrap();
    let counter = ws.path().join("runs");
    let tool = staged(
        ws.path(),
        "flaky",
        &format!(
            "#!/bin/sh\nset -eu\nprintf 'x' >> '{}'\nwc -c < '{}'\n",
            counter.display(),
            counter.display()
        ),
        json!({ "p1": "x" }),
    );
    let verdict = prove(&Chain::empty(), &tool, ws.path()).await;
    assert!(
        matches!(&verdict, WitnessVerdict::Unstable { .. }),
        "{verdict:?}"
    );
    assert!(verdict.summary().starts_with("unstable"), "{verdict:?}");
}

/// No witness input, no proof. Reported as such rather than as a pass.
#[tokio::test]
async fn a_manifest_with_no_witness_input_cannot_be_proven() {
    let ws = tempfile::tempdir().unwrap();
    let tool = staged(ws.path(), "unprovable", "#!/bin/sh\necho hi\n", json!({}));
    let verdict = prove(&Chain::empty(), &tool, ws.path()).await;
    assert!(
        matches!(
            &verdict,
            WitnessVerdict::Vacuous(VacuousWitness::NoWitnessInput { .. })
        ),
        "{verdict:?}"
    );
}

/// A witness input that does not cover every required hole is rejected before
/// anything runs: `set -u` would fail the script for a reason that has nothing
/// to do with the capability, and reporting that as `Refuted` would blame the
/// tool for the manifest's gap.
#[test]
fn an_incomplete_witness_input_is_caught_before_the_tool_runs() {
    let ws = tempfile::tempdir().unwrap();
    let mut tool = staged(
        ws.path(),
        "partial",
        "#!/bin/sh\ntrue\n",
        json!({ "p1": "a" }),
    );
    tool.input_schema = json!({
        "type": "object",
        "properties": { "p1": { "type": "string" }, "p2": { "type": "string" } },
        "required": ["p1", "p2"],
    });
    let err = witness_input(&tool).expect_err("must not run with a hole unfilled");
    assert!(err.to_string().contains("p2"), "{err}");
}

/// A hand-written manifest has no `[foundry]` table and therefore no witness —
/// which is correct: this whole protocol governs what Stella authored, and a
/// developer's own tool was never in scope.
#[test]
fn a_hand_written_manifest_has_no_witness_input() {
    let tool = CustomTool {
        name: "deploy".into(),
        description: "a developer's own".into(),
        command: vec!["./deploy.sh".into()],
        timeout_ms: 1000,
        input_schema: json!({ "type": "object" }),
        env: Default::default(),
        source: PathBuf::from("/x/deploy.toml"),
        foundry: None,
    };
    assert!(witness_input(&tool).is_err());
}

/// Every value the authoring pass stamps as witness input is a value the
/// receipts actually carried — never one this code invented.
#[test]
fn the_witness_input_is_drawn_from_observed_arguments() {
    let ws = tempfile::tempdir().unwrap();
    let tool = authored_in(
        ws.path(),
        &["cat notes/a.txt", "cat notes/b.txt", "cat notes/c.txt"],
    );
    let provenance = tool.foundry.as_ref().expect("foundry table");
    assert_eq!(provenance.authored_by, crate::foundry_gate::AUTHORED_BY);
    let value = provenance.witness_input["p1"].as_str().expect("p1");
    assert!(
        ["notes/a.txt", "notes/b.txt", "notes/c.txt"].contains(&value),
        "witness input must be an OBSERVED argument, got `{value}`"
    );
}

/// A numeric hole keeps its JSON type through the manifest round trip, so the
/// witness input still satisfies the schema the same manifest declares.
#[test]
fn a_numeric_hole_round_trips_as_a_number() {
    let ws = tempfile::tempdir().unwrap();
    let tool = authored_in(
        ws.path(),
        &[
            "git log --oneline -5",
            "git log --oneline -10",
            "git log --oneline -20",
        ],
    );
    let provenance = tool.foundry.as_ref().expect("foundry table");
    assert!(
        provenance.witness_input["p1"].is_number(),
        "a `number` schema property needs a number witness input: {:?}",
        provenance.witness_input
    );
    assert_eq!(tool.input_schema["properties"]["p1"]["type"], "number");
    witness_input(&tool).expect("a complete numeric input is runnable");
}
