//! The Ed25519 enrollment certificate.
//!
//! Oxagen signs the claims with a private key that only it holds. Stella
//! checks the sign with a public key it keeps by key id. So no shared secret
//! sits on the client.

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use serde_json::Value;
use stella_store::enterprise_telemetry::ManagedModelDimension;

use super::{EnrollmentEventClass, HostDataIsolation, encode_claims};

const CERTIFICATE_DOMAIN: &[u8] = b"stella.enterprise.telemetry.enrollment-certificate.v1";
const CERTIFICATE_ALG: &str = "ed25519";

/// The claims Oxagen signs. They are the HMAC claims plus the device's own
/// public key.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CertificateClaims {
    schema: String,
    issuer: String,
    audience: String,
    enrollment_id: String,
    organization_id: String,
    workspace_id: String,
    endpoint: String,
    credential_env: String,
    device_public_key: String,
    event_classes: Vec<EnrollmentEventClass>,
    // Parsed so that any other mode is refused. Only one mode exists, so the
    // bytes write `process_free` as a fixed string.
    #[allow(dead_code, reason = "read only by serde to refuse other modes")]
    host_data_isolation: HostDataIsolation,
    model_catalog: Vec<ManagedModelDimension>,
    issued_at_unix_s: i64,
    expires_at_unix_s: i64,
}

/// The signature block beside the claims.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CertificateSignature {
    kid: String,
    alg: String,
    sig: String,
}

/// Parse certificate claims from their JSON form.
pub(crate) fn parse_certificate_claims(value: &Value) -> Result<CertificateClaims, String> {
    serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid enrollment certificate claims: {error}"))
}

/// The bytes that Oxagen signs. They add `device_public_key` after
/// `credential_env`. Each other field sits where the HMAC bytes put it.
pub(crate) fn canonical_certificate_bytes(claims: &CertificateClaims) -> Result<Vec<u8>, String> {
    encode_claims(
        CERTIFICATE_DOMAIN,
        &[
            claims.schema.as_str(),
            claims.issuer.as_str(),
            claims.audience.as_str(),
            claims.enrollment_id.as_str(),
            claims.organization_id.as_str(),
            claims.workspace_id.as_str(),
            claims.endpoint.as_str(),
            claims.credential_env.as_str(),
            claims.device_public_key.as_str(),
        ],
        &claims.event_classes,
        &claims.model_catalog,
        claims.issued_at_unix_s,
        claims.expires_at_unix_s,
    )
}

/// Check the sign on the claims with the kept keys.
///
/// The key id must be one we keep. The `alg` must be `ed25519`. The `sig`
/// must be base64 of 64 bytes. `verify_strict` also turns down a weak key and
/// a sign in a form that is not the one true form.
pub(crate) fn verify_certificate(
    claims: &CertificateClaims,
    signature: &CertificateSignature,
    pinned: &BTreeMap<String, VerifyingKey>,
) -> Result<(), String> {
    let key = pinned
        .get(&signature.kid)
        .ok_or_else(|| "enrollment certificate key id is not pinned".to_string())?;
    if signature.alg != CERTIFICATE_ALG {
        return Err("enrollment certificate algorithm must be ed25519".into());
    }
    let raw = STANDARD
        .decode(&signature.sig)
        .map_err(|_| "enrollment certificate signature is not base64".to_string())?;
    let signature = Signature::from_slice(&raw)
        .map_err(|_| "enrollment certificate signature must be 64 bytes".to_string())?;
    let message = canonical_certificate_bytes(claims)?;
    key.verify_strict(&message, &signature)
        .map_err(|_| "enrollment certificate signature mismatch".to_string())
}

#[cfg(test)]
mod tests;
