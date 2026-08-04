//! Resolving the values a provider needs *beyond* its one API key.
//!
//! `config.rs` owns the one-key chain — CLI flag, env var (+aliases), the
//! anonymous-FD handoff, `~/.stella/credentials.toml`, an interactive prompt.
//! Bedrock is the only provider today that needs more than one value, and it
//! needs four: an access key id (which *is* the one key, `AWS_ACCESS_KEY_ID`),
//! a secret access key, an optional session token, and a region.
//!
//! Those three extra values used to be read straight from the process
//! environment inside the adapter's own construction, which made Bedrock work
//! in exactly one situation — a shell with the standard AWS variables exported
//! — and nowhere else. `stella auth set bedrock` stored the access key id and
//! then the run failed on the missing secret; a sealed benchmark process, whose
//! whole design keeps provider secrets out of the environment, could not reach
//! Bedrock at all (#1301).
//!
//! This module walks the same chain for each of those names and hands the
//! result to the adapter as an [`AuxCredentials`] set. One vocabulary
//! throughout: names are the canonical environment-variable spellings, in the
//! handoff, in the environment, and as the keys of the
//! `[credential_fields.bedrock]` table in the credentials file.
//!
//! ## Which credential sources are supported
//!
//! **Supported:** the anonymous-FD handoff (secrets only), the standard AWS
//! environment variables, and `~/.stella/credentials.toml`.
//!
//! **Deliberately excluded:** everything that resolves credentials *ambiently*
//! — the shared config/credentials profile files (`AWS_PROFILE`,
//! `~/.aws/credentials`), SSO caches, IMDS and the ECS/EKS container-role
//! endpoints, and web-identity token files. Supporting those would mean any
//! Stella process inherits whatever AWS identity its host happens to be
//! carrying, which is precisely the ambient authority the one-key chain exists
//! to avoid — and, under a sealed launcher, would reach back out to host state
//! the launcher spent its whole design excluding. Explicit credentials only.

use stella_model::credential::{AuxCredentials, BedrockCredentials, CredentialsFile};

use super::{Dialect, ProviderConfig};

/// Resolve every auxiliary value `provider` needs, in the same precedence
/// order the one-key chain uses: the trusted handoff, then the environment,
/// then the credentials file.
///
/// Returns an empty set for every dialect that authenticates with one key —
/// which is every dialect but Bedrock. An empty set is not an error and never
/// blocks construction: a provider with nothing extra to resolve has nothing
/// extra to fail on, and the Bedrock adapter reports its own named error for a
/// value that resolved nowhere.
///
/// `credentials_file` is passed in rather than loaded here so this shares the
/// caller's decision about whether the filesystem store may be read at all —
/// under a trusted handoff or `STELLA_NO_SETTINGS` the caller supplies an
/// in-memory `CredentialsFile::empty()`, and this resolves the same way it
/// resolves everything else: through the object it was handed.
pub(crate) fn provider_aux(
    provider: &ProviderConfig,
    credentials_file: &CredentialsFile,
) -> AuxCredentials {
    let names: &[&str] = match provider.dialect {
        Dialect::Bedrock => BedrockCredentials::AUX_ENV_NAMES,
        _ => return AuxCredentials::new(),
    };

    let mut resolved = AuxCredentials::new();
    for name in names {
        if let Some(value) = resolve_one(provider.id, name, credentials_file) {
            resolved.insert(*name, value);
        }
    }
    resolved
}

/// One auxiliary value, through the chain.
///
/// The handoff comes first for the same reason it does in
/// `resolve_provider_key`: it has env-var semantics without ever placing the
/// raw value in `/proc/self/environ` or a child environment, so a launcher
/// that sent one must win over anything a task image managed to export. Only
/// secrets can arrive that way — a routing value like `AWS_REGION` is not an
/// allowed handoff target — so the region always resolves from the two lower
/// steps.
fn resolve_one(
    provider_id: &str,
    name: &str,
    credentials_file: &CredentialsFile,
) -> Option<String> {
    if let Some(key) = crate::credential_handoff::key_for(name) {
        return Some(key.reveal().to_string());
    }
    if let Ok(value) = std::env::var(name)
        && !value.is_empty()
    {
        return Some(value);
    }
    credentials_file
        .field(provider_id, name)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Whether `provider` has every auxiliary value it cannot construct without.
///
/// Only Bedrock has such a value today: SigV4 cannot sign without
/// `AWS_SECRET_ACCESS_KEY`, so an access key id on its own is a provider that
/// resolves and then fails to build. That distinction matters for
/// auto-detection specifically — `AWS_ACCESS_KEY_ID` is exported in plenty of
/// shells for reasons that have nothing to do with Bedrock, which is why the
/// provider table already sorts Bedrock last. Sorting it last stops it
/// out-ranking a real choice; this stops it being selected when it is the
/// *only* candidate and cannot work, so the user gets the "no API key found"
/// first-run message naming providers they can actually use instead of a SigV4
/// error from a provider they never asked for.
///
/// An explicit `--model bedrock/…` does not come through here: pinning a
/// provider is unambiguous, and it must fail with the adapter's named
/// "needs AWS_SECRET_ACCESS_KEY" error rather than silently resolve elsewhere.
pub(crate) fn has_required_aux(provider: &ProviderConfig, aux: &AuxCredentials) -> bool {
    match provider.dialect {
        Dialect::Bedrock => aux.get("AWS_SECRET_ACCESS_KEY").is_some(),
        _ => true,
    }
}

/// One auxiliary value `stella auth set` can store for a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AuxField {
    /// The canonical environment-variable name — also the key it is stored
    /// under in `[credential_fields.<provider>]`, and the name it would carry
    /// through a launcher handoff. One vocabulary, every store.
    pub name: &'static str,
    /// Whether this is a secret. Secrets are read through a masked prompt and
    /// never echoed back; a routing value (the region) is typed in the clear,
    /// because masking a value that gets published in a run manifest would
    /// misrepresent what it is.
    pub secret: bool,
    /// What a user sees when asked for it, and what an omission means.
    pub prompt: &'static str,
}

/// The auxiliary values `stella auth set <provider>` offers to store, in
/// prompt order. Empty for every provider that authenticates with one key.
///
/// `AWS_DEFAULT_REGION` is deliberately absent even though the chain reads it:
/// it exists as a fallback for shells that already export it, and offering
/// *two* places to write the same setting is how a stored region and an
/// effective region end up disagreeing.
pub(crate) fn settable_aux_fields(provider: &ProviderConfig) -> &'static [AuxField] {
    match provider.dialect {
        Dialect::Bedrock => &[
            AuxField {
                name: "AWS_SECRET_ACCESS_KEY",
                secret: true,
                prompt: "AWS secret access key (required — SigV4 cannot sign without it)",
            },
            AuxField {
                name: "AWS_SESSION_TOKEN",
                secret: true,
                prompt: "AWS session token (blank unless these are temporary credentials)",
            },
            AuxField {
                name: "AWS_REGION",
                secret: false,
                prompt: "AWS region (blank for us-east-1)",
            },
        ],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bedrock() -> ProviderConfig {
        super::super::PROVIDERS
            .iter()
            .find(|p| p.id == "bedrock")
            .expect("bedrock is a built-in provider")
            .clone()
    }

    fn anthropic() -> ProviderConfig {
        super::super::PROVIDERS
            .iter()
            .find(|p| p.id == "anthropic")
            .expect("anthropic is a built-in provider")
            .clone()
    }

    #[test]
    fn a_one_key_provider_resolves_no_auxiliary_values() {
        let aux = provider_aux(&anthropic(), &CredentialsFile::empty());
        assert!(aux.is_empty());
        assert!(has_required_aux(&anthropic(), &aux));
    }

    #[test]
    fn bedrock_reads_the_credentials_file_when_the_environment_has_nothing() {
        let _env = crate::test_env::lock();
        // The gap this closes: `stella auth set bedrock` could store the access
        // key id and nothing else, so a user who stored their credentials
        // instead of exporting them got "needs AWS_SECRET_ACCESS_KEY" with no
        // supported place to put one.
        let mut file = CredentialsFile::empty();
        file.set_field("bedrock", "AWS_SECRET_ACCESS_KEY", "secret-from-the-file");
        file.set_field("bedrock", "AWS_REGION", "eu-west-2");

        // SAFETY: guarded by test_env's process-wide lock.
        unsafe {
            std::env::remove_var("AWS_SECRET_ACCESS_KEY");
            std::env::remove_var("AWS_REGION");
        }

        let aux = provider_aux(&bedrock(), &file);
        assert_eq!(
            aux.get("AWS_SECRET_ACCESS_KEY"),
            Some("secret-from-the-file")
        );
        assert_eq!(aux.get("AWS_REGION"), Some("eu-west-2"));
        assert_eq!(aux.get("AWS_SESSION_TOKEN"), None);
        assert!(has_required_aux(&bedrock(), &aux));
    }

    #[test]
    fn the_environment_outranks_the_credentials_file() {
        let _env = crate::test_env::lock();
        let mut file = CredentialsFile::empty();
        file.set_field("bedrock", "AWS_SECRET_ACCESS_KEY", "secret-from-the-file");

        // SAFETY: guarded by test_env's process-wide lock; removed below.
        unsafe {
            std::env::set_var("AWS_SECRET_ACCESS_KEY", "secret-from-the-environment");
        }
        let aux = provider_aux(&bedrock(), &file);
        unsafe {
            std::env::remove_var("AWS_SECRET_ACCESS_KEY");
        }

        assert_eq!(
            aux.get("AWS_SECRET_ACCESS_KEY"),
            Some("secret-from-the-environment")
        );
    }

    #[test]
    fn every_settable_field_is_a_name_the_chain_actually_reads() {
        // The failure this prevents is silent: `stella auth set` writes a field
        // name that `provider_aux` never looks up, so the value is stored,
        // reported as stored, and never used.
        for field in settable_aux_fields(&bedrock()) {
            assert!(
                BedrockCredentials::AUX_ENV_NAMES.contains(&field.name),
                "{} is offered for storage but never resolved",
                field.name
            );
        }
        assert!(settable_aux_fields(&anthropic()).is_empty());
    }

    #[test]
    fn bedrock_without_a_secret_anywhere_is_not_auto_detectable() {
        let _env = crate::test_env::lock();
        // SAFETY: guarded by test_env's process-wide lock.
        unsafe {
            std::env::remove_var("AWS_SECRET_ACCESS_KEY");
        }
        let aux = provider_aux(&bedrock(), &CredentialsFile::empty());
        assert!(
            !has_required_aux(&bedrock(), &aux),
            "an access key id alone must not make Bedrock the auto-detected provider"
        );
    }
}
