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
}
