//! What each role will *actually* run — the per-role half of `stella config`.
//!
//! `stella config` used to report only the session's provider, model and
//! credential, and none of the shaping that rides with it: a model whose slug
//! is spelled for the wrong gateway, an `effort` the auto-mode is silently
//! replacing. None of it was visible without reading the resolver.
//!
//! Worse, it was not *checkable*. Confirming a settings change meant editing
//! in a deliberate typo to see whether stella complained about the key next to
//! it — an inference from silence. This module makes the answer printable:
//! for every role, the model that will be sent, the effort and thinking that
//! ride with it, and the setting that decided each.
//!
//! It resolves through the same functions the request path uses
//! ([`model_spec_for`], [`tuning_for`], [`AgentEngineConfig::model_for`]'s
//! precedence) rather than re-deriving the rules, because a config report that
//! can disagree with the engine is worse than none.
//!
//! # More than one row, and why it is still a `Vec`
//!
//! It reported six rows until #3908 — `default`, `worker`, `verifier`,
//! `triage`, `research`, `plan` — of which four resolved settings keys that
//! had stopped steering anything when the staged pipeline left (#3865). A
//! report whose whole purpose is "what will actually run" was answering for
//! four roles that would never run, which is the most expensive place in the
//! product to be wrong.
//!
//! It stayed a `Vec` past that collapse to one row, and #6088 is why: a
//! plugin-declared seat resolves through the same [`resolve`] and lands in
//! the same list, right after the `default` row. [`RoleWiring::role`] is a
//! `String` for that reason: the deck's `envelope::roles::role_table` already
//! renders a row for a role it has never heard of, so a seat needs no new
//! plumbing here — only a row.

use crate::engine_config::{ModelSpec, model_spec_for, parse_model_spec, tuning_for};
use crate::settings::AgentEngineConfig;
use stella_protocol::ReasoningEffort;

/// The session's own role. [`resolve`] appends one row per plugin-declared
/// seat after it (#6088).
pub const DEFAULT_ROLE: &str = "default";

/// One role's resolved wiring.
#[derive(Debug, Clone, PartialEq)]
pub struct RoleWiring {
    /// The role's name — [`DEFAULT_ROLE`], or a plugin-declared seat's key
    /// (`<plugin-id>/<role>`). A `String` because core does not enumerate the
    /// possibilities.
    pub role: String,
    /// Provider id and the slug as it goes on the wire.
    pub model: ModelSpec,
    /// The setting that decided `model` — the string a user would edit.
    pub source: ModelSource,
    pub effort: Option<ReasoningEffort>,
    pub reasoning: Option<bool>,
    /// Set when `effort_auto` replaced an effort the user pinned for this
    /// role. The pinned value is carried so the report can name what was
    /// discarded — the failure this whole module exists to surface.
    pub effort_auto_replaced: Option<ReasoningEffort>,
    /// Same, for `reasoning_auto` over a pinned `reasoning`.
    pub reasoning_auto_replaced: Option<bool>,
}

/// Which setting decided a role's model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSource {
    /// `--model` / `STELLA_MODEL`. Pins the session for this invocation and
    /// suppresses the settings spec (see `resolve_engine_wiring`).
    Flag,
    /// `agents.default.model`, which outranks the flat key.
    PerAgent,
    /// `default_model`.
    DefaultModel,
    /// Nothing named it: the role rides the session's resolved model, which
    /// is the provider row's own default when settings are silent too. Also
    /// what an unassigned or unresolvable seat falls back to.
    SessionDefault,
    /// `seat_models.<key>` — a plugin-declared seat the user assigned a
    /// model to. Carries the key so the label names the exact entry.
    Seat(String),
}

impl ModelSource {
    /// The settings key a user would edit to change this, as they would type
    /// it. Not a description — a path.
    pub fn label(&self) -> String {
        match self {
            Self::Flag => "--model (this invocation)".to_string(),
            Self::PerAgent => format!("agents.{DEFAULT_ROLE}.model"),
            Self::DefaultModel => "default_model".to_string(),
            Self::SessionDefault => "session default".to_string(),
            Self::Seat(key) => format!("seat_models.{key}"),
        }
    }
}

/// Which setting fed [`AgentEngineConfig::model_for`], mirroring its
/// precedence exactly: `agents.default.model` > `default_model`.
fn model_source(engine: &AgentEngineConfig) -> ModelSource {
    if engine.agent().and_then(|a| a.model.as_deref()).is_some() {
        ModelSource::PerAgent
    } else if engine.default_model.is_some() {
        ModelSource::DefaultModel
    } else {
        ModelSource::SessionDefault
    }
}

/// Resolve every role of the session: the `default` row, then one row per
/// plugin-declared seat.
///
/// `session` is the model the session actually resolved to (`Config`'s own
/// provider + model), which is what a role falls back to when no setting
/// names one — including the case where settings are absent entirely.
///
/// `model_pinned_by_flag` reproduces `resolve_engine_wiring`'s rule: an
/// explicit `--model` is a per-invocation pin, so it suppresses the settings
/// spec for the `default` row. Reporting the settings value there would name
/// a model that is not going to run. A `--model` flag pins only the session's
/// own role — a seat's assignment is untouched by it, matching what actually
/// runs (`resolve_seat_models` reads `seat_models` unconditionally).
///
/// `declared` is `(seat key, plugin display name)` — the pairs
/// [`crate::agent::seats::installed_seats`] builds from the installed
/// roster. Pass `&[]` for a caller with no seats to resolve (a test, or a
/// surface that only cares about the session's own role).
pub fn resolve(
    engine: Option<&AgentEngineConfig>,
    session: &ModelSpec,
    model_pinned_by_flag: bool,
    is_provider: &dyn Fn(&str) -> bool,
    declared: &[(String, String)],
) -> Vec<RoleWiring> {
    let default_row = match engine {
        None => RoleWiring {
            role: DEFAULT_ROLE.to_string(),
            model: session.clone(),
            source: ModelSource::SessionDefault,
            effort: None,
            reasoning: None,
            effort_auto_replaced: None,
            reasoning_auto_replaced: None,
        },
        Some(engine) => {
            let spec = (!model_pinned_by_flag)
                .then(|| model_spec_for(engine, is_provider))
                .flatten();
            let (model, source) = match spec {
                Some(spec) => (spec, model_source(engine)),
                None if model_pinned_by_flag => (session.clone(), ModelSource::Flag),
                None => (session.clone(), ModelSource::SessionDefault),
            };
            let tuning = tuning_for(engine);
            // What the user pinned, to compare against what auto resolved.
            let pinned = engine.agent();
            let effort_auto_replaced = pinned
                .and_then(|a| a.effort)
                .filter(|_| engine.effort_auto_on());
            let reasoning_auto_replaced = pinned
                .and_then(|a| a.reasoning)
                .map(|t| t.is_on())
                .filter(|_| engine.reasoning_auto_on());
            RoleWiring {
                role: DEFAULT_ROLE.to_string(),
                model,
                source,
                effort: tuning.effort,
                reasoning: tuning.reasoning,
                effort_auto_replaced,
                reasoning_auto_replaced,
            }
        }
    };
    let mut wiring = vec![default_row];
    wiring.extend(resolve_seats(engine, declared, session, is_provider));
    wiring
}

/// Resolve every declared seat's wiring: one [`RoleWiring`] per entry in
/// `declared`, in order.
///
/// A seat's model comes from `engine.seat_models`. It is looked up and
/// parsed the same way [`crate::agent::seats::resolve_seat_models`] does for
/// a real turn. An unassigned seat, or one naming no configured provider,
/// falls back to the session's own model. That is the answer the seat
/// actually gets. This report never refuses a run; it only says what would
/// happen.
///
/// A seat has no effort or reasoning setting of its own yet. Those cells
/// read "provider default" and "thinking default", and the auto-replaced
/// fields stay `None`.
fn resolve_seats(
    engine: Option<&AgentEngineConfig>,
    declared: &[(String, String)],
    session: &ModelSpec,
    is_provider: &dyn Fn(&str) -> bool,
) -> Vec<RoleWiring> {
    let assignments = engine.and_then(|e| e.seat_models.as_ref());
    declared
        .iter()
        .map(|(key, _from)| {
            let (model, source) = assignments
                .and_then(|map| map.get(key))
                .and_then(|raw| parse_model_spec(raw, is_provider))
                .map(|spec| (spec, ModelSource::Seat(key.clone())))
                .unwrap_or_else(|| (session.clone(), ModelSource::SessionDefault));
            RoleWiring {
                role: key.clone(),
                model,
                source,
                effort: None,
                reasoning: None,
                effort_auto_replaced: None,
                reasoning_auto_replaced: None,
            }
        })
        .collect()
}

/// How an effort renders, including the auto-mode's theft when it happened.
fn effort_cell(row: &RoleWiring) -> String {
    let resolved = row
        .effort
        .map(|e| effort_word(e).to_string())
        .unwrap_or_else(|| "provider default".to_string());
    match row.effort_auto_replaced {
        // Only worth saying when auto actually CHANGED the answer — an
        // `effort_auto` that happens to agree with the pin is not a surprise
        // anyone needs warning about.
        Some(pinned) if Some(pinned) != row.effort => {
            format!(
                "{resolved}  (effort_auto replaced \"{}\")",
                effort_word(pinned)
            )
        }
        _ => resolved,
    }
}

/// Same for thinking.
fn reasoning_cell(row: &RoleWiring) -> String {
    let resolved = match row.reasoning {
        Some(true) => "thinking on",
        Some(false) => "thinking off",
        None => "thinking default",
    };
    match row.reasoning_auto_replaced {
        Some(pinned) if Some(pinned) != row.reasoning => {
            format!(
                "{resolved}  (reasoning_auto replaced \"{}\")",
                if pinned { "on" } else { "off" }
            )
        }
        _ => resolved.to_string(),
    }
}

fn effort_word(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Xhigh => "xhigh",
        ReasoningEffort::Max => "max",
    }
}

/// The report as plain rows (`role`, `provider/model`, `effort`, `thinking`,
/// `source`), column-aligned. Returned rather than printed so the same text is
/// what tests assert on.
pub fn rows(wiring: &[RoleWiring]) -> Vec<[String; 5]> {
    wiring
        .iter()
        .map(|row| {
            [
                row.role.clone(),
                format!("{}/{}", row.model.provider, row.model.model),
                effort_cell(row),
                reasoning_cell(row),
                row.source.label(),
            ]
        })
        .collect()
}

/// Render the rows as the indented block `stella config` prints, padded so the
/// columns line up. Plain text — the caller colorizes.
pub fn render(wiring: &[RoleWiring]) -> Vec<String> {
    let rows = rows(wiring);
    let widths = (0..4).map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0));
    let widths: Vec<usize> = widths.collect();
    rows.iter()
        .map(|r| {
            let mut line = String::new();
            for (i, cell) in r.iter().take(4).enumerate() {
                line.push_str(cell);
                line.push_str(&" ".repeat(widths[i].saturating_sub(cell.chars().count()) + 2));
            }
            line.push_str(&r[4]);
            line.trim_end().to_string()
        })
        .collect()
}

/// The same five cells [`rows`] renders, handed to the deck as the `/models`
/// dialog's row type.
///
/// Resolved here rather than in the TUI on purpose: this module is the one
/// place that mirrors the request path's precedence, so the dialog and
/// `stella config` cannot give different answers to "what will the verifier
/// run". `providers` is the caller's already-computed
/// [`crate::config::discover_configured_providers`] id list — the same
/// vocabulary `print_role_wiring` resolves a `provider/` prefix against.
///
/// Reads `cfg.engine_settings` (what THIS session resolved at start), not a
/// fresh scope-chain load: a settings edit made mid-session applies to runs
/// started from now on, and a dialog that showed it as though it were in
/// force would misreport the very thing it exists to answer.
///
/// `declared` is the same `(seat key, plugin name)` list the SEATS pane's
/// snapshot carries ([`crate::agent::seats::installed_seats`]) — passed in
/// rather than re-read here so a caller that already loaded the roster for
/// the SEATS pane does not load it twice.
pub fn deck_rows(
    cfg: &crate::config::Config,
    providers: &[String],
    declared: &[(String, String)],
) -> Vec<stella_tui::envelope::RoleWiringRow> {
    let is_provider = |id: &str| providers.iter().any(|p| p == id);
    let session = ModelSpec {
        provider: cfg.provider.id.to_string(),
        model: cfg.model_id.clone(),
    };
    let wiring = resolve(
        cfg.engine_settings.as_ref(),
        &session,
        cfg.model_pinned_by_flag,
        &is_provider,
        declared,
    );
    // The same resolution against the settings **as saved on disk right now**,
    // which is what a session started from here would get. `next` is only ever
    // read to say "this cell would differ" — the running answer above is never
    // taken from it.
    let next = next_session_wiring(cfg, &session, &is_provider, declared);

    rows(&wiring)
        .into_iter()
        .map(
            |[role, model, effort, thinking, source]| stella_tui::envelope::RoleWiringRow {
                next_session: next.as_ref().and_then(|next| {
                    let row = next.iter().find(|n| n[0] == role)?;
                    next_session_note(&[&model, &effort, &thinking, &source], row)
                }),
                role,
                model,
                effort,
                thinking,
                source,
            },
        )
        .collect()
}

/// Re-resolve every role from the settings as they sit on disk *now*.
///
/// `None` when the settings cannot be read — an unreadable file is not
/// evidence of a pending change, and the dialog must not claim one.
fn next_session_wiring(
    cfg: &crate::config::Config,
    session: &ModelSpec,
    is_provider: &impl Fn(&str) -> bool,
    declared: &[(String, String)],
) -> Option<Vec<[String; 5]>> {
    let saved = crate::settings::Settings::load(&cfg.workspace_root)
        .ok()?
        .agent_engine_config;
    // This uses the same session pin, `--model` flag and declared seats as
    // the running answer. So a difference here is a settings edit, and
    // nothing else.
    Some(rows(&resolve(
        saved.as_ref(),
        session,
        cfg.model_pinned_by_flag,
        is_provider,
        declared,
    )))
}

/// The cells of `next` that differ from `current`, joined the way the rest of
/// this module joins a role's detail — or `None` when nothing differs.
///
/// Only the differing cells, because the note sits *under* a row that already
/// prints all four: repeating the ones that agree would bury the change that
/// is the entire reason the note is there.
fn next_session_note(current: &[&str; 4], next: &[String; 5]) -> Option<String> {
    // `next[0]` is the role key; cells 1..=4 line up with `current`.
    let changed: Vec<&str> = (1..=4)
        .filter(|i| next[*i] != current[i - 1])
        .map(|i| next[i].as_str())
        .collect();
    (!changed.is_empty()).then(|| changed.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{AgentEngineAgent, AgentEngineAgents, Toggle};

    fn spec(provider: &str, model: &str) -> ModelSpec {
        ModelSpec {
            provider: provider.to_string(),
            model: model.to_string(),
        }
    }

    fn known(id: &str) -> bool {
        matches!(id, "openrouter" | "anthropic" | "zai")
    }

    /// Every test above resolves with no declared seats, so the `default`
    /// row is the whole answer — this unwraps it.
    fn only(wiring: &[RoleWiring]) -> &RoleWiring {
        assert_eq!(wiring.len(), 1, "no seats declared: {wiring:#?}");
        &wiring[0]
    }

    /// With no engine settings at all, the row rides the session model and
    /// says so — the report must never imply a pin that does not exist.
    #[test]
    fn absent_settings_report_the_session_model() {
        let session = spec("openrouter", "moonshotai/kimi-k3");
        let wiring = resolve(None, &session, false, &known, &[]);
        let row = only(&wiring);
        assert_eq!(row.role, DEFAULT_ROLE);
        assert_eq!(row.model, session);
        assert_eq!(row.source, ModelSource::SessionDefault);
        assert_eq!(row.effort, None);
        assert_eq!(row.reasoning, None);
    }

    /// The posture this feature was written to make checkable: the row names
    /// the key that decided it, in the spelling a user would edit.
    #[test]
    fn the_row_names_the_setting_that_decided_its_model() {
        let engine = AgentEngineConfig {
            default_model: Some("openrouter/anthropic/claude-opus-5".into()),
            ..AgentEngineConfig::default()
        };
        let session = spec("openrouter", "moonshotai/kimi-k3");
        let wiring = resolve(Some(&engine), &session, false, &known, &[]);
        let row = only(&wiring);
        assert_eq!(row.model.model, "anthropic/claude-opus-5");
        assert_eq!(row.source, ModelSource::DefaultModel);
        assert_eq!(row.source.label(), "default_model");
    }

    /// `agents.default.model` outranks the flat key, and the report has to say
    /// which one won — editing the flat key when the per-agent one is set is
    /// the change that appears to do nothing.
    #[test]
    fn a_per_agent_model_outranks_the_flat_key_in_the_report() {
        let engine = AgentEngineConfig {
            default_model: Some("openrouter/moonshotai/kimi-k3".into()),
            agents: Some(AgentEngineAgents {
                default: Some(AgentEngineAgent {
                    model: Some("openrouter/anthropic/claude-fable-5".into()),
                    ..AgentEngineAgent::default()
                }),
            }),
            ..AgentEngineConfig::default()
        };
        let session = spec("openrouter", "moonshotai/kimi-k3");
        let wiring = resolve(Some(&engine), &session, false, &known, &[]);
        let row = only(&wiring);
        assert_eq!(row.model.model, "anthropic/claude-fable-5");
        assert_eq!(row.source.label(), "agents.default.model");
    }

    /// The bug this exists to catch. `effort_auto: on` silently replaces a
    /// pinned effort with its own rung, so a config that reads `"effort":
    /// "max"` runs at medium. The row must show the resolved value AND name
    /// what was discarded.
    #[test]
    fn effort_auto_replacing_a_pin_is_named_in_the_row() {
        let engine = AgentEngineConfig {
            default_model: Some("openrouter/moonshotai/kimi-k3".into()),
            effort_auto: Some(Toggle::On),
            agents: Some(AgentEngineAgents {
                default: Some(AgentEngineAgent {
                    effort: Some(ReasoningEffort::Max),
                    ..AgentEngineAgent::default()
                }),
            }),
            ..AgentEngineConfig::default()
        };
        let session = spec("openrouter", "moonshotai/kimi-k3");
        let wiring = resolve(Some(&engine), &session, false, &known, &[]);
        let row = only(&wiring);
        assert_eq!(
            row.effort,
            Some(ReasoningEffort::Medium),
            "effort_auto's rung is what actually runs"
        );
        assert_eq!(row.effort_auto_replaced, Some(ReasoningEffort::Max));
        let cell = effort_cell(row);
        assert!(
            cell.contains("medium") && cell.contains("effort_auto replaced \"max\""),
            "the row states both what runs and what was thrown away: {cell}"
        );
    }

    /// An auto mode that happens to agree with the pin is not a surprise, and
    /// saying so on every row would train people to ignore the warning.
    #[test]
    fn an_auto_mode_that_agrees_with_the_pin_says_nothing() {
        let engine = AgentEngineConfig {
            default_model: Some("openrouter/moonshotai/kimi-k3".into()),
            effort_auto: Some(Toggle::On),
            agents: Some(AgentEngineAgents {
                default: Some(AgentEngineAgent {
                    // The same rung `effort_auto` resolves to.
                    effort: Some(ReasoningEffort::Medium),
                    ..AgentEngineAgent::default()
                }),
            }),
            ..AgentEngineConfig::default()
        };
        let session = spec("openrouter", "moonshotai/kimi-k3");
        let wiring = resolve(Some(&engine), &session, false, &known, &[]);
        let cell = effort_cell(only(&wiring));
        assert_eq!(cell, "medium", "no noise when nothing was replaced: {cell}");
    }

    /// `--model` pins the model for one invocation and suppresses the settings
    /// spec. Reporting the settings value there would name a model that will
    /// not run — the exact mismeasurement this report exists to prevent.
    #[test]
    fn an_explicit_model_flag_owns_the_row() {
        let engine = AgentEngineConfig {
            default_model: Some("openrouter/z-ai/glm-5.2".into()),
            ..AgentEngineConfig::default()
        };
        let session = spec("anthropic", "claude-sonnet-5");
        let wiring = resolve(Some(&engine), &session, true, &known, &[]);
        let row = only(&wiring);
        assert_eq!(row.model, session, "the flag's model is what runs");
        assert_eq!(row.source, ModelSource::Flag);
        assert_eq!(row.source.label(), "--model (this invocation)");
    }

    /// The rendered block carries the source, and the columns are padded from
    /// the widest cell rather than a fixed width.
    #[test]
    fn the_rendered_block_aligns_and_keeps_the_source_on_the_row() {
        let engine = AgentEngineConfig {
            default_model: Some("openrouter/anthropic/claude-opus-5".into()),
            ..AgentEngineConfig::default()
        };
        let session = spec("openrouter", "moonshotai/kimi-k3");
        let lines = render(&resolve(Some(&engine), &session, false, &known, &[]));
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains("default_model"),
            "the row names its source: {}",
            lines[0]
        );
        assert!(
            lines[0].starts_with(DEFAULT_ROLE),
            "the row leads with the role name: {}",
            lines[0]
        );
        // No trailing padding survives into the rendered line.
        assert_eq!(lines[0], lines[0].trim_end());
    }

    /// The #6088 witness: a declared seat becomes a second row, right after
    /// `default`, and an unassigned one runs on the session's own model
    /// rather than inventing one. Fails on the old `resolve` by construction
    /// — it took no `declared` parameter at all.
    #[test]
    fn a_declared_seat_with_no_assignment_runs_on_the_session_model() {
        let session = spec("openrouter", "moonshotai/kimi-k3");
        let declared = vec![("acme/reviewer".to_string(), "acme".to_string())];
        let wiring = resolve(None, &session, false, &known, &declared);
        assert_eq!(wiring.len(), 2, "the default row, then the declared seat");
        assert_eq!(wiring[0].role, DEFAULT_ROLE);
        let seat = &wiring[1];
        assert_eq!(seat.role, "acme/reviewer");
        assert_eq!(seat.model, session, "unassigned rides the session model");
        assert_eq!(seat.source, ModelSource::SessionDefault);
    }

    /// A seat the user assigned a model to names `seat_models.<key>` as its
    /// source — the exact entry a user would edit to change it — and its row
    /// carries that model rather than the session's.
    #[test]
    fn a_declared_seat_with_an_assignment_names_the_seat_models_key() {
        let mut seat_models = std::collections::BTreeMap::new();
        seat_models.insert(
            "acme/reviewer".to_string(),
            "anthropic/claude-opus-5".to_string(),
        );
        let engine = AgentEngineConfig {
            seat_models: Some(seat_models),
            ..AgentEngineConfig::default()
        };
        let session = spec("openrouter", "moonshotai/kimi-k3");
        let declared = vec![("acme/reviewer".to_string(), "acme".to_string())];
        let wiring = resolve(Some(&engine), &session, false, &known, &declared);
        assert_eq!(wiring.len(), 2);
        let seat = &wiring[1];
        assert_eq!(seat.model.provider, "anthropic");
        assert_eq!(seat.model.model, "claude-opus-5");
        assert_eq!(seat.source.label(), "seat_models.acme/reviewer");
    }

    /// An assignment naming no configured provider cannot resolve at request
    /// time either. `resolve_seat_models` falls back to the session model
    /// then. This report must say the same thing, not a model that will
    /// never run.
    #[test]
    fn a_seat_assignment_that_names_no_provider_falls_back_to_the_session() {
        let mut seat_models = std::collections::BTreeMap::new();
        seat_models.insert("acme/reviewer".to_string(), "no-such-model".to_string());
        let engine = AgentEngineConfig {
            seat_models: Some(seat_models),
            ..AgentEngineConfig::default()
        };
        let session = spec("openrouter", "moonshotai/kimi-k3");
        let declared = vec![("acme/reviewer".to_string(), "acme".to_string())];
        let wiring = resolve(Some(&engine), &session, false, &known, &declared);
        let seat = &wiring[1];
        assert_eq!(seat.model, session);
        assert_eq!(seat.source, ModelSource::SessionDefault);
    }

    /// Two declared seats resolve in the order they were declared, each
    /// keeping its own key — a plugin's process can have more than one
    /// participant and the report must not collapse or reorder them.
    #[test]
    fn several_declared_seats_resolve_in_order() {
        let session = spec("openrouter", "moonshotai/kimi-k3");
        let declared = vec![
            ("acme/planner".to_string(), "acme".to_string()),
            ("acme/reviewer".to_string(), "acme".to_string()),
        ];
        let wiring = resolve(None, &session, false, &known, &declared);
        let roles: Vec<&str> = wiring.iter().map(|r| r.role.as_str()).collect();
        assert_eq!(roles, [DEFAULT_ROLE, "acme/planner", "acme/reviewer"]);
    }
}
