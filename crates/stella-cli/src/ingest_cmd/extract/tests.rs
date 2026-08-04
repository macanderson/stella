//! Tests for the deterministic half of extraction: the claim→proposal mapping,
//! the gate/probe integration, and the pure helpers. The model call itself is
//! not exercised here — `build_proposal` takes a already-parsed [`Claim`], so
//! every safety decision is testable without a provider.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use stella_core::context_record::RecordProposalStatus;
use stella_core::ingest::{ContextFile, ProbeKind, Verdict};

use stella_protocol::{
    CompletionRequestRef, CompletionResult, CompletionUsage, FinishReason, ProviderError,
};

use super::*;

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("stella-ingest-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn claim(json: serde_json::Value) -> Claim {
    serde_json::from_value(json).expect("valid claim")
}

fn defaults() -> Defaults {
    Defaults {
        sharing_scope: Some(stella_core::ingest::SharingScope::Repository),
        origin: Some(Origin::Imported),
        status: Some(stella_core::context_record::RecordStatus::Active),
        review_every: None,
        provenance: None,
    }
}

fn build(root: &Path, json: serde_json::Value) -> Proposal {
    build_proposal(
        root,
        "acme.web",
        &defaults(),
        "2026-07-27T09:00:00Z",
        "explicit_instruction",
        claim(json),
    )
}

#[test]
fn a_constraint_with_a_guard_is_eligible_stamped_and_supported() {
    let root = temp_root("constraint");
    std::fs::write(root.join("pnpm-lock.yaml"), "lock").expect("write");

    let proposal = build(
        &root,
        serde_json::json!({
            "lineage_suffix": "pkg-manager",
            "kind": "constraint",
            "statement": "This repository uses pnpm exclusively; npm must not be used.",
            "force": "must",
            "enforcement_mode": "hard",
            "guard_tool": "Bash",
            "guard_deny_command": "npm *",
            "basis": "decree",
            "confidence": 95,
            "probe": { "kind": "path_exists", "path": "pnpm-lock.yaml" }
        }),
    );

    assert_eq!(proposal.status, RecordProposalStatus::Eligible);
    assert!(proposal.dismissed_reason.is_none());
    // Content-derived identity is stamped.
    let record_id = proposal.record.record_id.as_deref().expect("stamped id");
    assert!(record_id.starts_with("rec_acme_web_pkg_manager_"));
    assert!(proposal.candidate_id.starts_with("pkg-manager-"));
    assert_eq!(proposal.record.origin, Some(Origin::Imported));
    // The probe ran and supported the claim.
    let refutation = proposal.refutation.as_ref().expect("refutation");
    assert_eq!(refutation.verdict, Verdict::Supported);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_stale_claim_is_refuted_but_still_eligible_for_review() {
    let root = temp_root("stale");
    std::fs::write(root.join(".nvmrc"), "22.11.0\n").expect("write");

    let proposal = build(
        &root,
        serde_json::json!({
            "lineage_suffix": "node-version",
            "kind": "constraint",
            "statement": "All development happens on Node 20.x.",
            "basis": "decree",
            "confidence": 90,
            "probe": { "kind": "file_contains", "path": ".nvmrc", "pattern": "20." }
        }),
    );

    // The gate passes it (atomic, no executable), but the probe refutes it.
    assert_eq!(proposal.status, RecordProposalStatus::Eligible);
    let refutation = proposal.refutation.as_ref().expect("refutation");
    assert_eq!(refutation.verdict, Verdict::Refuted);
    assert_eq!(refutation.recommend.as_deref(), Some("ignore"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn executable_content_is_quarantined_and_dismissed() {
    let root = temp_root("exec");
    let proposal = build(
        &root,
        serde_json::json!({
            "lineage_suffix": "reset-env",
            "kind": "procedure",
            "statement": "A broken local environment is reset by clearing node_modules.",
            "executable": "rm -rf node_modules && pnpm install --force"
        }),
    );

    assert_eq!(proposal.status, RecordProposalStatus::Dismissed);
    assert_eq!(
        proposal.dismissed_reason.as_deref(),
        Some("quarantined_executable")
    );
    let quarantine = proposal.quarantine.as_ref().expect("quarantine");
    assert_eq!(
        quarantine.raw,
        "rm -rf node_modules && pnpm install --force"
    );
    // A dismissed proposal carries no refutation — its finding is the quarantine.
    assert!(proposal.refutation.is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_compound_claim_is_dismissed_for_re_extraction() {
    let root = temp_root("compound");
    let proposal = build(
        &root,
        serde_json::json!({
            "lineage_suffix": "deploy",
            "kind": "procedure",
            "statement": "Deploys run from main, happen on Tuesdays, and are triggered with make ship.",
            "compound": true
        }),
    );
    assert_eq!(proposal.status, RecordProposalStatus::Dismissed);
    assert_eq!(proposal.dismissed_reason.as_deref(), Some("compound_claim"));
    assert!(proposal.validation.is_some());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_gated_probe_is_stripped_and_the_claim_is_unfalsifiable() {
    let root = temp_root("gated");
    let proposal = build(
        &root,
        serde_json::json!({
            "lineage_suffix": "healthz",
            "kind": "fact",
            "statement": "The API answers on /healthz.",
            "probe": { "kind": "http_ok", "path": "https://evil.example/healthz" }
        }),
    );
    // The gated probe never reaches the record.
    let truth = proposal.record.truth.as_ref().expect("truth");
    assert!(truth.probe.is_none(), "a gated probe must be stripped");
    // With no honored probe, the claim is honestly unfalsifiable.
    let refutation = proposal.refutation.as_ref().expect("refutation");
    assert_eq!(refutation.verdict, Verdict::Unfalsifiable);
    assert_eq!(refutation.probe_kind, ProbeKind::None);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn parse_claims_tolerates_prose_and_fences() {
    let text = "Sure:\n```json\n[{\"lineage_suffix\":\"x\",\"kind\":\"fact\",\"statement\":\"y.\"}]\n```\n";
    let claims = parse_claims(text).expect("parses");
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].kind, RecordKind::Fact);
}

#[test]
fn org_repo_is_parsed_from_ssh_and_https_remotes() {
    assert_eq!(
        org_repo_from_remote("git@github.com:acme/web.git").as_deref(),
        Some("acme.web")
    );
    assert_eq!(
        org_repo_from_remote("https://github.com/acme/web").as_deref(),
        Some("acme.web")
    );
    assert_eq!(org_repo_from_remote("not-a-remote"), None);
}

#[test]
fn slugs_are_lowercase_dashed_and_collapse_runs() {
    assert_eq!(kebab("Package Manager!!"), "package-manager");
    assert_eq!(kebab("docs/context pr.md"), "docs-context-pr.md");
    assert_eq!(kebab_dots("acme/Web"), "acme-web");
}

#[test]
fn candidate_id_uses_the_hash_prefix_and_falls_back_to_the_slug() {
    assert_eq!(
        candidate_id("pkg-manager", Some("sha256:a41f9c2b71d4e603")),
        "pkg-manager-a41f9c2b"
    );
    assert_eq!(candidate_id("pkg-manager", None), "pkg-manager");
}

/// A provider that returns a fixed reply, so the whole extraction path —
/// model call, gate, probes, TOML write — runs offline.
struct CannedProvider {
    reply: String,
}

#[async_trait]
impl Provider for CannedProvider {
    fn id(&self) -> &str {
        "canned"
    }

    async fn complete_ref(
        &self,
        _request: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        Ok(CompletionResult {
            text: self.reply.clone(),
            tool_calls: Vec::new(),
            usage: CompletionUsage {
                reported: true,
                input_tokens: 20,
                output_tokens: 8,
                ..CompletionUsage::default()
            },
            model: "canned-model".into(),
            cost_usd: 0.001,
            finish_reason: None,
        })
    }
}

/// A provider whose first reply is cut off at the token limit (`finish_reason:
/// Length`, no closing `]`) and whose second reply is the same records,
/// complete — the shape a real truncation-then-retry takes. Records the
/// `max_output_tokens` each call was asked for, so the test can prove the
/// retry actually grew the budget rather than only asking more politely.
struct TruncatesOnceProvider {
    calls: AtomicUsize,
    requested_budgets: std::sync::Mutex<Vec<Option<u32>>>,
}

#[async_trait]
impl Provider for TruncatesOnceProvider {
    fn id(&self) -> &str {
        "truncates-once"
    }

    async fn complete_ref(
        &self,
        request: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        self.requested_budgets
            .lock()
            .expect("lock")
            .push(request.max_output_tokens);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let (text, finish_reason) = if call == 0 {
            // Cut off mid-object: no closing `}` for the record, no closing `]`
            // for the array — exactly what a token-limit truncation leaves.
            (
                "[{\"lineage_suffix\":\"pkg-manager\",\"kind\":\"fact\",\"statement\":\"Th"
                    .to_string(),
                Some(FinishReason::Length),
            )
        } else {
            (
                "[{\"lineage_suffix\":\"pkg-manager\",\"kind\":\"fact\",\
                 \"statement\":\"This repository uses pnpm.\"}]"
                    .to_string(),
                Some(FinishReason::Stop),
            )
        };
        Ok(CompletionResult {
            text,
            tool_calls: Vec::new(),
            usage: CompletionUsage {
                reported: true,
                input_tokens: 20,
                output_tokens: 8,
                ..CompletionUsage::default()
            },
            model: "canned-model".into(),
            cost_usd: 0.001,
            finish_reason,
        })
    }
}

#[test]
fn a_truncated_reply_is_retried_with_a_larger_budget_instead_of_failing() {
    let root = temp_root("truncated-retry");
    let provider = TruncatesOnceProvider {
        calls: AtomicUsize::new(0),
        requested_budgets: std::sync::Mutex::new(Vec::new()),
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let (claims, _cost) = runtime
        .block_on(call_model(
            &provider,
            "canned-model",
            &root,
            "AGENTS.md",
            "# Conventions\nThis repository uses pnpm.\n",
        ))
        .expect("recovers from the truncated first reply");

    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].lineage_suffix, "pkg-manager");

    let budgets = provider.requested_budgets.lock().expect("lock").clone();
    assert_eq!(
        budgets,
        vec![Some(BASE_OUTPUT_TOKENS), Some(BASE_OUTPUT_TOKENS * 2)],
        "the retry must ask for more room, not repeat the same budget"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn extraction_writes_a_valid_proposal_file_end_to_end() {
    let root = temp_root("e2e");
    // So the pkg-manager probe has something to confirm.
    std::fs::write(root.join("pnpm-lock.yaml"), "lock").expect("write lock");

    // A model reply wrapped in prose, with one eligible constraint (guarded,
    // probed) and one procedure carrying an executable to be quarantined.
    let reply = "Here are the records:\n[\n  {\"lineage_suffix\":\"pkg-manager\",\
        \"kind\":\"constraint\",\"statement\":\"This repository uses pnpm exclusively.\",\
        \"force\":\"must\",\"enforcement_mode\":\"hard\",\"guard_tool\":\"Bash\",\
        \"guard_deny_command\":\"npm *\",\"basis\":\"decree\",\"confidence\":95,\
        \"probe\":{\"kind\":\"path_exists\",\"path\":\"pnpm-lock.yaml\"}},\n  \
        {\"lineage_suffix\":\"reset\",\"kind\":\"procedure\",\
        \"statement\":\"Reset the environment by clearing node_modules.\",\
        \"executable\":\"rm -rf node_modules\"}\n]";
    let provider = CannedProvider {
        reply: reply.to_string(),
    };
    let doc = NamedDoc {
        rel: "CLAUDE.md".to_string(),
        content: "# Conventions\nThis repository uses pnpm.\n".to_string(),
        tier: Tier::Primary,
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let summary = runtime
        .block_on(extract_document(
            &provider,
            "canned-model",
            &root,
            &doc,
            "acme.web",
            "ing_test",
            "2026-07-27T09:00:00Z",
        ))
        .expect("extraction succeeds");

    assert_eq!(summary.total, 2);
    assert_eq!(summary.eligible, 1);
    assert_eq!(summary.dismissed, 1);

    // The file is on disk and is valid TOML in the surface shape.
    let written = root.join(&summary.written_to);
    let body = std::fs::read_to_string(&written).expect("proposals written");
    let parsed: ContextFile = toml::from_str(&body).expect("valid TOML surface");
    assert_eq!(parsed.schema, stella_core::ingest::SCHEMA_TAG);
    assert_eq!(parsed.proposals.len(), 2);

    // The eligible constraint was probed and supported; the procedure was
    // quarantined for carrying an executable.
    let pkg = parsed
        .proposals
        .iter()
        .find(|p| p.candidate_id.starts_with("pkg-manager"))
        .expect("pkg-manager proposal");
    assert_eq!(pkg.status, RecordProposalStatus::Eligible);
    assert_eq!(
        pkg.refutation.as_ref().expect("refutation").verdict,
        Verdict::Supported
    );
    assert!(pkg.record.record_id.is_some(), "eligible record is stamped");

    assert!(
        parsed
            .proposals
            .iter()
            .any(|p| p.dismissed_reason.as_deref() == Some("quarantined_executable")),
        "the executable procedure must be quarantined"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn defaults_map_kinds_to_forces_bases_and_modes() {
    assert_eq!(default_force(RecordKind::Constraint), Force::Must);
    assert_eq!(default_force(RecordKind::Fact), Force::Info);
    assert_eq!(default_basis(RecordKind::Fact), TruthBasis::Measured);
    assert_eq!(default_basis(RecordKind::Constraint), TruthBasis::Decree);
    // A guard forces hard enforcement regardless of kind.
    let guarded = claim(serde_json::json!({
        "lineage_suffix": "x", "kind": "constraint", "statement": "s.",
        "guard_deny_command": "npm *"
    }));
    assert_eq!(
        default_mode(RecordKind::Constraint, &guarded),
        EnforcementMode::Hard
    );
    let plain = claim(serde_json::json!({
        "lineage_suffix": "x", "kind": "fact", "statement": "s."
    }));
    assert_eq!(
        default_mode(RecordKind::Fact, &plain),
        EnforcementMode::None
    );
}

#[test]
fn a_built_proposal_serializes_to_the_toml_surface() {
    let root = temp_root("toml");
    let proposal = build(
        &root,
        serde_json::json!({
            "lineage_suffix": "pr-descriptions",
            "kind": "preference",
            "statement": "A PR description's first paragraph states why the change exists.",
            "force": "should"
        }),
    );
    let mut file = ContextFile::new_ingest("acme.web", "ing_test", defaults());
    file.proposals.push(proposal);

    let body = toml::to_string_pretty(&file).expect("serialize");
    let parsed: ContextFile = toml::from_str(&body).expect("round-trip");
    assert_eq!(parsed.proposals.len(), 1);
    assert_eq!(parsed.schema, stella_core::ingest::SCHEMA_TAG);
    let _ = std::fs::remove_dir_all(&root);
}
