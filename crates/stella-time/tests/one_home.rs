// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The witness for this crate: the real time sources and the shared sleeper
//! doubles live here and nowhere else.
//!
//! Three crates each kept a wall clock. Two kept a Tokio sleeper. One kept a
//! sleeper trait of its own. About thirty test files each wrote the same
//! no-op double. This test reads the tree and fails on the first copy that
//! comes back. It reads source, which a test of "where does this live" has
//! to do. `stella-core` would refuse that read. This crate does not.

use std::fs;
use std::path::{Path, PathBuf};

/// A `Sleeper` impl outside this crate that stays on purpose, with the
/// reason. Each one has a shape a shared double cannot have.
const SLEEPER_IMPLS_KEPT: &[(&str, &str)] = &[
    (
        "crates/stella-core/src/tests.rs",
        "the copy the compiler forces: a lib's unit tests are a second build of the lib, and a \
         dev-dependency that links the lib implements Sleeper for the first",
    ),
    (
        "crates/stella-core/src/retry.rs",
        "the retry tests' recording sleeper: it logs every requested delay and draws a seeded jitter",
    ),
    (
        "crates/stella-fleet/src/monitor.rs",
        "the monitor tests' advancing sleeper: a sleep moves the injected clock instead of waiting",
    ),
    (
        "crates/stella-core/src/driver/tests/audit_fixes.rs",
        "the hanging sleeper: it announces its first sleep and then parks forever, for a test that drops a turn mid-backoff",
    ),
    (
        "crates/stella-core/src/step/tests.rs",
        "a sleeper on unpaused tokio time: the bound tests race a trickling call against a sleep that has to take time",
    ),
];

/// A clock struct named like one of ours that is not ours.
const CLOCK_STRUCTS_KEPT: &[(&str, &str)] = &[(
    "crates/stella-context/src/clock.rs",
    "stella-context's own Clock trait, which reads RFC 3339 text rather than milliseconds",
)];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/stella-time sits two levels below the workspace root")
        .to_path_buf()
}

/// Every `.rs` file under `crates/*/src` and `crates/*/tests`, as paths
/// relative to the workspace root, skipping this crate.
fn rust_sources(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let crates = root.join("crates");
    for entry in fs::read_dir(&crates).expect("crates/ is readable") {
        let dir = entry.expect("a crate directory").path();
        if dir.file_name().is_some_and(|name| name == "stella-time") {
            continue;
        }
        for sub in ["src", "tests"] {
            collect(&dir.join(sub), root, &mut out);
        }
    }
    out.sort();
    out
}

fn collect(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            collect(&path, root, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let rel = path
                .strip_prefix(root)
                .expect("under the root")
                .to_string_lossy()
                .replace('\\', "/");
            let text = fs::read_to_string(&path).expect("a source file is readable");
            out.push((rel, text));
        }
    }
}

fn is_kept(path: &str, kept: &[(&str, &str)]) -> bool {
    kept.iter().any(|(kept_path, _)| *kept_path == path)
}

#[test]
fn every_sleeper_impl_outside_this_crate_is_one_a_shared_double_cannot_be() {
    let root = workspace_root();
    let mut strays = Vec::new();
    for (path, text) in rust_sources(&root) {
        let impls = text
            .lines()
            .filter(|line| {
                let line = line.trim_start();
                line.starts_with("impl ")
                    && line.contains("Sleeper for ")
                    && !line.contains("ParkSupervisor")
            })
            .count();
        if impls > 0 && !is_kept(&path, SLEEPER_IMPLS_KEPT) {
            strays.push(format!("{path}: {impls} impl(s)"));
        }
    }
    assert!(
        strays.is_empty(),
        "a Sleeper impl outside stella-time is a copy of one it already has: take \
         `stella_time::TokioSleeper`, or `stella_time::test_util::{{PausedSleeper, NoopSleeper}}` \
         behind the `test-util` feature. Found:\n  {}",
        strays.join("\n  ")
    );
}

#[test]
fn every_kept_sleeper_impl_is_still_there() {
    let root = workspace_root();
    for (path, reason) in SLEEPER_IMPLS_KEPT {
        let text = fs::read_to_string(root.join(path)).expect("a kept path exists");
        assert!(
            text.contains("Sleeper for "),
            "{path} has no Sleeper impl; drop it from SLEEPER_IMPLS_KEPT ({reason})"
        );
    }
}

#[test]
fn no_crate_keeps_its_own_wall_or_monotonic_clock() {
    let root = workspace_root();
    let names = ["WallClock", "HostClock", "SystemClock", "MonotonicClock"];
    let mut strays = Vec::new();
    for (path, text) in rust_sources(&root) {
        if is_kept(&path, CLOCK_STRUCTS_KEPT) {
            continue;
        }
        for name in names {
            let needle = format!("struct {name}");
            if text.contains(&needle) {
                strays.push(format!("{path}: {needle}"));
            }
        }
    }
    assert!(
        strays.is_empty(),
        "a clock struct outside stella-time is a copy of one it already has. Found:\n  {}",
        strays.join("\n  ")
    );
}
