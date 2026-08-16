//! The plugin manifest crate — a plugin's declared say in the turn loop.
//!
//! Slice A of #3245 (plugins as turn-loop participants): parse and validate
//! the manifest blocks that grade a plugin's participation — `[loop]` with
//! its monotone ladder (`none` < `observer` < `steering` < `arbiter`),
//! `[requirements]`, `[oracle]`, `[subloop]`, and `[roles]` — as pure
//! functions over borrowed text. No I/O, no environment, no workspace-crate
//! dependencies: the engine never learns plugins exist, and the host that
//! binds these grants to the engine's gates (the Stop gate, the hook runner,
//! the sub-agent primitive) lives elsewhere and merely consumes this crate's
//! answers.
//!
//! The one function a host must not bypass is [`LoopGrant::permits_hook`]:
//! it is the authoritative filter behind the epic's rule that an undeclared
//! hook is never invoked, even if the plugin's process registers for it.

mod error;
mod manifest;

pub use error::ManifestError;
pub use manifest::{
    FlipPolicy, HookEvent, LoopGrant, Oracle, OracleCommand, Participation, PluginManifest, Role,
    Subloop, TamperPolicy,
};
