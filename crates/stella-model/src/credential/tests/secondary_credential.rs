use crate::credential::*;

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

/// Two sessions in one process, two GCP projects: the host passes each one in.
#[test]
fn vertex_addressing_prefers_host_resolved_values_over_the_environment() {
    let mut first = AuxCredentials::new();
    first.insert("VERTEX_PROJECT_ID", "tenant-a");
    first.insert("VERTEX_LOCATION", "us-central1");
    let mut second = AuxCredentials::new();
    second.insert("VERTEX_PROJECT_ID", "tenant-b");
    second.insert("VERTEX_LOCATION", "europe-west4");

    let first = VertexAddressing::resolve_with(&first).unwrap();
    let second = VertexAddressing::resolve_with(&second).unwrap();

    assert_eq!(first.project, "tenant-a");
    assert_eq!(second.project, "tenant-b");
    assert_eq!(first.location, "us-central1");
    assert_eq!(second.location, "europe-west4");
}

/// One field, two names. Either one sets the project, so the environment is
/// not read for the other.
#[test]
fn a_host_supplied_project_wins_under_either_spelling() {
    let mut aux = AuxCredentials::new();
    aux.insert("GOOGLE_CLOUD_PROJECT", "tenant-a");
    assert_eq!(
        VertexAddressing::resolve_with(&aux).unwrap().project,
        "tenant-a"
    );
}

/// The names a host walks are the names resolution reads.
#[test]
fn vertex_aux_env_names_is_every_name_resolution_reads() {
    let reads: Vec<&str> = VertexAddressing::PROJECT_ENV_NAMES
        .iter()
        .copied()
        .chain(std::iter::once(VertexAddressing::LOCATION_ENV_NAME))
        .collect();
    assert_eq!(VertexAddressing::AUX_ENV_NAMES, reads.as_slice());
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
