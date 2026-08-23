//! The construction sequence's own witnesses (#3733).
//!
//! `RuntimeBuilder::build` and the four `parts::*` steps are what every host
//! of this crate calls first, and until this file they were covered by
//! nothing: no in-module test in `session.rs` or `parts.rs`, and no file under
//! `tests/` naming them. `RuntimeError::WorkspaceRoot` existed at exactly two
//! sites — its declaration and its one construction — and no test asserted it.
//!
//! Every root here is an explicit `TempDir`, never the process's own
//! directory: the crate's contract is that construction is a pure function of
//! a `RuntimeSpec` (`tests/no_ambient_reads.rs`), and a test that leaned on
//! ambient state would be asserting something weaker than the contract.

use async_trait::async_trait;
use stella_core::budget::{BudgetAxis, BudgetOutcome};
use stella_protocol::{CompletionRequestRef, CompletionResult, Provider, ProviderError};
use stella_runtime::{
    Notice, NoticeSubject, Persistence, ProviderParts, RuntimeBuilder, RuntimeError, RuntimeSpec,
    budget_guard, open_store, seed_calibration,
};
use tempfile::TempDir;

/// A provider that answers nothing.
///
/// The builder never dispatches a completion, so the only thing this double
/// has to do is be distinguishable from the adapter the spec would have built
/// — which is what `id()` is for.
struct NamedProvider(&'static str);

#[async_trait]
impl Provider for NamedProvider {
    fn id(&self) -> &str {
        self.0
    }

    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        Err(ProviderError::Terminal(
            "this double exists to be identified, never called".to_string(),
        ))
    }
}

/// A spec rooted at `root`, naming an unseeded provider so no catalog lookup
/// stands between the test and the step it is about.
fn spec_at(root: &TempDir) -> RuntimeSpec {
    RuntimeSpec {
        workspace_root: root.path().to_path_buf(),
        provider: ProviderParts {
            id: "from-the-spec".to_string(),
            display_name: "From The Spec".to_string(),
            dialect: stella_model::factory::Dialect::OpenaiCompatible,
            seeded: false,
            cache_ttl: stella_model::CacheTtl::default(),
            upstream_pin: Vec::new(),
            model_id: "a-model".to_string(),
            api_key: stella_model::ApiKey::new("test-key"),
            base_url: "http://127.0.0.1:1/v1".to_string(),
            base_url_override: None,
            aux: stella_model::AuxCredentials::new(),
        },
        persistence: Persistence::Disabled,
        budget_limit_usd: None,
    }
}

#[tokio::test]
async fn a_workspace_root_that_is_not_a_directory_is_refused_before_anything_is_built() {
    let root = TempDir::new().expect("temp dir");
    let mut spec = spec_at(&root);
    spec.workspace_root = root.path().join("no-such-directory");
    let missing = spec.workspace_root.clone();

    let Err(error) = RuntimeBuilder::new(spec).build().await else {
        panic!("a nonexistent root must not assemble");
    };

    match error {
        RuntimeError::WorkspaceRoot(path) => assert_eq!(path, missing),
        other => panic!("expected WorkspaceRoot, got {other:?}"),
    }
}

#[tokio::test]
async fn a_file_is_not_a_workspace_root() {
    let root = TempDir::new().expect("temp dir");
    let file = root.path().join("not-a-directory");
    std::fs::write(&file, b"").expect("write file");
    let mut spec = spec_at(&root);
    spec.workspace_root = file.clone();

    let Err(error) = RuntimeBuilder::new(spec).build().await else {
        panic!("a file root must not assemble");
    };

    match error {
        RuntimeError::WorkspaceRoot(path) => assert_eq!(path, file),
        other => panic!("expected WorkspaceRoot, got {other:?}"),
    }
}

#[tokio::test]
async fn a_handed_in_provider_is_used_instead_of_one_built_from_the_spec() {
    let root = TempDir::new().expect("temp dir");
    let spec = spec_at(&root);
    let spec_id = spec.provider.id.clone();

    let runtime = RuntimeBuilder::new(spec)
        .with_provider(Box::new(NamedProvider("the-host's-own")))
        .build()
        .await
        .expect("assembles");

    assert_eq!(runtime.provider.id(), "the-host's-own");
    assert_ne!(runtime.provider.id(), spec_id);
}

#[tokio::test]
async fn the_built_runtime_is_rooted_and_pinned_where_the_spec_said() {
    let root = TempDir::new().expect("temp dir");
    let spec = spec_at(&root);

    let runtime = RuntimeBuilder::new(spec)
        .with_provider(Box::new(NamedProvider("the-host's-own")))
        .build()
        .await
        .expect("assembles");

    assert_eq!(runtime.workspace_root, root.path());
    assert_eq!(runtime.model_ref.provider, "from-the-spec");
    assert_eq!(runtime.model_ref.model_id, "a-model");
    assert!(runtime.store.is_none(), "persistence was disabled");
    assert!(runtime.notices.is_empty(), "the ordinary path is quiet");
}

#[test]
fn disabled_persistence_opens_no_store_and_leaves_the_workspace_untouched() {
    let root = TempDir::new().expect("temp dir");

    let (store, notice) = open_store(root.path(), Persistence::Disabled);

    assert!(store.is_none());
    assert!(notice.is_none(), "declining to open is not a degradation");
    assert!(
        !root.path().join(".stella").exists(),
        "a disabled session must not create workspace state"
    );
}

#[test]
fn a_store_that_will_not_open_degrades_to_a_notice_rather_than_a_failure() {
    let root = TempDir::new().expect("temp dir");
    // `.stella` occupied by a plain file: the state directory cannot be made,
    // so the store cannot open. Persistence is observability, not a work
    // dependency, so the session must still get its resources.
    std::fs::write(root.path().join(".stella"), b"").expect("write file");

    let (store, notice) = open_store(root.path(), Persistence::Enabled);

    assert!(store.is_none());
    let Some(Notice { subject, message }) = notice else {
        panic!("an unopenable store must say so");
    };
    assert_eq!(subject, NoticeSubject::Store);
    assert!(
        message.contains("local store unavailable"),
        "the notice names what degraded: {message}"
    );
}

#[test]
fn an_openable_store_yields_a_store_and_no_notice() {
    let root = TempDir::new().expect("temp dir");

    let (store, notice) = open_store(root.path(), Persistence::Enabled);

    assert!(store.is_some(), "a fresh workspace opens");
    assert!(notice.is_none());
}

#[test]
fn calibration_without_a_store_starts_uncorrected() {
    let calibration = seed_calibration(None, "a-provider", "a-model");

    assert_eq!(calibration.factor(Some("a-model")), 1.0);
    assert!(calibration.report().is_empty(), "nothing was seeded");
}

#[test]
fn calibration_from_a_store_with_no_samples_starts_uncorrected() {
    let root = TempDir::new().expect("temp dir");
    let (store, _) = open_store(root.path(), Persistence::Enabled);
    let store = store.expect("a fresh workspace opens");

    let calibration = seed_calibration(Some(&store), "a-provider", "a-model");

    assert_eq!(calibration.factor(Some("a-model")), 1.0);
    assert!(calibration.report().is_empty());
}

#[test]
fn no_limit_measures_spend_without_blocking_a_step() {
    let mut guard = budget_guard(None);

    assert_eq!(guard.record_spend(1_000.0), BudgetOutcome::Continue);
}

#[test]
fn a_limit_aborts_the_turn_once_it_is_passed() {
    let mut guard = budget_guard(Some(1.0));

    assert_eq!(guard.record_spend(0.5), BudgetOutcome::Continue);
    match guard.record_spend(0.75) {
        BudgetOutcome::AbortTurn {
            axis,
            spent_usd,
            limit_usd,
        } => {
            assert_eq!(axis, BudgetAxis::Session);
            assert!((spent_usd - 1.25).abs() < f64::EPSILON);
            assert!((limit_usd - 1.0).abs() < f64::EPSILON);
        }
        other => panic!("expected AbortTurn, got {other:?}"),
    }
}
