//! Opt-in capture of the request body Stella actually puts on the wire.
//!
//! A benchmark can measure what a run *cost* from `step_usage` — tokens,
//! price, upstream, latency — and, since #4565 and #4621, the whole
//! content-free half of what the request asked for: the resolved effort, the
//! output ceiling, the temperature and the generation params (top_p, seed,
//! verbosity, service tier). The gap that closed is why a 20-task
//! head-to-head could establish that Stella emits 1.94x Claude Code's output
//! tokens and still not say whether the two `low` efforts resolved to the
//! same thinking budget.
//!
//! What remains here is the content-bearing half — the tool schemas, and the
//! prompt itself. Those cannot ride a metering event whose whole contract is
//! that it carries no content (AGENTS.md rule 3), so this file is where they
//! are seen or nowhere.
//!
//! A proxy is the obvious way to see this and the wrong one here: an
//! OpenRouter benchmark run must reach the canonical endpoint, and the harbor
//! adapter refuses a configured base URL precisely so a measured run cannot be
//! routed through something that alters it. Capturing in-process keeps that
//! guarantee intact and costs no network hop, so the wall-clock number the
//! same run produces stays honest.
//!
//! **Off unless [`WIRE_LOG_ENV`] names a file.** Unset — every normal run —
//! this compiles to one `OnceLock` read per call and nothing else.
//!
//! **Bodies only, never headers.** The credential rides in `Authorization`;
//! a chat-completions request body carries none. Nothing here can leak a key
//! because nothing here can see one.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Names the file to append captured requests to. Absent = capture off.
pub const WIRE_LOG_ENV: &str = "STELLA_WIRE_LOG";

fn sink() -> Option<&'static Mutex<std::fs::File>> {
    static SINK: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let path = PathBuf::from(std::env::var_os(WIRE_LOG_ENV)?);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Append: one file per session, many calls, and a truncating open
        // would keep only the last request of a trial.
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()
            .map(Mutex::new)
    })
    .as_ref()
}

/// Append one outbound request, as the JSON the provider will receive.
///
/// Best-effort by construction: capture is an observation of a run, never a
/// participant in it, so a full disk or a bad path must not fail the call
/// being observed. Serialization failure is recorded as a row naming the
/// failure rather than dropped, so a gap in the transcript is always visible
/// as a gap.
pub(crate) fn record_request<T: serde::Serialize>(url: &str, provider: &str, body: &T) {
    let Some(sink) = sink() else {
        return;
    };
    let row = match serde_json::to_value(body) {
        Ok(value) => serde_json::json!({
            "ts_ms": now_ms(),
            "direction": "request",
            "provider": provider,
            "url": url,
            "body": value,
        }),
        Err(error) => serde_json::json!({
            "ts_ms": now_ms(),
            "direction": "request",
            "provider": provider,
            "url": url,
            "error": error.to_string(),
        }),
    };
    let Ok(mut file) = sink.lock() else {
        return;
    };
    let _ = writeln!(file, "{row}");
    let _ = file.flush();
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The capture is off by default, and being off is silent: no file, no
    /// directory, no error. A benchmark rig that forgets to set the variable
    /// gets exactly the behaviour it had before this module existed.
    #[test]
    fn capture_is_off_without_the_env_var() {
        // `sink()` memoizes, so this asserts the shape of the decision rather
        // than mutating process env under a parallel test binary.
        assert_eq!(WIRE_LOG_ENV, "STELLA_WIRE_LOG");
        assert!(std::env::var_os("STELLA_WIRE_LOG").is_none() || sink().is_some());
    }

    /// A row is one line of JSON with the body nested under `body`, so a
    /// transcript can be read with `jq` and joined to `step_usage` by time.
    #[test]
    fn a_row_carries_the_body_verbatim() {
        let body = serde_json::json!({
            "model": "anthropic/claude-sonnet-5",
            "reasoning": {"effort": "low"},
            "max_tokens": 8192,
        });
        let row = serde_json::json!({
            "ts_ms": 1_u128,
            "direction": "request",
            "provider": "openrouter",
            "url": "https://openrouter.ai/api/v1/chat/completions",
            "body": body.clone(),
        });
        assert_eq!(row["body"]["reasoning"]["effort"], "low");
        assert_eq!(row["body"]["max_tokens"], 8192);
        // One line, so the file is JSONL and a partial write is one lost row.
        assert!(!row.to_string().contains('\n'));
    }
}
