//! The hub→cloud drain transport and invocation site (#404).
//!
//! Everything else already shipped: the wire contract (`DrainBatch`,
//! `stella-store/src/drain.rs`, #468), the pager→deliver→ack loop with
//! poison-row bisection (`drain_org`, #467), and the monotonic per-org cursor.
//! This module supplies the two missing pieces — the HTTPS [`CloudIntake`]
//! adapter and the code that actually invokes the loop — plus the attempt
//! record `stella cloud status` renders (#464).
//!
//! Opt-in by construction: the drain runs only when `~/.stella/cloud.json`
//! carries both an `org_id` (registration) and a `drain` block (the endpoint).
//! An unregistered or unconfigured install performs zero network I/O here.
//!
//! The HTTP posture mirrors `enterprise_telemetry`'s sender deliberately —
//! no proxy, no redirects, short timeouts, bounded response reads — but this
//! is a *different pipe* with a different wire contract (the versioned
//! `DrainBatch`, not the operational spool's string-schema batch), so it is
//! an adapter over the drain port rather than a second use of that spool.

use std::time::Duration;

use stella_store::drain::{CloudIntake, DrainBatch, DrainOutcome, DrainRejection, drain_org};
use stella_store::identity::CloudDrainConfig;
use stella_store::usage::UsageStore;

/// Response-body bytes worth keeping as a rejection diagnostic.
const MAX_DETAIL_BYTES: usize = 300;

/// The HTTPS adapter over the drain's [`CloudIntake`] port.
pub(crate) struct HttpCloudIntake {
    url: reqwest::Url,
    bearer: Option<String>,
}

impl HttpCloudIntake {
    /// Build the adapter, validating the endpoint. `bearer` is the intake
    /// credential: the OAuth token when the login (#405) has stored one,
    /// otherwise the drain config's static token.
    pub(crate) fn new(url: &str, bearer: Option<String>) -> Result<Self, String> {
        let url =
            reqwest::Url::parse(url).map_err(|e| format!("invalid drain url `{url}`: {e}"))?;
        Ok(Self { url, bearer })
    }

    async fn post(&self, body: Vec<u8>) -> Result<(), DrainRejection> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("stella/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| {
                DrainRejection::terminal_batch(None, format!("cannot build client: {e}"))
            })?;
        let mut request = client
            .post(self.url.clone())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
        if let Some(token) = &self.bearer {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .map_err(|e| DrainRejection::transient(None, e.to_string()))?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        // The body is the intake's diagnostic — data for the rejection, read
        // bounded so a hostile or broken intake cannot balloon memory.
        let detail = match response.bytes().await {
            Ok(bytes) => {
                String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_DETAIL_BYTES)]).into_owned()
            }
            Err(_) => String::new(),
        };
        Err(DrainRejection::from_http_status(status.as_u16(), detail))
    }
}

impl CloudIntake for HttpCloudIntake {
    fn deliver(&self, batch: &DrainBatch) -> Result<(), DrainRejection> {
        let body = serde_json::to_vec(batch).map_err(|e| {
            DrainRejection::terminal_batch(None, format!("cannot encode batch: {e}"))
        })?;
        // `deliver` is a sync port and this may be called with or without an
        // ambient tokio runtime (CLI command vs a future background site), so
        // the request runs on its own thread with its own single-thread
        // runtime. Batches are large and infrequent; the per-call thread is
        // noise next to the network round-trip.
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| {
                            DrainRejection::terminal_batch(
                                None,
                                format!("cannot build drain runtime: {e}"),
                            )
                        })?;
                    runtime.block_on(self.post(body))
                })
                .join()
                .unwrap_or_else(|_| {
                    Err(DrainRejection::terminal_batch(
                        None,
                        "drain thread panicked",
                    ))
                })
        })
    }
}

/// One drain pass for `org_id` against `intake`, with the attempt recorded in
/// the hub for `stella cloud status` (#464). Returns the loop's outcome.
pub(crate) fn drain_once(
    hub: &UsageStore,
    org_id: &str,
    intake: &dyn CloudIntake,
    batch_size: usize,
) -> Result<DrainOutcome, String> {
    let outcome = drain_org(hub, org_id, intake, batch_size)
        .map_err(|e| format!("drain failed against the hub: {e}"))?;
    let now = stella_context_free_now();
    let (ok, detail) = match &outcome.stopped_by {
        None => (
            true,
            format!(
                "{} delivered, {} quarantined",
                outcome.delivered, outcome.quarantined
            ),
        ),
        Some(rejection) => (
            false,
            format!(
                "stopped after {} delivered: {}{}",
                outcome.delivered,
                rejection
                    .http_status
                    .map(|s| format!("HTTP {s}: "))
                    .unwrap_or_default(),
                rejection.detail
            ),
        ),
    };
    // Best-effort: the drain outcome is the product; the attempt record is
    // observability about it.
    let _ = hub.record_drain_attempt(org_id, &now, ok, &detail);
    Ok(outcome)
}

/// The credential the drain presents: the OAuth token when the login (#405)
/// has stored one, else the drain config's static token.
pub(crate) fn resolve_bearer(
    registration: &stella_store::identity::CloudRegistration,
    drain: &CloudDrainConfig,
) -> Option<String> {
    registration
        .oauth_token
        .clone()
        .or_else(|| drain.token.clone())
}

/// RFC 3339 UTC now, matching the timestamp format everything else in the
/// store writes.
fn stella_context_free_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    stella_context::format_rfc3339(secs as i64)
}

/// The drain section `stella cloud status` prints for a registered org: how
/// deep the backlog is, where the cursor sits, and what the last attempt did
/// (#464). `drain_configured` distinguishes "nothing to do" from "nobody ever
/// configured a drain".
pub(crate) fn print_drain_status(hub: &UsageStore, org_id: &str, drain_configured: bool) {
    use colored::Colorize as _;
    let pending = hub.cloud_pending_count(org_id).unwrap_or(0);
    let cursor = hub.cloud_cursor(org_id).unwrap_or(0);
    println!(
        "{}     {} row(s) staged for upload (cursor at hub row {})",
        "pending:".bold(),
        pending,
        cursor
    );
    match hub.last_drain_attempt(org_id) {
        Ok(Some(attempt)) => {
            let outcome = if attempt.ok {
                attempt.detail.green().to_string()
            } else {
                attempt.detail.yellow().to_string()
            };
            println!(
                "{}  {} — {}",
                "last drain:".bold(),
                attempt.attempted_at,
                outcome
            );
        }
        Ok(None) if drain_configured => {
            println!(
                "{}  never — run `stella cloud sync` to drain",
                "last drain:".bold()
            );
        }
        Ok(None) => {
            println!(
                "{}  not configured — add a `drain` block ({{\"url\": …}}) to \
                 ~/.stella/cloud.json to enable upload",
                "drain:".bold()
            );
        }
        Err(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_store::drain::RejectionClass;

    struct FlakyIntake {
        fail_first: std::sync::atomic::AtomicBool,
    }

    impl CloudIntake for FlakyIntake {
        fn deliver(&self, _batch: &DrainBatch) -> Result<(), DrainRejection> {
            if self
                .fail_first
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(DrainRejection::transient(Some(503), "blip"));
            }
            Ok(())
        }
    }

    fn seeded_hub() -> UsageStore {
        let hub = UsageStore::in_memory().expect("hub");
        let scope = stella_store::identity::TelemetryScope {
            org_id: Some("acme".into()),
            workspace_id: Some("ws-1".into()),
            repo_id: "repo01".into(),
            project_id: "proj_a".into(),
        };
        let row = |source_rowid: i64| stella_store::SourceTelemetryRow {
            source_rowid,
            execution_id: 1,
            recorded_at: "2026-07-28T06:00:00Z".into(),
            telemetry: stella_store::TelemetryRow {
                step: 1,
                provider: "zai".into(),
                call_role: "engine".into(),
                model: "glm-5.2".into(),
                input_tokens: 10,
                estimated_input_tokens: 10,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_miss_tokens: 10,
                cache_write_tokens: 0,
                cost_usd: 0.01,
                duration_ms: 100,
                retries: 0,
                tool_calls: 0,
                usage_complete: true,
            },
        };
        hub.replicate_telemetry(&scope, &[row(1), row(2)])
            .expect("seed");
        hub
    }

    /// Witness for #404's invocation site: a drain pass delivers the staged
    /// rows, records its attempt for `cloud status` (#464), and a transient
    /// stop is recorded as such — then the retry backfills, at-least-once.
    #[test]
    fn drain_once_delivers_and_records_the_attempt() {
        let hub = seeded_hub();
        let intake = FlakyIntake {
            fail_first: std::sync::atomic::AtomicBool::new(true),
        };

        let stopped = drain_once(&hub, "acme", &intake, 10).expect("pass");
        assert_eq!(stopped.delivered, 0);
        assert_eq!(
            stopped.stopped_by.as_ref().map(|r| r.class),
            Some(RejectionClass::Transient)
        );
        let attempt = hub.last_drain_attempt("acme").unwrap().expect("recorded");
        assert!(!attempt.ok);
        assert!(attempt.detail.contains("HTTP 503"), "{}", attempt.detail);

        let retried = drain_once(&hub, "acme", &intake, 10).expect("pass");
        assert_eq!(retried.delivered, 2);
        assert!(retried.is_complete());
        let attempt = hub.last_drain_attempt("acme").unwrap().expect("recorded");
        assert!(attempt.ok);
        assert_eq!(hub.cloud_pending_count("acme").unwrap(), 0);
    }

    #[test]
    fn the_oauth_token_outranks_the_static_drain_token() {
        let drain = CloudDrainConfig {
            url: "https://intake.example/v1/batch".into(),
            format: "stella".into(),
            token: Some("static".into()),
            batch_size: 200,
        };
        let mut registration = stella_store::identity::CloudRegistration::default();
        assert_eq!(
            resolve_bearer(&registration, &drain).as_deref(),
            Some("static")
        );
        registration.oauth_token = Some("oauth".into());
        assert_eq!(
            resolve_bearer(&registration, &drain).as_deref(),
            Some("oauth")
        );
    }

    #[test]
    fn an_invalid_drain_url_is_refused_up_front() {
        assert!(HttpCloudIntake::new("not a url", None).is_err());
        assert!(HttpCloudIntake::new("https://intake.example/v1/batch", None).is_ok());
    }
}
