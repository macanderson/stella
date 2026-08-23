// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `src/server.rs`'s module doc claims to list every endpoint this transport
//! exposes. This is what makes the claim true (#3758).
//!
//! It was not. The table listed sixteen of the eighteen routes `Route::ALL`
//! declares, silently missing `GET /v1/calibration` (#1298) and
//! `POST /v1/turns/{id}/provider-delta` (#1165) — both live, both dispatched
//! in `route()`'s own match. The omission was found while correcting a website
//! page that cited this table as a source of truth, which is the cost of a doc
//! that undercounts: the next reader believes it.
//!
//! A source-text guard rather than a rustdoc convention, for the same reason
//! `one_oracle_story.rs` reads `stella-plugin`'s sources: the claim is about
//! prose, and nothing in the type system can hold prose to an enum.

use std::path::Path;

use stella_serve::observe::event::Route;

/// The module doc — every `//!` line at the top of `src/server.rs`, up to the
/// first line that is not one.
fn module_doc() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/server.rs");
    let text = std::fs::read_to_string(&path).expect("src/server.rs is readable");
    text.lines()
        .skip_while(|line| !line.starts_with("//!"))
        .take_while(|line| line.starts_with("//!"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_endpoint_table_names_every_route_the_server_declares() {
    let doc = module_doc();
    assert!(
        doc.contains("Endpoints (all under a bearer token except `/healthz`)"),
        "the table this test is about is gone; it was renamed, not deleted, or \
         this guard now proves nothing:\n{doc}"
    );

    let missing: Vec<&str> = Route::ALL
        .iter()
        .map(|route| route.template())
        .filter(|template| !doc.contains(*template))
        .collect();

    assert!(
        missing.is_empty(),
        "`src/server.rs`'s module doc claims to list every endpoint, and these \
         are live routes it does not name: {missing:?}"
    );
}

#[test]
fn the_endpoint_table_invents_no_route() {
    let doc = module_doc();
    // Every `/v1/...` path the table spells, taken out of its backticked
    // cells. The other direction of the same claim: a row for a path nothing
    // routes sends a host to a 404 the doc promised would work.
    let mut spelled = Vec::new();
    for cell in doc.split('`') {
        if cell.starts_with("/v1/") || cell == "/healthz" || cell == "/readyz" {
            spelled.push(cell);
        }
    }
    assert!(
        !spelled.is_empty(),
        "no path was recovered from the table, so a green result would prove \
         nothing:\n{doc}"
    );

    let templates: Vec<&str> = Route::ALL.iter().map(|route| route.template()).collect();
    for path in spelled {
        assert!(
            templates.contains(&path),
            "the table names `{path}`, which no `Route` declares"
        );
    }
}
