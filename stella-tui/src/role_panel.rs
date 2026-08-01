//! The deck's pipeline-roles panel: the pure span builder behind the
//! statline's `MODELS` cell.
//!
//! The deck used to show one `MODEL`, read off the tail of the route log
//! (`WorkspaceModel::latest_model`). That had two faults the head-to-head
//! bench runs kept tripping over. It named a single model where the staged
//! pipeline runs three different pins — triage, worker and judge — so the
//! cell answered "what ran last" when the question is "what is this session
//! configured to run". And it named the model without the provider, which is
//! only half an identity when the same slug is reachable through more than
//! one upstream at different prices.
//!
//! Both halves were already in the event stream: `AgentEvent::StepUsage`
//! carries `role`, `provider` and `model` together, and the fold was keeping
//! only the last of the three. This module turns the folded per-role pins
//! into the compact cell the statline draws.
//!
//! Kept out of `deck.rs` so the model layer stays free of ratatui types, and
//! out of `deck_render.rs` so the formatting is unit-testable without a full
//! frame — the same split [`crate::cache_panel`] makes, for the same reasons.

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::deck::{PipelineRole, WorkspaceModel};
use crate::theme;

/// `MODELS` cell: the pin serving each of triage / worker / judge, with the
/// role that most recently served drawn in the brand accent.
///
/// A role that has not served yet reads `—` rather than borrowing another
/// role's pin or showing a configured default. The distinction matters in a
/// scored run: "the judge is pinned to X" and "the judge ran on X" are
/// different claims, and only the second one is evidence.
///
/// `highlight` gates the accent. The caller passes `false` when nothing is
/// active, so a finished session shows its three pins without implying one of
/// them is still working. A lead that is only monitoring subagents its own
/// session spawned still counts as active — it is holding the turn open.
pub fn models_cell(model: &WorkspaceModel, highlight: bool) -> Vec<Span<'static>> {
    let label = Style::default().fg(theme::TEXT_TERTIARY);
    let val = Style::default().fg(theme::TEXT_PRIMARY);
    let active = Style::default()
        .fg(theme::ACCENT)
        .add_modifier(Modifier::BOLD);

    let mut spans = Vec::with_capacity(PipelineRole::ORDER.len() * 2);
    for (i, role) in PipelineRole::ORDER.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", label));
        }
        let is_active = highlight && model.active_role == Some(*role);
        spans.push(Span::styled(
            format!("{}·", role.initial()),
            if is_active { active } else { label },
        ));
        match model.role_pins.get(role) {
            Some(pin) => {
                let style = if is_active {
                    active
                } else if pin.served {
                    val
                } else {
                    // Configured but not yet reached. Drawn at label weight so
                    // intent never reads as evidence — a judge that has not
                    // run must not look like one that has.
                    label
                };
                spans.push(Span::styled(pin.slug(), style));
            }
            None => spans.push(Span::styled("—", label)),
        }
    }
    spans
}

#[cfg(test)]
mod tests_support {
    use crate::deck::{PipelineRole, RolePin, WorkspaceModel};
    use crate::envelope::{AgentMeta, Inbound};
    use stella_protocol::{AgentEvent, ModelCallRole};

    /// The driver's startup envelope, as it would arrive before any turn.
    pub fn configured(pins: &[(PipelineRole, &str, &str)]) -> Inbound {
        Inbound::ConfiguredRoles(
            pins.iter()
                .map(|(role, provider, model)| {
                    (
                        *role,
                        RolePin {
                            provider: (*provider).to_string(),
                            model: (*model).to_string(),
                            served: false,
                        },
                    )
                })
                .collect(),
        )
    }

    pub fn step_usage(role: ModelCallRole, provider: &str, model: &str) -> AgentEvent {
        AgentEvent::StepUsage {
            step: 1,
            role,
            provider: provider.to_string(),
            output_text: None,
            model: model.to_string(),
            input_tokens: 10,
            output_tokens: 5,
            cached_input_tokens: 0,
            cache_write_tokens: 0,
            estimated_input_tokens: 0,
            cost_usd: 0.01,
            duration_ms: 100,
            retries: 0,
            tool_calls: 0,
            complete: true,
        }
    }

    pub fn deck_with(events: &[AgentEvent]) -> WorkspaceModel {
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        for event in events {
            model.apply_inbound(&Inbound::Event {
                agent: "lead".into(),
                event: event.clone(),
            });
        }
        model
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::*;
    use super::*;
    use stella_protocol::ModelCallRole;

    fn rendered(spans: &[Span<'static>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The defect: before any step lands there is nothing to show, and the
    /// cell must say so for each role rather than collapsing to one dash that
    /// reads as "one model, unknown".
    #[test]
    fn every_role_reads_as_unknown_before_any_step() {
        let model = deck_with(&[]);
        assert_eq!(rendered(&models_cell(&model, true)), "T·—  W·—  J·—");
    }

    #[test]
    fn a_served_role_shows_its_provider_and_model() {
        let model = deck_with(&[step_usage(ModelCallRole::Worker, "moonshot", "kimi-k3")]);
        assert_eq!(
            rendered(&models_cell(&model, true)),
            "T·—  W·moonshot/kimi-k3  J·—"
        );
    }

    /// All three roles at once — the whole point of the cell.
    #[test]
    fn the_three_pipeline_roles_are_shown_together() {
        let model = deck_with(&[
            step_usage(ModelCallRole::Triage, "zai", "glm-5.2"),
            step_usage(ModelCallRole::Worker, "moonshot", "kimi-k3"),
            step_usage(ModelCallRole::Judge, "anthropic", "opus-5"),
        ]);
        assert_eq!(
            rendered(&models_cell(&model, true)),
            "T·zai/glm-5.2  W·moonshot/kimi-k3  J·anthropic/opus-5"
        );
    }

    /// The accent follows whichever role served most recently.
    #[test]
    fn the_highlight_follows_the_most_recent_role() {
        let model = deck_with(&[
            step_usage(ModelCallRole::Triage, "zai", "glm-5.2"),
            step_usage(ModelCallRole::Worker, "moonshot", "kimi-k3"),
        ]);
        let spans = models_cell(&model, true);
        let accented: Vec<&str> = spans
            .iter()
            .filter(|s| s.style.fg == Some(theme::ACCENT))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(accented, vec!["W·", "moonshot/kimi-k3"]);
    }

    /// A finished session shows its pins without implying one is still
    /// working — otherwise the deck claims live activity after the turn ended.
    #[test]
    fn nothing_is_accented_when_nothing_is_active() {
        let model = deck_with(&[step_usage(ModelCallRole::Worker, "moonshot", "kimi-k3")]);
        let spans = models_cell(&model, false);
        assert!(
            !spans.iter().any(|s| s.style.fg == Some(theme::ACCENT)),
            "an idle session accented a role: {:?}",
            rendered(&spans)
        );
    }

    /// The whole main line of work is one pin, so planning and witness
    /// authoring land in the worker slot rather than inventing slots of their
    /// own — three copies of one model would imply three configured models.
    #[test]
    fn planning_and_witness_calls_fold_into_the_worker_slot() {
        let model = deck_with(&[
            step_usage(ModelCallRole::Plan, "moonshot", "kimi-k3"),
            step_usage(ModelCallRole::WitnessAuthor, "moonshot", "kimi-k3"),
        ]);
        assert_eq!(
            rendered(&models_cell(&model, true)),
            "T·—  W·moonshot/kimi-k3  J·—"
        );
    }

    /// Auxiliary roles are real calls but not part of the compared pipeline,
    /// so they must not overwrite a slot or steal the highlight.
    #[test]
    fn an_auxiliary_role_never_claims_a_pipeline_slot() {
        let model = deck_with(&[
            step_usage(ModelCallRole::Worker, "moonshot", "kimi-k3"),
            step_usage(ModelCallRole::Summarization, "zai", "glm-4.6"),
        ]);
        assert_eq!(
            rendered(&models_cell(&model, true)),
            "T·—  W·moonshot/kimi-k3  J·—",
            "a summarization call leaked into a pipeline slot"
        );
        assert_eq!(
            model.active_role,
            Some(PipelineRole::Worker),
            "an auxiliary call stole the highlight"
        );
    }

    /// Legacy events carry no provider; the slug degrades to the bare model
    /// rather than rendering a leading slash.
    #[test]
    fn a_missing_provider_degrades_to_the_bare_model() {
        let model = deck_with(&[step_usage(ModelCallRole::Worker, "", "kimi-k3")]);
        assert_eq!(rendered(&models_cell(&model, true)), "T·—  W·kimi-k3  J·—");
    }

    /// The complaint this fixes: before the driver sent its pins, the row was
    /// empty until a role's first call landed, so the deck could not say what
    /// it was about to run.
    #[test]
    fn configured_pins_are_shown_before_any_turn_has_run() {
        let mut model = deck_with(&[]);
        model.apply_inbound(&configured(&[
            (PipelineRole::Triage, "zai", "glm-5.2"),
            (PipelineRole::Worker, "moonshot", "kimi-k3"),
            (PipelineRole::Judge, "anthropic", "opus-5"),
        ]));
        assert_eq!(
            rendered(&models_cell(&model, true)),
            "T·zai/glm-5.2  W·moonshot/kimi-k3  J·anthropic/opus-5"
        );
    }

    /// Configured is intent, served is evidence, and the row must not let the
    /// first pass for the second.
    #[test]
    fn a_configured_pin_stays_dim_until_it_actually_serves() {
        let mut model = deck_with(&[]);
        model.apply_inbound(&configured(&[
            (PipelineRole::Worker, "moonshot", "kimi-k3"),
            (PipelineRole::Judge, "anthropic", "opus-5"),
        ]));
        let before = models_cell(&model, true);
        let dim_before: Vec<&str> = before
            .iter()
            .filter(|s| s.style.fg == Some(theme::TEXT_PRIMARY))
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            dim_before.is_empty(),
            "a merely-configured pin was drawn as served: {dim_before:?}"
        );

        model.apply_inbound(&crate::envelope::Inbound::Event {
            agent: "lead".into(),
            event: step_usage(ModelCallRole::Judge, "anthropic", "opus-5"),
        });
        let after = models_cell(&model, false);
        let served: Vec<&str> = after
            .iter()
            .filter(|s| s.style.fg == Some(theme::TEXT_PRIMARY))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(
            served,
            vec!["anthropic/opus-5"],
            "only the role that ran should read as served"
        );
    }

    /// The provider that served wins over the provider that was configured —
    /// a gateway can route the same slug to a different upstream, and the row
    /// must report what happened.
    #[test]
    fn a_served_call_replaces_the_configured_pin() {
        let mut model = deck_with(&[]);
        model.apply_inbound(&configured(&[(
            PipelineRole::Worker,
            "openrouter",
            "kimi-k3",
        )]));
        model.apply_inbound(&crate::envelope::Inbound::Event {
            agent: "lead".into(),
            event: step_usage(ModelCallRole::Worker, "moonshot", "kimi-k3"),
        });
        assert_eq!(
            rendered(&models_cell(&model, true)),
            "T·—  W·moonshot/kimi-k3  J·—"
        );
    }

    /// A late startup envelope must not demote a role that has already run.
    #[test]
    fn configured_pins_never_overwrite_one_that_already_served() {
        let mut model = deck_with(&[step_usage(ModelCallRole::Worker, "moonshot", "kimi-k3")]);
        model.apply_inbound(&configured(&[(
            PipelineRole::Worker,
            "openrouter",
            "some-other",
        )]));
        assert_eq!(
            rendered(&models_cell(&model, true)),
            "T·—  W·moonshot/kimi-k3  J·—"
        );
    }
}
