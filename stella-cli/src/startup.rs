//! The startup phase — the window in `main` where writing the process
//! environment is sound, and a guard that keeps it that way.
//!
//! `stella-cli` writes to the process environment in exactly three production
//! paths: `env_files::maybe_load` (dotenv loading), `credential_handoff`'s
//! metadata scrub, and `enterprise_telemetry`'s privileged-value rollback. All
//! three are correct today because `main` calls them within twenty lines of
//! each other, before the first thread of any kind exists. Until this module,
//! nothing but a prose comment said so (#1140).
//!
//! # Why guard something that is already correct
//!
//! The asymmetry, not the current state, is the argument:
//!
//! - **Severity if violated: high.** Concurrent `setenv`/`getenv` is genuine
//!   UB on POSIX — glibc may reallocate the `environ` block while another
//!   thread reads it, giving a segfault or a torn read. In a shipped binary
//!   that is a rare, load-dependent crash: the worst failure profile to
//!   diagnose there is.
//! - **Detectability if violated: near zero.** No test would catch it, it
//!   would not fail deterministically, and `cargo test` would stay green.
//! - **Cost of the guard: a token parameter and one assertion.**
//!
//! The realistic regressions are ordinary, not exotic: a second caller of
//! `env_files::maybe_load` added to reload dotenv on a config change, runtime
//! initialization moved earlier in `main`, or
//! `StartupAuthoritySnapshot::restore_after_project_env` called from a later
//! lifecycle hook. None of those would have tripped anything before.
//!
//! # Two guards, because they catch different mistakes
//!
//! [`StartupPhase`] is a token minted once, in `main`, and required by every
//! function that writes the environment. It is neither `Copy` nor `Clone` and
//! nothing hands out a `'static` one, so a would-be caller elsewhere in the
//! program has to thread a reference to `main`'s local all the way to itself —
//! which is visible in every signature along the path, at review time, before
//! the code runs. That catches the *new* caller.
//!
//! [`StartupPhase::close`] catches the other shape: the same three call sites,
//! still holding a legitimate token, reached after `main` has moved the
//! boundary. `main` closes the window in one place — by consuming the token,
//! so there is no second place it could be done from — and each site asserts
//! the window is still open.

use std::sync::atomic::{AtomicBool, Ordering};

/// Proof that the caller is running during single-threaded process startup,
/// where mutating the process environment races nothing.
///
/// Deliberately not `Copy`, not `Clone`, and not obtainable from a static:
/// [`claim`](Self::claim) hands out exactly one per process, and `main` owns
/// it as a local.
pub(crate) struct StartupPhase(());

/// Whether [`StartupPhase::claim`] has already answered.
static CLAIMED: AtomicBool = AtomicBool::new(false);

/// Whether the process has left the single-threaded startup window.
static RUNTIME_STARTED: AtomicBool = AtomicBool::new(false);

impl StartupPhase {
    /// Mint the process's one startup token. `None` on every call after the
    /// first, so a second `main`-like entry point cannot manufacture a second
    /// license to write the environment.
    pub(crate) fn claim() -> Option<Self> {
        (!CLAIMED.swap(true, Ordering::SeqCst)).then_some(Self(()))
    }

    /// Assert that `site` really is running before the process grew a second
    /// thread. Called at the top of every function that writes the process
    /// environment, *before* the write.
    ///
    /// Enabled by `debug_assertions` — the release binary pays nothing — but
    /// also unconditionally under `cfg(test)`, so the witness test that proves
    /// this fires still proves it under `cargo test --release`. A guard that
    /// silently stops guarding in the configuration someone runs the suite in
    /// is the failure mode being avoided, so it is spelled out rather than
    /// left to `debug_assert!`'s definition.
    pub(crate) fn assert_before_runtime(&self, site: &str) {
        if cfg!(debug_assertions) || cfg!(test) {
            assert!(
                !runtime_started(),
                "{site} writes the process environment and ran after the \
                 startup window closed. Concurrent setenv/getenv is undefined \
                 behaviour on POSIX: move the call back into main's \
                 single-threaded startup window, above the \
                 `StartupPhase::close()` that ends it."
            );
        }
    }

    /// Close the startup window: the process is about to grow a second thread,
    /// and no environment write may follow.
    ///
    /// Takes `self`, which is what makes "`main` marks the boundary in exactly
    /// one place" structural rather than a convention. The token is consumed,
    /// so the three functions that require one are unreachable from the rest of
    /// `main` — the compiler says so, without waiting for the assertion to fire
    /// at runtime.
    ///
    /// Deliberately not a `Drop` impl. A token dropped at the end of a scope
    /// would close the window as a side effect of going out of scope, which is
    /// precisely wrong for the tests that mint one to drive a guarded function.
    pub(crate) fn close(self) {
        RUNTIME_STARTED.store(true, Ordering::SeqCst);
    }

    /// A token for tests that need to drive a guarded function directly.
    ///
    /// Safe to hand out freely: the compile-time half of the guard is that a
    /// caller must *have* a token, and the only production source of one is
    /// [`claim`](Self::claim) in `main`.
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self(())
    }
}

/// Whether [`StartupPhase::close`] has been reached.
pub(crate) fn runtime_started() -> bool {
    RUNTIME_STARTED.load(Ordering::SeqCst)
}

/// Pretend the runtime has started, and put the flag back on drop.
///
/// The flag is process-global — it has to be, since the threads it is about
/// are — so a test arming it **must** hold [`crate::test_env::lock`] for the
/// guard's whole lifetime, the same rule the environment-mutating tests
/// already follow. The three guarded functions are only ever called under that
/// lock, so nothing else can observe the armed window.
#[cfg(test)]
#[must_use]
pub(crate) struct RuntimeStartedGuard {
    previous: bool,
}

#[cfg(test)]
pub(crate) fn test_runtime_started() -> RuntimeStartedGuard {
    RuntimeStartedGuard {
        previous: RUNTIME_STARTED.swap(true, Ordering::SeqCst),
    }
}

#[cfg(test)]
impl Drop for RuntimeStartedGuard {
    fn drop(&mut self) {
        RUNTIME_STARTED.store(self.previous, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Witness for #1140: each of the three production environment writers
    /// refuses to run once the startup window has closed.
    ///
    /// It asserts on the *wiring*, not on a helper: every call below is the
    /// real function, entered exactly as `main` enters it, and each is run
    /// twice — once inside the window, where it must succeed, and once outside
    /// it, where it must not. A guard that fires unconditionally would pass
    /// half of this test; one that never fires would pass the other half.
    ///
    /// Without it the guard could rot silently: an assertion nobody executes
    /// is indistinguishable from an assertion that was deleted.
    #[test]
    fn every_environment_writer_refuses_to_run_after_the_runtime_starts() {
        let _env = crate::test_env::lock();
        // Two of the three do real work when their inputs are present. Neutered
        // here so what the assertions observe is the guard and nothing else:
        // dotenv loading off, no credential handoff configured. The third —
        // the authority rollback — writes each privileged name back to the
        // value it was just captured holding, so it is already a no-op.
        let _restore = crate::test_env::EnvRestore::capture(&[
            "STELLA_NO_ENV_FILE",
            crate::credential_handoff::HANDOFF_FD_ENV,
        ]);
        // SAFETY: the binary-wide environment lock is held for the whole
        // mutate-read-restore window above.
        unsafe {
            std::env::set_var("STELLA_NO_ENV_FILE", "1");
            std::env::remove_var(crate::credential_handoff::HANDOFF_FD_ENV);
        }

        /// One production environment writer, named as `main` reaches it.
        type WriterSite<'a> = (&'a str, &'a dyn Fn(&StartupPhase));

        let phase = StartupPhase::for_test();
        let sites: [WriterSite<'_>; 3] = [
            ("env_files::maybe_load", &|phase| {
                let _ = crate::env_files::maybe_load(phase);
            }),
            ("credential_handoff::consume_at_startup", &|phase| {
                let _ = crate::credential_handoff::consume_at_startup(phase);
            }),
            (
                "StartupAuthoritySnapshot::restore_after_project_env",
                &|phase| {
                    let _ = crate::enterprise_telemetry::StartupAuthoritySnapshot::capture(None)
                        .restore_after_project_env(phase, &[]);
                },
            ),
        ];

        let armed = test_runtime_started();
        for (site, call) in sites {
            let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| call(&phase)));
            assert!(
                refused.is_err(),
                "{site} wrote the process environment after the runtime started"
            );
        }
        drop(armed);

        for (site, call) in sites {
            let allowed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| call(&phase)));
            assert!(
                allowed.is_ok(),
                "{site} must still run normally inside the startup window"
            );
        }
    }

    /// The token is minted once per process, so nothing downstream can
    /// manufacture a second license to write the environment.
    ///
    /// `main` never runs under the test harness, which makes this test the
    /// process's only claimant — keep it that way: a second `claim()` anywhere
    /// in the suite would make this depend on test order.
    #[test]
    fn the_startup_token_is_claimable_exactly_once() {
        assert!(
            StartupPhase::claim().is_some(),
            "the first claim is the one main makes"
        );
        assert!(
            StartupPhase::claim().is_none(),
            "every claim after the first is refused"
        );
    }
}
