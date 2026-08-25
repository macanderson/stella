// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The custom-tool group-kill witness's own child (#4698), the same shape as
//! `stella-runtime`'s `wrapper-plugin-fixture` (#3497): a portable binary
//! instead of a `/bin/sh` script, so the witness needs no shell at all —
//! `run_custom` spawns `command[0]` directly with no shell in between, and a
//! shebang script would not run on Windows regardless of anything a shell
//! could fix.
//!
//! # Modes
//!
//! - `background <path>` — start a grandchild `heartbeat <path>` process and
//!   hang, so the dropped-future witness has a tree to fail to kill, not
//!   just a direct child `kill_on_drop` already reaches on its own.
//! - `heartbeat <path>` — the grandchild: append a byte to `<path>` forever.

use std::io::Write;
use std::time::Duration;

/// How often a heartbeat appends. Short enough that the witness's
/// observation window separates a live process from a killed one by tens of
/// bytes.
const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(20);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("background") => background(arg(&args, 1)),
        Some("heartbeat") => heartbeat(arg(&args, 1)),
        other => {
            eprintln!("group-kill-fixture: no such mode `{other:?}`");
            std::process::exit(64);
        }
    }
}

fn arg(args: &[String], index: usize) -> &str {
    args.get(index).map_or("", String::as_str)
}

fn heartbeat(path: &str) -> ! {
    loop {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = file.write_all(b".");
            let _ = file.flush();
        }
        std::thread::sleep(HEARTBEAT_INTERVAL);
    }
}

fn background(path: &str) -> ! {
    let started = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe)
            .args(["heartbeat", path])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    });
    if let Err(error) = started {
        // The witness's whole subject is a grandchild that outlives the
        // caller, so one that never started must not be mistaken for one
        // that was killed.
        eprintln!("group-kill-fixture: no grandchild started: {error}");
        std::process::exit(70);
    }
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}
