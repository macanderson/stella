// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella-tools` — the built-in tool set the agent loop calls.
//!
//! Every tool implements [`registry::Tool`], takes a JSON input from the model,
//! and returns a [`stella_protocol::ToolOutput`] (success or a typed, named
//! error — never a bare string).
//!
//! The dispatchable surface is deliberately small: the sub-agent spawn tool
//! (`task`), the session task board (`task_create` / `task_list` /
//! `task_start` / `task_complete` / `task_cancel` / `task_assign`), the
//! session scratch state plane (`save_state` / `get_state` / `list_state` /
//! `delete_state`), and the environment report (`get_environment`). Every
//! other capability reaches the model as a developer-defined custom script
//! tool ([`custom`]), an MCP tool (`stella-mcp`), or a CLI session-layer
//! tool — never as a built-in here.
//!
//! Beside the tools, this crate owns the session tool *mechanisms*: the
//! registry and its dispatch gates ([`registry`]), the custom-tool loader and
//! its execution contract ([`custom`], [`exec`], [`subprocess_env`]), the
//! tool-foundry authorship/adoption plane ([`foundry_author`],
//! [`foundry_gate`], [`foundry_witness`]), skill tool grants
//! ([`skill_grant`]), operator tool policy ([`policy`]), and the extension
//! hook runner/bridge ([`hook_runner`], [`hook_bridge`]).

pub mod agent_use;
pub mod catalog;
pub mod contracts;
pub mod ctx;
pub mod custom;
pub mod environment;
pub mod exec;
pub mod foundry_author;
pub mod foundry_gate;
pub mod foundry_witness;
pub mod gated;
pub mod hook_bridge;
pub mod hook_runner;
pub mod input;
pub mod policy;
pub mod registry;
pub mod scratch;
pub mod skill_grant;
pub mod subagent;
/// Shared environment policy for every subprocess that can execute model- or
/// repository-controlled code. Downstream Stella crates must use this rather
/// than maintaining a second, drifting credential deny-list.
pub mod subprocess_env;
pub mod tasks;
pub mod validate;

pub use registry::ToolRegistry;
