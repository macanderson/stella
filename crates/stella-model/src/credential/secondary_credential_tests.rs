use super::*;

use std::collections::BTreeMap;

fn lookup_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
    let map: BTreeMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    move |name: &str| map.get(name).cloned()
}

/// The project falls back `VERTEX_PROJECT_ID` → `GOOGLE_CLOUD_PROJECT`,
/// and the location defaults to `global` — the exact behavior stella-cli
/// used to hand-roll, now owned by the crate that owns the adapter.
#[test]
fn vertex_addressing_prefers_vertex_project_id_and_defaults_to_global() {
    let resolved = VertexAddressing::resolve_from(lookup_from(&[
        ("VERTEX_PROJECT_ID", "primary"),
        ("GOOGLE_CLOUD_PROJECT", "fallback"),
    ]))
    .unwrap();
    assert_eq!(resolved.project, "primary");
    assert_eq!(resolved.location, "global");

    let resolved =
        VertexAddressing::resolve_from(lookup_from(&[("GOOGLE_CLOUD_PROJECT", "fallback")]))
            .unwrap();
    assert_eq!(resolved.project, "fallback");

    let resolved = VertexAddressing::resolve_from(lookup_from(&[
        ("VERTEX_PROJECT_ID", "primary"),
        ("VERTEX_LOCATION", "us-central1"),
    ]))
    .unwrap();
    assert_eq!(resolved.location, "us-central1");
}

#[test]
fn vertex_addressing_without_a_project_names_both_variables() {
    let err = VertexAddressing::resolve_from(lookup_from(&[])).unwrap_err();
    assert_eq!(err, CredentialError::VertexProjectMissing);
    let message = err.to_string();
    assert!(message.contains("VERTEX_PROJECT_ID"), "{message}");
    assert!(message.contains("GOOGLE_CLOUD_PROJECT"), "{message}");
}

#[test]
fn bedrock_region_falls_back_then_defaults_and_the_session_token_is_optional() {
    let resolved =
        BedrockCredentials::resolve_from(lookup_from(&[("AWS_SECRET_ACCESS_KEY", "secret")]))
            .unwrap();
    assert_eq!(resolved.secret_access_key.reveal(), "secret");
    assert!(resolved.session_token.is_none());
    assert_eq!(resolved.region, "us-east-1");

    let resolved = BedrockCredentials::resolve_from(lookup_from(&[
        ("AWS_SECRET_ACCESS_KEY", "secret"),
        ("AWS_DEFAULT_REGION", "eu-west-1"),
    ]))
    .unwrap();
    assert_eq!(resolved.region, "eu-west-1");

    let resolved = BedrockCredentials::resolve_from(lookup_from(&[
        ("AWS_SECRET_ACCESS_KEY", "secret"),
        ("AWS_REGION", "ap-south-1"),
        ("AWS_DEFAULT_REGION", "eu-west-1"),
        ("AWS_SESSION_TOKEN", "token"),
    ]))
    .unwrap();
    assert_eq!(resolved.region, "ap-south-1");
    assert_eq!(resolved.session_token.unwrap().reveal(), "token");
}

#[test]
fn bedrock_without_a_secret_is_a_named_error_not_an_unsigned_request() {
    let err =
        BedrockCredentials::resolve_from(lookup_from(&[("AWS_REGION", "us-east-1")])).unwrap_err();
    assert_eq!(err, CredentialError::BedrockSecretMissing);
    assert!(err.to_string().contains("AWS_SECRET_ACCESS_KEY"));
}
