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
//! One file lands in `<out-dir>` (`docs/wire/` in the repo):
//!
//! - `wrapper.wire.json` — every message the socket carries, in both its
//!   fullest and its emptiest legal form, serialized by the same `Serialize`
//!   impls the transport uses.
//!
//! It is a corpus rather than a JSON Schema, for the reason
//! [`stella_plugin::wire_corpus`] states at length: a `schemars` schema needs a
//! `JsonSchema` impl on the types themselves, and a hand-written one here would
//! be a second copy of the contract. What it catches and what it does not is
//! spelled out there (#3532).
//!
//! Byte-deterministic: run it twice and the second run produces no diff.
//! `scripts/check-wire-schema.sh` depends on exactly that.
//!
//! Only exists when the `schema` feature is on (`required-features` in
//! Cargo.toml), so a default build of the workspace never compiles it.

use std::path::PathBuf;
use std::process::ExitCode;

use stella_plugin::wire_corpus;

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

    let artifacts = match wire_corpus::artifacts() {
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
