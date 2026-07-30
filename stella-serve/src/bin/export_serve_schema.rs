// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Writes the committed wire-contract artifacts for the `stella-serve`
//! transport — the envelope around `AgentEvent`, which
//! `stella-protocol`'s `export-wire-schema` covers separately.
//!
//! ```text
//! cargo run -p stella-serve --features schema --bin export-serve-schema -- <out-dir>
//! ```
//!
//! Three files land in `<out-dir>` (`docs/wire/` in the repo):
//!
//! - `serveframe.schema.json`   — JSON Schema 2020-12 for `ServerFrame`.
//! - `serveinbound.schema.json` — the two result bodies a host POSTs back.
//! - `serveframe.d.ts`          — both, as TypeScript, plus the `seq` envelope
//!   the schema cannot express.
//!
//! Byte-deterministic: run it twice and the second run produces no diff.
//! `scripts/check-wire-schema.sh` depends on exactly that.
//!
//! Only exists when the `schema` feature is on (`required-features` in
//! Cargo.toml), so a default build never compiles it or `schemars`.

use std::path::PathBuf;
use std::process::ExitCode;

use stella_serve::schema_export;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let Some(out_dir) = args.next().map(PathBuf::from) else {
        eprintln!("usage: export-serve-schema <out-dir>");
        return ExitCode::FAILURE;
    };
    if args.next().is_some() {
        eprintln!("usage: export-serve-schema <out-dir>  (exactly one argument)");
        return ExitCode::FAILURE;
    }

    if let Err(err) = std::fs::create_dir_all(&out_dir) {
        eprintln!("export-serve-schema: creating {}: {err}", out_dir.display());
        return ExitCode::FAILURE;
    }

    let artifacts = match schema_export::artifacts() {
        Ok(artifacts) => artifacts,
        Err(err) => {
            eprintln!("export-serve-schema: {err}");
            eprintln!(
                "  The generated schema uses a construct the TypeScript printer does not\n  \
                 model. Extend `stella-protocol/src/schema_export.rs` — a silently wrong\n  \
                 .d.ts is worse than no .d.ts."
            );
            return ExitCode::FAILURE;
        }
    };

    for (name, body) in artifacts {
        let path = out_dir.join(name);
        if let Err(err) = std::fs::write(&path, body) {
            eprintln!("export-serve-schema: writing {}: {err}", path.display());
            return ExitCode::FAILURE;
        }
        println!("wrote {}", path.display());
    }
    ExitCode::SUCCESS
}
