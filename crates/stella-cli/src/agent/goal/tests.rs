// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The goal door's own wiring, exercised through [`super::run_goal_turn`]
//! rather than around it.
//!
//! A test that rebuilt the door's engine itself would pass with the door's
//! wiring deleted — the trap `agent/tests/engine_wiring.rs` names in its own
//! header — so this drives the real function with a scripted provider and a
//! real Unix socket, and reads the answer off what the next model call was
//! asked.

use super::*;

/// A model call, as the scripted provider saw it: every message's content,
/// joined, which is enough to answer "did the next call see the steer?".
type SeenCalls = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// A provider that whistles at its own session from inside its first model
/// call, then answers plainly forever.
///
/// Whistling from inside the call is what makes the timing deterministic: the
/// message is queued while round 1's worker turn is in flight, so if the door
/// attached the tap it lands at the next step boundary — the first step of
/// round 2 — and if it did not, it is never drained at all.
struct WhistlingProvider {
    socket: std::path::PathBuf,
    message: &'static str,
    seen: SeenCalls,
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl stella_model::provider::Provider for WhistlingProvider {
    fn id(&self) -> &str {
        "whistling"
    }

    async fn complete_ref(
        &self,
        request: stella_protocol::CompletionRequestRef<'_>,
    ) -> Result<stella_protocol::CompletionResult, stella_protocol::ProviderError> {
        self.seen.lock().unwrap_or_else(|p| p.into_inner()).push(
            request
                .messages
                .iter()
                .map(|m| m.content.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            whistle_over_the_socket(&self.socket, self.message).await;
        }
        Ok(stella_protocol::CompletionResult {
            text: "working on it".to_string(),
            tool_calls: Vec::new(),
            usage: stella_protocol::CompletionUsage::reported_zero(),
            model: "whistling".to_string(),
            cost_usd: 0.0,
            finish_reason: None,
            upstream_provider: None,
        })
    }
}

/// One `stella whistle` delivery, spoken over a real socket by the same frame
/// protocol `crate::whistle::cmd` sends — the second process, minus the
/// process.
#[cfg(unix)]
async fn whistle_over_the_socket(socket: &std::path::Path, message: &str) {
    let mut stream = tokio::net::UnixStream::connect(socket)
        .await
        .expect("the door under test must have bound this session's whistle socket");
    crate::whistle::wire::write_frame(
        &mut stream,
        &crate::whistle::wire::WhistleRequest {
            text: message.to_string(),
            deep: false,
            interrupt: false,
        },
    )
    .await
    .expect("write the frame");
    let ack: crate::whistle::wire::WhistleAck = crate::whistle::wire::read_frame(&mut stream)
        .await
        .expect("ack");
    assert!(ack.delivered, "the listener must acknowledge the delivery");
}

/// Clear every environment variable a provider credential can resolve
/// through, restoring them when the returned guard drops.
///
/// `Config::for_tests` carries a dummy key, so the door's own worker provider
/// is the stub above either way — this is about
/// `resolve_cross_family_verifier`, which discovers whatever the *machine*
/// has configured. On a developer's box that is a live key, and the goal loop
/// would build that provider and call it.
///
/// Read off `crate::config::PROVIDERS` rather than spelled out, so a
/// provider added there is denied here without this function being edited.
fn credentials_denied() -> crate::test_env::EnvRestore {
    let names: Vec<&'static str> = crate::config::PROVIDERS
        .iter()
        .flat_map(|p| std::iter::once(p.env_var).chain(p.env_var_aliases.iter().copied()))
        .collect();
    let restore = crate::test_env::EnvRestore::capture(&names);
    // SAFETY: the caller holds `test_env::lock` for the guard's whole lifetime.
    unsafe {
        for name in names {
            std::env::remove_var(name);
        }
    }
    restore
}

/// **The witness (#4769).** `stella whistle` reaches a `stella goal` arc: the
/// message queued during round 1 is in front of the model on a later call.
///
/// Fails on `main`, where `run_goal_turn` opened no whistle listener and
/// attached no steering — the connect above is what fails first, and deleting
/// only the `.with_steering(...)` leaves the socket bound and the assertion
/// below unmet, because nothing ever drains the tap.
///
/// A plain `#[test]` driving its own runtime rather than `#[tokio::test]`:
/// the environment sandbox below is a `std::sync::MutexGuard`, and holding
/// one across an `await` is a clippy denial. Building the runtime here keeps
/// every await inside `block_on`, where the guard is a thing this thread
/// holds rather than a thing a suspended task does.
#[cfg(unix)]
#[test]
fn a_whistle_reaches_the_next_round_of_a_goal_arc() {
    const WHISTLED: &str = "stop compiling and read the failing test first";

    let _env = crate::test_env::lock();
    // Under `/tmp`, not the platform temp dir: a `sockaddr_un` path is capped
    // at ~104 bytes and macOS puts its temp dirs 50 characters deep, which is
    // enough on its own to make `bind` fail `SUN_LEN`. The session id is short
    // for the same reason — it is a path component of the socket.
    let home = tempfile::Builder::new()
        .prefix("sw")
        .tempdir_in("/tmp")
        .expect("home");
    let _home = crate::test_env::home_sandbox(home.path());
    // No provider credential may resolve while this runs, or the goal loop
    // routes a cross-family verifier — a REAL provider, built from the
    // developer's own key, called over the network from a unit test.
    let _keys = credentials_denied();
    let workspace = tempfile::tempdir().expect("workspace");

    let session = "sw1";
    let socket = stella_store::SessionRegistry::open_default().whistle_socket_path(session);

    let seen: SeenCalls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = WhistlingProvider {
        socket,
        message: WHISTLED,
        seen: seen.clone(),
        calls: std::sync::atomic::AtomicUsize::new(0),
    };

    let mut cfg = Config::for_tests(crate::config::PROVIDERS[0].clone(), "m".to_string());
    cfg.workspace_root = workspace.path().to_path_buf();
    let registry = ToolRegistry::new(workspace.path().to_path_buf());
    let mut messages = vec![CompletionMessage::system("system")];
    let mut budget = crate::agent::build_budget_guard(None);

    // The arc runs to its round cap: every verdict this provider returns is
    // unparseable, which `Engine::assess` reads as "not met" and another round.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async {
            let _ = super::run_goal_turn(
                &provider,
                &registry,
                &[],
                &registry,
                &mut messages,
                &mut budget,
                &CalibrationMap::default(),
                &cfg,
                &None,
                "make the tests pass",
                Some(session),
                crate::memory::OpeningRecall::default(),
                None,
                None,
            )
            .await;
        });

    let calls = seen.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let landed = calls
        .iter()
        .position(|call| call.contains(WHISTLED))
        .expect("the whistled text must reach a later model call");
    assert!(
        landed > 0,
        "the steer cannot have been in front of the call that sent it"
    );
}
