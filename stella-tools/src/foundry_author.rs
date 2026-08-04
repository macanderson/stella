//! Tool-foundry authorship — turn a detector proposal into a **staged**
//! custom-tool definition: a manifest plus a POSIX-sh wrapper script.
//!
//! This is the second slice of self-improvement issue #830. The first slice
//! (`stella_core::tool_foundry`) mines `bash` receipts and *proposes* a tool;
//! this module *authors* one — a complete `<name>.toml` manifest and
//! `<name>.sh` script, both rendered as text for the caller to write under
//! `.stella/tools/proposed/`. Discovery ([`crate::custom::discover`]) only
//! scans `*.toml` files directly in `.stella/tools/`, so nothing staged in the
//! `proposed/` subdirectory registers, advertises, or executes.
//!
//! Staging is only the first guardrail, and on its own a weak one — it makes a
//! tool inert by putting it somewhere unread, which means moving the file is
//! the whole approval. [`crate::foundry_gate`] replaces that with a standing
//! check: an authored manifest carries a typed `[foundry]` table, and a
//! manifest carrying one registers only while the workspace's adoption ledger
//! says it was proven ([`crate::foundry_witness`]), enabled by a human, and
//! still holds the bytes that were proven. So a hand-move now grants nothing,
//! and the path forward is `stella tools --adopt` then `--enable`.
//!
//! **Authorship is proven before it is returned.** [`author`] feeds its own
//! rendered manifest back through [`crate::custom::parse_manifest`] — the
//! exact parser discovery uses — and fails closed on any disagreement. An
//! authored tool can therefore never be malformed-on-arrival: if it returns
//! `Ok`, the manifest parses, the name is valid and unreserved, and the
//! schema round-trips.
//!
//! **Injection posture.** The generated script expands every parameter as a
//! quoted `"$STELLA_INPUT_PN"` reference. Custom tools deliver each scalar
//! input property as a `STELLA_INPUT_*` environment variable (see
//! [`crate::custom`]), and a quoted variable expansion in POSIX sh is data —
//! never word-split, globbed, or evaluated — so a parameter value cannot
//! inject shell syntax into the authored command. What a value *can* still do
//! is start with `-` and be read as a flag by the underlying program;
//! argument-position vetting is exactly what the human review before adoption
//! is for.

use stella_core::{ParamKind, ProposedTool, ToolParameter};

/// Workspace-relative directory staged tools land in. A subdirectory of the
/// custom-tools dir on purpose: near where an adopted manifest will live, but
/// invisible to discovery's non-recursive `*.toml` scan.
pub const PROPOSED_DIR: &str = ".stella/tools/proposed";

/// Suffix appended when a synthesized name collides with a reserved built-in
/// (`grep`, `glob`, …): `grep` → `grep_cmd`.
const DECONFLICT_SUFFIX: &str = "_cmd";

/// A fully authored, self-validated tool definition, rendered as file text.
/// Nothing here has touched the filesystem — writing (and the explicit human
/// decision to adopt) belongs to the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoredTool {
    /// Final tool name — the proposal's name, deconflicted against
    /// [`crate::custom::RESERVED_NAMES`].
    pub name: String,
    /// Complete manifest file text: provenance comment header + TOML body.
    pub manifest_toml: String,
    /// Complete wrapper-script text (`#!/bin/sh`, `set -eu`, the command).
    pub script: String,
    /// `<name>.toml` — file name to write under [`PROPOSED_DIR`].
    pub manifest_filename: String,
    /// `<name>.sh` — file name to write under [`PROPOSED_DIR`].
    pub script_filename: String,
}

/// Wire shape of the manifest body. Field order is load-bearing for the TOML
/// serializer: every scalar and array precedes the two tables (`foundry`,
/// `input_schema`), so serialization can never emit a value after a table.
#[derive(serde::Serialize)]
struct ManifestBody<'a> {
    name: &'a str,
    description: String,
    command: [String; 1],
    foundry: crate::foundry_gate::FoundryProvenance,
    input_schema: serde_json::Value,
}

/// Author a staged tool definition from a detector proposal. Pure text-in,
/// text-out; the only failure modes are a proposal whose signature and
/// parameter list disagree (a detector-contract violation) and a rendered
/// manifest the real parser rejects — both impossible for proposals built by
/// `stella_core::detect_tool_gaps`, both checked anyway.
pub fn author(proposal: &ProposedTool) -> Result<AuthoredTool, String> {
    let name = deconflict_name(&proposal.name);
    let (command_line, holes) = render_command(&proposal.signature);
    if holes != proposal.parameters.len() {
        return Err(format!(
            "signature `{}` has {holes} holes but the proposal carries {} parameters",
            proposal.signature,
            proposal.parameters.len()
        ));
    }

    let script = render_script(&name, &command_line);
    let manifest_filename = format!("{name}.toml");
    let script_filename = format!("{name}.sh");

    let body = ManifestBody {
        name: &name,
        description: description_for(proposal),
        command: [format!("./{PROPOSED_DIR}/{script_filename}")],
        foundry: crate::foundry_gate::FoundryProvenance {
            authored_by: crate::foundry_gate::AUTHORED_BY.to_string(),
            signature: proposal.signature.clone(),
            occurrences: proposal.occurrences as u32,
            witness_input: witness_input_for(&proposal.parameters),
        },
        input_schema: input_schema_for(&proposal.parameters),
    };
    let toml_body = toml::to_string_pretty(&body)
        .map_err(|e| format!("authored manifest failed to serialize: {e}"))?;
    let manifest_toml = format!("{}{toml_body}", provenance_header(proposal, &name));

    // The witness-before-write: the manifest must survive the exact parser
    // discovery will apply, or authorship failed and nothing should be staged.
    let parsed =
        crate::custom::parse_manifest(&manifest_toml, std::path::Path::new(&manifest_filename))
            .map_err(|e| format!("authored manifest failed its own parse check: {e}"))?;
    if parsed.name != name {
        return Err(format!(
            "authored manifest parsed to name `{}`, expected `{name}`",
            parsed.name
        ));
    }

    Ok(AuthoredTool {
        name,
        manifest_toml,
        script,
        manifest_filename,
        script_filename,
    })
}

/// The proposal's name, made safe to adopt: reserved built-in names get
/// [`DECONFLICT_SUFFIX`], with the base truncated first so the result stays
/// within the 64-char name rule.
fn deconflict_name(proposed: &str) -> String {
    if !crate::custom::RESERVED_NAMES.contains(&proposed) {
        return proposed.to_string();
    }
    let mut base = proposed.to_string();
    base.truncate(64 - DECONFLICT_SUFFIX.len());
    format!("{base}{DECONFLICT_SUFFIX}")
}

/// Rewrite the detector signature into the script's command line: each typed
/// placeholder becomes a quoted `"$STELLA_INPUT_PN"` expansion (keeping any
/// `--flag=` / `NAME=` prefix), every other token — skeleton words and shell
/// operators — passes through verbatim. Returns the line and the hole count.
///
/// Exact by construction: the tokenizer that built the signature treats `<`
/// and `>` as operator characters, so no skeleton word can contain a
/// placeholder-shaped substring, and a bare `<`/`>` operator token never ends
/// in `str>`/`path>`/`num>`.
fn render_command(signature: &str) -> (String, usize) {
    let mut parts: Vec<String> = Vec::new();
    let mut n = 0usize;
    'token: for part in signature.split(' ') {
        for kind in [ParamKind::Str, ParamKind::Path, ParamKind::Number] {
            if let Some(prefix) = part.strip_suffix(kind.placeholder())
                && (prefix.is_empty() || prefix.ends_with('='))
            {
                n += 1;
                parts.push(format!("{prefix}\"$STELLA_INPUT_P{n}\""));
                continue 'token;
            }
        }
        parts.push(part.to_string());
    }
    (parts.join(" "), n)
}

/// The wrapper script: shebang, a provenance note, strict mode, the command.
fn render_script(name: &str, command_line: &str) -> String {
    format!(
        "#!/bin/sh\n\
         # {name} — authored by stella's tool foundry from observed shell usage.\n\
         # Parameters arrive as STELLA_INPUT_P* environment variables and are\n\
         # expanded quoted, so values stay data — never shell syntax.\n\
         set -eu\n\
         {command_line}\n"
    )
}

/// The description the model will see once the tool is adopted.
fn description_for(proposal: &ProposedTool) -> String {
    format!(
        "Run `{}` with the parameters filling the placeholders in order. \
         Authored by the tool foundry from {} observed uses of this shell shape.",
        proposal.signature, proposal.occurrences
    )
}

/// The input the capability witness will run this tool with: the FIRST value
/// actually observed at each hole, typed to match the schema.
///
/// Real observed arguments, not invented ones. A witness over made-up input
/// proves the tool runs on input nobody has ever given it — the receipts are
/// the whole reason there is a proposal here, and they are the only arguments
/// known to have worked. Stamping them into the manifest is also what lets
/// adoption happen long after those receipts have rolled out of the
/// detector's mining window (see [`crate::foundry_witness`]).
///
/// A numeric hole whose observed value will not parse as a number is left
/// out rather than smuggled through as a string: the schema says `number`,
/// and an input that violates the tool's own schema is not a witness. The
/// missing key makes the witness report `NoWitnessInput`, which is the
/// honest answer.
fn witness_input_for(parameters: &[ToolParameter]) -> serde_json::Value {
    let mut input = serde_json::Map::new();
    for param in parameters {
        let Some(example) = param.examples.first() else {
            continue;
        };
        let value = match param.kind {
            ParamKind::Str | ParamKind::Path => serde_json::Value::String(example.clone()),
            ParamKind::Number => match example.parse::<serde_json::Number>() {
                Ok(number) => serde_json::Value::Number(number),
                Err(_) => continue,
            },
        };
        input.insert(param.name.clone(), value);
    }
    serde_json::Value::Object(input)
}

/// JSON Schema for the tool input: one required property per hole, typed
/// string/number, described by what was actually observed at that position.
fn input_schema_for(parameters: &[ToolParameter]) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    let mut required: Vec<serde_json::Value> = Vec::new();
    for param in parameters {
        let (json_type, noun) = match param.kind {
            ParamKind::Str => ("string", "string value"),
            ParamKind::Path => ("string", "path"),
            ParamKind::Number => ("number", "number"),
        };
        let observed = if param.examples.is_empty() {
            String::new()
        } else {
            format!(" (observed: {})", param.examples.join(", "))
        };
        properties.insert(
            param.name.clone(),
            serde_json::json!({
                "type": json_type,
                "description": format!("{noun} for this position{observed}"),
            }),
        );
        required.push(serde_json::Value::String(param.name.clone()));
    }
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// The comment block above the TOML body: what this is, why it is inert, how
/// to enable it, and the receipts it was authored from. Every borrowed line is
/// comment-sanitized so a multi-line or control-character command cannot break
/// the file's TOML validity.
fn provenance_header(proposal: &ProposedTool, name: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# {name} — authored by stella's tool foundry (self-improvement #830).\n\
         # STAGED ONLY: discovery scans .stella/tools/*.toml non-recursively, so\n\
         # nothing under proposed/ is registered, advertised, or executable.\n\
         #\n\
         # To pre-flight:  stella tools --validate {PROPOSED_DIR}\n\
         # To adopt:       stella tools --adopt {name}    (runs its witness)\n\
         # To enable:      stella tools --enable {name}   (the human approval)\n\
         #\n\
         # Evidence: {} invocations, {} distinct argument sets.\n",
        proposal.occurrences, proposal.distinct_arguments,
    ));
    for line in comment_lines(&format!("signature: {}", proposal.signature)) {
        out.push_str(&format!("# {line}\n"));
    }
    for example in &proposal.examples {
        for line in comment_lines(&format!("$ {example}")) {
            out.push_str(&format!("#   {line}\n"));
        }
    }
    out.push('\n');
    out
}

/// Split borrowed text into TOML-comment-safe lines: newlines become separate
/// comment lines, and remaining control characters (a bare `\r`, an escape
/// byte) become spaces — TOML forbids them inside comments.
fn comment_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| {
            line.chars()
                .map(|c| if c.is_control() && c != '\t' { ' ' } else { c })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use stella_core::{GapDetectionConfig, ShellInvocation, detect_tool_gaps};

    use super::*;

    /// Run the real detector over a history and return its proposals — every
    /// test authors from genuine detector output, never hand-built structs.
    fn proposals_for(commands: &[&str]) -> Vec<ProposedTool> {
        let history: Vec<ShellInvocation> =
            commands.iter().map(|c| ShellInvocation::ok(*c)).collect();
        detect_tool_gaps(&history, GapDetectionConfig::default())
    }

    fn author_one(commands: &[&str]) -> AuthoredTool {
        let proposals = proposals_for(commands);
        assert_eq!(proposals.len(), 1, "expected exactly one proposal");
        author(&proposals[0]).expect("authorship must succeed")
    }

    #[test]
    fn the_motivating_jq_case_authors_a_parseable_manifest() {
        let authored = author_one(&[
            "jq '.items[0]' a.json",
            "jq '.name' b.json",
            "jq '.error.code' c.json",
        ]);
        assert_eq!(authored.name, "jq");
        assert_eq!(authored.manifest_filename, "jq.toml");
        assert_eq!(authored.script_filename, "jq.sh");

        // The parse check already ran inside author(); parse again here to
        // inspect what discovery would see.
        let parsed =
            crate::custom::parse_manifest(&authored.manifest_toml, std::path::Path::new("jq.toml"))
                .expect("manifest parses");
        assert_eq!(parsed.name, "jq");
        assert_eq!(parsed.command, vec![format!("./{PROPOSED_DIR}/jq.sh")]);
        assert_eq!(parsed.input_schema["type"], "object");
        assert_eq!(parsed.input_schema["properties"]["p1"]["type"], "string");
        assert_eq!(parsed.input_schema["properties"]["p2"]["type"], "string");
        assert_eq!(
            parsed.input_schema["required"],
            serde_json::json!(["p1", "p2"])
        );
        assert_eq!(parsed.input_schema["additionalProperties"], false);

        assert!(
            authored
                .script
                .contains("jq \"$STELLA_INPUT_P1\" \"$STELLA_INPUT_P2\""),
            "script quotes every hole: {}",
            authored.script
        );
        assert!(authored.script.starts_with("#!/bin/sh\n"));
        assert!(authored.script.contains("set -eu"));
    }

    #[test]
    fn a_reserved_name_is_deconflicted() {
        // `grep <str> <path>` synthesizes the name `grep`, which is a
        // reserved built-in — the authored tool must not shadow it.
        let authored = author_one(&[
            "grep 'todo' src/a.rs",
            "grep 'fixme' src/b.rs",
            "grep 'hack' src/c.rs",
        ]);
        assert_eq!(authored.name, "grep_cmd");
        assert!(authored.manifest_toml.contains("name = \"grep_cmd\""));
    }

    #[test]
    fn numeric_holes_are_typed_number() {
        let authored = author_one(&[
            "git log --oneline -5",
            "git log --oneline -10",
            "git log --oneline -20",
        ]);
        let parsed =
            crate::custom::parse_manifest(&authored.manifest_toml, std::path::Path::new("x.toml"))
                .unwrap();
        assert_eq!(parsed.input_schema["properties"]["p1"]["type"], "number");
        assert!(
            authored
                .script
                .contains("git log --oneline \"$STELLA_INPUT_P1\""),
            "{}",
            authored.script
        );
    }

    #[test]
    fn flag_and_env_prefixes_survive_in_the_script() {
        let flagged = author_one(&[
            "curl --retry=3 https://a.test/x",
            "curl --retry=5 https://b.test/y",
            "curl --retry=1 https://c.test/z",
        ]);
        assert!(
            flagged
                .script
                .contains("curl --retry=\"$STELLA_INPUT_P1\" \"$STELLA_INPUT_P2\""),
            "{}",
            flagged.script
        );

        let env_assign = author_one(&[
            "env RUST_LOG=debug cargo test",
            "env RUST_LOG=trace cargo test",
            "env RUST_LOG=info cargo test",
        ]);
        assert!(
            env_assign
                .script
                .contains("env RUST_LOG=\"$STELLA_INPUT_P1\" cargo test"),
            "{}",
            env_assign.script
        );
    }

    #[test]
    fn pipeline_operators_pass_through() {
        let authored = author_one(&[
            "jq '.a' one.json | head",
            "jq '.b' two.json | head",
            "jq '.c' three.json | head",
        ]);
        assert!(
            authored
                .script
                .contains("jq \"$STELLA_INPUT_P1\" \"$STELLA_INPUT_P2\" | head"),
            "{}",
            authored.script
        );
    }

    #[test]
    fn provenance_header_carries_evidence_and_stays_valid_toml() {
        let authored = author_one(&["jq '.a' a.json", "jq '.b' b.json", "jq '.c' c.json"]);
        assert!(authored.manifest_toml.contains("STAGED ONLY"));
        assert!(authored.manifest_toml.contains("3 invocations"));
        assert!(authored.manifest_toml.contains("$ jq '.a' a.json"));
        // The whole file — comments included — is valid TOML.
        toml::from_str::<toml::Value>(&authored.manifest_toml).expect("valid TOML");
    }

    #[tokio::test]
    async fn an_authored_tool_actually_executes_through_the_custom_path() {
        use stella_core::ports::ToolExecutor;
        use stella_protocol::tool::ToolOutput;

        // A shape with no external dependency: `echo` + a path hole.
        let authored = author_one(&["echo built a.txt", "echo built b.txt", "echo built c.txt"]);

        // Stage it in a real workspace layout and run it via the same
        // executor a session would use.
        let ws = tempfile::tempdir().unwrap();
        let staged = ws.path().join(PROPOSED_DIR);
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(
            staged.join(&authored.manifest_filename),
            &authored.manifest_toml,
        )
        .unwrap();
        let script_path = staged.join(&authored.script_filename);
        std::fs::write(&script_path, &authored.script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let tool = crate::custom::parse_manifest(
            &authored.manifest_toml,
            std::path::Path::new("echo_built.toml"),
        )
        .unwrap();
        struct NoInner;
        #[async_trait::async_trait]
        impl ToolExecutor for NoInner {
            fn schemas(&self) -> Vec<stella_protocol::tool::ToolSchema> {
                Vec::new()
            }
            async fn execute(&self, _: &str, _: &serde_json::Value) -> ToolOutput {
                ToolOutput::Error {
                    message: "no inner".into(),
                }
            }
        }
        let inner = NoInner;
        let name = tool.name.clone();
        let set = crate::custom::CustomToolSet::new(&inner, vec![tool], ws.path().to_path_buf());
        let out = set
            .execute(&name, &serde_json::json!({ "p1": "delivered.txt" }))
            .await;
        match out {
            ToolOutput::Ok { content } => {
                assert!(content.contains("built delivered.txt"), "{content}")
            }
            ToolOutput::Error { message } => panic!("expected ok: {message}"),
        }
    }

    #[test]
    fn discovery_never_sees_a_staged_tool() {
        // The guardrail itself: files under proposed/ are invisible to the
        // scan that registers custom tools.
        let authored = author_one(&["jq '.a' a.json", "jq '.b' b.json", "jq '.c' c.json"]);
        let ws = tempfile::tempdir().unwrap();
        let staged = ws.path().join(PROPOSED_DIR);
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(
            staged.join(&authored.manifest_filename),
            &authored.manifest_toml,
        )
        .unwrap();
        std::fs::write(staged.join(&authored.script_filename), &authored.script).unwrap();

        let report = crate::custom::discover_in(ws.path(), None);
        assert!(report.tools.is_empty(), "staged tools must not register");
        assert!(report.diagnostics.is_empty(), "and must not even warn");
    }

    proptest::proptest! {
        /// Every proposal the detector can produce authors successfully, and
        /// the authored manifest round-trips through the real parser with a
        /// schema property per parameter and a quoted expansion per hole.
        #[test]
        fn every_detector_proposal_authors_and_round_trips(
            history in proptest::collection::vec(arb_invocation(), 0..60),
        ) {
            for proposal in detect_tool_gaps(&history, GapDetectionConfig::default()) {
                let authored = author(&proposal).expect("authorship succeeds");
                let parsed = crate::custom::parse_manifest(
                    &authored.manifest_toml,
                    std::path::Path::new(&authored.manifest_filename),
                ).expect("round-trips through the real parser");
                proptest::prop_assert_eq!(&parsed.name, &authored.name);
                let props = parsed.input_schema["properties"]
                    .as_object()
                    .expect("schema has properties");
                proptest::prop_assert_eq!(props.len(), proposal.parameters.len());
                let expansions = authored.script.matches("\"$STELLA_INPUT_P").count();
                proptest::prop_assert_eq!(expansions, proposal.parameters.len());
            }
        }
    }

    /// Mirrors the detector's own property-test alphabet: overlapping shapes
    /// so runs exercise real clustering.
    fn arb_invocation() -> impl proptest::prelude::Strategy<Value = ShellInvocation> {
        use proptest::prelude::*;
        (0..4usize, 0..4usize, any::<bool>()).prop_map(|(prog, arg, succeeded)| {
            let progs = ["jq", "sed", "grep", "git log"];
            let args = ["'.x' a.json", "'.y' b.json", "-5", "--flag=1 c.json"];
            ShellInvocation {
                command: format!("{} {}", progs[prog], args[arg]),
                succeeded,
            }
        })
    }
}
