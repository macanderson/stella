//! The one thing every `stella-cli` test that spawns the binary shares: the
//! embedder backend is removed from the child's environment before it runs.
//!
//! Not compiled as a test binary of its own — `tests/` subdirectories are
//! modules, and each harness pulls this in with `mod common;`.

use std::process::Command;

/// Remove the semantic-embedding backend from a child's environment.
///
/// # Why every spawning test needs this
///
/// A session's code-graph build warms a semantic index, and
/// `stella_embed::EmbedderEnv::from_process` resolves a *hosted* backend from
/// a bare `VOYAGE_API_KEY` or `OPENAI_API_KEY` — no base URL required. A
/// child inherits the developer's environment, so on a machine with either
/// key exported `cargo test -p stella-cli` makes live, billed calls to
/// api.voyageai.com. That is not hypothetical: it was observed during #4540,
/// which hardened its own new test and left the other sixteen (#4542).
///
/// Stripping the two key names is not enough, which is why this clears
/// `stella_embed::ENV_VARS` — the crate's whole surface, re-exported rather
/// than transcribed, so a variable added there cannot be missed here.
/// `STELLA_EMBED_URL` redirects the backend just as effectively as a key
/// selects one.
///
/// # Shape
///
/// An extension method rather than a free function so it drops into the
/// middle of the `Command::new(…).args(…).env(…)` chains these tests are
/// already written as, at any point before `.output()`/`.spawn()`.
pub trait SealsEmbedderBackend {
    /// Clear every variable `stella-embed` resolves a backend from.
    fn without_embedder_backend(&mut self) -> &mut Self;
}

impl SealsEmbedderBackend for Command {
    fn without_embedder_backend(&mut self) -> &mut Self {
        for name in stella_embed::ENV_VARS {
            self.env_remove(name);
        }
        self
    }
}
