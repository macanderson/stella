//! Typed, named failures from the construction sequence.
//!
//! The CLI originals returned `Result<_, String>` — fine for a binary that
//! prints the string and exits, useless to a server that has to decide
//! whether a session-create failed because the *caller* sent a bad model slug
//! (a 400) or because the *host* is misconfigured (a 500). Naming the variants
//! is what makes that decision possible without parsing prose.

use std::path::PathBuf;

/// Why a runtime could not be assembled.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// The provider adapter could not be constructed — an unknown model slug
    /// for a seeded provider, or a dialect that rejected the given parts.
    /// Attributable to the request, not the host.
    #[error("cannot build provider `{provider}` for model `{model}`: {reason}")]
    Provider {
        /// The provider id that was asked for.
        provider: String,
        /// The model slug that was asked for.
        model: String,
        /// The factory's own explanation.
        reason: String,
    },

    /// The workspace root does not exist, or is not a directory.
    ///
    /// Checked explicitly rather than left to the first tool call: a server
    /// that accepts a session against a nonexistent root would otherwise fail
    /// deep inside a turn, after the caller has been told the session exists.
    #[error("workspace root is not an existing directory: {}", .0.display())]
    WorkspaceRoot(PathBuf),
}
