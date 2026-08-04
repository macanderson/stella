//! The inputs to the construction sequence, as **values**.
//!
//! Every field here is something the CLI reads from the ambient process today
//! — `std::env`, `current_dir()`, a `#[cfg(test)]` thread-local, an on-disk
//! settings file. Naming them as struct fields is the whole point of this
//! crate: a builder that reads the environment can only ever build *one*
//! stack per process, and the serve sidecar needs N of them, with different
//! workspace roots and different trust postures, side by side.
//!
//! The rule this module exists to enforce: **nothing below `stella-runtime`
//! reads the process environment.** `stella-fleet` already proved the
//! resource layer is safe to run N-up (`Store::open(root)`,
//! `ContextStore::open(path)`, `CodeGraph::open(root, db)` are all
//! path-injected); the leak was never the resources, it was the handful of
//! global switches read *around* them. Those switches are the enums below.

use std::path::PathBuf;

use stella_model::{ApiKey, AuxCredentials, factory::Dialect};

/// Whether this session may touch the workspace's on-disk state at all.
///
/// The CLI derives this from `STELLA_NO_SETTINGS` (claim-mode trials run
/// isolated and ephemeral, and must not read prior telemetry or create
/// `.stella/store.db` in the task). A server derives it per session from the
/// host's trust decision. Same switch, no ambient read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Persistence {
    /// Open the workspace store and let this session's telemetry accumulate.
    Enabled,
    /// Run without durable state: no store, no calibration seed, no
    /// filesystem-backed tool backends.
    Disabled,
}

impl Persistence {
    /// `true` when durable workspace state is off — the condition
    /// `stella-cli`'s `filesystem_settings_disabled()` names.
    #[must_use]
    pub fn is_disabled(self) -> bool {
        matches!(self, Self::Disabled)
    }
}

/// The provider to build, as already-resolved parts.
///
/// Deliberately owned rather than borrowed from a `Config`: the serve sidecar
/// builds one of these per session from a request body, and the CLI builds one
/// from its loaded config. Neither shape is privileged, so neither is baked in.
///
/// `base_url` is the URL requests actually go to (override-or-default);
/// `base_url_override` is the raw `--base-url`, which only the Vertex and
/// Bedrock arms consume because they build region/project-scoped URLs
/// themselves. Both are carried because the factory distinguishes them.
#[derive(Debug, Clone)]
pub struct ProviderParts {
    /// Stable provider id — becomes the adapter's `Provider::id()`.
    pub id: String,
    /// Human label used in the OpenAI-compatible arm's error messages.
    pub display_name: String,
    /// Which adapter to construct.
    pub dialect: Dialect,
    /// Whether this provider's models are curated in the catalog seed. A
    /// seeded provider rejects an unknown slug; `local` and user-defined
    /// endpoints serve whatever the user pulled into them.
    pub seeded: bool,
    /// The model slug to run.
    pub model_id: String,
    /// The credential, already wrapped in the zeroizing newtype — cloned
    /// rather than reconstructed from a revealed string.
    pub api_key: ApiKey,
    /// Where requests go: the override if one was given, else the default.
    pub base_url: String,
    /// The raw override, if any. Only Vertex/Bedrock read this.
    pub base_url_override: Option<String>,
    /// Values this provider needs *beyond* `api_key`, already resolved by the
    /// host's own credential chain — Bedrock's AWS secret access key, optional
    /// session token, and region today. Empty for every other dialect, and
    /// empty is not an error: the Bedrock adapter falls back to the standard
    /// AWS environment variables when the host resolved nothing.
    ///
    /// Filled by the caller for the same reason every other field is: this is
    /// where the ambient reads belong. `stella-cli` walks handoff → env →
    /// `~/.stella/credentials.toml` for each name; a server fills it from its
    /// session-create request.
    pub aux: AuxCredentials,
}

impl ProviderParts {
    /// Borrow these parts as the factory's `ProviderSpec`.
    ///
    /// A method rather than a `From` impl because the factory's spec is
    /// lifetime-bound to `self` and inference reads better at the call site.
    #[must_use]
    pub fn factory_spec(&self) -> stella_model::factory::ProviderSpec<'_> {
        stella_model::factory::ProviderSpec {
            id: &self.id,
            display_name: &self.display_name,
            dialect: self.dialect,
            seeded: self.seeded,
        }
    }
}

/// Everything the construction sequence needs to assemble one session's stack.
///
/// Held by value and consumed by [`crate::RuntimeBuilder`]. Constructing this
/// is the caller's job precisely because that is where the ambient reads
/// belong: `stella-cli` fills it from a loaded `Config` plus its process
/// globals, and `stella-serve` fills it from a session-create request. The
/// builder itself is then a pure function of this struct.
///
/// No `Debug`: `RegistryOptions` does not implement it, and deriving one here
/// would also put a credential one `{:?}` away from a log line.
#[derive(Clone)]
pub struct RuntimeSpec {
    /// The workspace this session is rooted at. Every store, registry and
    /// port below is scoped to it — which is exactly why N sessions with N
    /// different roots coexist in one process.
    pub workspace_root: PathBuf,
    /// The provider to build.
    pub provider: ProviderParts,
    /// Tool-registry options, already resolved by the caller. Resolved
    /// upstream rather than here because the media-journal and host-isolation
    /// decisions are host policy, and a server's answer differs from the
    /// CLI's.
    pub registry_options: stella_tools::RegistryOptions,
    /// Whether durable workspace state is available to this session.
    pub persistence: Persistence,
    /// The per-turn USD ceiling, if the caller set one. `None` runs in
    /// observed mode: spend is measured and reported, never enforced.
    pub budget_limit_usd: Option<f64>,
}

/// A diagnostic the construction sequence produced.
///
/// Returned rather than printed. The CLI's wiring writes these to stderr with
/// its own glyphs and colors; the serve sidecar turns them into `AgentEvent`s
/// or structured log lines. A library that calls `eprintln!` has decided for
/// every one of its hosts, and one of those hosts writes machine-readable
/// JSON to the very stream it would be scribbling on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// What degraded.
    pub subject: NoticeSubject,
    /// Operator-facing detail, already formatted.
    pub message: String,
}

/// Which part of the stack a [`Notice`] is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeSubject {
    /// The workspace store would not open. Persistence is observability, not
    /// a work dependency: the session runs on without it.
    Store,
    /// Calibration could not be seeded from prior telemetry, so the token
    /// estimator starts uncorrected at factor 1.0.
    Calibration,
}
