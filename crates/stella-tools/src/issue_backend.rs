//! Where a [`crate::ToolRegistry`]'s issue-tracker backend comes from.
//!
//! Detecting it ambiently costs a `gh auth status` subprocess and several
//! environment reads ([`crate::issues::detect_issue_backend`]). Doing that
//! inside a constructor made it a cost no caller opted into, and defeated
//! `stella-runtime`'s documented guarantee that nothing it builds reads the
//! process environment: `RuntimeBuilder::build` reached
//! `ToolRegistry::new_detected` one call below the crate the guarantee is
//! scoped to, so two sessions in one process could disagree about ambient
//! state — the exact property that crate exists to prevent (#1596).
//!
//! So detection is a **port** (invariant 1) rather than a concretion, and the
//! default is [`IssueBackendSource::None`]: construction reads no environment
//! and spawns no process unless a caller asks for it in as many words. The
//! ambient implementation still lives here in `stella-tools`, where I/O is
//! allowed — what changed is that reaching it is now a decision at the call
//! site, not a property of having called a constructor.

use std::sync::Arc;

use crate::issues::IssueBackend;

/// A host probe answering "which issue tracker is this machine signed in to?".
///
/// Deliberately **blocking**, not `async`. The two registry constructors have
/// to stay behaviorally aligned, and a blocking method is the shape both can
/// honour exactly: the sync constructor calls it directly, and the async one
/// routes it through the blocking pool because the `gh` probe costs tens to
/// hundreds of milliseconds and must not stall a runtime worker (#64). An
/// `async` trait would leave the sync path unable to call it at all.
pub trait IssueBackendProbe: Send + Sync + 'static {
    /// Answer from the host. May spawn processes and read the environment;
    /// returns `None` when no tracker is configured.
    fn detect(&self) -> Option<IssueBackend>;
}

/// The probe that reads the host: `LINEAR_API_KEY`, the stored OAuth
/// connections, then ambient `gh auth status`.
///
/// This is the behaviour every constructor had before #1596. It is preserved
/// exactly — a caller that installs this gets the old semantics — and it is
/// now the thing a caller has to name.
pub struct AmbientIssueBackendProbe;

impl IssueBackendProbe for AmbientIssueBackendProbe {
    fn detect(&self) -> Option<IssueBackend> {
        crate::issues::detect_issue_backend()
    }
}

/// How a registry learns its issue backend.
#[derive(Clone, Default)]
pub enum IssueBackendSource {
    /// No tracker. Registers no issue tools, asks the host nothing.
    #[default]
    None,
    /// The caller already knows. Used as-is, with no probe of any kind — the
    /// shape a host that carries its own credentials (a serve sidecar, a
    /// test) wants.
    Declared(IssueBackend),
    /// Ask the host at construction time.
    Probe(Arc<dyn IssueBackendProbe>),
}

impl IssueBackendSource {
    /// The ambient probe, as an explicit opt-in.
    #[must_use]
    pub fn ambient() -> Self {
        Self::Probe(Arc::new(AmbientIssueBackendProbe))
    }

    /// Resolve on the current thread.
    ///
    /// `process_free` gates the **probe** and nothing else. A declaration
    /// involves no subprocess, so withholding it would withhold a credential
    /// the host handed us on the grounds that a *different* mechanism spawns
    /// processes — and `with_backends_and_options` has always accepted an
    /// explicit backend under the same isolation.
    #[must_use]
    pub fn resolve(&self, process_free: bool) -> Option<IssueBackend> {
        match self {
            Self::None => None,
            Self::Declared(backend) => Some(backend.clone()),
            Self::Probe(_) if process_free => None,
            Self::Probe(probe) => probe.detect(),
        }
    }

    /// Resolve without blocking the async executor (#64).
    ///
    /// Only the probe arm reaches the blocking pool; the other two are
    /// already answers.
    pub async fn resolve_async(&self, process_free: bool) -> Option<IssueBackend> {
        match self {
            Self::None => None,
            Self::Declared(backend) => Some(backend.clone()),
            Self::Probe(_) if process_free => None,
            Self::Probe(probe) => {
                let probe = Arc::clone(probe);
                tokio::task::spawn_blocking(move || probe.detect())
                    .await
                    .unwrap_or(None)
            }
        }
    }
}

impl crate::RegistryOptions {
    /// Default options that *do* probe the host for an issue backend.
    ///
    /// The opt-in in one call, for the surfaces whose whole job is to mirror
    /// what a real session on this machine would build.
    #[must_use]
    pub fn ambient() -> Self {
        Self {
            issue_backend: IssueBackendSource::ambient(),
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A probe that fails the test if the registry ever consults the host.
    struct Forbidden;

    impl IssueBackendProbe for Forbidden {
        fn detect(&self) -> Option<IssueBackend> {
            panic!("the host must not be probed when the caller declared a backend");
        }
    }

    fn declared() -> IssueBackend {
        IssueBackend::Linear {
            api_key: "witness".to_string(),
            api_url: "https://example.invalid/graphql".to_string(),
        }
    }

    /// **Witness (#1596).** The default source asks the host nothing.
    ///
    /// Before this, `ToolRegistry::new`/`new_detected` reached the ambient
    /// probe unconditionally — there was no source to default, so a caller
    /// wanting a registry without a `gh auth status` subprocess in its
    /// constructor had no way to say so.
    #[test]
    fn the_default_source_probes_nothing() {
        let source = IssueBackendSource::default();
        assert!(
            matches!(source, IssueBackendSource::None),
            "construction must be inert until a caller opts in"
        );
        assert!(source.resolve(false).is_none());
    }

    /// A declaration is used as-is; the probe is never consulted.
    ///
    /// `Forbidden` panics if reached, so this cannot pass by coincidence.
    #[test]
    fn a_declaration_is_taken_without_asking_the_host() {
        let resolved = IssueBackendSource::Declared(declared()).resolve(false);
        assert!(matches!(resolved, Some(IssueBackend::Linear { .. })));

        // Same shape, but with a probe installed that would abort the test.
        let forbidden = IssueBackendSource::Probe(Arc::new(Forbidden));
        assert!(
            forbidden.resolve(true).is_none(),
            "process-free isolation withholds the probe rather than running it"
        );
    }

    /// A declaration survives process-free isolation; a probe does not.
    ///
    /// The distinction is the whole reason `resolve` takes the flag: isolation
    /// is about subprocess authority, and a credential the host handed us
    /// spawns nothing.
    #[test]
    fn process_free_gates_the_probe_and_not_the_declaration() {
        assert!(
            IssueBackendSource::Declared(declared())
                .resolve(true)
                .is_some(),
            "a declared backend involves no subprocess and must survive isolation"
        );
        assert!(
            IssueBackendSource::Probe(Arc::new(Forbidden))
                .resolve(true)
                .is_none()
        );
    }
}
