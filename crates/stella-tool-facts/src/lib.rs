// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Facts about the tool surface. No executor is attached.
//!
//! A screen that draws tools asks what the executor asks. Which tools exist.
//! What each one risks. Which ones an operator turned off. What must never
//! reach a child process. Each answer is data, or a pure test over data.
//!
//! The registry is not. Nor is the dispatch gate, the MCP client, or the code
//! graph. Those stay in `stella-tools`.
//!
//! This crate is the half a screen may take. It takes one workspace crate,
//! and that crate is types alone. So a screen can read the tool table and
//! link no executor. It links no code graph either. That saves nine grammar
//! builds and a bundled database.
//!
//! `stella-tools` re-exports each item here. The old path still works.

pub mod catalog;
pub mod policy;
pub mod readiness;
pub mod subprocess_env;
