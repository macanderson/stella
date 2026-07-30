//! The construction sequence, once.
//!
//! # Why this crate exists
//!
//! `stella-cli` is a bin-only crate — no `[lib]` target — so nothing inside it
//! is callable from anywhere else. That one manifest fact is the whole reason
//! the same nineteen-step engine setup got re-typed at seven call sites
//! (`agent.rs` twice, `agent/goal.rs` twice, `command_deck.rs`, `fleet_cmd.rs`
//! and `subsession.rs`): there was no shared home for it, and each new driver
//! copied the nearest existing one. The duplication is a maintenance tax
//! today — a fix to the ordering has to be applied seven times, and has been
//! missed before — and a hard blocker tomorrow, because `stella-serve` cannot
//! link a binary.
//!
//! This crate is that shared home. It is deliberately the *bottom* half of the
//! stack only: resources, not ports. See [`SessionRuntime`] for where the line
//! falls and why.
//!
//! # The invariant
//!
//! **Nothing in this crate reads the process environment or the current
//! directory.** Every ambient switch the CLI consults — `STELLA_NO_SETTINGS`,
//! the enterprise process-free authority flag, `HOME` — is a field on
//! [`RuntimeSpec`] instead, filled in by the caller. That is what makes N
//! concurrent sessions with N different workspace roots and N different trust
//! postures sound in one process, which is the thing a server needs and a CLI
//! never did.
//!
//! `stella-fleet` already proved the resource layer itself is safe N-up: the
//! three SQLite stores are all path-injected and nothing below `Config::load`
//! reads `current_dir()`. The leak was never the resources — it was the
//! handful of global switches read *around* them. Naming them as struct
//! fields closes it. The rule is enforced executably by
//! `tests/no_ambient_reads.rs`, not just asserted here.
//!
//! # Usage
//!
//! ```no_run
//! # async fn example(spec: stella_runtime::RuntimeSpec) -> Result<(), stella_runtime::RuntimeError> {
//! let runtime = stella_runtime::RuntimeBuilder::new(spec).build().await?;
//! for notice in &runtime.notices {
//!     eprintln!("  ! {}", notice.message);
//! }
//! # Ok(())
//! # }
//! ```

pub mod error;
pub mod parts;
pub mod session;
pub mod spec;

pub use error::RuntimeError;
pub use parts::{budget_guard, build_provider, open_store, seed_calibration, tool_registry};
pub use session::{RuntimeBuilder, SessionRuntime};
pub use spec::{Notice, NoticeSubject, Persistence, ProviderParts, RuntimeSpec};
