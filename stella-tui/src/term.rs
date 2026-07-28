//! Terminal enter/restore shared by both shells (single-session and deck).
//!
//! Restoration must not rely on `Drop` alone: release builds abort on panic
//! (`[profile.release] panic = "abort"`), so destructors never run on a
//! panicking process and a Drop-only guard leaves the user's terminal
//! stranded in raw mode on the alternate screen. The panic hook installed by
//! [`PanicHookGuard`] therefore restores the terminal *directly* in abort
//! builds — sharing the guard's acquired-state flags — and then delegates to
//! the previously installed hook, so the standard panic message (and any
//! `RUST_BACKTRACE` output) lands on the user's real screen, not the
//! alternate one.
//!
//! In unwind (dev) builds the hook never restores. The hook fires for EVERY
//! panic on ANY thread or tokio task, and a session can outlive a panic in
//! two ways that make hook-time restoration actively harmful: panel panics
//! are caught by `render::guarded_panel` and rendered as an error card in
//! place (see [`PanelBoundary`]), and a panic on a background task can leave
//! the deck session running while other sessions continue — restoring there
//! would tear the live UI out from under them. Any panic that really does
//! end the program unwinds into [`TerminalGuard`]'s `Drop`, which performs
//! the restore on that orderly path. Printing at hook time is just as wrong
//! as restoring — stderr goes to the alternate screen and vanishes with it —
//! so the unwind-build hook instead writes the debug log and STASHES the
//! panic report (message + backtrace) on the shared state; the restore
//! replays it on stderr once the real screen is back. Without that replay a
//! fatal dev panic exits silently. Under abort none of that machinery gets a
//! chance to run — the process dies inside the hook — so the hook is the
//! only restoration point and restoring (then delegating to the previous
//! hook for the message) is always right, even mid-panel-draw.

use std::cell::Cell;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};

/// Which terminal states are currently held. Shared between the guard (for
/// Drop-time restore on the orderly path) and the panic hook (for abort-safe
/// restore from whichever thread panicked).
#[derive(Debug, Default)]
struct TermState {
    raw: AtomicBool,
    alt: AtomicBool,
    mouse: AtomicBool,
    paste: AtomicBool,
    kitty: AtomicBool,
    /// Panic reports stashed by the unwind-build hook, which must neither
    /// restore nor print (module docs): stderr at hook time lands on the
    /// alternate screen and vanishes with it. Replayed by [`Self::restore`]
    /// once the real screen is back, so a fatal dev panic still reaches
    /// stderr instead of exiting silently.
    panics: Mutex<Vec<String>>,
}

impl TermState {
    /// Roll back exactly the states that were acquired. Idempotent: the
    /// `swap(false)` means a hook-time restore leaves nothing for a later
    /// Drop-time restore to redo.
    fn restore(&self) {
        let mut out = io::stdout();
        if self.kitty.swap(false, Ordering::SeqCst) {
            let _ = execute!(out, PopKeyboardEnhancementFlags);
        }
        if self.paste.swap(false, Ordering::SeqCst) {
            let _ = execute!(out, DisableBracketedPaste);
        }
        if self.mouse.swap(false, Ordering::SeqCst) {
            let _ = execute!(out, DisableMouseCapture);
        }
        if self.alt.swap(false, Ordering::SeqCst) {
            let _ = execute!(out, LeaveAlternateScreen);
        }
        if self.raw.swap(false, Ordering::SeqCst) {
            let _ = disable_raw_mode();
        }
        // Only now is stderr the user's real screen. Draining makes the
        // replay as idempotent as the flag swaps above.
        for report in self.take_panics() {
            eprintln!("{report}");
        }
    }

    /// Queue a panic report for replay after the terminal is restored.
    /// Poison-tolerant: this runs inside the panic hook, and a lost report
    /// is exactly the failure mode it exists to prevent.
    fn stash_panic(&self, report: String) {
        self.panics
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(report);
    }

    fn take_panics(&self) -> Vec<String> {
        std::mem::take(&mut *self.panics.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// Restores the terminal (raw mode + alternate screen, and mouse capture if
/// it was enabled) on drop — and, via [`PanicHookGuard`], on panic even in
/// abort builds.
///
/// Each terminal state is flagged as it is acquired, and the guard exists
/// BEFORE the first acquisition — so an error partway through `enter` (raw
/// mode on, alternate screen failed) still drops the guard and rolls back
/// exactly the states that were entered, never stranding the user's
/// terminal in raw mode.
pub(crate) struct TerminalGuard {
    state: Arc<TermState>,
}

impl TerminalGuard {
    pub(crate) fn enter(mouse: bool) -> io::Result<Self> {
        let guard = Self {
            state: Arc::new(TermState::default()),
        };
        let mut out = io::stdout();
        enable_raw_mode()?;
        guard.state.raw.store(true, Ordering::SeqCst);
        execute!(out, EnterAlternateScreen)?;
        guard.state.alt.store(true, Ordering::SeqCst);
        // Always on: without bracketed paste a multi-line paste arrives as
        // raw key events, so every newline acts as Enter — one paste becomes
        // N separate prompt submissions. With it, the whole paste arrives as
        // one `Event::Paste` the composer folds into a chip.
        execute!(out, EnableBracketedPaste)?;
        guard.state.paste.store(true, Ordering::SeqCst);
        if mouse {
            execute!(out, EnableMouseCapture)?;
            guard.state.mouse.store(true, Ordering::SeqCst);
        }
        // The kitty keyboard protocol disambiguates modified keys, letting
        // `⌘⏎`/`⌃⏎` submit while plain `⏎` inserts a line break. Probing
        // needs raw mode, so this comes last; best-effort (`false` on any
        // probe error → legacy Enter semantics).
        if matches!(supports_keyboard_enhancement(), Ok(true)) {
            execute!(
                out,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
            guard.state.kitty.store(true, Ordering::SeqCst);
        }
        Ok(guard)
    }

    /// Whether the kitty keyboard protocol was pushed. When it is active, a
    /// modified Enter (`⌘⏎`/`⌃⏎`) is reportable and the composer runs full
    /// textarea semantics; without it the shell falls back to Enter-submits
    /// (see `crate::composer::classify_enter`).
    pub(crate) fn kitty(&self) -> bool {
        self.state.kitty.load(Ordering::SeqCst)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.state.restore();
    }
}

thread_local! {
    /// True while the current thread is inside a `guarded_panel` draw whose
    /// panics are caught and rendered in place (dev builds only — see the
    /// module docs for the abort-build caveat).
    static IN_GUARDED_PANEL: Cell<bool> = const { Cell::new(false) };
}

/// RAII marker for a panel draw whose panics are caught by the caller
/// (`render::guarded_panel`, effective in unwind builds only).
///
/// The unwind-build panic hook consults this flag to tell caught-in-place
/// panics from fatal ones: a guarded panel draw renders its panic as an
/// error card, so its report is not stashed for the after-restore stderr
/// replay (module docs). Under abort the catch is inert — the process is
/// about to die — so restoring even during a marked panel draw is always
/// right and the flag is ignored.
pub(crate) struct PanelBoundary {
    prev: bool,
}

impl PanelBoundary {
    pub(crate) fn enter() -> Self {
        let prev = IN_GUARDED_PANEL.with(|f| f.replace(true));
        Self { prev }
    }
}

impl Drop for PanelBoundary {
    fn drop(&mut self) {
        let prev = self.prev;
        IN_GUARDED_PANEL.with(|f| f.set(prev));
    }
}

/// The boxed panic-hook type `std::panic::take_hook` hands back — aliased so
/// the guard's field stays readable.
type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send + 'static>;

/// Installs a panic hook that, in abort builds, restores the terminal before
/// the process dies — and puts the previous hook back on drop.
///
/// The hook: (1) always appends the panic to the structured debug log when
/// one is configured; (2) under `panic = "abort"` ONLY, restores the
/// terminal via the shared state and then delegates to the previously
/// installed hook, so the standard panic message and `RUST_BACKTRACE`
/// output print on the user's real screen, not the alternate one. In unwind
/// builds step (2) never happens — the hook fires for panics the session
/// survives (caught panel draws, background tasks), and real teardown
/// restores via [`TerminalGuard`]'s `Drop` instead. What an unwind build
/// does instead is stash the report (unless a `guarded_panel` catch renders
/// it in place) so the restore can replay it on stderr — a fatal dev panic
/// must not exit silently (see the module docs).
pub(crate) struct PanicHookGuard {
    prev: Arc<PanicHook>,
}

impl PanicHookGuard {
    pub(crate) fn install(debug_log_path: Option<PathBuf>, terminal: &TerminalGuard) -> Self {
        let state = terminal.state.clone();
        // Shared with the installed closure so Drop can also reinstall it:
        // the closure delegates to the previous hook for the standard
        // message + backtrace, and the guard hands it back on teardown.
        let prev: Arc<PanicHook> = Arc::new(std::panic::take_hook());
        let delegate = Arc::clone(&prev);
        std::panic::set_hook(Box::new(move |info| {
            if let Some(path) = &debug_log_path {
                let _ = crate::shell::append_json_line(
                    path,
                    "panic",
                    serde_json::json!({ "info": info.to_string() }),
                );
            }
            // Abort builds only: the process dies right after this hook, so
            // it is the last chance to leave the terminal usable. Unwind
            // builds must NOT restore or print here — the panic may be one
            // the session survives, and stderr would land on the alternate
            // screen anyway — so the report is stashed for the restore to
            // replay on the real screen (see the module docs). A guarded
            // panel draw is exempt: its catcher renders the panic in place.
            if cfg!(panic = "abort") {
                state.restore();
                (*delegate)(info);
            } else if !IN_GUARDED_PANEL.with(Cell::get) {
                state.stash_panic(panic_report(info));
            }
        }));
        Self { prev }
    }
}

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        let prev = Arc::clone(&self.prev);
        std::panic::set_hook(Box::new(move |info| (*prev)(info)));
    }
}

/// The stashed form of one panic: the standard message plus a backtrace on
/// the usual `RUST_BACKTRACE` terms (captured NOW, at hook time — by replay
/// time the panicking frames are long gone).
fn panic_report(info: &std::panic::PanicHookInfo<'_>) -> String {
    use std::backtrace::{Backtrace, BacktraceStatus};
    let mut report = format!("thread panicked: {info}");
    let backtrace = Backtrace::capture();
    match backtrace.status() {
        BacktraceStatus::Captured => {
            report.push_str(&format!("\nstack backtrace:\n{backtrace}"));
        }
        BacktraceStatus::Disabled => {
            report.push_str(
                "\nnote: run with `RUST_BACKTRACE=1` environment variable to display a backtrace",
            );
        }
        _ => {}
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_boundary_flag_nests_and_resets() {
        assert!(!IN_GUARDED_PANEL.with(Cell::get));
        {
            let _outer = PanelBoundary::enter();
            assert!(IN_GUARDED_PANEL.with(Cell::get));
            {
                let _inner = PanelBoundary::enter();
                assert!(IN_GUARDED_PANEL.with(Cell::get));
            }
            assert!(
                IN_GUARDED_PANEL.with(Cell::get),
                "inner drop must restore the outer boundary, not clear it"
            );
        }
        assert!(!IN_GUARDED_PANEL.with(Cell::get));
    }

    #[test]
    fn term_state_restore_is_idempotent() {
        // No terminal is entered here: with all flags false, restore must be
        // a no-op both times (the swap(false) discipline), so calling it from
        // the panic hook and then again from Drop never double-emits escape
        // sequences.
        let state = TermState::default();
        state.restore();
        state.restore();
        assert!(!state.raw.load(Ordering::SeqCst));
        assert!(!state.alt.load(Ordering::SeqCst));
        assert!(!state.mouse.load(Ordering::SeqCst));
    }

    #[test]
    fn restore_drains_stashed_panic_reports() {
        // The unwind-build hook stashes; restore replays-and-drains, so the
        // report reaches stderr exactly once however many restores follow
        // (hook-then-Drop, or a Drop after an already-drained hook restore).
        let state = TermState::default();
        state.stash_panic("boom at src/x.rs:1".into());
        state.stash_panic("second boom".into());
        assert_eq!(
            state.panics.lock().unwrap().len(),
            2,
            "reports queue until a restore"
        );
        state.restore();
        assert!(
            state.take_panics().is_empty(),
            "restore must drain the stash — a later restore would double-print"
        );
    }

    #[test]
    fn panic_report_carries_the_message_for_the_replay() {
        // The report is built at hook time (the panicking frames are gone by
        // replay time); whatever else RUST_BACKTRACE adds, the message and
        // location must be in it — this is all a silent dev exit leaves behind.
        let prev = std::panic::take_hook();
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        std::panic::set_hook(Box::new(move |info| {
            sink.lock().unwrap().push(panic_report(info));
        }));
        let result = std::panic::catch_unwind(|| panic!("deliberate test panic"));
        std::panic::set_hook(prev);
        assert!(result.is_err());
        // Any-entry match: an unrelated test panicking during the swap window
        // would append its own report, which must not fail this one.
        let reports = captured.lock().unwrap();
        assert!(
            reports
                .iter()
                .any(|r| r.contains("deliberate test panic") && r.contains("term.rs")),
            "message + location survive: {reports:?}"
        );
    }
}
