// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Owner-only files, the way the rest of the workspace already writes them.
//!
//! `docs/design/diagnostics.md` §7.4 puts the crash ring at
//! `.stella/private/crash-<ts>.jsonl` — "the 0700 directory `trace.rs` already
//! establishes, 0600 file". This module is that idiom, factored out so the log
//! file (§6) and the crash file get identical treatment rather than two
//! near-copies that drift.
//!
//! Permissions are **re-asserted, not assumed**: the mode is set at creation
//! *and* again on the opened path, because a file that already existed does not
//! inherit the mode passed to `open`.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io;
use std::path::Path;

/// Create `dir` and its parents, owner-only.
pub(crate) fn create_dir(dir: &Path) -> io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Open `path` for appending, owner-only, creating its parent directory if
/// needed.
pub(crate) fn open_append(path: &Path) -> io::Result<File> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        create_dir(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    tighten(path);
    Ok(file)
}

/// Re-assert owner-only permissions on an existing path.
///
/// Best-effort: a failure here means the file is readable by someone it should
/// not be, which is worth neither a panic nor failing the caller's turn — the
/// alternative is losing the diagnostic entirely, and the file's *contents* are
/// content-free by construction (§5.2), which is why this degrades rather than
/// aborts.
pub(crate) fn tighten(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}
