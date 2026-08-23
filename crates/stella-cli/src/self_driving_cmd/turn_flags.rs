// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The session flags a turn inherits from the invocation that spawned it.
//!
//! A turn is a child `stella run` ([`super::work::run_turn`]), so every
//! session-wide choice the parent parsed has to be handed on explicitly or it
//! is simply lost. `--model` was: `stella self-driving drive --model
//! anthropic/claude-fable-5` accepted the flag — it is `global = true`, so it
//! registers with every subcommand — and then ran every turn on the project's
//! configured default instead. Two launches against a repository pinned to an
//! OpenRouter model failed every triage turn with `HTTP 402 insufficient
//! credits` while the operator watched a command line that named Anthropic
//! (#4352).
//!
//! Exporting `STELLA_MODEL` worked throughout, which is the evidence that the
//! flag alone was being dropped: the child inherits the environment, so the
//! env spelling of the same setting arrived while the argument did not.
//!
//! A flag that is accepted and ignored is the silent-drop shape AGENTS.md
//! invariant 8 exists to catch, and the help text for `--model` promises the
//! opposite in as many words: *scoped to this invocation*.
//!
//! # Why these and not every global
//!
//! What reaches a turn is what changes how the turn *runs*: routing, the
//! ceilings, and the write authority. The UI globals (`--plain`,
//! `--accessible`, the animation toggles) describe the parent's terminal, and
//! the child's stdout is a pipe.
//!
//! `--api-key` is deliberately **not** forwarded. It is the one global whose
//! forwarding is a credential decision rather than a routing one — it would
//! put the key in a second process's argv, where `ps` can read it for the
//! whole length of a turn — and the credential chain already reaches the child
//! by every other route (`STELLA_API_KEY`, `~/.stella/credentials.toml`). That
//! is a judgement worth making on its own rather than inside this fix.

use std::process::Command;
use std::time::Duration;

/// What the parent invocation decided, in the form a child turn needs.
///
/// Owned rather than borrowed from the clap struct: the loop outlives the
/// argument parse, and `stella_cli::cli` is the CLI's vocabulary rather than
/// the loop's.
// No `Eq`: `spend_limit` is an `f64`, and a USD ceiling has no reflexive
// equality to offer.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct TurnFlags {
    /// `--model` — the worker model, `provider/model_id`.
    pub(crate) model: Option<String>,
    /// `--base-url` — the endpoint override.
    pub(crate) base_url: Option<String>,
    /// `--upstream-pin` — the gateway upstream order, in the order given.
    pub(crate) upstream_pin: Vec<String>,
    /// `--allow-dir` — extra directories a write tool may touch.
    pub(crate) allow_dir: Vec<String>,
    /// `--spend-limit` — the USD ceiling for the whole run.
    ///
    /// Read as the run's cap, never a child turn's (#4353). What each child is
    /// handed is what [`super::budget::RunBudget`] has left of it, so a
    /// caller that reaches for this field directly and passes it to a turn is
    /// re-introducing the defect: ten issues under `--spend-limit 30` spending
    /// thirty dollars each, plus a triage turn per issue on top.
    pub(crate) spend_limit: Option<f64>,
    /// `--turn-timeout` — wall-clock seconds one turn may spend.
    pub(crate) turn_timeout: Option<Duration>,
    /// `--max-output-tokens` — the per-step output ceiling.
    pub(crate) max_output_tokens: Option<u32>,
}

impl TurnFlags {
    /// Read the parent's parsed globals.
    pub(crate) fn from_globals(globals: &crate::cli::GlobalArgs) -> Self {
        Self {
            model: globals.model.clone(),
            base_url: globals.base_url.clone(),
            upstream_pin: globals.upstream_pin.clone(),
            allow_dir: globals.allow_dir.clone(),
            spend_limit: globals.spend_limit,
            turn_timeout: globals.turn_timeout,
            max_output_tokens: globals.max_output_tokens,
        }
    }

    /// Append every flag that was actually set to a child `stella run`.
    ///
    /// Only what the parent set: an absent flag stays absent so the child's
    /// own resolution — settings, `stella.toml`, the model's own ceiling —
    /// still decides, exactly as it does for a `stella run` typed by hand.
    ///
    /// The repeated flag rather than the comma-joined form for the two `Vec`
    /// fields: both accept either, and a value that itself contains a comma
    /// round-trips only through the repeated spelling.
    pub(super) fn push_onto(&self, cmd: &mut Command) {
        if let Some(model) = &self.model {
            cmd.arg("--model").arg(model);
        }
        if let Some(base_url) = &self.base_url {
            cmd.arg("--base-url").arg(base_url);
        }
        for upstream in &self.upstream_pin {
            cmd.arg("--upstream-pin").arg(upstream);
        }
        for dir in &self.allow_dir {
            cmd.arg("--allow-dir").arg(dir);
        }
        if let Some(limit) = self.spend_limit {
            cmd.arg("--spend-limit").arg(limit.to_string());
        }
        if let Some(timeout) = self.turn_timeout {
            // Seconds, which is what `--turn-timeout` parses: the parent's
            // value came from the same spelling, so this round-trips.
            cmd.arg("--turn-timeout")
                .arg(timeout.as_secs_f64().to_string());
        }
        if let Some(cap) = self.max_output_tokens {
            cmd.arg("--max-output-tokens").arg(cap.to_string());
        }
    }
}
