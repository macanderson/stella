//! The `hooks.lifecycle` parity row's CLI witness.
//!
//! `stella-core` tests its own hook matching and blocking. But that does not
//! prove a hook set in a real `Config` reaches the CLI's own wiring, not
//! just the core crate's. These tests call the real `HostHookRunner`, which
//! spawns a real shell. So they run an actual `echo`, not a scripted fake.

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

/// A `SessionStart` hook set on a real `Config` fires through the CLI's own
/// wiring. Its stdout lands in the system prompt.
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

// ---- UserPromptSubmit ---------------------------------------------------

fn cfg_with_user_prompt_submit_hook(command: &str) -> Config {
    let mut cfg = cfg_for("anthropic");
    cfg.hooks = Some(Hooks {
        user_prompt_submit: Some(vec![HookMatcher {
            matcher: None,
            hooks: vec![HookAction::new(command)],
        }]),
        ..Hooks::default()
    });
    cfg
}

/// A `Config` with no hooks set leaves the prompt as-is, same as
/// `SessionStart`'s wiring.
#[tokio::test]
async fn no_hooks_configured_leaves_the_submitted_prompt_untouched() {
    let cfg = cfg_for("anthropic");
    assert!(cfg.hooks.is_none());

    let prompt = user_prompt_submit_hook(&cfg, "fix the flaky test").await;

    assert_eq!(prompt, Ok("fix the flaky test".to_string()));
}

/// A `UserPromptSubmit` hook set on a real `Config` fires through the CLI's
/// own wiring. A `deny` decision rejects the prompt with the hook's reason.
/// No turn runs.
#[tokio::test]
async fn a_deny_decision_rejects_the_prompt_with_the_hooks_reason() {
    let cfg = cfg_with_user_prompt_submit_hook(
        r#"echo '{"action":"deny","reason":"no prompts before coffee"}'"#,
    );

    let result = user_prompt_submit_hook(&cfg, "fix the flaky test").await;

    assert_eq!(result, Err("no prompts before coffee".to_string()));
}

/// A `modify` decision rewrites the prompt the turn actually runs with.
#[tokio::test]
async fn a_modify_decision_rewrites_the_prompt() {
    let cfg = cfg_with_user_prompt_submit_hook(
        r#"echo '{"action":"modify","payload":{"prompt":"fix the flaky retry test"}}'"#,
    );

    let result = user_prompt_submit_hook(&cfg, "fix the flaky test").await;

    assert_eq!(result, Ok("fix the flaky retry test".to_string()));
}

/// A hook that never runs fails closed, the same way `PreToolUse` does.
/// This is a permission gate, not a turn-boundary gate. A broken hook must
/// not wave a prompt through.
#[tokio::test]
async fn a_broken_hook_rejects_the_prompt_rather_than_waving_it_through() {
    let cfg = cfg_with_user_prompt_submit_hook("/no/such/command/2836");

    let result = user_prompt_submit_hook(&cfg, "fix the flaky test").await;

    assert!(
        result.is_err(),
        "a hook that never ran must fail closed: {result:?}"
    );
}

/// A hook that allows without modifying leaves the prompt exactly as typed.
#[tokio::test]
async fn an_allowing_hook_leaves_the_prompt_untouched() {
    let cfg = cfg_with_user_prompt_submit_hook("exit 0");

    let result = user_prompt_submit_hook(&cfg, "fix the flaky test").await;

    assert_eq!(result, Ok("fix the flaky test".to_string()));
}
