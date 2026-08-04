//! Serialization and panic-safe restore for tests that mutate process
//! environment variables.
//!
//! The environment is process-global and `cargo test` runs a crate's tests on
//! a thread pool, so a test that sets `STELLA_DATA_DIR` is mutating state every
//! concurrently-running test in the same binary can observe — `respond` reads
//! it through `usage_db_path` on the hub-telemetry and project-drill paths.
//!
//! Two distinct hazards, and covering only one leaves the bug:
//!
//!   1. **Concurrency.** Take [`lock`] before touching the environment and
//!      hold it for as long as the override must stay in place. One lock for
//!      the whole crate, not one per variable: the hazard is the shared
//!      environment, and a second private mutex elsewhere serializes nothing
//!      against this one.
//!   2. **Unwinding.** A "set, act, unset" sequence never reaches its unset
//!      step when something panics first — an `unwrap` on a malformed fixture,
//!      or `respond` itself — leaking the override into every test that runs
//!      after it. Worse here than most: the leaked value points at a `TempDir`
//!      that is about to be deleted, so later tests resolve a path that no
//!      longer exists. [`EnvRestore`] restores from `Drop`, which runs while
//!      unwinding.
//!
//! This mirrors `stella-cli`'s module of the same name, added by #911. See
//! #1137, which is what this crate having no restore guard at all cost.

/// Acquire the env lock, recovering from a poisoned mutex (a prior
/// env-mutating test that panicked mid-hold must not cascade into unrelated
/// failures).
pub(crate) fn lock() -> std::sync::MutexGuard<'static, ()> {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// Captures the current value of each named env var and restores it on drop,
/// including while unwinding from a failed assertion.
///
/// Callers must hold [`lock`] for the entire lifetime of the returned guard.
#[must_use]
pub(crate) struct EnvRestore(Vec<(String, Option<std::ffi::OsString>)>);

impl EnvRestore {
    pub(crate) fn capture(names: &[&str]) -> Self {
        Self(
            names
                .iter()
                .map(|name| ((*name).to_string(), std::env::var_os(name)))
                .collect(),
        )
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        // SAFETY: the caller holds `lock()` for this guard's whole lifetime,
        // so no other test thread is reading or writing these vars.
        unsafe {
            for (name, value) in self.0.drain(..) {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

/// Witness for #1137: the "set, act, unset" sequence this replaces never
/// reaches its unset step when something panics first. Fails on that pattern,
/// passes on [`EnvRestore`], whose `Drop` runs during unwinding.
#[test]
fn restore_runs_on_unwind_not_just_normal_return() {
    let _outer = lock();
    let key = "STELLA_OBSERVATORY_TEST_ENV_RESTORE_WITNESS";
    let original = std::env::var_os(key);
    // SAFETY: the lock is held for the whole test.
    unsafe { std::env::remove_var(key) };

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _restore = EnvRestore::capture(&[key]);
        // SAFETY: as above.
        unsafe { std::env::set_var(key, "mutated-by-panicking-test") };
        panic!("simulated fixture failure mid-test");
    }))
    .is_err();

    assert!(panicked, "the closure must have actually unwound");
    assert_eq!(
        std::env::var_os(key),
        original,
        "EnvRestore must undo the mutation even when the guarded body panics"
    );
}
