use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::VerifyingKey;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{
    CertificateSignature, canonical_certificate_bytes, parse_certificate_claims, verify_certificate,
};

/// The Ed25519 certificate vector, copied byte for byte from
/// `packages/handlers/src/lib/fixtures/stella-enrollment-certificate-conformance.v1.json`
/// in oxageninc/product. Oxagen's TypeScript signer pins the same file, so an
/// encoding change on either side turns a test red on both.
const CERTIFICATE_FIXTURE: &str =
    include_str!("../fixtures/enrollment_certificate_conformance_v1.json");

/// The DER header of an Ed25519 SubjectPublicKeyInfo. The 32-byte key follows it.
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

fn fixture() -> Value {
    serde_json::from_str(CERTIFICATE_FIXTURE).unwrap()
}

fn pinned_key(fixture: &Value) -> BTreeMap<String, VerifyingKey> {
    let pem = fixture["public_key_pem"].as_str().unwrap();
    let body: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();
    let der = STANDARD.decode(body).unwrap();
    assert_eq!(der.len(), 44, "an Ed25519 SPKI is 44 bytes");
    assert_eq!(der[..12], ED25519_SPKI_PREFIX);
    let key: [u8; 32] = der[12..].try_into().unwrap();
    BTreeMap::from([(
        fixture["kid"].as_str().unwrap().to_string(),
        VerifyingKey::from_bytes(&key).unwrap(),
    )])
}

fn signature(fixture: &Value) -> CertificateSignature {
    serde_json::from_value(fixture["signature"].clone()).unwrap()
}

fn verify(fixture: &Value) -> Result<(), String> {
    let claims = parse_certificate_claims(&fixture["claims"])?;
    verify_certificate(&claims, &signature(fixture), &pinned_key(fixture))
}

#[test]
fn certificate_bytes_match_the_committed_conformance_vector() {
    let fixture = fixture();
    let claims = parse_certificate_claims(&fixture["claims"]).unwrap();
    let bytes = canonical_certificate_bytes(&claims).unwrap();
    assert_eq!(bytes.len(), 487);
    assert_eq!(
        Some(bytes.len() as u64),
        fixture["canonical_bytes_length"].as_u64()
    );
    let digest: String = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(
        digest,
        "44bc599e20cfaa94cc5390f969c7f0858a009339b92f243f85622a26f90615c9"
    );
    assert_eq!(
        digest,
        fixture["canonical_bytes_sha256"].as_str().unwrap(),
        "the certificate bytes do not match the conformance vector. An \
         encoding change must update this fixture and the copy in \
         oxageninc/product in the same change"
    );
}

#[test]
fn certificate_signature_verifies_under_the_pinned_key() {
    verify(&fixture()).unwrap();
}

#[test]
fn certificate_refuses_a_tampered_claim() {
    let mut fixture = fixture();
    fixture["claims"]["organization_id"] = json!("org_conformancf");
    assert_eq!(
        verify(&fixture).unwrap_err(),
        "enrollment certificate signature mismatch"
    );
}

#[test]
fn certificate_refuses_an_unknown_key_id() {
    let fixture = fixture();
    let claims = parse_certificate_claims(&fixture["claims"]).unwrap();
    let mut signature = signature(&fixture);
    signature.kid = "0000000000000000".to_string();
    assert_eq!(
        verify_certificate(&claims, &signature, &pinned_key(&fixture)).unwrap_err(),
        "enrollment certificate key id is not pinned"
    );
}

#[test]
fn certificate_refuses_a_flipped_signature_bit() {
    let mut fixture = fixture();
    let mut raw = STANDARD
        .decode(fixture["signature"]["sig"].as_str().unwrap())
        .unwrap();
    raw[0] ^= 0x01;
    fixture["signature"]["sig"] = json!(STANDARD.encode(raw));
    assert_eq!(
        verify(&fixture).unwrap_err(),
        "enrollment certificate signature mismatch"
    );
}

#[test]
fn certificate_refuses_a_wrong_algorithm() {
    let mut fixture = fixture();
    fixture["signature"]["alg"] = json!("hmac-sha256");
    assert_eq!(
        verify(&fixture).unwrap_err(),
        "enrollment certificate algorithm must be ed25519"
    );
}

#[test]
fn certificate_refuses_a_malformed_signature() {
    let mut fixture = fixture();
    fixture["signature"]["sig"] = json!("AAAA");
    assert_eq!(
        verify(&fixture).unwrap_err(),
        "enrollment certificate signature must be 64 bytes"
    );
    fixture["signature"]["sig"] = json!("not base64!");
    assert_eq!(
        verify(&fixture).unwrap_err(),
        "enrollment certificate signature is not base64"
    );
}

#[test]
fn certificate_refuses_an_unknown_claim() {
    let mut fixture = fixture();
    fixture["claims"]["extra"] = json!("value");
    assert!(
        verify(&fixture)
            .unwrap_err()
            .starts_with("invalid enrollment certificate claims")
    );
}
