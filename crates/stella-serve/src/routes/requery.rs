// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `POST /v1/turns/{id}/requery-result` — the host's answer to a
//! `requery_request`.
//!
//! Its own file, not one more handler in `routes.rs`. That file is the closest
//! one in this crate to the 1500-line cap
//! (`scripts/check-file-size.sh`). It moved its session handlers out the same
//! way.

use std::sync::Arc;

use crate::frame::RequeryResultIn;
use crate::observe::record::Responder;
use crate::routes::error_body;
use crate::state::ServerState;

/// Answer the parked step with the block the host chose, or with nothing.
///
/// An id nothing waits on gets a `409`, just as the two result routes give
/// one. A re-query the port gave up on is out of the map. Saying so tells the
/// host its answer was late; silence would not.
pub(crate) async fn handle_requery_result(
    res: &mut Responder<'_>,
    state: &Arc<ServerState>,
    id: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let Some(entry) = state.lookup(id) else {
        return res.json("404 Not Found", &error_body("unknown turn")).await;
    };
    let posted: RequeryResultIn = match serde_json::from_slice(body) {
        Ok(posted) => posted,
        Err(err) => {
            return res
                .json(
                    "400 Bad Request",
                    &error_body(&format!("invalid requery result: {err}")),
                )
                .await;
        }
    };
    match entry.pending.resolve_requery(
        &posted.request_id,
        crate::frame::RequeryAnswer {
            context: posted.context,
            cost_tokens: posted.cost_tokens,
        },
    ) {
        Ok(()) => res.json("200 OK", br#"{"status":"ok"}"#).await,
        Err(err) => {
            res.json("409 Conflict", &error_body(&err.to_string()))
                .await
        }
    }
}
