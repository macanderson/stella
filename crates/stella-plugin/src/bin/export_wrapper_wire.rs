// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Writes the committed wire-contract artifact for the **wrapper socket** —
//! the two messages an out-of-process plugin exchanges with its host, which
//! `stella-protocol`'s `export-wire-schema` and `stella-serve`'s
//! `export-serve-schema` do not cover.
//!
//! ```text
//! cargo run -p stella-plugin --features schema --bin export-wrapper-wire -- <out-dir>
//! ```
//!
//! Five files land in `<out-dir>` (`docs/wire/` in the repo).
//!
//! - `wrapper.wire.json`. Every message the wrapper socket, the host-call
//!   channel and the driver channel carry. Each in its fullest and its
//!   emptiest legal form, serialized by the same `Serialize` impls the
//!   transport uses.
//! - `wrapper.schema.json`. JSON Schema 2020-12 for the wrapper socket's
//!   request and response, derived by `schemars` from the types (#3532).
//! - `wrapper.d.ts`. The same contract as TypeScript declarations, printed
//!   by `stella_protocol::schema_export` (#4535).
//! - `driver.schema.json`. JSON Schema 2020-12 for the driver channel's four
//!   messages. The host-call channel stays corpus-only: one of its answers
//!   is an untagged union, and JSON Schema cannot check that kind of union.
//! - `driver.d.ts`. The driver channel's schema as TypeScript declarations,
//!   through the same printer.
//!
//! The corpus and the two schemas do not subsume one another. All are
//! committed. A schema states shapes a corpus cannot show, such as a widened
//! scalar or a string format. A corpus pins the bytes a schema cannot
//! recover. [`stella_plugin::wire_schema`] argues the split where a reader
//! meets it.
//!
//! Byte-deterministic: run it twice and the second run produces no diff.
//! `scripts/check-wire-schema.sh` depends on exactly that.
//!
//! Only exists when the `schema` feature is on (`required-features` in
//! Cargo.toml), so a default build of the workspace never compiles it — nor
//! `schemars`.

use std::path::PathBuf;
use std::process::ExitCode;

use stella_plugin::{wire_corpus, wire_schema};

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let Some(out_dir) = args.next().map(PathBuf::from) else {
        eprintln!("usage: export-wrapper-wire <out-dir>");
        return ExitCode::FAILURE;
    };
    if args.next().is_some() {
        eprintln!("usage: export-wrapper-wire <out-dir>  (exactly one argument)");
        return ExitCode::FAILURE;
    }

    if let Err(err) = std::fs::create_dir_all(&out_dir) {
        eprintln!("export-wrapper-wire: creating {}: {err}", out_dir.display());
        return ExitCode::FAILURE;
    }

    let mut artifacts = match wire_corpus::artifacts() {
        Ok(artifacts) => artifacts,
        Err(err) => {
            eprintln!("export-wrapper-wire: {err}");
            eprintln!(
                "  A wrapper message would not encode. That is a defect in the message\n  \
                 itself, not in the exporter — a shape the socket cannot put on the wire\n  \
                 must not be in `stella_plugin::wire`."
            );
            return ExitCode::FAILURE;
        }
    };
    match wire_schema::artifacts() {
        Ok(schema) => artifacts.extend(schema),
        Err(err) => {
            eprintln!("export-wrapper-wire: {err}");
            eprintln!(
                "  The socket's contract would not publish. Every input to that step is a\n  \
                 type in this crate, so the defect is in `stella_plugin::wire` or in the\n  \
                 shared TypeScript printer — not in how the exporter was called."
            );
            return ExitCode::FAILURE;
        }
    }
    match wire_schema::driver_artifacts() {
        Ok(schema) => artifacts.extend(schema),
        Err(err) => {
            eprintln!("export-wrapper-wire: {err}");
            eprintln!(
                "  The driver channel's contract would not publish. Every input to that step\n  \
                 is a type in this crate, so the defect is in `stella_plugin::driver` or in\n  \
                 the shared TypeScript printer — not in how the exporter was called."
            );
            return ExitCode::FAILURE;
        }
    }

    for (name, body) in artifacts {
        let path = out_dir.join(name);
        if let Err(err) = std::fs::write(&path, body) {
            eprintln!("export-wrapper-wire: writing {}: {err}", path.display());
            return ExitCode::FAILURE;
        }
        println!("wrote {}", path.display());
    }
    ExitCode::SUCCESS
}
