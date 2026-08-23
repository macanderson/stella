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
//! # What drives it today: only the parts
//!
//! `stella-cli` calls the [`parts`] free functions directly, and nothing in the
//! workspace constructs the [`RuntimeBuilder`]/[`SessionRuntime`] composite —
//! `stella-serve` does not depend on this crate at all. The paragraph above is
//! the shape the composite is built to, not a path in use. That is gap **G1**
//! in `doc:engine-embedding` §4 (#3731); closing it means serve assembling its
//! resource half through [`RuntimeBuilder::with_provider`] rather than by hand.
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
//! # The other thing that lives here: the wrapper socket
//!
//! [`wrapper`] is the turn-loop wrapper contract (#3380, `doc:wrapper-socket`).
//! It is here for the same reason the construction sequence is: `before_turn`
//! performs recall and `after_turn` spawns processes, which invariant 2 forbids
//! in `stella-core`, and this crate already owns the layer above the engine
//! while reading no ambient environment. `judge` and `again` are free functions
//! beside the trait — synchronous, total and I/O-free — so no plugin, in any
//! language, can make a verdict a model call.
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
pub mod wrapper;

pub use error::RuntimeError;
pub use parts::{budget_guard, build_provider, open_store, seed_calibration};
pub use session::{RuntimeBuilder, SessionRuntime};
pub use spec::{Notice, NoticeSubject, Persistence, ProviderParts, RuntimeSpec};
pub use wrapper::{
    AdmittedContribution, AdmittedWrapper, DispatchReport, DrivenTurn, InProcessWrapper,
    RoundInput, SubprocessWrapper, TurnDriver, TurnPrelude, TurnWrapper, WrapperDispatch,
    WrapperError, WrapperHandler, admissible, again, judge, refuses_env_name,
};
