//! The `/profile` slash command — retune every engine role at once from one
//! word. Kept out of the already-large `command_deck` dispatcher: the parser,
//! the readout, and the settings write live here; `command_deck` only wires
//! them to `say`.
//!
//! The planning itself is [`crate::profile`], which is pure. This module is
//! the I/O half: read the credentials and the catalog, render the plan as
//! prose, and persist it through the same `save_to` the SETTINGS tab calls.

use crate::config::Config;
use crate::profile::{self, Candidate, Plan, Profile};
use crate::settings::EngineAgentKind;

/// `/profile …` — the persistent harness-posture setter.
#[derive(Debug, PartialEq)]
pub enum ProfileCommand {
    /// Bare `/profile` — show what is set and what each profile would choose.
    Show,
    /// `/profile auto` — hand the dials back to the engine's own ladder.
    Auto,
    /// `/profile <name>` — one token to resolve and apply.
    Set(String),
    /// `/profile <two or more tokens>` — not a profile name.
    Usage,
}

/// Parse `trimmed` as a [`ProfileCommand`]; `None` leaves it on the normal
/// path.
pub fn parse_profile_command(trimmed: &str) -> Option<ProfileCommand> {
    let Some((head, rest)) = trimmed.split_once(char::is_whitespace) else {
        // No whitespace to split on: either the bare command or something else
        // entirely.
        return (trimmed == "/profile").then_some(ProfileCommand::Show);
    };
    if head != "/profile" {
        return None;
    }
    let mut words = rest.split_whitespace();
    match (words.next(), words.next()) {
        (Some(word), None) if word.eq_ignore_ascii_case("auto") => Some(ProfileCommand::Auto),
        (Some(name), None) => Some(ProfileCommand::Set(name.to_string())),
        // Trailing whitespace only — the same intent as the bare command.
        (None, _) => Some(ProfileCommand::Show),
        // More than one token is not a profile name.
        _ => Some(ProfileCommand::Usage),
    }
}

/// What the deck should do with a handled `/profile`.
pub struct Reply {
    /// The text to show in the transcript.
    pub message: String,
    /// `true` when settings were written, so an open SETTINGS tab should
    /// re-render from the merged scope chain.
    pub settings_changed: bool,
}

/// Handle every form of `/profile`, or `None` to leave the input on the normal
/// path.
///
/// The whole command lives behind this one call so the deck's dispatcher —
/// already thousands of lines — gains a wiring block rather than a branch per
/// form. That is the same reason the parser and the settings write are in this
/// module and not in `command_deck`.
pub fn handle(cfg: &Config, trimmed: &str) -> Option<Reply> {
    let reply = |message: String, settings_changed: bool| {
        Some(Reply {
            message,
            settings_changed,
        })
    };
    match parse_profile_command(trimmed)? {
        ProfileCommand::Show => reply(current_summary(cfg), false),
        ProfileCommand::Usage => reply(usage(), false),
        ProfileCommand::Auto => match set_auto() {
            Ok(message) => reply(message, true),
            Err(message) => reply(message, false),
        },
        ProfileCommand::Set(name) => match set_profile(cfg, &name) {
            Ok(message) => reply(message, true),
            Err(message) => reply(message, false),
        },
    }
}

/// The usage line, listing every profile and what it means.
pub fn usage() -> String {
    let mut out = String::from(
        "usage: `/profile <fast|balanced|pro|ultra|auto>` — retune every role at once.\n\n",
    );
    for profile in Profile::ALL {
        out.push_str(&format!("  {:<9} {}\n", profile.name(), profile.tagline()));
    }
    out.push_str(&format!("  {:<9} {}\n", "auto", AUTO_TAGLINE));
    out.push_str("\nRun `/profile` alone to see what is set now and what each would choose.");
    out
}

/// What `auto` means, in the same voice as the profile taglines.
const AUTO_TAGLINE: &str =
    "no profile — the engine picks effort per role (judge high, worker medium, triage low)";

/// `/profile auto` — restore the auto switches and drop the per-role effort
/// pins, handing the dials back to the engine. The way out of a profile.
pub fn set_auto() -> Result<String, String> {
    let path = crate::settings::user_config_path().ok_or_else(|| {
        "could not save: the user settings path is unavailable (is $HOME set?)".to_string()
    })?;
    let mut engine = crate::settings::user_engine_config_at(&path);
    profile::restore_auto(&mut engine);
    engine
        .save_to(&path)
        .map_err(|e| format!("could not save: {e}"))?;
    Ok(format!(
        "profile cleared — {AUTO_TAGLINE}.\n\
         effort_auto, reasoning_auto and auto_mode are back on, and the per-role effort, \
         thinking, verbosity and service-tier pins a profile had written are gone.\n\
         Model choices were left alone: a pin may predate the profile, and `auto_mode` \
         re-picks the judge on its own."
    ))
}

/// One `role  model  effort · thinking` line per role.
fn plan_rows(plan: &Plan) -> String {
    let mut out = String::new();
    for kind in EngineAgentKind::ALL {
        let Some(pick) = plan.pick(kind) else {
            continue;
        };
        let model = pick
            .model
            .as_ref()
            .map(Candidate::qualified)
            .unwrap_or_else(|| "(unchanged — no model reachable)".to_string());
        let effort = match pick.effort {
            Some(effort) => crate::engine_config::effort_to_str(effort).to_string(),
            None => "no effort knob".to_string(),
        };
        let thinking = match pick.reasoning {
            Some(true) => "thinking on",
            Some(false) => "thinking off",
            None => "cannot think",
        };
        out.push_str(&format!(
            "  {:<8} {:<38} {effort} · {thinking}\n",
            role_label(kind),
            model
        ));
    }
    out
}

fn role_label(kind: EngineAgentKind) -> &'static str {
    match kind {
        EngineAgentKind::Default => "default",
        EngineAgentKind::Worker => "worker",
        EngineAgentKind::Judge => "judge",
        EngineAgentKind::Triage => "triage",
    }
}

/// The caveats worth saying out loud rather than letting them look like bugs:
/// a thin catalog, and any effort the provider clamped down.
fn plan_notes(plan: &Plan) -> Vec<String> {
    let mut notes = Vec::new();
    if plan.ranked_count == 0 {
        notes.push(
            "no models are reachable — every role keeps what it had. Add a key with \
             `stella auth` or set one in the SETTINGS tab."
                .to_string(),
        );
    } else if plan.ranked_count == 1 {
        notes.push(
            "only one model is reachable, so every profile picks it — the profiles differ \
             by effort alone until a second provider is configured."
                .to_string(),
        );
    }
    if plan.judge_shares_worker_family {
        notes.push(
            "the judge shares the worker's model family — no reachable model was both \
             independent of it and at least as capable, so review is correlated with the \
             work it grades. A key from another vendor fixes this."
                .to_string(),
        );
    }
    for kind in EngineAgentKind::ALL {
        let Some(pick) = plan.pick(kind) else {
            continue;
        };
        let (Some(from), Some(to)) = (pick.effort_downgraded_from, pick.effort) else {
            continue;
        };
        notes.push(format!(
            "{}: effort {} → {} — that provider does not expose the higher rungs.",
            role_label(kind),
            crate::engine_config::effort_to_str(from),
            crate::engine_config::effort_to_str(to),
        ));
    }
    notes
}

fn render_notes(notes: &[String]) -> String {
    if notes.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n");
    for note in notes {
        out.push_str(&format!("  note: {note}\n"));
    }
    out
}

/// The bare-`/profile` readout: which profile the persisted settings match,
/// what this session is actually running, and what each profile would choose
/// from the models these credentials can reach.
pub fn current_summary(cfg: &Config) -> String {
    let candidates = profile::candidates();
    let engine = crate::settings::Settings::load(&cfg.workspace_root)
        .ok()
        .and_then(|s| s.agent_engine_config)
        .unwrap_or_default();
    let current = match profile::detect(&engine, &candidates) {
        Some(profile) => format!("{} — {}", profile.name(), profile.tagline()),
        // `auto` is a real state, not an absence: distinguishing it from a
        // hand-edited config is the difference between "you are on the engine's
        // ladder" and "something here does not match anything".
        None if profile::is_auto(&engine) => format!("auto — {AUTO_TAGLINE}"),
        None => "(custom — no profile matches these settings)".to_string(),
    };

    let mut providers: Vec<&str> = candidates.iter().map(|c| c.provider.as_str()).collect();
    providers.sort_unstable();
    providers.dedup();
    let reach = if candidates.is_empty() {
        "no models reachable — no provider credential resolves".to_string()
    } else {
        format!(
            "{} model{} across {}",
            candidates.len(),
            if candidates.len() == 1 { "" } else { "s" },
            providers.join(", ")
        )
    };

    let mut out = format!(
        "profile (new sessions): {current}\n\
         this session is running: {}/{}\n\
         reachable now:           {reach}\n\n\
         what each profile would choose right now:\n",
        cfg.provider.id, cfg.model_id,
    );
    for name in Profile::ALL {
        let plan = profile::plan(name, &candidates);
        out.push_str(&format!("\n{} — {}\n", name.name(), name.tagline()));
        out.push_str(&plan_rows(&plan));
    }
    out.push_str("\nset one with `/profile <fast|balanced|pro|ultra>`.");
    out
}

/// Resolve `name`, plan it against the reachable models, and persist it as
/// `agent_engine_config` in user-scope settings through the same `save_to` the
/// SETTINGS tab calls. `Ok` carries the confirmation to show; `Err` is a
/// message to show without saving.
pub fn set_profile(cfg: &Config, name: &str) -> Result<String, String> {
    let Some(chosen) = Profile::parse(name) else {
        return Err(format!("no profile named `{name}`.\n\n{}", usage()));
    };
    let candidates = profile::candidates();
    let plan = profile::plan(chosen, &candidates);

    let path = crate::settings::user_config_path().ok_or_else(|| {
        "could not save the profile: the user settings path is unavailable (is $HOME set?)"
            .to_string()
    })?;
    // Load-mutate-save, never construct fresh: `save_to` replaces the whole
    // `agent_engine_config` block, so building a new one would silently drop a
    // custom judge prompt or a pinned temperature.
    //
    // Read the file being written, NOT the merged view: the merged config also
    // carries the project's and the org's opinions, and saving that to user
    // scope would promote this repo's pins to a machine-wide default.
    let mut engine = crate::settings::user_engine_config_at(&path);
    profile::apply(&plan, &mut engine);
    engine
        .save_to(&path)
        .map_err(|e| format!("could not save the profile: {e}"))?;

    let name = chosen.name();
    let tagline = chosen.tagline();
    let (provider, model) = (cfg.provider.id, cfg.model_id.as_str());
    let rows = plan_rows(&plan);
    // The session-wide knobs, in the same column shape as the per-role rows so
    // the block reads as one table.
    let shared = format!(
        "  {:<8} {} verbosity · {} service tier\n",
        "replies",
        crate::engine_config::verbosity_to_str(plan.verbosity),
        crate::engine_config::service_tier_to_str(plan.service_tier),
    );
    let notes = render_notes(&plan_notes(&plan));
    Ok(format!(
        "profile set to {name} — {tagline}.\n\
         Applies to sessions started from now on (this session keeps {provider}/{model}).\n\n\
         {rows}{shared}\n\
         effort_auto, reasoning_auto and auto_mode are now off, so these are the levels that \
         actually run. Per-agent prompts, providers, temperatures and your model list were \
         left as they were.{notes}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_profile_command_takes_exactly_one_name() {
        assert!(matches!(
            parse_profile_command("/profile ultra"),
            Some(ProfileCommand::Set(name)) if name == "ultra"
        ));
        // The value is validated downstream, so an unknown word still parses
        // as a Set and gets a named error rather than a wasted model call.
        assert!(matches!(
            parse_profile_command("/profile turbo"),
            Some(ProfileCommand::Set(name)) if name == "turbo"
        ));
        // More than one token is not a profile name.
        assert!(matches!(
            parse_profile_command("/profile which one should I use"),
            Some(ProfileCommand::Usage)
        ));
        // Bare, or bare with trailing whitespace, is the readout — the two
        // must not diverge just because a space was typed.
        assert_eq!(
            parse_profile_command("/profile"),
            Some(ProfileCommand::Show)
        );
        assert_eq!(
            parse_profile_command("/profile   "),
            Some(ProfileCommand::Show)
        );
        // And it must not swallow a neighbouring command, nor a bare word that
        // merely starts the same way.
        assert_eq!(parse_profile_command("/profiles"), None);
        assert_eq!(parse_profile_command("/profiles list"), None);
        assert_eq!(parse_profile_command("/model zai/glm-5.2"), None);
        assert_eq!(parse_profile_command("what is my profile"), None);
    }

    /// Two families, five models, one provider with no effort knob — enough
    /// to exercise every branch the readout has prose for.
    fn sample() -> Vec<Candidate> {
        vec![
            Candidate {
                provider: "deepseek".into(),
                model: "deepseek-chat".into(),
                family: "deepseek".into(),
                output_usd_per_mtok: 1.0,
                supports_reasoning: Some(true),
                effort_levels: &["low", "medium", "high"],
            },
            Candidate {
                provider: "zai".into(),
                model: "glm-5.2".into(),
                family: "glm".into(),
                output_usd_per_mtok: 5.0,
                supports_reasoning: Some(true),
                effort_levels: &[],
            },
            Candidate {
                provider: "anthropic".into(),
                model: "claude-fable-5".into(),
                family: "anthropic".into(),
                output_usd_per_mtok: 75.0,
                supports_reasoning: Some(true),
                effort_levels: &["low", "medium", "high", "xhigh", "max"],
            },
        ]
    }

    #[test]
    fn the_readout_names_every_role_and_its_model() {
        let plan = profile::plan(Profile::Ultra, &sample());
        let rows = plan_rows(&plan);
        for role in ["default", "worker", "judge", "triage"] {
            assert!(rows.contains(role), "readout omits the {role} row:\n{rows}");
        }
        assert!(rows.contains("anthropic/claude-fable-5"));
        // A provider with no effort control must say so rather than printing a
        // level that never reaches the wire.
        let fast = plan_rows(&profile::plan(Profile::Fast, &sample()));
        assert!(fast.contains("deepseek/deepseek-chat"), "{fast}");
        println!("--- ultra ---\n{rows}\n--- fast ---\n{fast}");
    }

    #[test]
    fn notes_explain_a_clamped_effort_and_a_correlated_judge() {
        // One cheap outsider and a wall of same-family models: the only
        // independent judge is too far below the worker to follow the work, so
        // capability takes over and the correlation must be spoken.
        let correlated = vec![
            Candidate {
                provider: "deepseek".into(),
                model: "deepseek-chat".into(),
                family: "deepseek".into(),
                output_usd_per_mtok: 1.0,
                supports_reasoning: Some(true),
                effort_levels: &["low", "medium", "high"],
            },
            Candidate {
                provider: "anthropic".into(),
                model: "claude-a".into(),
                family: "anthropic".into(),
                output_usd_per_mtok: 40.0,
                supports_reasoning: Some(true),
                effort_levels: &["low", "medium", "high", "xhigh", "max"],
            },
            Candidate {
                provider: "anthropic".into(),
                model: "claude-b".into(),
                family: "anthropic".into(),
                output_usd_per_mtok: 75.0,
                supports_reasoning: Some(true),
                effort_levels: &["low", "medium", "high", "xhigh", "max"],
            },
        ];
        let notes = plan_notes(&profile::plan(Profile::Ultra, &correlated));
        assert!(
            notes.iter().any(|n| n.contains("shares the worker's")),
            "a correlated judge went unreported: {notes:?}"
        );
        // A single model whose provider stops at `high` must report the clamp.
        let capped = vec![Candidate {
            provider: "openai".into(),
            model: "gpt-5.5".into(),
            family: "gpt".into(),
            output_usd_per_mtok: 10.0,
            supports_reasoning: Some(true),
            effort_levels: &["low", "medium", "high"],
        }];
        let notes = plan_notes(&profile::plan(Profile::Ultra, &capped));
        assert!(
            notes.iter().any(|n| n.contains("max → high")),
            "a clamped effort went unreported: {notes:?}"
        );
    }

    #[test]
    fn usage_names_every_profile_and_the_way_out() {
        let text = usage();
        for profile in Profile::ALL {
            assert!(
                text.contains(profile.name()),
                "usage text omits `{}`",
                profile.name()
            );
        }
        assert!(
            text.contains("auto"),
            "usage text omits the way out of a profile"
        );
    }

    #[test]
    fn auto_is_its_own_form_not_a_profile_name() {
        // `auto` must not fall through to `Profile::parse`, which would answer
        // "no profile named `auto`" instead of clearing the profile.
        assert_eq!(
            parse_profile_command("/profile auto"),
            Some(ProfileCommand::Auto)
        );
        assert_eq!(
            parse_profile_command("/profile AUTO"),
            Some(ProfileCommand::Auto)
        );
        assert_eq!(Profile::parse("auto"), None);
    }
}
