// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella-model` — the `Provider` trait plus its concrete adapters: Z.ai
//! (GLM 5.2, OpenAI-compatible chat), Anthropic (Messages API), OpenAI
//! (Responses API), Gemini direct (native generateContent), Vertex AI
//! (generateContent, enterprise auth), and Amazon Bedrock (Converse,
//! SigV4-signed).
//!
//! The seam is one port and five *wire dialects*, not one adapter per
//! vendor: the trait itself lives in `stella-protocol` (re-exported by
//! [`provider`]) so `stella-core` drives every model call through
//! `&dyn Provider` and never names a vendor. Anything that genuinely speaks
//! the OpenAI Chat Completions shape — xAI, DeepSeek, OpenRouter, a local
//! server, a settings-defined gateway — rides the ONE adapter in [`zai`],
//! re-identified through `ZaiProvider::with_identity`, so a new
//! OpenAI-compatible endpoint costs a config row rather than a module. The
//! four structurally different dialects ([`anthropic`], [`openai`],
//! [`gemini`] + [`vertex`], [`bedrock`]) each get their own adapter because
//! their request/response shapes genuinely differ, not because their vendor
//! does.
//!
//! Supporting modules, all vendor-neutral by design:
//! - [`catalog`] — the slug → model/pricing/dialect table every adapter
//!   resolves against; an unknown slug is a hard, named error, never a
//!   silent fallback.
//! - [`credential`] — the resolution chain (flag → env → file → prompt) and
//!   the non-`Display` [`ApiKey`] secret wrapper.
//! - [`provider_parity`] — the per-provider cache/reasoning posture matrix
//!   that turns "adapter folklore" into a structurally enforced table.
//! - [`cache_economics`] — prompt-cache savings arithmetic and low-hit-rate
//!   diagnosis, keyed off catalog pricing and the parity matrix.
//! - [`modelsdev`] / [`provider_listing`] — third-party master list and
//!   provider-native `/models` discovery, both fetch-and-parse only.
//! - [`sse`] — the shared, dependency-free SSE + incremental UTF-8 decoder
//!   every streaming adapter feeds.
pub mod anthropic;
pub(crate) mod attachment;
pub mod bedrock;
pub mod cache_economics;
pub mod catalog;
pub mod credential;
pub mod gemini;
pub(crate) mod http;
pub mod modelsdev;
pub mod openai;
pub mod provider;
pub mod provider_listing;
pub mod provider_parity;
pub mod sse;
pub mod vertex;
pub mod zai;

pub use cache_economics::{
    CacheWarmth, cache_write_premium_multiplier, diagnose_cache, hit_rate as cache_hit_rate,
    is_cache_expired_rewrite, provider_cache_ttl_secs,
};
pub use catalog::{Catalog, CatalogEntry, Pricing, ToolDialect};
pub use credential::{ApiKey, CredentialError};
pub use provider::Provider;
