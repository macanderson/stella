// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Getting the ring onto disk — the second half of
//! `docs/spec/diagnostics.md` §7.4.
//!
//! The ring is written **only** on a panic or a non-zero exit. That is the
//! whole retention policy, and it is why the plane costs a successful run
//! nothing on disk.
//!
//! The panic hook wraps whatever hook was installed before it rather than
//! replacing it, so the ordinary panic message a user expects still reaches
//! stderr, and `stella-tui`'s terminal-restoring hook keeps working. Announcing
//! the file is the caller's job: naming a path for a human to read is
//! *presentation*, and §2.1's routing rule puts that in the CLI, not here.

use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::cx::Cx;
use crate::dx::Dx;
use crate::field::Fields;
use crate::level::Level;
use crate::record::Record;
use crate::{Redacted, note};

/// Record that the process panicked, then dump the ring into `dir`.
///
/// Returns the file written, if anything was written. Safe to call from a
/// panic hook: it allocates and touches the filesystem, but it never panics —
/// a panic here would abort the process and destroy the very artifact this
/// exists to produce.
pub fn dump_after_panic(dx: &Dx, dir: &Path, info: &PanicHookInfo<'_>) -> Option<PathBuf> {
    let mut fields = Fields::new();
    if let Some(location) = info.location() {
        // A panic location is a path inside stella's own source tree, fixed at
        // compile time — but `Location::file` returns a borrowed `&str` rather
        // than the `&'static str` it really is, so it cannot satisfy §5.2's
        // type rule directly. The reviewed hatch is the honest way through, and
        // this is precisely the case it exists for.
        fields = fields
            .with(
                "file",
                Redacted::reviewed(
                    location.file(),
                    note!(
                        "a panic location names a file in stella's own source tree, baked in at \
                         compile time; it is never a user path, model output, or workspace content"
                    ),
                ),
            )
            .with("line", location.line())
            .with("column", location.column());
    }
    // The panic *payload* is deliberately absent. It is a formatted message
    // that routinely interpolates runtime values — a path, a tool argument, a
    // provider response — and §5.2 has no exception for the message that
    // happens to be attached to a crash. The previous hook has already printed
    // it to stderr for the human; the file stays content-free.
    dx.emit(Record::new(
        Level::Error,
        "diag.panic",
        module_path!(),
        Cx::EMPTY,
        fields,
    ));
    dump(dx, dir)
}

/// Dump the ring into `dir`, returning the file written.
///
/// `None` means there was nothing to write. An `Err` from the dump is
/// swallowed on purpose: this runs on the failure path, and a crash file that
/// could not be written must not become a second, louder failure.
pub fn dump(dx: &Dx, dir: &Path) -> Option<PathBuf> {
    dx.flush();
    let written = dx.ring().dump_into(dir).ok().flatten()?;
    // One dump per failed run adds up, and nothing else prunes this directory.
    // After writing rather than before, so a normal run keeps the newest dump.
    // Note this does NOT guarantee the just-written file survives: pruning
    // ranks by the filename stamp, so a directory already holding CRASH_KEEP
    // newer stamps (clock skew, a restored backup) deletes it — and the path
    // returned here is then announced for a file that no longer exists.
    crate::ring::prune_crash_files(dir, crate::ring::CRASH_KEEP);
    Some(written)
}

/// Install a panic hook that dumps the ring into `dir`.
///
/// `announce` is handed the file that was written, so the caller can tell the
/// user where to find it in the caller's own voice. It runs inside the panic
/// hook, so it should print and nothing more.
pub fn install_panic_hook<F>(dx: Arc<Dx>, dir: PathBuf, announce: F)
where
    F: Fn(&Path) + Send + Sync + 'static,
{
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // The pre-existing hook first: the user's panic message, and any
        // terminal restoration a TUI installed, must not wait on a file write.
        previous(info);
        if let Some(path) = dump_after_panic(&dx, &dir, info) {
            announce(&path);
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag;
    use crate::ring::latest_crash_file;
    use std::sync::Mutex;

    /// Panic hooks are process-global, so the tests that install one take
    /// turns. Without this they would clobber each other's hook and the
    /// failure would look like flakiness rather than like a shared resource.
    static HOOK: Mutex<()> = Mutex::new(());

    /// Property 5 of §12: the ring survives a panic.
    #[test]
    fn a_panic_writes_an_owner_only_jsonl_dump_holding_the_last_record() {
        let _guard = HOOK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().expect("tempdir");
        let dx = Arc::new(Dx::disabled());

        diag!(&dx, debug, "test.before_panic", step = 41_u32);

        let restore = std::panic::take_hook();
        install_panic_hook(dx.clone(), dir.path().to_path_buf(), |_| {});
        let outcome = std::panic::catch_unwind(|| panic!("wedged mid-turn"));
        std::panic::set_hook(restore);
        assert!(outcome.is_err(), "the panic must still propagate");

        let path = latest_crash_file(dir.path()).expect("a crash file");
        let text = std::fs::read_to_string(&path).expect("read");
        let records: Vec<Record> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("each line parses as JSONL"))
            .collect();

        let codes: Vec<_> = records.iter().map(|r| r.code.as_str()).collect();
        assert!(
            codes.contains(&"test.before_panic"),
            "the last pre-panic record must be in the dump: {codes:?}"
        );
        assert!(codes.contains(&"diag.panic"), "{codes:?}");

        let panic_record = records
            .iter()
            .find(|r| r.code.as_str() == "diag.panic")
            .expect("a panic record");
        assert_eq!(panic_record.level, Level::Error);
        assert!(panic_record.fields.get("line").is_some());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "a crash file is owner-only");
        }
    }

    /// The dump is what a maintainer asks a stranger to attach, so the panic
    /// message — which routinely interpolates runtime values — must not be in
    /// it.
    #[test]
    fn the_panic_payload_never_reaches_the_dump() {
        let _guard = HOOK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().expect("tempdir");
        let dx = Arc::new(Dx::disabled());
        diag!(&dx, info, "test.seed");

        let restore = std::panic::take_hook();
        // Silence the wrapped hook's stderr write for this one case: the point
        // is what lands in the FILE, and an expected panic message printed by
        // a passing test reads as a failure.
        std::panic::set_hook(Box::new(|_| {}));
        install_panic_hook(dx.clone(), dir.path().to_path_buf(), |_| {});
        let outcome =
            std::panic::catch_unwind(|| panic!("token sk-live-DEADBEEF rejected by provider"));
        std::panic::set_hook(restore);
        assert!(outcome.is_err());

        let path = latest_crash_file(dir.path()).expect("a crash file");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(
            !text.contains("sk-live-DEADBEEF"),
            "the panic payload reached the dump: {text}"
        );
    }

    #[test]
    fn the_previous_hook_still_runs() {
        let _guard = HOOK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        static RAN: Mutex<bool> = Mutex::new(false);
        let dir = tempfile::tempdir().expect("tempdir");
        let dx = Arc::new(Dx::disabled());
        diag!(&dx, info, "test.seed");

        let restore = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {
            *RAN.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        }));
        install_panic_hook(dx.clone(), dir.path().to_path_buf(), |_| {});
        let _ = std::panic::catch_unwind(|| panic!("boom"));
        std::panic::set_hook(restore);

        assert!(
            *RAN.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            "wrapping a hook must not replace it"
        );
    }

    #[test]
    fn a_dump_with_nothing_to_say_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(dump(&Dx::disabled(), dir.path()), None);
    }
}
