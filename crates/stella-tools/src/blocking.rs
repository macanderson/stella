// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Rendering a blocking-worker [`JoinError`] into the message a tool reports.
//!
//! Every tool that hops work to the blocking pool awaits a
//! [`tokio::task::spawn_blocking`] handle, and the [`JoinError`] that await
//! can produce has exactly two causes: the closure **panicked**, or the task
//! was **cancelled** (a runtime tearing down). The two demand opposite
//! debugging — a panic is a crash in our own indexer or scanner, a
//! cancellation is an operational fact about the process — and reporting a
//! panic as "cancelled" sends whoever reads the message (the model, a human
//! on the transcript, a bench census) after a shutdown race instead of the
//! bug (#3141). So every such site funnels through [`worker_failure`], and
//! each cause keeps its own words.

use tokio::task::JoinError;

/// The one rendering of a blocking-worker failure, shared by every
/// `spawn_blocking` site in the crate so the siblings stay in lockstep.
///
/// `what` is the operation as a noun phrase — "the search", "diagnostics
/// detection" — chosen so the cancelled arm reproduces the exact wording
/// each call site reported before this helper existed. A panic is named as
/// one, carrying its payload when the payload is a string: the
/// `&'static str`-then-`String` downcast pair is the same one tokio's own
/// `JoinError: Display` uses, because those are the two types `panic!`
/// produces.
pub(crate) fn worker_failure(what: &str, error: JoinError) -> String {
    if error.is_panic() {
        let payload = error.into_panic();
        let message = payload
            .downcast_ref::<&'static str>()
            .copied()
            .map(str::to_owned)
            .or_else(|| payload.downcast_ref::<String>().cloned());
        match message {
            Some(message) => format!("{what} panicked: {message}"),
            None => format!("{what} panicked"),
        }
    } else {
        format!("{what} was cancelled")
    }
}

/// [`worker_failure`] applied to a `spawn_blocking` result whose closure
/// already returns `Result<T, String>` — flattens the join layer so the
/// call site stays a single expression.
pub(crate) fn flatten<T>(
    what: &str,
    joined: Result<Result<T, String>, JoinError>,
) -> Result<T, String> {
    joined.unwrap_or_else(|error| Err(worker_failure(what, error)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The witness for #3141: a panicked blocking closure must be reported
    /// as a panic — payload and all — never as a cancellation.
    #[tokio::test]
    async fn a_panicked_worker_names_the_panic_not_a_cancellation() {
        let error = tokio::task::spawn_blocking(|| panic!("boom in the indexer"))
            .await
            .expect_err("the closure panicked");
        assert!(error.is_panic());
        let message = worker_failure("the search", error);
        assert!(message.contains("panicked"), "panic unnamed: {message}");
        assert!(
            message.contains("boom in the indexer"),
            "payload lost: {message}"
        );
        assert!(
            !message.contains("cancelled"),
            "a panic is not a cancellation: {message}"
        );
    }

    /// A formatted panic carries a `String` payload — the other downcast arm.
    #[tokio::test]
    async fn a_string_panic_payload_survives_into_the_message() {
        let error = tokio::task::spawn_blocking(|| panic!("row {} poisoned", 7))
            .await
            .expect_err("the closure panicked");
        let message = worker_failure("the code-graph query", error);
        assert_eq!(message, "the code-graph query panicked: row 7 poisoned");
    }

    /// A real cancellation keeps the wording every call site had before.
    #[tokio::test]
    async fn a_cancelled_task_keeps_the_cancelled_wording() {
        let handle = tokio::spawn(std::future::pending::<()>());
        handle.abort();
        let error = handle.await.expect_err("the task was aborted");
        assert!(error.is_cancelled());
        assert_eq!(
            worker_failure("the search", error),
            "the search was cancelled"
        );
    }

    /// `flatten` routes a join failure through the same rendering and leaves
    /// the closure's own `Err` untouched.
    #[tokio::test]
    async fn flatten_renders_the_join_layer_and_preserves_the_inner_error() {
        let joined: Result<Result<(), String>, _> =
            tokio::task::spawn_blocking(|| panic!("manifest walk crashed")).await;
        assert_eq!(
            flatten("diagnostics detection", joined),
            Err("diagnostics detection panicked: manifest walk crashed".into())
        );

        let inner: Result<Result<(), String>, tokio::task::JoinError> =
            Ok(Err("no toolchain resolves here".into()));
        assert_eq!(
            flatten("diagnostics detection", inner),
            Err("no toolchain resolves here".into())
        );
    }
}
