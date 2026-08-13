//! Workspace domain inference — the `stella init` command.
//!
//! A **domain** is a semantic area of the workspace ("auth", "billing",
//! "cli", "ingestion") with the path prefixes that belong to it. Domains
//! are the tagging vocabulary for the whole context plane: memories
//! (including post-turn reflection lessons), code-graph nodes/edges, and
//! context facts all carry one or more domain tags, and recall uses domain
//! overlap as a relevance signal — so a lesson learned while touching
//! `stella-model` surfaces again when a future turn works in that
//! area.
//!
//! Inference is model-assisted with a deterministic fallback: `stella init`
//! summarizes the repo's shape (top-level structure + README head + key
//! manifests), asks the worker model for a domain taxonomy as structured
//! JSON (one bounded repair attempt on parse failure), and falls back to a
//! directory-name heuristic when no provider is configured or the call
//! fails — `init` always succeeds, offline included. Output is data on
//! disk (`.stella/domains.toml`), never code.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use stella_protocol::{CompletionMessage, CompletionRequest, ModelCallRole, Provider};

/// One inferred domain: a name, a one-line description, and the path
/// prefixes (workspace-relative, `/`-separated) that belong to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Domain {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub paths: Vec<String>,
}

/// The `.stella/domains.toml` document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Domains {
    /// Format version — additive evolution only.
    #[serde(default = "default_version")]
    pub version: u32,
    /// How this taxonomy was produced: `"model"` or `"heuristic"`.
    #[serde(default)]
    pub inferred_by: String,
    /// [`repo_shape_fingerprint`] of the [`summarize_repo`] summary the model
    /// inferred this taxonomy from (#3102). This is what lets a re-run of
    /// `stella init` skip the inference call when the repo's shape has not
    /// changed — the same content-hash skip the code-graph indexer uses,
    /// keyed on exactly the bytes the inference reads. Absent on heuristic
    /// taxonomies (so a provider-configured re-run always upgrades them) and
    /// on documents written before the field existed (which therefore
    /// re-infer once and gain it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_fingerprint: Option<String>,
    #[serde(default, rename = "domain")]
    pub domains: Vec<Domain>,
}

fn default_version() -> u32 {
    1
}

impl Domains {
    pub fn path_for(workspace_root: &Path) -> PathBuf {
        workspace_root.join(".stella").join("domains.toml")
    }

    /// Load the workspace's domains, if `stella init` has run. `None` when
    /// the file is absent (callers treat "no domains yet" as an empty tag
    /// vocabulary, never an error). Consumed by `SessionMemory` (memory.rs)
    /// to scope reflection tagging and recall to the workspace's domains.
    pub fn load(workspace_root: &Path) -> Result<Option<Self>, String> {
        let path = Self::path_for(workspace_root);
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text)
                .map(Some)
                .map_err(|e| format!("{} is malformed: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        }
    }

    pub fn save(&self, workspace_root: &Path) -> Result<PathBuf, String> {
        let path = Self::path_for(workspace_root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// The bare domain-name vocabulary (what reflection tagging and recall
    /// filters consume). Consumed by `SessionMemory` (memory.rs) to scope
    /// `recall_scoped` and per-turn reflection to the workspace's domains.
    pub fn names(&self) -> Vec<String> {
        self.domains.iter().map(|d| d.name.clone()).collect()
    }

    /// Resolve the domains a workspace-relative path belongs to, by prefix
    /// match. A path matching nothing gets an empty set — untagged is
    /// valid, not an error. Consumed by memory write-back to tag episodes
    /// with their domain context.
    pub fn domains_for_path(&self, rel_path: &str) -> Vec<String> {
        let normalized = rel_path.trim_start_matches("./");
        self.domains
            .iter()
            .filter(|d| {
                d.paths.iter().any(|prefix| {
                    let prefix = prefix.trim_end_matches('/');
                    normalized == prefix || normalized.starts_with(&format!("{prefix}/"))
                })
            })
            .map(|d| d.name.clone())
            .collect()
    }
}

/// Build the repo-shape summary the inference prompt sees: top-level (and
/// one nested level of) directories, README head, and which manifest files
/// exist. Deliberately shallow and bounded — this is a prompt, not an
/// index.
pub fn summarize_repo(root: &Path) -> String {
    let mut lines = Vec::new();

    let mut dirs: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }
            if entry.path().is_dir() {
                dirs.push(name.clone());
                if let Ok(nested) = std::fs::read_dir(entry.path()) {
                    for sub in nested.flatten().take(20) {
                        if sub.path().is_dir() {
                            let sub_name = sub.file_name().to_string_lossy().to_string();
                            if !sub_name.starts_with('.') {
                                dirs.push(format!("{name}/{sub_name}"));
                            }
                        }
                    }
                }
            }
        }
    }
    dirs.sort();
    lines.push(format!("Directories:\n{}", dirs.join("\n")));

    for manifest in [
        "package.json",
        "Cargo.toml",
        "pyproject.toml",
        "go.mod",
        "pom.xml",
    ] {
        if root.join(manifest).exists() {
            lines.push(format!("Has manifest: {manifest}"));
        }
    }

    for readme in ["README.md", "readme.md", "README"] {
        if let Ok(text) = std::fs::read_to_string(root.join(readme)) {
            let head: String = text.chars().take(1500).collect();
            lines.push(format!("README head:\n{head}"));
            break;
        }
    }

    lines.join("\n\n")
}

/// `sha256:<hex>` of the repo-shape summary the domain inference reads —
/// the key of the inference-skip gate (#3102). Hashing the summary rather
/// than the tree is deliberate: the inference is a function of exactly these
/// bytes, so any tree change that could change its answer changes the
/// summary, and any change that cannot (a body edited inside an existing
/// file) does not force a re-derivation of an unchanged answer.
pub fn repo_shape_fingerprint(summary: &str) -> String {
    use std::fmt::Write as _;
    let mut hasher = Sha256::new();
    hasher.update(summary.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(71);
    hex.push_str("sha256:");
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// The inference-skip gate (#3102): the existing `.stella/domains.toml`,
/// **iff** it can be reused without spending a model call — it was
/// model-inferred and its recorded [`Domains::source_fingerprint`] matches
/// the fingerprint of the summary the inference would read now. Everything
/// else returns `None` and re-infers: a heuristic taxonomy (so a run with a
/// provider upgrades it), a document from before the fingerprint existed, a
/// mismatched fingerprint (the repo's shape changed), or a missing/broken
/// file.
pub fn cached_taxonomy(root: &Path, summary: &str) -> Option<Domains> {
    let existing = Domains::load(root).ok().flatten()?;
    (existing.inferred_by == "model"
        && existing.source_fingerprint.as_deref() == Some(repo_shape_fingerprint(summary).as_str()))
    .then_some(existing)
}

/// Infer domains with the worker model; one bounded repair attempt on
/// unparseable output; heuristic fallback on any failure.
pub async fn infer_domains(
    provider: &dyn Provider,
    root: &Path,
    model_hint: &str,
    budget_limit: Option<f64>,
) -> (Domains, f64) {
    let summary = summarize_repo(root);
    let prompt = format!(
        "Analyze this repository's shape and infer its semantic DOMAINS — the 4-10 major \
         functional areas of the codebase (examples from other projects: auth, billing, \
         ingestion, cli, knowledge-graph, api, ui). For each domain give: name (short \
         kebab-case), description (one line), paths (the workspace-relative directory \
         prefixes that belong to it — only prefixes that actually appear in the listing \
         below).\n\nRespond with ONLY a JSON array, no prose:\n\
         [{{\"name\": \"...\", \"description\": \"...\", \"paths\": [\"...\"]}}]\n\n{summary}"
    );

    let mut messages = vec![
        CompletionMessage::system(
            "You infer domain taxonomies from repository structure. Respond with only valid JSON.",
        ),
        CompletionMessage::user(&prompt),
    ];
    let mut total_cost_usd = 0.0;

    for _attempt in 0..2 {
        let remaining_budget = budget_limit.map(|limit| (limit - total_cost_usd).max(0.0));
        let req = CompletionRequest {
            messages: messages.clone(),
            // Unstated on purpose: `DomainInference`'s output contract is
            // declared once at the chokepoint
            // (`accounted_call::standalone_bounds`), which keeps the 2,048 this
            // call always sent and adds the reasoning headroom it never had —
            // a reasoning model bills its thinking against the same number, so
            // the bare contract could be spent before the first `[` (#2444).
            max_output_tokens: None,
            temperature: Some(0.0),
            effort: None,
            tools: vec![],
            reasoning: None,
            params: None,
        };
        match crate::accounted_call::complete_standalone(
            root,
            provider,
            ModelCallRole::DomainInference,
            "domain_inference",
            model_hint,
            remaining_budget,
            req,
        )
        .await
        {
            Ok(accounted) => {
                total_cost_usd += accounted.cost_usd;
                match parse_domains_json(&accounted.result.text) {
                    Ok(domains) if !domains.is_empty() => {
                        return (
                            Domains {
                                version: 1,
                                inferred_by: "model".into(),
                                // Stamped so the next `stella init` over an
                                // unchanged repo shape reuses this answer
                                // instead of re-buying it (#3102).
                                source_fingerprint: Some(repo_shape_fingerprint(&summary)),
                                domains,
                            },
                            total_cost_usd,
                        );
                    }
                    Ok(_) | Err(_) => {
                        // Bounded repair: feed the failure back once.
                        messages.push(CompletionMessage {
                            role: stella_protocol::MessageRole::Assistant,
                            content: accounted.result.text.clone(),
                            tool_calls: vec![],
                            tool_results: vec![],
                            attachments: Vec::new(),
                        });
                        messages.push(CompletionMessage::user(
                            "That was not a valid non-empty JSON array of domains. Respond with \
                         ONLY the JSON array.",
                        ));
                    }
                }
            }
            Err(error) => {
                total_cost_usd += error.cost_usd;
                break;
            } // provider trouble → heuristic, don't hammer
        }
    }

    (heuristic_domains(root), total_cost_usd)
}

/// Extract and parse the first JSON array in `text` (models love prose and
/// code fences; tolerate both).
fn parse_domains_json(text: &str) -> Result<Vec<Domain>, String> {
    let start = text.find('[').ok_or("no JSON array found")?;
    let end = text.rfind(']').ok_or("unterminated JSON array")?;
    if end <= start {
        return Err("malformed JSON array".into());
    }
    serde_json::from_str::<Vec<Domain>>(&text[start..=end]).map_err(|e| e.to_string())
}

/// Offline fallback: each meaningful top-level directory becomes a domain.
/// Crude but deterministic — and honestly labeled `inferred_by =
/// "heuristic"` so a later `stella init` with a key configured can upgrade
/// it.
pub fn heuristic_domains(root: &Path) -> Domains {
    let mut domains = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| {
                !n.starts_with('.')
                    && ![
                        "node_modules",
                        "target",
                        "dist",
                        "build",
                        "out",
                        "vendor",
                        "coverage",
                        "tmp",
                    ]
                    .contains(&n.as_str())
            })
            .collect();
        names.sort();
        for name in names {
            domains.push(Domain {
                name: name.to_lowercase().replace(['_', ' '], "-"),
                description: format!("code under {name}/"),
                paths: vec![name],
            });
        }
    }
    Domains {
        version: 1,
        inferred_by: "heuristic".into(),
        // Deliberately unfingerprinted: a heuristic taxonomy must never arm
        // the inference-skip gate, so a later run with a provider upgrades it.
        source_fingerprint: None,
        domains,
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use stella_protocol::{CompletionRequestRef, CompletionResult, CompletionUsage, ProviderError};

    use super::*;

    struct RepairProvider {
        responses: tokio::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Provider for RepairProvider {
        fn id(&self) -> &str {
            "paid-domains"
        }

        async fn complete_ref(
            &self,
            _request: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            let text = self.responses.lock().await.remove(0);
            Ok(CompletionResult {
                upstream_provider: None,
                text,
                tool_calls: Vec::new(),
                usage: CompletionUsage {
                    reported: true,
                    input_tokens: 10,
                    output_tokens: 2,
                    ..CompletionUsage::default()
                },
                model: "paid-domains-model".into(),
                cost_usd: 0.006,
                finish_reason: None,
            })
        }
    }

    fn temp_root(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("stella-domains-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn save_load_round_trips() {
        let root = temp_root("roundtrip");
        let domains = Domains {
            version: 1,
            inferred_by: "model".into(),
            source_fingerprint: Some("sha256:abc".into()),
            domains: vec![Domain {
                name: "llm".into(),
                description: "provider adapters".into(),
                paths: vec!["crates/stella-model".into()],
            }],
        };
        domains.save(&root).expect("save");
        let loaded = Domains::load(&root).expect("load").expect("present");
        assert_eq!(loaded, domains);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_absent_is_none_not_an_error() {
        let root = temp_root("absent");
        assert!(Domains::load(&root).expect("ok").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn domains_for_path_prefix_matches_and_unmatched_is_empty() {
        let domains = Domains {
            version: 1,
            inferred_by: "model".into(),
            source_fingerprint: None,
            domains: vec![
                Domain {
                    name: "llm".into(),
                    description: String::new(),
                    paths: vec!["crates/stella-model".into()],
                },
                Domain {
                    name: "cli".into(),
                    description: String::new(),
                    paths: vec!["crates/stella-cli".into(), "crates/stella-tui".into()],
                },
            ],
        };
        assert_eq!(
            domains.domains_for_path("crates/stella-model/src/zai.rs"),
            vec!["llm".to_string()]
        );
        assert_eq!(
            domains.domains_for_path("crates/stella-tui/src/lib.rs"),
            vec!["cli".to_string()]
        );
        // Prefix must be segment-aligned: stella-model-extras is NOT under
        // stella-model.
        assert!(
            domains
                .domains_for_path("crates/stella-model-extras/src/lib.rs")
                .is_empty()
        );
        assert!(domains.domains_for_path("docs/README.md").is_empty());
    }

    #[test]
    fn parse_tolerates_prose_and_code_fences() {
        let text = "Here you go:\n```json\n[{\"name\": \"api\", \"description\": \"routes\", \
                    \"paths\": [\"src/api\"]}]\n```\nHope that helps!";
        let parsed = parse_domains_json(text).expect("parses");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "api");
    }

    #[test]
    fn heuristic_fallback_derives_domains_from_directories() {
        let root = temp_root("heuristic");
        std::fs::create_dir_all(root.join("api")).expect("mkdir");
        std::fs::create_dir_all(root.join("web_app")).expect("mkdir");
        std::fs::create_dir_all(root.join("node_modules")).expect("mkdir");
        std::fs::create_dir_all(root.join(".git")).expect("mkdir");

        let domains = heuristic_domains(&root);
        let names = domains.names();
        assert!(names.contains(&"api".to_string()));
        assert!(names.contains(&"web-app".to_string()), "{names:?}");
        assert!(!names.iter().any(|n| n.contains("node_modules")));
        assert_eq!(domains.inferred_by, "heuristic");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn summarize_repo_is_bounded_and_names_structure() {
        let root = temp_root("summary");
        std::fs::create_dir_all(root.join("src/routes")).expect("mkdir");
        std::fs::write(root.join("Cargo.toml"), "[package]").expect("write");
        std::fs::write(root.join("README.md"), "# My project\nDoes things.").expect("write");
        let summary = summarize_repo(&root);
        assert!(summary.contains("src/routes"));
        assert!(summary.contains("Has manifest: Cargo.toml"));
        assert!(summary.contains("My project"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The witness for #3102 finding 3, decision half: the inference-skip
    /// gate reuses a model-inferred taxonomy exactly when the repo shape it
    /// was derived from is unchanged — and in no other case.
    #[test]
    fn cached_taxonomy_reuses_only_a_model_taxonomy_with_a_matching_fingerprint() {
        let root = temp_root("cached");
        std::fs::create_dir_all(root.join("api")).expect("mkdir");
        let summary = summarize_repo(&root);

        // Absent file → re-infer.
        assert!(cached_taxonomy(&root, &summary).is_none());

        // Model-inferred with the matching fingerprint → reused.
        let mut domains = Domains {
            version: 1,
            inferred_by: "model".into(),
            source_fingerprint: Some(repo_shape_fingerprint(&summary)),
            domains: vec![Domain {
                name: "api".into(),
                description: String::new(),
                paths: vec!["api".into()],
            }],
        };
        domains.save(&root).expect("save");
        assert_eq!(cached_taxonomy(&root, &summary).as_ref(), Some(&domains));

        // The repo's shape changes → the fingerprint no longer matches.
        std::fs::create_dir_all(root.join("billing")).expect("mkdir");
        let changed = summarize_repo(&root);
        assert!(cached_taxonomy(&root, &changed).is_none());

        // A heuristic taxonomy never arms the gate, fingerprint or not — a
        // run with a provider must upgrade it.
        domains.inferred_by = "heuristic".into();
        domains.source_fingerprint = Some(repo_shape_fingerprint(&changed));
        domains.save(&root).expect("save");
        assert!(cached_taxonomy(&root, &changed).is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The witness for #3102 finding 3, stamping half: a successful model
    /// inference records the fingerprint of the summary it read, which is
    /// what makes the next run's skip possible at all.
    #[tokio::test]
    async fn a_model_inference_stamps_the_repo_shape_fingerprint() {
        let root = temp_root("stamp");
        std::fs::create_dir_all(root.join("api")).expect("mkdir");
        let provider = RepairProvider {
            responses: tokio::sync::Mutex::new(vec![
                r#"[{"name":"api","description":"routes","paths":["api"]}]"#.into(),
            ]),
        };

        let (domains, _cost) = infer_domains(&provider, &root, "paid-domains-model", None).await;

        assert_eq!(domains.inferred_by, "model");
        assert_eq!(
            domains.source_fingerprint,
            Some(repo_shape_fingerprint(&summarize_repo(&root))),
            "the stamped fingerprint must be the one the skip gate will compare"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn repair_attempt_cannot_apply_output_after_aggregate_budget_is_exceeded() {
        let root = temp_root("repair-budget");
        std::fs::create_dir_all(root.join("fallback-domain")).expect("mkdir");
        let provider = RepairProvider {
            responses: tokio::sync::Mutex::new(vec![
                "not json".into(),
                r#"[{"name":"model-domain","description":"model","paths":["fallback-domain"]}]"#
                    .into(),
            ]),
        };

        let (domains, cost_usd) =
            infer_domains(&provider, &root, "paid-domains-model", Some(0.01)).await;

        assert_eq!(cost_usd, 0.012, "both settled calls remain attributable");
        assert_eq!(domains.inferred_by, "heuristic");
        assert!(
            !domains.names().contains(&"model-domain".to_string()),
            "over-budget repair output must not be applied"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
