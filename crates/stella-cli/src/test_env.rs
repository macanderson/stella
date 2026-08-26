//! Serializes tests that mutate process environment variables, and gives them
//! one way to point the user tier at a fixture.
//!
//! `setenv` / `getenv` from concurrent threads is documented UB on POSIX, and
//! the test harness runs this binary's test modules on parallel threads — so
//! every test that calls `std::env::set_var`/`remove_var` (agent.rs provider
//! routing, config.rs key resolution) must hold [`lock`] for its whole
//! mutate-read-cleanup window.
//!
//! # Why this is a file rather than an inline module
//!
//! It lived inside `main.rs` until #3996 grew it. `main.rs` is not a
//! grandfathered god file and must not become one, and a test-support module
//! is exactly the kind of thing AGENTS.md says lands in a sibling rather than
//! in the file that is running out of room.

/// A held env lock. Releases on drop, including on an unwinding panic.
///
/// Neither `Send` nor `Sync`, which is what the `PhantomData` is for: the
/// nesting depth below is per-thread, so a handle that reached another thread
/// would decrement a count it never incremented.
#[must_use]
pub(crate) struct EnvLock(std::marker::PhantomData<*const ()>);

std::thread_local! {
    /// How many [`EnvLock`]s this thread holds. The mutex itself is taken on
    /// the way from 0 to 1 and released on the way back.
    static DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The guard, parked here rather than inside the outermost [`EnvLock`], so
    /// that releasing depends on the count reaching zero and not on which
    /// handle happens to drop first.
    static HELD: std::cell::RefCell<Option<std::sync::MutexGuard<'static, ()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Acquire the env lock, recovering from a poisoned mutex (a prior
/// env-mutating test that panicked mid-hold must not cascade).
///
/// # Re-entrant, on purpose
///
/// A nested acquisition on a thread that already holds the lock returns a
/// handle that releases nothing, so a helper needing the lock can take it
/// without knowing whether its caller already did. That is what lets
/// [`crate::paths::test_user_home`] acquire the lock itself (#4980): it
/// mutates the process environment, so it needs one, and half its call sites
/// already hold one for their own `set_var`. The alternative — a token
/// threaded through every call site and every fixture helper that returns a
/// guard — puts the requirement in ~90 signatures and still lets a caller
/// hand the token to something that outlives it.
pub(crate) fn lock() -> EnvLock {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let depth = DEPTH.with(std::cell::Cell::get);
    if depth == 0 {
        let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        HELD.with(|held| *held.borrow_mut() = Some(guard));
    }
    DEPTH.with(|d| d.set(depth + 1));
    EnvLock(std::marker::PhantomData)
}

impl Drop for EnvLock {
    fn drop(&mut self) {
        let depth = DEPTH.with(std::cell::Cell::get);
        DEPTH.with(|d| d.set(depth - 1));
        if depth == 1 {
            HELD.with(|held| held.borrow_mut().take());
        }
    }
}

/// Captures the current value of each named env var and restores it on
/// drop. Unlike a hand-rolled "read the old value, mutate, restore at
/// the end of the test" sequence, this also restores on an unwinding
/// panic (e.g. a failed `assert!` mid-test) — without it, a panicking
/// test leaves `HOME`/`STELLA_DATA_DIR`/etc. mutated for every test that
/// runs after it in this binary. Callers must hold [`lock`] for the
/// entire lifetime of the returned guard.
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

/// The env names a test that redirects the user tier must capture, plus
/// whatever else it mutates.
///
/// `HOME` alone is not enough. `STELLA_HOME` moves the whole stella home and
/// `STELLA_DATA_DIR` moves the data tier, and both **outrank** `HOME` — so a
/// test that points `HOME` at a fixture while one of them is set in the
/// ambient environment reads the developer's real user settings and fails on
/// an assertion about the fixture (#4350), or writes into it (#3996). They
/// come from [`stella_home::OVERRIDE_ENV_VARS`], which is exact in both
/// directions, so a third override added there is captured here without this
/// list being edited.
pub(crate) fn home_env_names(also: &[&'static str]) -> Vec<&'static str> {
    let mut names = vec![stella_home::HOME_ENV];
    names.extend(stella_home::OVERRIDE_ENV_VARS);
    names.extend_from_slice(also);
    names
}

/// Point the user tier at `home`, and clear everything that could move it
/// somewhere else — the mutation half of [`home_sandbox`], for a caller that
/// already holds one `EnvRestore` over [`home_env_names`] and mutates more in
/// the same `unsafe` block.
///
/// The clearing **iterates [`stella_home::OVERRIDE_ENV_VARS`]** rather than
/// naming the variables, so a third override added there is cleared here
/// without this function being edited. That is the direction that fails safe:
/// with every override unset, each resolver falls back to `$HOME/.stella`,
/// which is the fixture.
///
/// # Safety
///
/// The caller holds [`lock`] for the whole mutate-read-restore window and an
/// [`EnvRestore`] over [`home_env_names`], because `setenv` racing a
/// concurrent `getenv` is UB on POSIX.
pub(crate) unsafe fn point_home_at(home: &std::path::Path) {
    unsafe {
        std::env::set_var(stella_home::HOME_ENV, home);
        for name in stella_home::OVERRIDE_ENV_VARS {
            std::env::remove_var(name);
        }
    }
}

/// Point every home resolver at `home`, so a test that reads or writes the
/// user tier reaches its own fixture and never the developer's real
/// `~/.stella`. Returns the [`EnvRestore`] guard; the caller holds [`lock`]
/// for its whole lifetime.
///
/// This is [`home_env_names`] + [`point_home_at`] in one call, which is what
/// a test wants when the home is the only thing it redirects. #3996's two
/// remaining offenders — `tool_switches`' settings round-trip, which writes
/// to `settings::user_settings_path()`, and the enterprise-telemetry
/// sandboxes — each set `HOME` and nothing else, so on a machine with
/// `STELLA_HOME` exported they read and wrote the developer's real home.
pub(crate) fn home_sandbox(home: &std::path::Path) -> EnvRestore {
    let restore = EnvRestore::capture(&home_env_names(&[]));
    // SAFETY: the caller holds `lock` for the guard's whole lifetime.
    unsafe { point_home_at(home) };
    restore
}

/// Witness for #3996: a sandbox that sets `HOME` alone leaves `STELLA_HOME`
/// pointing wherever the developer's shell put it, so
/// `stella_home::stella_home()` resolves outside the fixture. This fails on
/// the `set_var("HOME", …)` pattern and passes on [`home_sandbox`].
#[test]
fn a_home_sandbox_moves_the_stella_home_not_just_the_unix_home() {
    let _env = lock();
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = dir.path().join("developers-real-home");
    let outer = EnvRestore::capture(&stella_home::OVERRIDE_ENV_VARS);
    // SAFETY: serialized behind the binary-wide environment lock, and `outer`
    // outlives the whole body.
    unsafe { std::env::set_var(stella_home::STELLA_HOME_ENV, &elsewhere) };

    let fixture = dir.path().join("fixture");
    let sandbox = home_sandbox(&fixture);

    assert_eq!(
        stella_home::stella_home(),
        Some(fixture.join(".stella")),
        "the sandbox must move the stella home, not only HOME"
    );
    assert!(
        !stella_home::any_override_set(),
        "an override left set can still point the user tier outside the fixture"
    );

    drop(sandbox);
    assert_eq!(
        std::env::var_os(stella_home::STELLA_HOME_ENV),
        Some(elsewhere.as_os_str().to_owned()),
        "the guard must put the outer STELLA_HOME back"
    );
    drop(outer);
}

/// Witness for #4980: the lock nests. A second acquisition on a thread that
/// already holds it must return rather than block, and the mutex must still be
/// released once the outermost handle is gone — otherwise
/// [`crate::paths::test_user_home`] could not take the lock itself, which is
/// what stops a call site forgetting it.
///
/// A regression here shows up as a hang rather than a failure, which is the
/// price of witnessing a deadlock; the depth and guard assertions below fail
/// fast on the accounting bugs that do not deadlock.
#[test]
fn a_nested_acquisition_does_not_deadlock_and_still_releases() {
    let outer = lock();
    assert_eq!(DEPTH.with(std::cell::Cell::get), 1);
    let inner = lock();
    assert_eq!(DEPTH.with(std::cell::Cell::get), 2);
    assert!(
        HELD.with(|held| held.borrow().is_some()),
        "the mutex is held for the whole nest, taken once"
    );

    drop(inner);
    assert!(
        HELD.with(|held| held.borrow().is_some()),
        "a nested handle releases nothing"
    );

    drop(outer);
    assert_eq!(DEPTH.with(std::cell::Cell::get), 0);
    assert!(
        HELD.with(|held| held.borrow().is_none()),
        "the outermost handle releases the mutex, or every later test blocks"
    );
}

/// Witness for #911: the hand-rolled "capture previous, mutate, restore
/// at the end of the function" sequence this replaced across ~42 tests
/// never runs its restore step when an assertion panics first — this
/// fails on that pattern and passes on [`EnvRestore`], whose `Drop` runs
/// during unwinding too.
#[test]
fn restore_runs_on_unwind_not_just_normal_return() {
    let _outer = lock();
    let key = "STELLA_TEST_ENV_RESTORE_WITNESS";
    let original = std::env::var_os(key);
    unsafe { std::env::remove_var(key) };

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _restore = EnvRestore::capture(&[key]);
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
