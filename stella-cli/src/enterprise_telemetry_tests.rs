use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::Sha256;
use stella_protocol::ToolOutput;
use stella_store::enterprise_telemetry::StellaOperationalEventV1;
use stella_store::usage::ExecutionRollupRow;

use crate::enterprise_telemetry::{
    BatchSender, build_runtime_from_managed, host_spool_path, register_project_env_names,
    validate_response_status, verify_managed_enrollment,
};
use crate::settings::Settings;
use crate::{Cli, Command, TelemetryCmd};
use clap::Parser;

struct EnvRestore(Vec<(String, Option<std::ffi::OsString>)>);

impl EnvRestore {
    fn capture(names: &[&str]) -> Self {
        Self(
            names
                .iter()
                .map(|name| ((*name).to_string(), std::env::var_os(name)))
                .collect(),
        )
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        unsafe {
            for (name, value) in self.0.drain(..) {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

#[derive(Clone, Serialize)]
struct TestClaims {
    schema: &'static str,
    issuer: &'static str,
    audience: &'static str,
    enrollment_id: &'static str,
    organization_id: &'static str,
    workspace_id: &'static str,
    endpoint: &'static str,
    credential_env: &'static str,
    event_classes: Vec<&'static str>,
    issued_at_unix_s: i64,
    expires_at_unix_s: i64,
}

fn signed_managed(secret_env: &str, secret: &[u8], claims: TestClaims) -> Value {
    let bytes = serde_json::to_vec(&claims).unwrap();
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(&bytes);
    let signature_hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    json!({
        "verification_secret_env": secret_env,
        "allowed_issuers": ["oxagen-enterprise"],
        "allowed_audiences": ["stella-cli"],
        "allowed_endpoints": ["https://telemetry.oxagen.test/v1/events"],
        "enrollment": {
            "claims": claims,
            "signature_hex": signature_hex
        }
    })
}

fn valid_claims() -> TestClaims {
    TestClaims {
        schema: "stella.enterprise.telemetry.enrollment.v1",
        issuer: "oxagen-enterprise",
        audience: "stella-cli",
        enrollment_id: "enroll_01",
        organization_id: "org_01",
        workspace_id: "workspace_01",
        endpoint: "https://telemetry.oxagen.test/v1/events",
        credential_env: "STELLA_TEST_TELEMETRY_TOKEN",
        event_classes: vec!["execution_rollup"],
        issued_at_unix_s: 1_700_000_000,
        expires_at_unix_s: 1_700_003_600,
    }
}

fn rollup(id: i64) -> ExecutionRollupRow {
    ExecutionRollupRow {
        project_id: "local-project-id".into(),
        project_name: "private-name".into(),
        project_root: "/private/path".into(),
        execution_id: id,
        kind: "run".into(),
        prompt_digest: "private-digest".into(),
        prompt_preview: "private prompt".into(),
        model: "anthropic/claude-sonnet-4".into(),
        provider: "anthropic".into(),
        outcome: "completed".into(),
        cost_usd: 0.01,
        input_tokens: 10,
        output_tokens: 5,
        duration_ms: 12,
        tool_calls: 1,
        files_written: 1,
        produced_output: true,
        self_rating: None,
        started_at: "2026-07-21 12:00:00".into(),
        day: "2026-07-21".into(),
        tool_histogram: Vec::new(),
    }
}

struct Sender {
    attempts: AtomicUsize,
    fail: Mutex<bool>,
}

#[async_trait]
impl BatchSender for Sender {
    async fn send(
        &self,
        _endpoint: &reqwest::Url,
        _bearer_token: &str,
        _events: &[StellaOperationalEventV1],
    ) -> Result<(), String> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        if *self.fail.lock().unwrap() {
            Err("simulated HTTP failure".into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn absent_enrollment_builds_no_client_and_creates_no_host_state() {
    let _env = crate::test_env::lock();
    let _restore = EnvRestore::capture(&["STELLA_DATA_DIR"]);
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let data = dir.path().join("host-data");
    std::fs::create_dir_all(&workspace).unwrap();
    unsafe { std::env::set_var("STELLA_DATA_DIR", &data) };
    let builds = AtomicUsize::new(0);

    let runtime = build_runtime_from_managed(None, &workspace, 1_700_000_001, || {
        builds.fetch_add(1, Ordering::SeqCst);
        unreachable!("disabled telemetry must not construct an HTTP client")
    })
    .unwrap();

    assert!(runtime.is_none());
    assert_eq!(builds.load(Ordering::SeqCst), 0);
    assert!(!data.exists());
    unsafe { std::env::remove_var("STELLA_DATA_DIR") };
}

#[test]
fn telemetry_status_and_flush_are_explicit_provider_free_commands() {
    for (name, expected) in [
        ("status", TelemetryCmd::Status),
        ("flush", TelemetryCmd::Flush),
    ] {
        let cli = Cli::try_parse_from(["stella", "telemetry", name]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Telemetry { cmd }) if cmd == expected
        ));
    }
}

#[test]
fn redirects_and_non_success_http_responses_remain_retryable_failures() {
    assert!(validate_response_status(reqwest::StatusCode::TEMPORARY_REDIRECT).is_err());
    assert!(validate_response_status(reqwest::StatusCode::TOO_MANY_REQUESTS).is_err());
    assert!(validate_response_status(reqwest::StatusCode::NO_CONTENT).is_ok());
}

#[test]
fn enrollment_is_strict_signed_current_https_and_operational_only() {
    let _env = crate::test_env::lock();
    let _restore = EnvRestore::capture(&["STELLA_TEST_VERIFY_SECRET"]);
    let secret = b"0123456789abcdef0123456789abcdef";
    unsafe {
        std::env::set_var(
            "STELLA_TEST_VERIFY_SECRET",
            "0123456789abcdef0123456789abcdef",
        )
    };
    let now = 1_700_000_001;

    let valid = signed_managed("STELLA_TEST_VERIFY_SECRET", secret, valid_claims());
    assert!(verify_managed_enrollment(&valid, now).is_ok());

    let mut malformed_allowlist = valid.clone();
    malformed_allowlist["allowed_endpoints"] = json!([
        "https://telemetry.oxagen.test/v1/events",
        "http://telemetry.oxagen.test/v1/events"
    ]);
    assert!(
        verify_managed_enrollment(&malformed_allowlist, now).is_err(),
        "every allowlist entry must satisfy the strict endpoint policy"
    );

    let mut expired = valid_claims();
    expired.expires_at_unix_s = now;
    assert!(
        verify_managed_enrollment(
            &signed_managed("STELLA_TEST_VERIFY_SECRET", secret, expired),
            now
        )
        .is_err()
    );

    let wrong_signature = signed_managed(
        "STELLA_TEST_VERIFY_SECRET",
        b"abcdef0123456789abcdef0123456789",
        valid_claims(),
    );
    assert!(verify_managed_enrollment(&wrong_signature, now).is_err());

    for claims in [
        TestClaims {
            endpoint: "http://telemetry.oxagen.test/v1/events",
            ..valid_claims()
        },
        TestClaims {
            endpoint: "https://evil.example/v1/events",
            ..valid_claims()
        },
        TestClaims {
            issuer: "evil-issuer",
            ..valid_claims()
        },
        TestClaims {
            audience: "other-client",
            ..valid_claims()
        },
        TestClaims {
            schema: "stella.enterprise.telemetry.enrollment.v2",
            ..valid_claims()
        },
        TestClaims {
            event_classes: vec!["compliance_audit"],
            ..valid_claims()
        },
    ] {
        let managed = signed_managed("STELLA_TEST_VERIFY_SECRET", secret, claims);
        assert!(verify_managed_enrollment(&managed, now).is_err());
    }

    let mut unknown = valid;
    unknown["enrollment"]["claims"]["prompt"] = json!("must reject unknown content");
    assert!(verify_managed_enrollment(&unknown, now).is_err());
    unsafe { std::env::remove_var("STELLA_TEST_VERIFY_SECRET") };
}

#[test]
fn project_dotenv_cannot_supply_either_managed_credential() {
    let _env = crate::test_env::lock();
    let verify_ref = "STELLA_PROJECT_DOTENV_VERIFY_SECRET";
    let token_ref = "STELLA_PROJECT_DOTENV_BEARER_TOKEN";
    let _restore = EnvRestore::capture(&[verify_ref, token_ref]);
    let secret = b"fedcba9876543210fedcba9876543210";
    unsafe {
        std::env::set_var(verify_ref, "fedcba9876543210fedcba9876543210");
        std::env::set_var(token_ref, "project-controlled-token");
    }
    register_project_env_names([verify_ref.to_string(), token_ref.to_string()]);
    let managed = signed_managed(
        verify_ref,
        secret,
        TestClaims {
            credential_env: token_ref,
            ..valid_claims()
        },
    );
    let Err(error) = verify_managed_enrollment(&managed, 1_700_000_001) else {
        panic!("project dotenv credentials were accepted");
    };
    assert!(!error.contains(verify_ref));
    assert!(!error.contains(token_ref));
}

#[test]
fn host_spool_path_rejects_workspace_and_symlinked_data_roots() {
    let _env = crate::test_env::lock();
    let _restore = EnvRestore::capture(&["STELLA_DATA_DIR"]);
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    unsafe { std::env::set_var("STELLA_DATA_DIR", workspace.join(".stella")) };
    assert!(host_spool_path(&workspace).is_err());

    #[cfg(unix)]
    {
        let link = outside.join("linked-data");
        std::os::unix::fs::symlink(&workspace, &link).unwrap();
        unsafe { std::env::set_var("STELLA_DATA_DIR", &link) };
        assert!(host_spool_path(&workspace).is_err());
    }

    unsafe { std::env::set_var("STELLA_DATA_DIR", &outside) };
    let spool = host_spool_path(&workspace).unwrap();
    assert!(spool.starts_with(outside.canonicalize().unwrap()));
    assert!(!spool.starts_with(workspace.canonicalize().unwrap()));
    unsafe { std::env::remove_var("STELLA_DATA_DIR") };
}

#[test]
fn only_the_managed_settings_snapshot_can_supply_enrollment() {
    let _env = crate::test_env::lock();
    let _restore = EnvRestore::capture(&["HOME", "STELLA_MANAGED_SETTINGS"]);
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let project_dir = workspace.join(".stella");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("settings.json"),
        r#"{"enterprise_telemetry":{"source":"project"}}"#,
    )
    .unwrap();
    let absent_managed = dir.path().join("absent-managed.json");
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("STELLA_MANAGED_SETTINGS", &absent_managed);
    }
    let settings = Settings::load(&workspace).unwrap();
    assert!(settings.managed_enterprise_telemetry().is_none());

    let managed = dir.path().join("managed.json");
    std::fs::write(&managed, r#"{"enterprise_telemetry":{"source":"managed"}}"#).unwrap();
    unsafe { std::env::set_var("STELLA_MANAGED_SETTINGS", &managed) };
    let settings = Settings::load(&workspace).unwrap();
    assert_eq!(
        settings.managed_enterprise_telemetry().unwrap()["source"],
        "managed"
    );
    unsafe {
        std::env::remove_var("HOME");
        std::env::remove_var("STELLA_MANAGED_SETTINGS");
    }
}

#[test]
fn failed_delivery_stays_retryable_and_success_acks_the_same_event() {
    let _env = crate::test_env::lock();
    let _restore = EnvRestore::capture(&[
        "STELLA_DATA_DIR",
        "STELLA_TEST_VERIFY_SECRET",
        "STELLA_TEST_TELEMETRY_TOKEN",
    ]);
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let data = dir.path().join("host-data");
    std::fs::create_dir_all(&workspace).unwrap();
    unsafe {
        std::env::set_var("STELLA_DATA_DIR", &data);
        std::env::set_var(
            "STELLA_TEST_VERIFY_SECRET",
            "0123456789abcdef0123456789abcdef",
        );
        std::env::set_var("STELLA_TEST_TELEMETRY_TOKEN", "bearer-secret");
    }
    let managed = signed_managed(
        "STELLA_TEST_VERIFY_SECRET",
        b"0123456789abcdef0123456789abcdef",
        valid_claims(),
    );
    let sender = Arc::new(Sender {
        attempts: AtomicUsize::new(0),
        fail: Mutex::new(true),
    });
    let sender_for_runtime = sender.clone();
    let runtime = build_runtime_from_managed(Some(&managed), &workspace, 1_700_000_001, || {
        Ok(sender_for_runtime as Arc<dyn BatchSender>)
    })
    .unwrap()
    .unwrap();
    runtime.enqueue_rollup(&rollup(7), 10).unwrap();

    let handle = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert!(handle.block_on(runtime.flush(20)).is_err());
    assert_eq!(runtime.status().unwrap().pending_rows, 1);
    *sender.fail.lock().unwrap() = false;
    let flushed = handle.block_on(runtime.flush(2_000)).unwrap();
    assert_eq!(flushed, 1);
    assert_eq!(runtime.status().unwrap().pending_rows, 0);
    assert_eq!(sender.attempts.load(Ordering::SeqCst), 2);

    unsafe {
        std::env::remove_var("STELLA_DATA_DIR");
        std::env::remove_var("STELLA_TEST_VERIFY_SECRET");
        std::env::remove_var("STELLA_TEST_TELEMETRY_TOKEN");
    }
}

#[test]
fn enrolled_host_can_flush_but_run_tests_cannot_observe_its_credentials() {
    let _env = crate::test_env::lock();
    let _restore = EnvRestore::capture(&[
        "STELLA_DATA_DIR",
        "STELLA_TEST_VERIFY_SECRET",
        "STELLA_TEST_TELEMETRY_TOKEN",
    ]);
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let data = dir.path().join("host-data");
    std::fs::create_dir_all(&workspace).unwrap();
    unsafe {
        std::env::set_var("STELLA_DATA_DIR", &data);
        std::env::set_var(
            "STELLA_TEST_VERIFY_SECRET",
            "0123456789abcdef0123456789abcdef",
        );
        std::env::set_var("STELLA_TEST_TELEMETRY_TOKEN", "bearer-secret-value");
    }
    let managed = signed_managed(
        "STELLA_TEST_VERIFY_SECRET",
        b"0123456789abcdef0123456789abcdef",
        valid_claims(),
    );
    let sender = Arc::new(Sender {
        attempts: AtomicUsize::new(0),
        fail: Mutex::new(false),
    });
    let runtime = build_runtime_from_managed(Some(&managed), &workspace, 1_700_000_001, || {
        Ok(sender as Arc<dyn BatchSender>)
    })
    .unwrap()
    .unwrap();
    runtime.enqueue_rollup(&rollup(77), 10).unwrap();
    let handle = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert_eq!(handle.block_on(runtime.flush(20)).unwrap(), 1);

    let registry = stella_tools::ToolRegistry::with_backends(workspace, None, None);
    let output = handle.block_on(registry.execute("run_tests", &json!({"command": "env"})));
    let ToolOutput::Ok { content } = output else {
        panic!("run_tests failed: {output:?}");
    };
    for forbidden in [
        "STELLA_TEST_VERIFY_SECRET",
        "STELLA_TEST_TELEMETRY_TOKEN",
        "0123456789abcdef0123456789abcdef",
        "bearer-secret-value",
    ] {
        assert!(!content.contains(forbidden), "credential leaked: {content}");
    }
}

#[test]
fn finalization_stays_successful_when_telemetry_host_state_is_rejected() {
    let _env = crate::test_env::lock();
    let _restore = EnvRestore::capture(&[
        "HOME",
        "STELLA_MANAGED_SETTINGS",
        "STELLA_DATA_DIR",
        "STELLA_TEST_VERIFY_SECRET",
    ]);
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let managed_value = signed_managed(
        "STELLA_TEST_VERIFY_SECRET",
        b"0123456789abcdef0123456789abcdef",
        valid_claims(),
    );
    let managed_path = dir.path().join("managed.json");
    std::fs::write(
        &managed_path,
        serde_json::to_vec(&json!({
            "enterprise_telemetry": managed_value
        }))
        .unwrap(),
    )
    .unwrap();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("STELLA_MANAGED_SETTINGS", &managed_path);
        std::env::set_var("STELLA_DATA_DIR", workspace.join("model-visible-data"));
        std::env::set_var(
            "STELLA_TEST_VERIFY_SECRET",
            "0123456789abcdef0123456789abcdef",
        );
    }
    let store = stella_store::Store::open(&workspace).unwrap();
    let id = store
        .begin_execution("run", "private prompt", "anthropic", "claude-sonnet-4")
        .unwrap();
    let registry = stella_tools::ToolRegistry::with_backends(workspace.clone(), None, None);

    assert!(crate::agent::record_execution_end(
        &store,
        id,
        &registry,
        "completed",
        0.01,
    ));
    assert_eq!(
        store
            .execution_rollup(id, &workspace)
            .unwrap()
            .unwrap()
            .outcome,
        "completed"
    );
    assert!(
        !workspace
            .join("model-visible-data/enterprise-telemetry.db")
            .exists()
    );
    unsafe {
        std::env::remove_var("HOME");
        std::env::remove_var("STELLA_MANAGED_SETTINGS");
        std::env::remove_var("STELLA_DATA_DIR");
        std::env::remove_var("STELLA_TEST_VERIFY_SECRET");
    }
}
