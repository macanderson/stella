//! Serialization and panic-safe restore for tests that mutate process
//! environment variables.
//!
//! The environment is process-global and `cargo test` runs a crate's tests on
//! a thread pool, so a test that sets `STELLA_DATA_DIR` is mutating state that
//! every concurrently-running test in the same binary can observe. That is not
//! hypothetical here: [`crate::home::migrate_legacy_global_dirs`] silently
//! no-ops whenever `STELLA_HOME`, `STELLA_DATA_DIR` or `STELLA_CONFIG_DIR` is
//! set, and `UsageStore::open` calls it — so an unrelated test's override
//! changes what a store-opening test does.
//!
//! Two distinct hazards, and both need covering:
//!
//!   1. **Concurrency.** Take [`lock`] before touching the environment, and
//!      hold it for as long as the override must stay in place. Readers of
//!      the same variables take it too, or the lock only serializes half the
//!      race.
//!   2. **Unwinding.** A hand-rolled "set, assert, unset" sequence never runs
//!      its unset step when the assertion panics first, leaking the override
//!      into every test that runs after it in this binary. [`EnvRestore`]
//!      restores from `Drop`, which runs during unwinding too.
//!
//! This mirrors `stella-cli`'s module of the same name, added by #911. This
//! crate had no serialization mechanism at all — strictly worse than the bug
//! #911 fixed, which at least had a lock. See #1137.

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

/// Witness for #1137: the "set, assert, unset" sequence this replaces never
/// reaches its unset step when the assertion panics first. Fails on that
/// pattern, passes on [`EnvRestore`], whose `Drop` runs during unwinding.
#[test]
fn restore_runs_on_unwind_not_just_normal_return() {
    let _outer = lock();
    let key = "STELLA_STORE_TEST_ENV_RESTORE_WITNESS";
    let original = std::env::var_os(key);
    // SAFETY: the lock is held for the whole test.
    unsafe { std::env::remove_var(key) };

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _restore = EnvRestore::capture(&[key]);
        // SAFETY: as above.
        unsafe { std::env::set_var(key, "mutated-by-panicking-test") };
        panic!("simulated assertion failure mid-test");
    }))
    .is_err();

    assert!(panicked, "the closure must have actually unwound");
    assert_eq!(
        std::env::var_os(key),
        original,
        "EnvRestore must undo the mutation even when the guarded body panics"
    );
}
