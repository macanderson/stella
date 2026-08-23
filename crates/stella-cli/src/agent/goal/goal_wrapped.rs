// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella goal --pipeline <variant>`: each round's WORKER turn dispatched
//! through an installed wrapper plugin, bound once before the loop starts
//! (#3695, goal half).
//!
//! A submodule of [`crate::agent::goal`] rather than a `mod` block inside
//! `goal.rs` itself, for the reason AGENTS.md § "God files" gives for
//! [`crate::wrapper_plugin`] living beside `agent.rs`: `goal.rs` is already
//! close to the 1500-line ratchet, and this is new logic, not a fix to what
//! is already there.
//!
//! # What this deliberately does NOT touch
//!
//! The goal verifier — [`stella_core::Engine::assess`], the exact primitive
//! [`super::run_goal_turn`] (raw) and `run_goal_pipeline_turn`
//! (classic) both call — decides met/unmet here exactly as it does on those
//! two arms. Moving that decision onto the wrapper's own `judge`/`again`
//! would mean encoding [`stella_core::goal::GoalVerifierVerdict`]'s free-text
//! feedback into [`stella_plugin::EvidenceSet`]'s flip/tamper/measurement
//! vocabulary, which has no slot for it today — `docs/spec/turn-loop-wrappers.md`
//! §9.2 names the gap without settling the encoding. Out of scope for this
//! slice by design: the round loop below still decides continuation itself,
//! precisely as [`stella_core::Engine::run_goal`] does, and only the WORKER
//! turn inside each round is what reaches the wrapper.
//!
//! # One grant for the run, pinned before the first round
//!
//! Every round carries the same [`stella_plugin::CandidateGrant`] over the
//! shared work tree, and the same tamper baseline, minted once by
//! [`super::run_goal_cmd`] before the provider is built (#3835). Until that
//! existed this door sent `candidate: None` and reported
//! [`stella_plugin::TamperFinding::NotChecked`] on every round, so a plugin
//! had no root to read, no test plan to run, and no host vouching for what it
//! observed.
//!
//! What that does **not** yet buy is a decided verdict off a flip, and the
//! obstacle is a different rule rather than a missing wire. A tamper finding
//! only reaches a verdict through an `[oracle]`, an oracle is only decidable
//! against `[requirements]`, and `[requirements]` is refused below
//! `participation = "arbiter"`
//! ([`stella_plugin::ManifestError::RequirementsRequireArbiter`]) — a grade
//! this door refuses outright (#3832). So every plugin `stella goal` can bind
//! today carries an empty rule, which `judge` answers `Met` without consulting
//! the flip at all. The grant and the watch are what such an oracle would
//! need; admitting one is a decision about the two rules, not about this
//! module.
//!
//! Pinned once for the run rather than refreshed per round, and the safe
//! direction is the one that looks less careful. The watch covers the
//! artifacts the `--test-command` names — the witness the flip is judged
//! against — and those do not legitimately change while the loop runs. A
//! baseline retaken at the top of round 3 would vouch for a witness the worker
//! rewrote in round 2, which is the laundering the watch exists to refuse. So
//! the finding is sticky: once a witness has moved under this run, no later
//! round earns a `Clean`.
//!
//! # One round, one wrapper-internal turn
//!
//! Only an arbiter-grade wrapper can hold a round open past its first
//! internal turn (`again` → `Continuation::Again`, gated on
//! [`stella_plugin::Participation::Arbiter`]), and
//! `crate::wrapper_plugin::reject_arbiter_wrapper_on_goal` refuses that grade
//! on this door entirely, before this module is ever reached (#3832) — the
//! goal loop is this door's own completion arbiter, and a held-open round
//! would run a second hold loop via `WrapperDispatch` judging the same round
//! twice. So for every wrapper this module actually drives (steering,
//! observer), `WrapperDispatch::run` always returns after exactly one
//! internal turn. [`run_goal_wrapped_turn`]'s `DispatchReport::rounds != 1`
//! check below is kept anyway, as a defense-in-depth assertion rather than
//! the primary guard: [`GoalRoundDriver`] runs every internal turn at the SAME
//! `turn_instance` — the goal round's own slot — so a second internal turn
//! would silently collide its step manifests with the first's if the
//! pre-flight refusal above were ever bypassed or a future grade change
//! reopened the hole. See the module doc on [`crate::wrapper_plugin`] and
//! #3832 for the full reasoning.

use async_trait::async_trait;
use stella_plugin::TurnOutcome as WrapperTurnOutcome;
use stella_runtime::wrapper::{DrivenTurn, RoundInput, TurnDriver, TurnPrelude};

use super::*;
use crate::wrapper_plugin::BoundWrapper;

/// One goal round's worker turn, wrapped so an installed plugin's
/// `before_turn`/`after_turn` see it.
///
/// Unlike [`crate::wrapper_plugin::RawTurnDriver`] (`stella run`'s one-shot
/// driver), this does **not** go through [`crate::agent::run_turn`] — that
/// helper opens its own execution row, and a goal round's row is the one
/// [`run_goal_wrapped_turn`] already opened before the loop (one row per
/// goal run, `pipeline_variant` set once — the same shape
/// [`super::run_goal_turn`] (`None`) and `run_goal_pipeline_turn`
/// (`Some("classic")`) already use). This drives [`stella_core::Engine::run_turn`]
/// directly instead, at the round's own `turn_instance` offset
/// (`stella_core::goal::goal_round_turn_offset`) — exactly the offset
/// `Engine::run_goal`'s internal `round_engine` uses — with the part that
/// happens *after* the turn (the goal verifier) staying in
/// [`run_goal_wrapped_turn`], never here.
struct GoalRoundDriver<'r, 'e> {
    engine: &'r Engine<'e>,
    messages: &'r mut Vec<CompletionMessage>,
    budget: &'r mut BudgetGuard,
    events: &'r mpsc::UnboundedSender<AgentEvent>,
    /// The artifacts this host pinned before the **run**, and the finding it
    /// reports about them after every round's worker turn (#3835).
    ///
    /// Before the run and not before the round, and that is the decision
    /// rather than an approximation of one — see [`run_goal_wrapped_turn`]'s
    /// caller for why re-pinning per round would launder a witness the worker
    /// rewrote in an earlier one.
    watch: &'r crate::wrapper_candidate::TamperWatch,
    /// This round's outcome, written on the first call to [`Self::run_turn`]
    /// and left alone on any further one — see the module doc's
    /// turn_instance guard, enforced by [`run_goal_wrapped_turn`] reading
    /// [`stella_runtime::wrapper::DispatchReport::rounds`] rather than by
    /// this driver refusing the call itself, since a [`DrivenTurn`] must be
    /// returned either way.
    driven: Option<TurnOutcome>,
}

#[async_trait(?Send)]
impl TurnDriver for GoalRoundDriver<'_, '_> {
    async fn run_turn(&mut self, prelude: TurnPrelude) -> DrivenTurn {
        // Pinned before the turn, at the identity the declared artifacts have
        // now, so the comparison afterwards is about the work (#3587). Already
        // pinned artifacts keep their first baseline — see
        // `TamperWatch::pin_declared` for why re-observing would launder a
        // rewrite from an earlier round.
        self.watch.pin_declared(prelude.witness());
        self.messages.extend(prelude.into_messages());
        let outcome = self
            .engine
            .run_turn(self.messages, self.budget, self.events)
            .await;
        let turn = match &outcome {
            TurnOutcome::Completed { text, .. } => WrapperTurnOutcome {
                completed: true,
                answer: text.clone(),
                // Real tool/changed-file facts need the `turn_facts` fold
                // `crate::wrapper_plugin::RawTurnDriver` wires through
                // `crate::agent::run_turn` — not this direct
                // `Engine::run_turn` call. `None` is "this host does not
                // report it" (`stella_plugin`'s own wire contract), not a
                // guess at zero (#3834).
                tools: None,
                changed_files: None,
            },
            TurnOutcome::Aborted { reason, .. } => WrapperTurnOutcome {
                completed: false,
                answer: reason.clone(),
                tools: None,
                changed_files: None,
            },
        };
        if self.driven.is_none() {
            self.driven = Some(outcome);
        }
        DrivenTurn {
            outcome: turn,
            // The host's own comparison, over artifacts it pinned before the
            // run — never the plugin's claim about its own witness (#3499).
            // `NotChecked` survives as a real answer for an invocation that
            // named nothing to watch; see `crate::wrapper_candidate`.
            tamper: self.watch.finding(),
        }
    }
}

/// One goal loop wrapped by an installed plugin (#3695, goal half): each
/// round's WORKER turn is dispatched through `bound`, exactly as `stella run
/// --pipeline <variant>` dispatches its one turn
/// (`crate::wrapper_plugin::run_wrapped`) — `bound` is resolved and bound
/// once, before this function is ever called, by the same
/// `wrapper_plugin::resolve` + `ResolvedWrapper::serving` `run` uses.
///
/// See the module doc for what stays untouched (the goal verifier) and what
/// is refused rather than silently mishandled (a round a plugin's own
/// `[oracle]` holds open past one internal turn).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_goal_wrapped_turn(
    provider: &dyn Provider,
    base_tools: &dyn ToolExecutor,
    custom_tools: &[CustomTool],
    registry: &ToolRegistry,
    messages: &mut Vec<CompletionMessage>,
    budget: &mut BudgetGuard,
    calibration: &CalibrationMap,
    cfg: &Config,
    store: &Option<Arc<Store>>,
    goal: &str,
    session: Option<&str>,
    budget_limit: Option<f64>,
    // Phase 2 (#713): this turn's `ContextRecall`, carried from the caller.
    recall_event: Option<AgentEvent>,
    session_memory: Option<&mut crate::memory::SessionMemory>,
    bound: &BoundWrapper,
    // The grant over the tree every round runs in, and the tamper baseline
    // pinned before the first one (#3835). Minted by the caller, in the same
    // breath as the wrapper it belongs to.
    candidate: &crate::wrapper_candidate::GrantedCandidate,
    // One ledger per round, folded from this arc's journal and read by the
    // caller's reflection (#3976). The wrapped arm folds it exactly as
    // `run_goal_turn` does, and it covers the rounds this door drove — not the
    // plugin's own child turns, which ride the host-call channel and are
    // reported separately by `BoundWrapper::child_spends`.
    rounds: Option<&mut Vec<crate::memory::TurnFriction>>,
) -> Result<(), crate::failure::CliFailure> {
    let turn_start = Instant::now();
    // This is the WRAPPED arm — `run_goal_cmd` calls it only when
    // `pipeline.plugin()` is `Some` — so the row names the wrapper that
    // actually drove every round under it, the same honesty rule #3388/#3684
    // already hold the classic arm to.
    let variant = bound.variant();
    let execution = begin_execution(store, "goal", goal, cfg, session, Some(variant.as_str()));
    stamp_and_record_skill_usage(&execution, session_memory, goal, &cfg.workspace_root);

    let configured = crate::config::discover_configured_providers();
    let routed_verifier =
        resolve_cross_family_verifier(cfg.provider.id, &cfg.model_id, &configured);
    if let Some((_, verifier_id)) = &routed_verifier {
        println!(
            "  {} cross-family verifier: {} worker · {} verifier — independent, bias-resistant \
             assessment\n",
            "◆".bright_cyan(),
            cfg.provider.id.bright_magenta(),
            verifier_id.bright_green(),
        );
    }
    let verifier: &dyn Provider = match &routed_verifier {
        Some((boxed, _)) => &**boxed,
        None => provider,
    };

    let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
    let renderer = spawn_renderer(
        rx,
        OutputFormat::Text,
        execution.clone(),
        cfg.provider.id.to_string(),
        false,
        Some(goal.to_string()),
    );
    if let Some(event) = recall_event {
        let _ = tx.send(event);
    }

    let goal_config = GoalConfig::default();
    let tools = super::tool_stack::session_stack(
        base_tools,
        custom_tools.to_vec(),
        cfg,
        Principal::User,
        registry.hook_bus(),
    );
    let hook_runner = HostHookRunner;
    let mut engine = Engine::with_sleeper(provider, &tools, engine_config_for(cfg), &TokioSleeper)
        .with_calibration(calibration);
    if let Some(hooks) = &cfg.hooks {
        engine = engine.with_hooks(hooks, &hook_runner);
    }

    messages.push(CompletionMessage::user(
        stella_core::goal::goal_kickoff_text(goal),
    ));
    let starting_cost_usd = budget.session_spent_usd();
    // Read off the grant rather than re-derived: the plan on the wire is what
    // a plugin can actually run, so a signal saying a test exists must be the
    // same fact the grant carries (#3835).
    let signals = crate::wrapper_plugin::pre_turn_signals(
        candidate.grant.test.is_some(),
        budget_limit.is_some(),
    );

    let mut last_report = None;
    let outcome: GoalOutcome = 'rounds: {
        for round in 1..=goal_config.max_rounds {
            budget.begin_turn();
            let round_offset = stella_core::goal::goal_round_turn_offset(round);
            let round_engine = engine.with_turn_instance(round_offset);
            let _ = tx.send(AgentEvent::Stage {
                name: stella_protocol::StageKind::Execute.into(),
                scope: stella_protocol::StageScope::Turn,
            });

            let mut driver = GoalRoundDriver {
                engine: &round_engine,
                messages: &mut *messages,
                budget: &mut *budget,
                events: &tx,
                watch: &candidate.watch,
                driven: None,
            };
            let input = RoundInput {
                goal: goal.to_string(),
                signals,
                // The tree every round actually runs in — see
                // `crate::wrapper_candidate` for why that is the shared work
                // tree and not an isolated worktree.
                candidate: Some(candidate.grant.clone()),
            };
            let dispatched = bound.dispatch.run(input, &mut driver).await;
            // Read out before the settlement below, because the driver holds
            // this round's borrow of `budget` until its last use.
            let driven = driver.driven;
            // Whatever this round's `after_turn` spent on a child turn has no
            // next engine turn to fold it in until the *next* round runs, and
            // the last round has none at all — so every round settles its own
            // residual here, before the cost reads below (#3833). Draining is
            // destructive, so this takes only what no turn already took.
            crate::wrapper_plugin::settle_plugin_child_spend(registry, &mut *budget);
            // What this round's worker turn changed, measured at the round
            // boundary rather than at the run's end (#4159). The terminator
            // stays where it is, below and outside this loop: a goal run drives
            // several turns over one stream and owes exactly one ending, so
            // paying both debts here would end the run on its first round.
            //
            // Exactly once per boundary: `snapshot_worktree` consumes what it
            // reports, so a second reading in the same round would report an
            // unchanged tree.
            crate::turn_files::emit_shared_tree_changes_raw(cfg, &tx, execution.as_ref());
            let report = match dispatched {
                Ok(report) => report,
                Err(error) => {
                    break 'rounds GoalOutcome::Unmet {
                        rounds: round,
                        reason: format!(
                            "wrapper \"{}\" cannot be driven: {error}",
                            bound.variant()
                        ),
                        cost_usd: budget.session_spent_usd() - starting_cost_usd,
                        kind: None,
                    };
                }
            };
            // Defense-in-depth, not the primary guard: an arbiter-grade
            // wrapper — the only grade that can ever make `report.rounds`
            // anything but 1 — is refused up front by
            // `crate::wrapper_plugin::reject_arbiter_wrapper_on_goal` before
            // `run_goal_cmd` ever calls into this loop (#3832), so this arm
            // is unreachable for any wrapper actually bound here today. Kept
            // as a named, load-bearing assertion rather than deleted: it is
            // what turns a future hole in that pre-flight check (a grade
            // change, a bypassed call site) into a refused round instead of a
            // silent `turn_instance` collision.
            if report.rounds != 1 {
                break 'rounds GoalOutcome::Unmet {
                    rounds: round,
                    reason: format!(
                        "wrapper \"{}\" held this round open for {} internal turns — stella \
                         goal only supports a wrapper whose [oracle] resolves within a single \
                         turn per round today; an arbiter-grade wrapper should have been refused \
                         before this run started (#3832)",
                        bound.variant(),
                        report.rounds
                    ),
                    cost_usd: budget.session_spent_usd() - starting_cost_usd,
                    kind: None,
                };
            }
            last_report = Some(report);

            let Some(turn_outcome) = driven else {
                break 'rounds GoalOutcome::Unmet {
                    rounds: round,
                    reason: format!("wrapper \"{}\" drove a round with no turn", bound.variant()),
                    cost_usd: budget.session_spent_usd() - starting_cost_usd,
                    kind: None,
                };
            };
            if let TurnOutcome::Aborted { reason, kind, .. } = turn_outcome {
                break 'rounds GoalOutcome::Unmet {
                    rounds: round,
                    reason: format!("working turn aborted: {reason}"),
                    cost_usd: budget.session_spent_usd() - starting_cost_usd,
                    kind: Some(kind),
                };
            }

            let _ = tx.send(AgentEvent::Stage {
                name: stella_protocol::StageKind::Verdict.into(),
                scope: stella_protocol::StageScope::Turn,
            });
            let (verdict, verifier_cost) = match round_engine
                .assess(verifier, goal, messages, budget, &tx, &goal_config)
                .await
            {
                Ok(pair) => pair,
                Err(reason) => {
                    break 'rounds GoalOutcome::Unmet {
                        rounds: round,
                        reason: format!("verifier unavailable: {reason}"),
                        cost_usd: budget.session_spent_usd() - starting_cost_usd,
                        kind: None,
                    };
                }
            };
            let _ = tx.send(AgentEvent::GoalVerdict {
                round,
                met: verdict.met,
                reasoning: verdict.reasoning.clone(),
                cost_usd: verifier_cost,
            });

            if verdict.met {
                break 'rounds GoalOutcome::Met {
                    rounds: round,
                    verdict: verdict.reasoning,
                    cost_usd: budget.session_spent_usd() - starting_cost_usd,
                };
            }

            messages.push(CompletionMessage::user(
                stella_core::goal::verifier_feedback_text(goal, &verdict),
            ));
        }
        GoalOutcome::Unmet {
            rounds: goal_config.max_rounds,
            reason: format!(
                "round cap ({}) reached without a passing verdict",
                goal_config.max_rounds
            ),
            cost_usd: budget.session_spent_usd() - starting_cost_usd,
            kind: None,
        }
    };

    if let Some(report) = &last_report {
        // One wrapper over one goal in one process — nothing to attribute.
        bound.report(None, OutputFormat::Text, report);
    }

    // The rounds measured themselves as they closed (#4159, above); this is the
    // run's single terminator.
    let (GoalOutcome::Met { cost_usd, .. } | GoalOutcome::Unmet { cost_usd, .. }) = &outcome;
    persistence::emit_run_complete_on_raw(&tx, &cfg.model_id, *cost_usd);
    drop(tx);
    let rendered = renderer.await.unwrap_or_default();
    let persistence_complete = rendered.persistence_complete;
    // The arc's friction, split at each round's own `GoalVerdict` so no round
    // is credited with another's tools — the same fold `run_goal_turn` does,
    // at the same point, and for the same reason: this is the first moment the
    // whole stream is both complete and still owned (#3962, #3976).
    if let Some(slot) = rounds {
        *slot = crate::memory::TurnFriction::per_goal_round(&rendered.events);
    }

    let (outcome_label, cost) = match &outcome {
        GoalOutcome::Met { cost_usd, .. } => ("goal_met", *cost_usd),
        GoalOutcome::Unmet { cost_usd, .. } => ("goal_unmet", *cost_usd),
    };
    crate::agent::turn_close::close_turn(
        cfg,
        store,
        &execution,
        registry,
        session,
        crate::agent::turn_close::TurnOutcomeRecord {
            label: outcome_label,
            cost_usd: cost,
            persistence_complete,
        },
    );

    match outcome {
        GoalOutcome::Met {
            rounds,
            verdict,
            cost_usd,
        } => {
            println!(
                "\n  {} goal met after {rounds} round{}: {}",
                "✓".green().bold(),
                if rounds == 1 { "" } else { "s" },
                verdict
            );
            plain::cost_summary(
                cost_usd,
                &format!("{}/{}", cfg.provider.id, cfg.model_id),
                turn_start.elapsed(),
            );
            println!();
            Ok(())
        }
        GoalOutcome::Unmet {
            rounds,
            reason,
            cost_usd,
            kind,
        } => {
            plain::cost_summary(
                cost_usd,
                &format!("{}/{}", cfg.provider.id, cfg.model_id),
                turn_start.elapsed(),
            );
            Err(outcome::goal_unmet_failure(rounds, &reason, kind))
        }
    }
}
