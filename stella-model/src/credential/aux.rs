//! Auxiliary provider credentials — the values a provider needs *beyond* the
//! single API key [`crate::credential::ApiKey`] resolves.
//!
//! Most providers authenticate with exactly one secret, which is why the
//! resolution chain is shaped around one. Bedrock is not one of those: SigV4
//! needs an access key id **and** a secret access key, optionally a session
//! token, and a region to build the endpoint host from. Three of those four
//! have nowhere to live in a one-key chain, and the fourth — the region — is
//! not a credential at all.
//!
//! [`AuxCredentials`] is that home: a small, name-keyed set carried alongside
//! the primary key from wherever the host resolved it (an inherited
//! descriptor, the environment, a credentials file) down to the adapter that
//! needs it. Names are the canonical environment-variable spellings
//! (`AWS_SECRET_ACCESS_KEY`, `AWS_REGION`) rather than a second vocabulary,
//! so one set of names describes the value in every store it can come from.
//!
//! Routing values ride in the same set as secrets and get the same redaction
//! and zeroize discipline. Splitting them into two types would buy nothing —
//! nothing downstream treats a region differently — while making it possible
//! to put a secret in the non-secret half by mistake.

use std::collections::BTreeMap;
use std::fmt;

use zeroize::Zeroize;

/// Provider values resolved beside the primary API key, keyed by the
/// canonical environment-variable name of each.
///
/// Empty is the normal case: every provider but Bedrock authenticates with
/// one key and carries an empty set. An empty set is also what a library-style
/// caller passes when it wants the adapters' own environment fallback, which
/// is why [`AuxCredentials::default`] is the useful default rather than an
/// error.
///
/// Deliberately NOT `Debug`-derived: the map holds plaintext secrets, so a
/// derived impl would print every one of them. The hand-written impl below
/// names the *keys* (which are not secret and are the useful diagnostic) and
/// redacts every value — the same posture [`crate::credential::ApiKey`] and
/// [`crate::credential::CredentialsFile`] take.
#[derive(Clone, Default)]
pub struct AuxCredentials {
    values: BTreeMap<String, String>,
}

/// Every stored value is plaintext and the map outlives the call that reads
/// it, so the whole set is wiped when it goes away — the same posture
/// [`crate::credential::ApiKey`] takes for a single key.
impl Drop for AuxCredentials {
    fn drop(&mut self) {
        for value in self.values.values_mut() {
            value.zeroize();
        }
    }
}

impl AuxCredentials {
    /// An empty set — no auxiliary value resolved.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Store `value` under `name`, replacing any previous value.
    ///
    /// An empty `value` is dropped rather than stored: every consumer treats
    /// "present but empty" as absent anyway, and storing it would make an
    /// unset variable that happens to be exported outrank a real value from a
    /// lower step of the chain.
    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let value = value.into();
        if value.is_empty() {
            return;
        }
        self.values.insert(name.into(), value);
    }

    /// The value stored under `name`, if any.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    /// Whether nothing was resolved. Adapters use this to decide between
    /// "the host resolved these for me" and "fall back to my own environment
    /// lookup" — see [`crate::credential::BedrockCredentials::resolve_with`].
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Every name currently stored, alphabetically. Names only — this is what
    /// a display surface may print.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.values.keys().map(String::as_str)
    }

    /// Resolve each of `names` from the process environment, skipping unset
    /// and empty ones. The plain-environment builder, for hosts with no
    /// richer chain of their own.
    #[must_use]
    pub fn from_env(names: &[&str]) -> Self {
        let mut resolved = Self::new();
        for name in names {
            if let Ok(value) = std::env::var(name) {
                resolved.insert(*name, value);
            }
        }
        resolved
    }
}

/// Names the count and the keys, never a value.
impl fmt::Debug for AuxCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuxCredentials")
            .field("names", &self.values.keys().collect::<Vec<_>>())
            .field("values", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_names_the_keys_and_never_the_values() {
        let mut aux = AuxCredentials::new();
        aux.insert("AWS_SECRET_ACCESS_KEY", "super-secret-value");
        aux.insert("AWS_REGION", "eu-west-1");

        let debug = format!("{aux:?}");
        assert!(!debug.contains("super-secret-value"));
        // The region is not a secret, but it shares the map, so it shares the
        // redaction — the alternative is a type that can be told apart, which
        // is exactly the mistake this set exists to prevent.
        assert!(!debug.contains("eu-west-1"));
        assert!(debug.contains("AWS_SECRET_ACCESS_KEY"));
        assert!(debug.contains("AWS_REGION"));
    }

    #[test]
    fn an_empty_value_is_absent_not_an_empty_string() {
        let mut aux = AuxCredentials::new();
        aux.insert("AWS_SESSION_TOKEN", "");
        assert_eq!(aux.get("AWS_SESSION_TOKEN"), None);
        assert!(aux.is_empty());
    }

    #[test]
    fn insert_replaces_and_names_are_sorted() {
        let mut aux = AuxCredentials::new();
        aux.insert("AWS_REGION", "us-east-1");
        aux.insert("AWS_SECRET_ACCESS_KEY", "secret");
        aux.insert("AWS_REGION", "ap-southeast-2");

        assert_eq!(aux.get("AWS_REGION"), Some("ap-southeast-2"));
        assert_eq!(
            aux.names().collect::<Vec<_>>(),
            vec!["AWS_REGION", "AWS_SECRET_ACCESS_KEY"]
        );
    }

    #[test]
    fn a_missing_name_is_none_not_an_empty_string() {
        assert_eq!(AuxCredentials::new().get("AWS_REGION"), None);
    }
}
