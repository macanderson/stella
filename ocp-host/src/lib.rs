//! `ocp-host` — the Open Context Protocol host runtime.
//!
//! An OCP **host** is the side of the protocol that asks for context: it
//! discovers providers, negotiates capabilities, routes a
//! [`ContextQuery`](ocp_types::ContextQuery) to the ones that can answer,
//! budgets and cites what comes back, and gates what may leave the machine.
//! `stella-context` embeds this crate to serve every source — built-in or
//! external — through one interface, and it is deliberately usable by *any*
//! Rust agent that wants OCP support (`02-architecture.md` §2).
//! `docs/specs/stella-rust-cli/06-context-protocol.md` is the normative
//! specification; every module cites the section it implements.
//!
//! # Shape
//!
//! - [`Envelope`] + [`wire`] — the versioned NDJSON message envelope and its
//!   framing (§3.1). Version mismatch is a named error, never a hang.
//! - [`ContextProvider`] — the one trait every source implements, whether
//!   in-process, a stdio child, or a remote HTTP endpoint (§3.2, §3.3).
//! - [`StdioProvider`] / [`RawStdioConnection`] — child-process transport
//!   with scrubbed-environment isolation and process-group teardown (§3.5).
//! - [`HttpProvider`] — remote streamable-HTTP transport (§3.2).
//! - [`ConsentStore`] — the gate that keeps an egress provider un-queried
//!   until the user consents, naming what leaves (§3.5).
//! - [`Host`] — registers all three provider kinds behind one handle and
//!   [`Host::query_all`] fans a query out concurrently, enforcing timeouts,
//!   consent, and budget honesty (§2.3, §7).
//!
//! # Isolation invariants (`06-context-protocol.md` §3.5)
//!
//! Providers are quarantined: child processes inherit no credentials and no
//! ambient workspace filesystem access — a provider sees exactly the query
//! payload and what it indexed through its own declared inputs. An `egress`
//! provider is never auto-enabled. Frame content is untrusted data; this
//! crate only ever *transports* it — it never executes frame content, and a
//! host composing frames into a prompt must delimit them as quoted material.

pub mod consent;
pub mod error;
pub mod host;
pub mod http;
pub mod provider;
pub mod stdio;
pub mod wire;

pub use consent::{ConsentRecord, ConsentStore};
pub use error::HostError;
pub use host::{FanOut, Host, ProviderOutcome, ProviderResult};
pub use http::HttpProvider;
pub use provider::{ContextProvider, capability_matches, frame_kind_name};
pub use stdio::{RawStdioConnection, StdioProvider};
pub use wire::{Envelope, decode_line, encode_line, envelope_kind, versions_compatible};

/// The OCP protocol version this host speaks, re-exported from `ocp-types`
/// (`06-context-protocol.md` §3).
pub use ocp_types::PROTOCOL_VERSION;
