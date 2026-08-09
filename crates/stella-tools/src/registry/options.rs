//! Construction inputs for [`super::ToolRegistry`] — a child module of
//! `registry` (like [`super::turn_probe`]) so the registry file itself stays
//! under the size gate.

use std::sync::Arc;

/// Prerequisites and host attestations the registry needs to decide what it
/// *can* build. Deliberately NOT a policy surface: the `bash`/`web` booleans
/// that used to live here were operator policy welded into construction, and
/// they only ever covered built-ins — an MCP or custom tool never passed
/// through them. That job belongs to [`crate::policy::ToolPolicy`], enforced
/// once above the whole session tool stack.
#[derive(Clone, Default)]
pub struct RegistryOptions {
    /// Where the issue backend comes from. Defaults to
    /// [`crate::IssueBackendSource::None`], so construction spawns no process
    /// and reads no environment until a caller opts in (#1596).
    pub issue_backend: crate::IssueBackendSource,
    pub media_spend_gate: Option<Arc<dyn stella_media::MediaSpendGate>>,
    pub media_operation_ids: Option<Arc<dyn crate::media::MediaOperationIdSource>>,
    pub media_operation_journal: Option<Arc<dyn stella_media::MediaOperationJournal>>,
    pub media_requires_host_approval: bool,
    pub media_host_data_isolation: Option<crate::media::HostDataIsolation>,
    /// How the workspace probe treats paths the repository's own ignore
    /// rules exclude. The default ([`crate::shell_touch::IgnorePolicy::SkipIgnored`],
    /// the `ignore_gitignore` setting shipped on) never walks, records, or
    /// diffs them; outside a git repository nothing is ignored and the
    /// policy is inert. Not a tool switch — it configures the registry's own
    /// observation machinery, which no MCP or custom tool ever routes around.
    pub probe_ignore_policy: crate::shell_touch::IgnorePolicy,
    /// The process's diagnostic handle, when the host has one.
    ///
    /// `None` — the default — means the built-ins record nothing, which is
    /// what a library consumer and every test get unless they ask otherwise.
    /// A handle rides here rather than through a post-construction
    /// `attach_*` setter because the tools that use it are built once, in
    /// `registry::process_tools::builtins`, and a setter would have to be
    /// remembered at each of the ten sites that already call
    /// [`ToolRegistry::attach_events`](super::ToolRegistry::attach_events) —
    /// a gap that fails silently, by recording nothing, in exactly the runs
    /// this exists to explain.
    ///
    /// Only `verify_done` reads it today (#2486). It is the crate's seam for
    /// the diagnostic plane, not that tool's private channel: a built-in with
    /// a decision worth explaining takes it from here too.
    pub diagnostics: Option<Arc<stella_diag::Dx>>,
}

impl RegistryOptions {
    /// The policy the registry will actually use, once host attestations are
    /// applied to what the operator asked for.
    ///
    /// Process-free isolation outranks the setting. Consulting the
    /// repository's ignore rules means spawning `git`, and that mode's whole
    /// promise is that no built-in launches a child process — so an isolated
    /// registry walks unfiltered rather than quietly breaking the boundary
    /// the host attested to. The cost is walk time, which
    /// [`crate::shell_touch::MAX_RECORDED_TOUCHES`] already bounds; honoring
    /// the setting instead would cost the guarantee.
    pub(super) fn effective_probe_ignore_policy(
        &self,
        process_free: bool,
    ) -> crate::shell_touch::IgnorePolicy {
        if process_free {
            crate::shell_touch::IgnorePolicy::WalkAll
        } else {
            self.probe_ignore_policy
        }
    }
}
