//! The `hooks.lifecycle` parity row's CLI witness.
//!
//! `stella-core` tests its own hook matching and blocking; nothing proved
//! that a hook configured in a real `Config` actually reaches
//! `with_session_hook_context` through the CLI's own wiring rather than the
//! core crate's. `session_start_hook_context` calls the real
//! `HostHookRunner`, which spawns a real shell, so this drives an actual
//! `echo` rather than a scripted fake.

use stella_core::hooks::{HookAction, HookMatcher, Hooks};

use super::*;

fn cfg_with_session_start_hook(command: &str) -> Config {
    let mut cfg = cfg_for("anthropic");
    cfg.hooks = Some(Hooks {
        session_start: Some(vec![HookMatcher {
            matcher: None,
            hooks: vec![HookAction::new(command)],
        }]),
        ..Hooks::default()
    });
    cfg
}

/// A `SessionStart` hook configured on a real `Config` fires through the
/// CLI's own wiring — `session_start_hook_context` → `HostHookRunner` → a
/// real shell — and its stdout lands in the assembled system prompt.
#[tokio::test]
async fn a_configured_session_start_hook_reaches_the_system_prompt() {
    let cfg = cfg_with_session_start_hook("echo 'on-call: bob'");

    let prompt = with_session_hook_context("BASE PROMPT".to_string(), &cfg).await;

    assert!(
        prompt.starts_with("BASE PROMPT"),
        "the hook context is appended, not substituted: {prompt:?}"
    );
    assert!(
        prompt.contains("on-call: bob"),
        "the hook's real stdout must reach the prompt: {prompt:?}"
    );
}

/// A `Config` with no hooks configured leaves the prompt untouched — the
/// wiring must not fire on nothing.
#[tokio::test]
async fn no_hooks_configured_leaves_the_prompt_untouched() {
    let cfg = cfg_for("anthropic");
    assert!(cfg.hooks.is_none());

    let prompt = with_session_hook_context("BASE PROMPT".to_string(), &cfg).await;

    assert_eq!(prompt, "BASE PROMPT");
}
