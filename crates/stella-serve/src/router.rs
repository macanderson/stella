// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Path → [`Route`], and the two header values a routed request needs.
//!
//! Split out of `server.rs` so the routing table can grow without pushing that
//! module past the 1,500-line ratchet (`scripts/check-file-size.sh`), and
//! because it genuinely is a separate concern: nothing here touches server
//! state, a connection, or a turn — it is a pure function of the request line,
//! which is also what makes its tests as cheap as they are.

use crate::observe::event::Route;

/// Whether this route's member id names a turn (as opposed to a session) —
/// what decides if it belongs in a record's turn slot.
pub(crate) fn route_addresses_a_turn(route: Route) -> bool {
    matches!(
        route,
        Route::TurnEvents
            | Route::TurnToolResult
            | Route::TurnProviderResult
            | Route::TurnProviderDelta
            | Route::TurnCancel
            | Route::TurnSteer
            | Route::TurnPause
            | Route::TurnResume
            | Route::TurnApprove
            | Route::TurnCheckpoint
    )
}

/// Map a path to its route template and, where it has one, the turn id.
///
/// Classification is deliberately independent of the method, so a `405` records
/// which resource was addressed rather than collapsing to "unrouted" — the
/// difference between "you used the wrong verb on `/v1/turns`" and "you asked
/// for a path that does not exist" is exactly what makes a record diagnostic.
pub(crate) fn classify<'a>(segs: &[&'a str]) -> (Route, Option<&'a str>) {
    match segs {
        ["healthz"] => (Route::Healthz, None),
        ["readyz"] => (Route::Readyz, None),
        ["v1", "metrics"] => (Route::Metrics, None),
        ["v1", "calibration"] => (Route::Calibration, None),
        ["v1", "turns"] => (Route::TurnsCreate, None),
        ["v1", "turns", id, "events"] => (Route::TurnEvents, Some(id)),
        ["v1", "turns", id, "tool-result"] => (Route::TurnToolResult, Some(id)),
        ["v1", "turns", id, "provider-result"] => (Route::TurnProviderResult, Some(id)),
        ["v1", "turns", id, "provider-delta"] => (Route::TurnProviderDelta, Some(id)),
        ["v1", "turns", id, "cancel"] => (Route::TurnCancel, Some(id)),
        ["v1", "turns", id, "steer"] => (Route::TurnSteer, Some(id)),
        ["v1", "turns", id, "pause"] => (Route::TurnPause, Some(id)),
        ["v1", "turns", id, "resume"] => (Route::TurnResume, Some(id)),
        ["v1", "turns", id, "approve"] => (Route::TurnApprove, Some(id)),
        ["v1", "turns", id, "checkpoint"] => (Route::TurnCheckpoint, Some(id)),
        ["v1", "sessions"] => (Route::SessionsCreate, None),
        ["v1", "sessions", id] => (Route::Session, Some(id)),
        ["v1", "sessions", id, "turns"] => (Route::SessionTurns, Some(id)),
        ["v1", "sessions", id, "checkpoint"] => (Route::SessionCheckpoint, Some(id)),
        _ => (Route::Unrouted, None),
    }
}

/// Read one parameter out of a raw query string.
///
/// Deliberately minimal — no percent-decoding, no repeated-key semantics, no
/// dependency. The only parameter this server reads is `after`, whose value is
/// a decimal integer: every byte that could need decoding makes it fail to
/// parse as one, which is the correct outcome anyway. Growing a second
/// parameter with a richer value type is the moment to reach for a real
/// parser, not before.
pub(crate) fn query_param<'q>(query: &'q str, name: &str) -> Option<&'q str> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then_some(value)
    })
}

/// Where a subscriber wants the stream to resume from, if anywhere.
///
/// Two spellings of one number, because two kinds of client name it
/// differently. `?after=<seq>` is the explicit form, for a client that manages
/// its own reconnects. `Last-Event-ID` is the SSE standard's form, which a
/// browser `EventSource` sends **automatically** from the `id:` line each frame
/// carries — so a web host resumes with no client code at all, which is the
/// whole reason the `id:` is emitted.
///
/// `?after=` wins when both are present: it is the one the caller wrote on
/// purpose, whereas `Last-Event-ID` is replayed by the platform from whatever
/// it happened to see last.
pub(crate) fn resume_point(query: &str, req: &crate::http::Request) -> Option<u64> {
    query_param(query, "after")
        .or_else(|| req.header("last-event-id"))
        .and_then(|value| value.trim().parse::<u64>().ok())
}

/// The `Allow` header value for a known route.
pub(crate) fn allowed(route: Route) -> &'static str {
    match route {
        Route::Healthz
        | Route::Readyz
        | Route::Metrics
        | Route::Calibration
        | Route::TurnEvents => "GET",
        Route::TurnsCreate
        | Route::TurnToolResult
        | Route::TurnProviderResult
        | Route::TurnProviderDelta
        | Route::TurnCancel
        | Route::TurnSteer
        | Route::TurnPause
        | Route::TurnResume
        | Route::TurnApprove
        | Route::SessionsCreate
        | Route::SessionTurns => "POST",
        Route::Session | Route::TurnCheckpoint | Route::SessionCheckpoint => "GET, DELETE",
        // Never reached: `Unrouted` is answered as a 404 before this is called.
        Route::Unrouted => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path is classified independently of its method, so a 405 still records
    /// which resource was addressed.
    #[test]
    fn paths_classify_to_templates_and_surrender_their_turn_id() {
        let split = |path: &'static str| -> Vec<&'static str> {
            path.split('/').filter(|s| !s.is_empty()).collect()
        };
        assert_eq!(classify(&split("/healthz")), (Route::Healthz, None));
        assert_eq!(classify(&split("/v1/metrics")), (Route::Metrics, None));
        assert_eq!(classify(&split("/v1/turns")), (Route::TurnsCreate, None));
        assert_eq!(
            classify(&split("/v1/turns/turn-abc/events")),
            (Route::TurnEvents, Some("turn-abc"))
        );
        assert_eq!(
            classify(&split("/v1/turns/turn-abc/tool-result")),
            (Route::TurnToolResult, Some("turn-abc"))
        );
        assert_eq!(
            classify(&split("/v1/turns/turn-abc/provider-result")),
            (Route::TurnProviderResult, Some("turn-abc"))
        );
        assert_eq!(
            classify(&split("/v1/turns/turn-abc/cancel")),
            (Route::TurnCancel, Some("turn-abc"))
        );
        assert_eq!(classify(&split("/nope")), (Route::Unrouted, None));
        assert_eq!(classify(&split("/v1/turns/a/b/c")), (Route::Unrouted, None));
    }

    /// Every routable path must name a verb in its `Allow` header, or a 405 is
    /// half an answer. Iterates [`Route::ALL`], not a hand-listed subset —
    /// the previous list covered 7 of 14 routes, which is exactly the rot an
    /// enumerable registry exists to end.
    #[test]
    fn every_known_route_names_an_allowed_method() {
        for route in Route::ALL {
            assert!(
                !allowed(route).is_empty(),
                "{} has no allowed method",
                route.template()
            );
        }
        assert!(allowed(Route::Unrouted).is_empty(), "404s carry no Allow");
    }

    /// Every real route's template classifies back to the same variant — the
    /// structural guard against adding a `Route` variant whose path the
    /// dispatcher never learned (both dispatch matches carry catch-alls, so
    /// that mistake compiles).
    #[test]
    fn every_real_routes_template_classifies_back_to_itself() {
        for route in Route::ALL {
            let path = route.template().replace("{id}", "member-x");
            let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
            let (classified, member) = classify(&segs);
            assert_eq!(
                classified,
                route,
                "template {} did not classify to its own route",
                route.template()
            );
            assert_eq!(
                member.is_some(),
                route.template().contains("{id}"),
                "member-id capture must match the template shape for {}",
                route.template()
            );
        }
    }
}
