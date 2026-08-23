//! Which providers are actually configured right now: [`ConfiguredProvider`]
//! and [`discover_configured_providers`], the credential-discovery pass the
//! goal loop's role Router (and `stella models`/`stella config`) reads.
//!
//! Split out of `config.rs` (#3566, #4494): resolving ONE provider (what
//! `config.rs` still owns) and enumerating EVERY provider that currently
//! resolves are different questions, even though the second reuses the
//! first's own credential chain ([`super::resolve_provider_key`]) rather
//! than a second implementation of it. Re-exported at the old
//! `crate::config::{ConfiguredProvider, discover_configured_providers}`
//! paths, the same as `aux_credentials`/`providers`, so nothing about where
//! a caller reaches this moved.

use std::env;

use stella_model::credential::{ApiKey, AuxCredentials, CredentialsFile};

use super::{LOCAL_PROVIDER, PROVIDERS, ProviderConfig, custom_provider, effective_builtin};

/// A provider whose BYOK credential currently resolves, paired with the
/// resolved key. Produced by [`discover_configured_providers`] and consumed
/// by the goal loop's role Router: the `config` supplies the id/family/model
/// for a `stella_core::router::ProviderProfile`, the `api_key` builds the
/// concrete verifier adapter when this provider is routed as verifier. `api_key`
/// is an [`ApiKey`] (H3) so the derived `Debug` never leaks the secret.
#[derive(Debug, Clone)]
pub struct ConfiguredProvider {
    pub config: ProviderConfig,
    pub api_key: ApiKey,
    /// The provider's auxiliary values, resolved by the same chain and from
    /// the same store as `api_key` — so a routed verifier on Bedrock builds with
    /// the credentials discovery actually verified, not a second lookup that
    /// could disagree.
    pub aux: AuxCredentials,
}

/// Enumerate every provider in [`PROVIDERS`] whose credential currently
/// resolves, in preference order, pairing each with its resolved key. Uses
/// the SAME credential chain [`super::Config::load`] uses
/// ([`super::resolve_provider_key`], non-interactively — env var / alias /
/// credentials file, never a prompt), so a provider is "configured" here iff
/// `Config` could have auto-selected it. Never fails: an unreadable
/// credentials file yields no discovered providers. Under trusted
/// handoff/no-settings isolation the filesystem store is not read at all and
/// discovery uses an empty in-memory store.
///
/// The goal loop calls this to build a role Router that can pick a
/// cross-family VERIFIER; with one configured family
/// it returns a single entry and the verifier stays the worker provider.
pub fn discover_configured_providers() -> Vec<ConfiguredProvider> {
    // A trusted handoff/no-settings process must never inspect a task-image
    // credential store. Outside that boundary, a corrupt/unreadable file
    // yields no discovery: the goal loop simply keeps the worker as verifier.
    let credentials_sealed =
        crate::credential_handoff::is_present() || crate::settings::filesystem_settings_disabled();
    let credentials_file = if credentials_sealed {
        CredentialsFile::empty()
    } else {
        let Ok(credentials_file) = CredentialsFile::load_default() else {
            return Vec::new();
        };
        credentials_file
    };
    // Same degradation posture for settings: verifier routing is best-effort,
    // so an unreadable settings.json costs the config-defined providers,
    // never the built-ins. (`Config::load` is where a bad file is loud.)
    let settings = env::current_dir()
        .ok()
        .and_then(|ws| crate::settings::Settings::load(&ws).ok())
        .unwrap_or_default();

    let mut configured: Vec<ConfiguredProvider> = PROVIDERS
        .iter()
        .filter_map(|provider| {
            let provider = effective_builtin(provider, &settings);
            let settings_key = settings
                .providers
                .get(provider.id)
                .and_then(|e| e.api_key.clone());
            super::resolve_provider_key(
                &provider,
                None,
                settings_key.as_deref(),
                &credentials_file,
                false,
            )
            .ok()
            .and_then(|(api_key, _source)| {
                // A provider whose primary key resolves but whose required
                // companion value does not cannot build — see
                // `aux::has_required_aux`. Discovery is what auto-detection
                // and verifier routing pick from, so admitting it here would
                // hand both a provider that fails at construction.
                let aux = super::provider_aux(&provider, &credentials_file);
                super::has_required_aux(&provider, &aux).then_some(ConfiguredProvider {
                    config: provider,
                    api_key,
                    aux,
                })
            })
        })
        .collect();
    for (id, entry) in &settings.providers {
        if PROVIDERS.iter().any(|p| p.id == id.as_str()) || id == LOCAL_PROVIDER.id {
            continue;
        }
        // The verifier router needs a model to route to — an entry without
        // `default_model` can't serve as a verifier.
        if entry.default_model.as_deref().unwrap_or("").is_empty() {
            continue;
        }
        let Ok(provider) = custom_provider(id, entry) else {
            continue;
        };
        if let Ok((api_key, _)) = super::resolve_provider_key(
            &provider,
            None,
            entry.api_key.as_deref(),
            &credentials_file,
            false,
        ) {
            // A settings.json provider cannot declare the Vertex/Bedrock
            // dialects (`custom_provider` rejects both), so it never has an
            // auxiliary value to resolve.
            configured.push(ConfiguredProvider {
                config: provider,
                api_key,
                aux: AuxCredentials::new(),
            });
        }
    }
    configured
}
