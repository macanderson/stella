//! `context_providers.*` — third-party CGP context sources registered from
//! user config.
//!
//! Until now every context provider was in-process (`workspace-memory`,
//! `code-graph`) and `stella-context/src/provider.rs` documented the external
//! seam as "designed here but not yet built". The pinned CGP rev ships
//! `contextgraph-host`'s stdio and HTTP transports, so a third-party provider
//! is now a config entry rather than a fork.
//!
//! Two properties are load-bearing and deliberately **not** optional:
//!
//! * **Conformance is an admission gate, not a badge.** A provider is run
//!   through the protocol's own conformance suite *before* it is registered on
//!   the session host, so a source that lies about token cost, serves frames
//!   with no citation label, or cannot shut down cleanly never serves a turn.
//!   The alternative — register first, discover later — means the first
//!   evidence of non-conformance is a corrupted prompt.
//! * **Egress is consented, never assumed.** Every built-in is
//!   `egress: false`, so the consent store has never had occasion to prompt.
//!   An external provider is the first thing that can send workspace content
//!   off the machine, and it stays gated until the config records consent for
//!   every off-machine scope it declares.
//!
//! Enums are "loud": an unrecognized `transport` is a hard parse error, never
//! a silent fallback to something that happens to work.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The `context_providers` block: provider id → declaration. The map key is
/// the host's routing **and consent** key, so renaming an entry is a new
/// provider whose consent must be granted afresh — which is the point.
pub type ContextProviderSettings = BTreeMap<String, ExternalContextProvider>;

/// How the host reaches an external provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContextTransport {
    /// A child process speaking the protocol over stdio.
    #[default]
    Stdio,
    /// A remote endpoint over HTTP.
    Http,
}

/// One externally-served context provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ExternalContextProvider {
    /// Which transport carries the protocol.
    pub transport: ContextTransport,
    /// The program to spawn (`transport: "stdio"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Arguments for `command`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// The endpoint to connect to (`transport: "http"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Whether the entry is live. Defaults **false**: adding a provider to
    /// config is a declaration, turning it on is a separate, deliberate act —
    /// and a merged-in org-managed entry must never start serving a turn
    /// because a lower scope happened to define it.
    pub enabled: bool,
    /// Off-machine egress scopes the operator has consented to for this
    /// provider, as CGP scope strings (`org_tenant`, `third_party_index`,
    /// `third_party_model`, or a namespaced `acme:vector-store`).
    ///
    /// A provider that declares an off-machine scope with no matching entry
    /// here stays gated: the host refuses to transmit, and the query payload
    /// — which carries workspace content — never leaves.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub egress_consent: Vec<String>,
    /// Who granted the consent above, for the audit ledger. Free text (a user
    /// id, email, or policy name); absent means the local operator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consent_grantor: Option<String>,
}

impl ExternalContextProvider {
    /// The declaration's transport target, or a typed reason it is unusable.
    ///
    /// Validated here rather than at spawn time so a malformed entry is
    /// reported as configuration — naming the missing field — instead of
    /// surfacing later as a process that would not start.
    pub fn target(&self) -> Result<ProviderEndpoint, String> {
        match self.transport {
            ContextTransport::Stdio => match self.command.as_deref() {
                Some(command) if !command.trim().is_empty() => Ok(ProviderEndpoint::Stdio {
                    program: command.to_string(),
                    args: self.args.clone(),
                }),
                _ => Err("transport `stdio` requires a non-empty `command`".to_string()),
            },
            ContextTransport::Http => match self.url.as_deref() {
                Some(url) if !url.trim().is_empty() => Ok(ProviderEndpoint::Http {
                    url: url.to_string(),
                }),
                _ => Err("transport `http` requires a non-empty `url`".to_string()),
            },
        }
    }
}

/// A validated transport target — the config shape reduced to exactly what the
/// host and the conformance suite both need, with no `Option`s left to check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderEndpoint {
    Stdio { program: String, args: Vec<String> },
    Http { url: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    #[test]
    fn absent_block_registers_nothing() {
        let s: Settings = serde_json::from_str("{}").expect("parse");
        assert!(
            s.context_providers.is_empty(),
            "no config means no external providers — the shipping default"
        );
    }

    #[test]
    fn a_declared_provider_defaults_to_disabled_and_unconsented() {
        let s: Settings =
            serde_json::from_str(r#"{"context_providers":{"acme":{"command":"acme-cgp"}}}"#)
                .expect("parse");
        let acme = s.context_providers.get("acme").expect("entry");
        assert_eq!(acme.transport, ContextTransport::Stdio);
        assert!(
            !acme.enabled,
            "declaring a provider must not turn it on — that is a separate act"
        );
        assert!(
            acme.egress_consent.is_empty(),
            "consent is never implied by declaration"
        );
    }

    #[test]
    fn every_key_binds_to_its_field() {
        let json = r#"{"context_providers":{"acme":{
            "transport":"http",
            "url":"https://ctx.acme.test/cgp",
            "enabled":true,
            "egress_consent":["org_tenant","acme:vector-store"],
            "consent_grantor":"ada@acme.test"
        }}}"#;
        let s: Settings = serde_json::from_str(json).expect("parse");
        let acme = s.context_providers.get("acme").expect("entry");
        assert_eq!(acme.transport, ContextTransport::Http);
        assert_eq!(acme.url.as_deref(), Some("https://ctx.acme.test/cgp"));
        assert!(acme.enabled);
        assert_eq!(acme.egress_consent, vec!["org_tenant", "acme:vector-store"]);
        assert_eq!(acme.consent_grantor.as_deref(), Some("ada@acme.test"));
        assert!(acme.command.is_none());
    }

    #[test]
    fn transport_is_loud_on_an_unknown_value() {
        let err = serde_json::from_str::<Settings>(
            r#"{"context_providers":{"acme":{"transport":"grpc"}}}"#,
        );
        assert!(err.is_err(), "an unknown transport must fail to parse");
    }

    #[test]
    fn a_transport_without_its_required_field_is_a_typed_config_error() {
        let stdio = ExternalContextProvider {
            transport: ContextTransport::Stdio,
            ..Default::default()
        };
        assert!(stdio.target().unwrap_err().contains("`command`"));

        let http = ExternalContextProvider {
            transport: ContextTransport::Http,
            url: Some("   ".into()),
            ..Default::default()
        };
        assert!(http.target().unwrap_err().contains("`url`"));

        let good = ExternalContextProvider {
            transport: ContextTransport::Stdio,
            command: Some("acme-cgp".into()),
            args: vec!["--serve".into()],
            ..Default::default()
        };
        assert_eq!(
            good.target().expect("valid"),
            ProviderEndpoint::Stdio {
                program: "acme-cgp".into(),
                args: vec!["--serve".into()],
            }
        );
    }

    #[test]
    fn context_providers_round_trip_through_json() {
        let original = ExternalContextProvider {
            transport: ContextTransport::Http,
            command: None,
            args: vec![],
            url: Some("https://ctx.acme.test/cgp".into()),
            enabled: true,
            egress_consent: vec!["third_party_index".into()],
            consent_grantor: Some("policy:security-review".into()),
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let back: ExternalContextProvider = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, back);
    }
}
