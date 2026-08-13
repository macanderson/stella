//! The `stella init` flow — domain-taxonomy resolution (with the #3102
//! inference-skip gate), the code-graph build, extension sync, and the
//! shared [`init_workspace`] entry that `stella init` and the deck's `/init`
//! command both drive.
//!
//! Progress is a transcript, not an animation (#3102): `init` on a large
//! workspace does enough work to look wedged, and a spinner is
//! indistinguishable from a wedge. Every stage narrates itself through
//! `emit` as it happens — live counts from inside the index pass, batch
//! counts from the embedding passes, and an explicit line when a model call
//! is spent or skipped — so what the user reads afterwards is a record of
//! what ran.

use super::graph::build_code_graph;
use super::*;
use crate::domains::{cached_taxonomy, summarize_repo};

/// The domain step of init, wearing the #3102 inference-skip gate: a
/// model-inferred taxonomy whose recorded repo-shape fingerprint still
/// matches the tree is reused **without a model call** — the cost the gate
/// exists to stop is re-buying an unchanged answer on every `stella init`
/// (observed: $0.034 on a run whose index pass re-parsed zero files).
/// Every other case pays: no taxonomy yet, a changed repo shape, or a
/// heuristic taxonomy a provider run should upgrade. Offline, the
/// deterministic directory heuristic answers as before.
pub(crate) async fn resolve_domains(
    provider: Option<&dyn Provider>,
    workspace_root: &std::path::Path,
    model_hint: Option<&str>,
    budget_limit: Option<f64>,
    emit: &mut dyn FnMut(String),
) -> (Domains, f64) {
    let Some(provider) = provider else {
        return (heuristic_domains(workspace_root), 0.0);
    };
    let summary = summarize_repo(workspace_root);
    if let Some(existing) = cached_taxonomy(workspace_root, &summary) {
        emit(format!(
            "✓ domain taxonomy: repo shape unchanged — reusing .stella/domains.toml \
             ({} domains, no model call; delete the file to force re-inference)",
            existing.domains.len()
        ));
        return (existing, 0.0);
    }
    let model = model_hint
        .or_else(|| provider.model())
        .unwrap_or(UNKNOWN_MODEL);
    emit(format!("◈ inferring domain taxonomy with {model}…"));
    infer_domains(provider, workspace_root, model, budget_limit).await
}

/// The shared init flow behind `stella init` and the `/init` chat command:
/// infer the domain taxonomy (model-assisted when a provider is available,
/// directory heuristic otherwise, reused without a call when the repo shape
/// is unchanged — see [`resolve_domains`]), build the code-graph index,
/// persist `.stella/domains.toml`, and record the taxonomy into the context
/// plane. Progress lines stream to `emit` — the CLI prints them, the deck
/// forwards them into the transcript — so both surfaces share one
/// implementation.
pub(crate) async fn init_workspace(
    provider: Option<&dyn Provider>,
    workspace_root: &std::path::Path,
    model_hint: Option<&str>,
    budget_limit: Option<f64>,
    emit: &mut dyn FnMut(String),
) -> Result<(Domains, f64), String> {
    let (domains, inference_cost_usd) =
        resolve_domains(provider, workspace_root, model_hint, budget_limit, emit).await;

    // The code graph needs no provider — build it regardless of how the
    // domains were inferred, so the index exists even fully offline.
    build_code_graph(workspace_root, emit).await;

    // Adopt commands/skills/agents other code agents keep in `.claude/` and
    // `.agents/` (workspace + user scope) as symlinks into stella's own
    // directories — idempotent, never clobbers, never fatal.
    crate::extensions::sync_extensions(workspace_root, emit);

    let path = domains.save(workspace_root)?;

    // Persist the taxonomy into the context plane too: domain descriptions
    // plus bi-temporal `covers_path` facts, so recall can fuse on them and a
    // re-run after the taxonomy shifts supersedes (never deletes) the old
    // beliefs. Best-effort — a store that won't open already warned.
    if let Some(m) = SessionMemory::open(workspace_root, false) {
        m.record_taxonomy(&domains).await;
    }

    emit(format!(
        "✓ {} domains ({}) → {}",
        domains.domains.len(),
        domains.inferred_by,
        path.display()
    ));
    if inference_cost_usd > 0.0 {
        emit(format!(
            "domain inference model cost: ${inference_cost_usd:.6}"
        ));
    }
    Ok((domains, inference_cost_usd))
}

/// `stella init` — infer the workspace's domain taxonomy, build the code-graph
/// index, and write `.stella/domains.toml` (see `crate::domains`). Domain
/// inference is model-assisted when a provider resolves, with a deterministic
/// directory heuristic fallback, so init always succeeds — offline included.
/// The code graph (`.stella/private/codegraph.db`) is built unconditionally: it needs
/// no provider, only the on-disk source tree.
///
/// Progress prints straight to stdout as it happens — real counts in the
/// transcript, not a loading animation (#3102 retired the init cinematic:
/// a spinner is indistinguishable from a wedge, and what it drew was erased
/// on exit, leaving nothing to read afterwards).
pub async fn run_init(
    model_override: Option<&str>,
    api_key_override: Option<&str>,
    base_url_override: Option<&str>,
) -> Result<(), String> {
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;

    plain::section_header("Stella init");

    let (provider, model_hint) =
        match Config::load(model_override, api_key_override, base_url_override) {
            Ok(cfg) => {
                let provider = build_provider(&cfg)?;
                println!(
                    "  {} provider {}/{}",
                    "◈".bright_cyan(),
                    cfg.provider.id,
                    cfg.model_id
                );
                (Some(provider), Some(cfg.model_id))
            }
            Err(_) => {
                println!(
                    "  {} no provider configured — using the directory heuristic \
                 (re-run `stella init` with a key for a better taxonomy)",
                    "!".yellow()
                );
                (None, None)
            }
        };

    let mut emit = |line: String| println!("  {line}");
    let (domains, _inference_cost_usd) = init_workspace(
        provider.as_deref(),
        &workspace_root,
        model_hint.as_deref(),
        None,
        &mut emit,
    )
    .await?;

    for domain in &domains.domains {
        println!(
            "    {} {} — {} [{}]",
            "·".dimmed(),
            domain.name.bright_magenta(),
            domain.description.dimmed(),
            domain.paths.join(", ").dimmed()
        );
    }
    println!(
        "\n  {}",
        "Domains tag memories, reflections, and every code-graph node/edge; recall uses them \
         for relevance."
            .dimmed()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use stella_protocol::{CompletionRequestRef, CompletionResult, CompletionUsage, ProviderError};

    use super::*;

    /// A provider that counts its completions — the observable the #3102
    /// skip gate is judged by.
    struct CountingProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl stella_protocol::Provider for CountingProvider {
        fn id(&self) -> &str {
            "counting"
        }

        async fn complete_ref(
            &self,
            _request: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(CompletionResult {
                upstream_provider: None,
                text: r#"[{"name":"api","description":"routes","paths":["api"]}]"#.into(),
                tool_calls: Vec::new(),
                usage: CompletionUsage {
                    reported: true,
                    input_tokens: 10,
                    output_tokens: 2,
                    ..CompletionUsage::default()
                },
                model: "counting-model".into(),
                cost_usd: 0.006,
                finish_reason: None,
            })
        }
    }

    /// The witness for #3102 finding 3, end to end: the second `stella init`
    /// over an unchanged repo shape spends **zero** model calls and says so
    /// in the transcript, while the first paid for its inference exactly
    /// once. Fails on old code, which re-bought the taxonomy on every run.
    #[tokio::test]
    async fn an_unchanged_repo_shape_spends_no_second_inference_call() {
        let root = tempfile::tempdir().expect("tempdir");
        let root = root.path().canonicalize().expect("canonicalize");
        std::fs::create_dir_all(root.join("api")).expect("mkdir");
        let provider = CountingProvider {
            calls: AtomicUsize::new(0),
        };

        let mut lines: Vec<String> = Vec::new();
        let (first, first_cost) = resolve_domains(
            Some(&provider),
            &root,
            Some("counting-model"),
            None,
            &mut |line| lines.push(line),
        )
        .await;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert!(first_cost > 0.0, "the first run pays for its inference");
        first.save(&root).expect("save");

        let (second, second_cost) = resolve_domains(
            Some(&provider),
            &root,
            Some("counting-model"),
            None,
            &mut |line| lines.push(line),
        )
        .await;

        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "an unchanged repo shape must not re-buy the taxonomy"
        );
        assert_eq!(second_cost, 0.0);
        assert_eq!(second.domains, first.domains);
        assert!(
            lines.iter().any(|l| l.contains("no model call")),
            "the skip must be stated in the transcript, not silent: {lines:?}"
        );
    }

    /// The gate never hides a changed repo from the model: grow the tree's
    /// shape and the next run re-infers (and pays) again.
    #[tokio::test]
    async fn a_changed_repo_shape_re_infers() {
        let root = tempfile::tempdir().expect("tempdir");
        let root = root.path().canonicalize().expect("canonicalize");
        std::fs::create_dir_all(root.join("api")).expect("mkdir");
        let provider = CountingProvider {
            calls: AtomicUsize::new(0),
        };

        let mut emit = |_line: String| {};
        let (first, _) = resolve_domains(
            Some(&provider),
            &root,
            Some("counting-model"),
            None,
            &mut emit,
        )
        .await;
        first.save(&root).expect("save");

        std::fs::create_dir_all(root.join("billing")).expect("mkdir");
        let (_, _) = resolve_domains(
            Some(&provider),
            &root,
            Some("counting-model"),
            None,
            &mut emit,
        )
        .await;

        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            2,
            "a changed repo shape must re-infer"
        );
    }
}
