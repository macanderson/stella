//! **Witness (#1596).** Nothing this crate builds probes the host unless the
//! spec said to.
//!
//! `tests/no_ambient_reads.rs` scans this crate's own sources, so it could not
//! see the largest leak: `RuntimeBuilder::build` reached
//! `ToolRegistry::new_detected`, which spawned `gh auth status` and read
//! `LINEAR_API_KEY` from inside a constructor. The README's guarantee — "N
//! sessions with N trust postures in one process" — was defeated one call
//! below the crate it is scoped to, and a textual scan is structurally unable
//! to notice that.
//!
//! So this is a behavioural test rather than another needle list: it drives
//! the real construction path and counts how many times the host is asked.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use stella_runtime::{Persistence, tool_registry};
use stella_tools::issues::IssueBackend;
use stella_tools::{IssueBackendProbe, IssueBackendSource, RegistryOptions};

/// Counts consultations and answers with a backend that exists nowhere on
/// this machine — so a pass cannot be a coincidence of the host's real state.
struct CountingProbe(Arc<AtomicUsize>);

impl IssueBackendProbe for CountingProbe {
    fn detect(&self) -> Option<IssueBackend> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Some(IssueBackend::Linear {
            api_key: "witness-not-a-real-key".to_string(),
            api_url: "https://example.invalid/graphql".to_string(),
        })
    }
}

fn options(source: IssueBackendSource) -> RegistryOptions {
    RegistryOptions {
        issue_backend: source,
        ..Default::default()
    }
}

/// A spec that declares nothing asks the host nothing.
///
/// Before this, the same call spawned `gh auth status` unconditionally, and no
/// value a caller could put in a `RuntimeSpec` would stop it.
#[tokio::test]
async fn a_default_spec_never_consults_the_host() {
    let root = tempfile::tempdir().expect("tempdir");

    let registry = tool_registry(
        root.path().to_path_buf(),
        options(IssueBackendSource::default()),
        Persistence::Enabled,
    )
    .await;

    // Asserted through the registered surface rather than a counter: with no
    // probe installed a counter could not move, so it would prove nothing.
    // The absence of the tools is the observable that a `gh auth status` in
    // the constructor would have made present on a developer's own machine.
    let names: Vec<String> = stella_core::ports::ToolExecutor::schemas(&registry)
        .into_iter()
        .map(|schema| schema.name)
        .collect();
    assert!(
        !names.iter().any(|name| name == "create_issue"),
        "no declared backend, so no issue tools: {names:?}"
    );
}

/// The spec's declaration is what drives detection — and it is honoured
/// exactly once, not once per tool.
#[tokio::test]
async fn a_declared_probe_is_the_only_thing_consulted() {
    let root = tempfile::tempdir().expect("tempdir");
    let calls = Arc::new(AtomicUsize::new(0));

    let registry = tool_registry(
        root.path().to_path_buf(),
        options(IssueBackendSource::Probe(Arc::new(CountingProbe(
            Arc::clone(&calls),
        )))),
        Persistence::Enabled,
    )
    .await;

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the declared probe answers once per construction"
    );
    let names: Vec<String> = stella_core::ports::ToolExecutor::schemas(&registry)
        .into_iter()
        .map(|schema| schema.name)
        .collect();
    assert!(
        names.iter().any(|name| name == "create_issue"),
        "the declared backend must register its tools: {names:?}"
    );
}

/// Persistence-disabled construction stays inert too.
///
/// This arm already bypassed detection before the change; it is asserted so a
/// later refactor that routes both arms through one call cannot quietly give
/// a claim-mode trial a host probe.
#[tokio::test]
async fn a_claim_mode_trial_probes_nothing_even_when_one_is_declared() {
    let root = tempfile::tempdir().expect("tempdir");
    let calls = Arc::new(AtomicUsize::new(0));

    let _registry = tool_registry(
        root.path().to_path_buf(),
        options(IssueBackendSource::Probe(Arc::new(CountingProbe(
            Arc::clone(&calls),
        )))),
        Persistence::Disabled,
    )
    .await;

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a session denied durable state must not reach the host either"
    );
}
