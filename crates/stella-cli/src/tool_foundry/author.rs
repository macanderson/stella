//! Authoring — render a ledgered gap into a staged manifest+script pair
//! under `.stella/tools/proposed/`.
//!
//! This is the retired authoring slice rebuilt behind the autonomous
//! foundry's controls: the pair it writes is **inert** (discovery's non-recursive scan
//! cannot see the staging directory), and everything that could make it
//! runnable still goes through the adoption witness, the foundry gate's
//! per-call re-digest, and the spawn-time network denial. `stella tools
//! --draft <gap-id>` runs exactly this and stops — that IS draft-only mode.
//!
//! # The script preserves the observed shape byte-exactly
//!
//! The command line in the emitted script is the gap's `command_template`
//! with each `{pN}` hole replaced by a quoted `"${STELLA_INPUT_PN}"` — and
//! nothing else touched. Shell operators (`>`, `>>`, `2>&1`, `|`, `<`)
//! survive verbatim:
//! a proposed tool that runs a semantically different command than the
//! pattern it claims to generalize is wrong output, not a blemish.

use std::path::{Path, PathBuf};

use stella_tools::foundry_gate::{AUTHORED_BY, FoundryProvenance, PROPOSED_DIR};

use super::gaps::GapRecord;

/// A staged pair on disk, ready for `--adopt` (or for review and deletion).
#[derive(Debug, Clone)]
pub(crate) struct AuthoredPair {
    /// The tool name the pair was written under — the gap's synthesized name,
    /// possibly suffixed to dodge a collision.
    pub name: String,
    /// `.stella/tools/proposed/<name>.toml`.
    pub manifest_path: PathBuf,
    /// `.stella/tools/proposed/<name>.sh`.
    pub script_path: PathBuf,
}

/// Render and write the staged pair for one gap. Fails closed: the emitted
/// manifest is re-parsed through the same parser discovery uses, and the
/// script through [`lint_script`], before either is left on disk.
pub(crate) fn author_pair(root: &Path, gap: &GapRecord) -> Result<AuthoredPair, String> {
    let name = choose_name(root, &gap.name, &gap.gap_id);
    let script = render_script(&name, gap);
    let manifest = render_manifest(&name, gap)?;

    // Self-check before anything lands: the bytes must parse to a
    // foundry-authored tool pointing at the script beside them, and the
    // script must pass the static lint against the manifest's own schema.
    let staged_manifest_path = root.join(PROPOSED_DIR).join(format!("{name}.toml"));
    let parsed = stella_tools::custom::parse_manifest(&manifest, &staged_manifest_path)
        .map_err(|e| format!("the authored manifest failed its own parse check: {e}"))?;
    if !parsed
        .foundry
        .as_ref()
        .is_some_and(FoundryProvenance::is_foundry_authored)
    {
        return Err("the authored manifest lost its [foundry] table".to_string());
    }
    let expected_command = vec![format!("./{PROPOSED_DIR}/{name}.sh")];
    if parsed.command != expected_command {
        return Err(format!(
            "the authored manifest points at `{}`, not the staged script",
            parsed.command.first().map(String::as_str).unwrap_or("")
        ));
    }
    lint_script(&script, &parsed.input_schema)?;

    let staged_dir = root.join(PROPOSED_DIR);
    std::fs::create_dir_all(&staged_dir)
        .map_err(|e| format!("cannot create {PROPOSED_DIR}: {e}"))?;
    let script_path = staged_dir.join(format!("{name}.sh"));
    std::fs::write(&script_path, &script)
        .map_err(|e| format!("cannot write {}: {e}", script_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("cannot mark {} executable: {e}", script_path.display()))?;
    }
    std::fs::write(&staged_manifest_path, &manifest)
        .map_err(|e| format!("cannot write {}: {e}", staged_manifest_path.display()))?;

    Ok(AuthoredPair {
        name,
        manifest_path: staged_manifest_path,
        script_path,
    })
}

/// The static script lint — the "validate" half a dry run cannot cover
/// because it reads the text rather than one execution of it. Checks the
/// shape every authored script promises: a `sh` shebang, `set -eu` (so an
/// unset parameter is a loud failure, never an empty-string argument), and
/// no `STELLA_INPUT_*` reference the manifest's schema does not declare.
pub(crate) fn lint_script(script: &str, input_schema: &serde_json::Value) -> Result<(), String> {
    if script.contains('\0') {
        return Err("script lint: the script contains a NUL byte".to_string());
    }
    if !script.starts_with("#!/bin/sh\n") {
        return Err("script lint: the script must start with `#!/bin/sh`".to_string());
    }
    if !script.lines().any(|line| line.trim() == "set -eu") {
        return Err("script lint: the script must `set -eu`".to_string());
    }

    let declared: Vec<String> = input_schema
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|props| props.keys().map(|k| k.to_ascii_uppercase()).collect())
        .unwrap_or_default();
    let mut rest = script;
    while let Some(at) = rest.find("${STELLA_INPUT_") {
        let tail = &rest[at + "${STELLA_INPUT_".len()..];
        let Some(close) = tail.find('}') else {
            return Err("script lint: an unterminated ${STELLA_INPUT_ reference".to_string());
        };
        let key = &tail[..close];
        if !declared.iter().any(|d| d == key) {
            return Err(format!(
                "script lint: the script reads STELLA_INPUT_{key} but the manifest declares \
                 no `{}` property",
                key.to_ascii_lowercase()
            ));
        }
        rest = &tail[close + 1..];
    }
    Ok(())
}

/// A name the staged pair can actually claim: the gap's synthesized name,
/// suffixed with `_tool` when a built-in reserves it, and with a slice of
/// the gap id when a manifest by that name already exists in either the
/// staging or the live directory. Never silently replaces anything.
fn choose_name(root: &Path, candidate: &str, gap_id: &str) -> String {
    let mut name = candidate.to_string();
    if stella_tools::catalog::is_reserved(&name) {
        name = format!("{name}_tool");
    }
    let taken = |name: &str| {
        root.join(PROPOSED_DIR)
            .join(format!("{name}.toml"))
            .exists()
            || root
                .join(".stella")
                .join("tools")
                .join(format!("{name}.toml"))
                .exists()
    };
    if taken(&name) {
        let suffix: String = gap_id.chars().take(6).collect();
        name = format!("{name}_{suffix}");
    }
    // The 64-char ceiling survives both suffixes only if we enforce it.
    if name.len() > 64 {
        name.truncate(64);
    }
    name
}

/// The emitted script: shebang, provenance comment, `set -eu`, then the
/// observed command shape with each `{pN}` replaced by `"${STELLA_INPUT_PN}"`
/// — and every other byte of the template, shell operators included,
/// untouched.
fn render_script(name: &str, gap: &GapRecord) -> String {
    let mut command = gap.command_template.clone();
    for parameter in &gap.parameters {
        let hole = format!("{{{}}}", parameter.name);
        let substitution = format!(
            "\"${{STELLA_INPUT_{}}}\"",
            parameter.name.to_ascii_uppercase()
        );
        command = command.replace(&hole, &substitution);
    }
    format!(
        "#!/bin/sh\n\
         # `{name}` — authored by {AUTHORED_BY} from gap {gap_id}.\n\
         # Observed shape: {signature}\n\
         set -eu\n\
         {command}\n",
        gap_id = gap.gap_id,
        signature = gap.signature,
    )
}

/// The emitted manifest, serialized (never hand-concatenated) so a signature
/// carrying TOML-significant characters cannot break the document.
fn render_manifest(name: &str, gap: &GapRecord) -> Result<String, String> {
    #[derive(serde::Serialize)]
    struct ManifestDoc<'a> {
        name: &'a str,
        description: String,
        command: Vec<String>,
        foundry: &'a FoundryProvenance,
        input_schema: serde_json::Value,
    }

    let mut witness_input = serde_json::Map::new();
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for parameter in &gap.parameters {
        let example = parameter.examples.first().cloned().ok_or_else(|| {
            format!(
                "gap {} carries no example value for `{}` — an unprovable tool is not \
                     authored",
                gap.gap_id, parameter.name
            )
        })?;
        witness_input.insert(
            parameter.name.clone(),
            serde_json::Value::String(example.clone()),
        );
        properties.insert(
            parameter.name.clone(),
            serde_json::json!({
                "type": "string",
                "description": format!(
                    "{} value for this position (observed: {})",
                    parameter.kind,
                    parameter.examples.join(", ")
                ),
            }),
        );
        required.push(serde_json::Value::String(parameter.name.clone()));
    }

    let provenance = FoundryProvenance {
        authored_by: AUTHORED_BY.to_string(),
        signature: gap.signature.clone(),
        occurrences: u32::try_from(gap.occurrences).unwrap_or(u32::MAX),
        witness_input: serde_json::Value::Object(witness_input),
        gap_id: gap.gap_id.clone(),
        approved: None,
    };
    let doc = ManifestDoc {
        name,
        description: format!(
            "Run the observed shell shape `{}` ({}x across {} argument sets). Authored by \
             the tool foundry from gap {}.",
            gap.signature, gap.occurrences, gap.distinct_arguments, gap.gap_id
        ),
        command: vec![format!("./{PROPOSED_DIR}/{name}.sh")],
        foundry: &provenance,
        input_schema: serde_json::json!({
            "type": "object",
            "required": required,
            "properties": properties,
        }),
    };
    let body = toml::to_string(&doc).map_err(|e| format!("cannot serialize the manifest: {e}"))?;
    Ok(format!(
        "# Authored by {AUTHORED_BY} from observed shell history — inert until adopted\n\
         # (`stella tools --adopt {name}` proves it; the gate re-digests it on every call).\n\
         {body}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_foundry::gaps::{GapParameter, GapRecord, gap_id};

    fn gap(signature: &str, template: &str, params: &[(&str, &str)]) -> GapRecord {
        GapRecord {
            gap_id: gap_id(signature),
            name: "jq".into(),
            signature: signature.into(),
            command_template: template.into(),
            parameters: params
                .iter()
                .map(|(name, example)| GapParameter {
                    name: (*name).to_string(),
                    kind: "str".into(),
                    examples: vec![(*example).to_string()],
                })
                .collect(),
            occurrences: 6,
            distinct_arguments: 2,
            examples: vec![],
            detected_at: 0,
        }
    }

    /// The pair lands under the staging directory, parses through the real
    /// manifest parser, is foundry-authored, and carries the gap lineage.
    #[test]
    fn an_authored_pair_parses_and_carries_its_lineage() {
        let dir = tempfile::tempdir().expect("tmp");
        let g = gap(
            "jq <str> <path>",
            "jq {p1} {p2}",
            &[("p1", ".a"), ("p2", "a.json")],
        );
        let pair = author_pair(dir.path(), &g).expect("authored");
        assert_eq!(pair.name, "jq");
        let text = std::fs::read_to_string(&pair.manifest_path).expect("manifest");
        let parsed =
            stella_tools::custom::parse_manifest(&text, &pair.manifest_path).expect("parses");
        let provenance = parsed.foundry.expect("foundry table");
        assert!(provenance.is_foundry_authored());
        assert_eq!(provenance.gap_id, g.gap_id);
        assert_eq!(provenance.witness_input["p1"], ".a");
        let script = std::fs::read_to_string(&pair.script_path).expect("script");
        assert!(script.contains("jq \"${STELLA_INPUT_P1}\" \"${STELLA_INPUT_P2}\""));
    }

    /// The redirect regression, at the emitter: a redirect-heavy template
    /// survives rendering byte-exact — `>`, `>>`, `2>&1`, and `|` all appear
    /// verbatim in the emitted command line.
    #[test]
    fn a_redirect_heavy_template_survives_rendering_byte_exact() {
        let dir = tempfile::tempdir().expect("tmp");
        let g = gap(
            "sort < <path> | uniq -c > out.txt >> log.txt 2>&1",
            "sort < {p1} | uniq -c > out.txt >> log.txt 2>&1",
            &[("p1", "a.txt")],
        );
        let pair = author_pair(dir.path(), &g).expect("authored");
        let script = std::fs::read_to_string(&pair.script_path).expect("script");
        assert!(
            script.contains("sort < \"${STELLA_INPUT_P1}\" | uniq -c > out.txt >> log.txt 2>&1"),
            "operators must survive byte-exact:\n{script}"
        );
    }

    /// A reserved built-in name is dodged, never shadowed.
    #[test]
    fn a_reserved_name_is_suffixed() {
        let dir = tempfile::tempdir().expect("tmp");
        let mut g = gap("bash <path>", "bash {p1}", &[("p1", "x.sh")]);
        g.name = "bash".into();
        let pair = author_pair(dir.path(), &g).expect("authored");
        assert_eq!(pair.name, "bash_tool");
    }

    /// Authoring twice does not overwrite: the second pair lands under a
    /// gap-id-suffixed name.
    #[test]
    fn an_existing_pair_is_not_overwritten() {
        let dir = tempfile::tempdir().expect("tmp");
        let g = gap(
            "jq <str> <path>",
            "jq {p1} {p2}",
            &[("p1", ".a"), ("p2", "a.json")],
        );
        let first = author_pair(dir.path(), &g).expect("first");
        let second = author_pair(dir.path(), &g).expect("second");
        assert_ne!(first.name, second.name);
        assert!(second.name.starts_with("jq_"));
        assert!(first.manifest_path.exists());
    }

    /// The lint refuses a script reading an undeclared input, and one with
    /// no `set -eu`.
    #[test]
    fn the_lint_holds_the_scripts_promises() {
        let schema = serde_json::json!({
            "type": "object", "properties": { "p1": { "type": "string" } }
        });
        assert!(lint_script("#!/bin/sh\nset -eu\necho \"${STELLA_INPUT_P1}\"\n", &schema).is_ok());
        let undeclared =
            lint_script("#!/bin/sh\nset -eu\necho \"${STELLA_INPUT_P9}\"\n", &schema).unwrap_err();
        assert!(undeclared.contains("p9"), "{undeclared}");
        let lax = lint_script("#!/bin/sh\necho hi\n", &schema).unwrap_err();
        assert!(lax.contains("set -eu"), "{lax}");
    }

    /// A gap with no example for a hole is refused — an unprovable tool is
    /// not authored, mirroring the witness's own posture.
    #[test]
    fn a_gap_with_no_examples_is_refused() {
        let dir = tempfile::tempdir().expect("tmp");
        let mut g = gap("jq <str>", "jq {p1}", &[("p1", ".a")]);
        g.parameters[0].examples.clear();
        let err = author_pair(dir.path(), &g).unwrap_err();
        assert!(err.contains("no example value"), "{err}");
    }
}
