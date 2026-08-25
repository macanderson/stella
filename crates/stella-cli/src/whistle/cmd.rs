//! `stella whistle` — broadcast steering guidance to every live
//! non-interactive session on this machine, or a chosen few.
//!
//! Discovery reuses `stella_store::SessionRegistry::list` exactly as `stella
//! resume --list` and the deck's SESSIONS overlay do (pid/lock-checked
//! liveness, no separate bookkeeping). Delivery connects to each target's
//! whistle socket (`super::listener::WhistleListener`, bound by the session
//! itself at `crate::agent::presence::SessionPresence::announce` time) and
//! is entirely best-effort per session: one unreachable session is reported,
//! never allowed to abort delivery to the rest.

use stella_store::{SessionRecord, SessionRegistry};

#[cfg(unix)]
use super::wire::{WhistleAck, WhistleRequest, read_frame, write_frame};

#[cfg(unix)]
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
#[cfg(unix)]
const ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// One session's delivery outcome, in the order `stella whistle` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Delivery {
    Delivered,
    Unreachable(String),
}

/// Broadcast `message` to every live session, or to `session_ids` when it is
/// non-empty. `Err` only when at least one targeted session could not be
/// reached — the per-session outcomes are always printed first, so the
/// caller sees exactly which.
pub(crate) async fn run(message: &str, session_ids: &[String]) -> Result<(), String> {
    if message.trim().is_empty() {
        return Err("stella whistle: message must not be empty".to_string());
    }
    let registry = SessionRegistry::open_default();
    let targets = targets(&registry, session_ids);
    if targets.is_empty() {
        println!(
            "no {} to whistle at",
            if session_ids.is_empty() {
                "live stella sessions".to_string()
            } else {
                "matching stella sessions".to_string()
            }
        );
        return Ok(());
    }
    // Wrapped so the model reads this as an out-of-band directive rather
    // than an ordinary trailing remark — `AgentEvent::Steered` already
    // narrates the drain regardless, so the transcript records that a
    // steer landed; this wrapping is what tells the model to prioritize it.
    let wrapped = format!("[whistle — steering from another session] {message}");
    let mut any_unreachable = false;
    for record in &targets {
        let outcome = deliver_one(&registry, record, &wrapped).await;
        any_unreachable |= matches!(outcome, Delivery::Unreachable(_));
        print_outcome(record, &outcome);
    }
    if any_unreachable {
        Err("whistle: not every targeted session could be reached".to_string())
    } else {
        Ok(())
    }
}

/// The sessions to whistle at: explicit ids if given (whether or not they
/// are still live — an id typo or a session that just ended is reported as
/// "unreachable" rather than silently dropped), otherwise every session
/// `SessionRegistry` currently presents as live.
fn targets(registry: &SessionRegistry, session_ids: &[String]) -> Vec<SessionRecord> {
    let all = registry.list();
    if session_ids.is_empty() {
        all.into_iter()
            .filter(|r| SessionRegistry::presented_status(r).is_live())
            .collect()
    } else {
        all.into_iter()
            .filter(|r| session_ids.iter().any(|id| id == &r.id))
            .collect()
    }
}

fn print_outcome(record: &SessionRecord, outcome: &Delivery) {
    let title = if record.title.is_empty() {
        record.workspace.as_str()
    } else {
        record.title.as_str()
    };
    match outcome {
        Delivery::Delivered => println!("delivered    {:<24} {title}", record.id),
        Delivery::Unreachable(reason) => {
            println!("unreachable  {:<24} {title} ({reason})", record.id);
        }
    }
}

#[cfg(unix)]
async fn deliver_one(registry: &SessionRegistry, record: &SessionRecord, text: &str) -> Delivery {
    use tokio::net::UnixStream;
    use tokio::time::timeout;

    let socket_path = registry.whistle_socket_path(&record.id);
    let mut stream = match timeout(CONNECT_TIMEOUT, UnixStream::connect(&socket_path)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(_)) => {
            return Delivery::Unreachable(
                "no listener — the session isn't currently reachable".to_string(),
            );
        }
        Err(_) => return Delivery::Unreachable("connect timed out".to_string()),
    };
    let request = WhistleRequest {
        text: text.to_string(),
    };
    if write_frame(&mut stream, &request).await.is_err() {
        return Delivery::Unreachable("failed to send".to_string());
    }
    match timeout(ACK_TIMEOUT, read_frame::<_, WhistleAck>(&mut stream)).await {
        Ok(Ok(ack)) if ack.delivered => Delivery::Delivered,
        Ok(Ok(_)) => Delivery::Unreachable("the session declined the message".to_string()),
        Ok(Err(_)) | Err(_) => Delivery::Unreachable("no acknowledgment".to_string()),
    }
}

/// `tokio::net::UnixListener`/`UnixStream` have no Windows counterpart, and
/// Stella ships no Windows binary today (AGENTS.md's Windows section) — a
/// declared gap rather than a silent no-op, matching the `BestEffort`
/// convention `AGENTS.md`'s provider-parity axes use for the same shape of
/// "not yet reachable on this surface".
#[cfg(not(unix))]
async fn deliver_one(
    _registry: &SessionRegistry,
    _record: &SessionRecord,
    _text: &str,
) -> Delivery {
    Delivery::Unreachable(
        "agent whistle needs a Unix domain socket — not supported on this platform yet".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, status: stella_store::SessionStatus) -> SessionRecord {
        let mut r = SessionRecord::new("workspace".to_string(), "name".to_string());
        r.id = id.to_string();
        r.status = status;
        r
    }

    #[test]
    fn default_targets_are_every_live_session_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let registry = SessionRegistry::open(dir.path());
        let live = record("ses-live", stella_store::SessionStatus::InProgress);
        let waiting = record("ses-waiting", stella_store::SessionStatus::NeedsInput);
        let done = record("ses-done", stella_store::SessionStatus::Complete);
        for r in [&live, &waiting, &done] {
            registry.upsert(r).unwrap();
        }
        let mut ids: Vec<_> = targets(&registry, &[]).into_iter().map(|r| r.id).collect();
        ids.sort();
        assert_eq!(ids, vec!["ses-live", "ses-waiting"]);
    }

    #[test]
    fn explicit_session_ids_narrow_regardless_of_status() {
        let dir = tempfile::tempdir().unwrap();
        let registry = SessionRegistry::open(dir.path());
        let done = record("ses-done", stella_store::SessionStatus::Complete);
        registry.upsert(&done).unwrap();
        let ids: Vec<_> = targets(&registry, &["ses-done".to_string()])
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, vec!["ses-done"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_live_listener_is_delivered_to_and_an_absent_one_is_reported_unreachable() {
        let dir = tempfile::tempdir().unwrap();
        let registry = SessionRegistry::open(dir.path());
        let reachable = record("ses-reachable", stella_store::SessionStatus::InProgress);
        registry.upsert(&reachable).unwrap();
        registry.prepare_sidecar(&reachable.id).unwrap();
        let tap: std::sync::Arc<dyn super::super::tap::Whistleable> =
            std::sync::Arc::new(super::super::tap::HeadlessSteerTap::default());
        let socket_path = registry.whistle_socket_path(&reachable.id);
        let _listener = super::super::listener::WhistleListener::spawn_at(&socket_path, tap)
            .expect("bind must succeed against a fresh temp path");

        let unreachable = record("ses-unreachable", stella_store::SessionStatus::InProgress);
        registry.upsert(&unreachable).unwrap();

        assert_eq!(
            deliver_one(&registry, &reachable, "test").await,
            Delivery::Delivered
        );
        assert!(matches!(
            deliver_one(&registry, &unreachable, "test").await,
            Delivery::Unreachable(_)
        ));
    }
}
