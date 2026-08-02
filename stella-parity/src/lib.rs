// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The cross-surface capability matrix — the structural guard against a
//! feature shipping on one of Stella's surfaces and silently not the other.
//!
//! Stella is one engine behind (today) two customer-facing surfaces: the CLI
//! (`stella-cli`, the community tool) and the API (`stella-serve`, the
//! embeddable sidecar). Nothing used to enforce that a capability landing on
//! one surface landed on — or was *deliberately declared absent from* — the
//! other, and the two drifted exactly the way per-provider features drifted
//! before `stella-model/src/provider_parity.rs`: at the time this matrix was
//! written, the API could set precisely ONE of `EngineConfig`'s ~15 tuning
//! knobs, the goal loop and sub-agents were CLI-only, and the serve crate's
//! own route tests covered 7 of its 14 routes. This module makes that class
//! of gap structural instead of tribal, with the same three instruments the
//! provider matrix proved out:
//!
//! - **A declared row per capability**, with a posture on every surface. An
//!   absence is legal only as a [`SurfacePosture::Deferred`] (with what it is
//!   waiting on) or a [`SurfacePosture::NotApplicable`] (with the design
//!   reason) — never as silence.
//! - **Witness tests named and checked.** A `Shipped` posture names the test
//!   that proves the wiring on that surface, and this crate's tests fail when
//!   the named function no longer exists in the surface's sources.
//! - **Completeness enforced from both ends.** Every real API route
//!   ([`stella_serve::observe::Route::ALL`]) must be claimed by a row, and
//!   every public `Engine` entry point in `stella-core`'s driver/goal modules
//!   must be claimed by a row or by the composition-seam allowlist — so
//!   adding a route or an engine capability without a matrix decision fails
//!   `cargo test --workspace`, in the same PR that added it.
//!
//! **The law for new features:** adding an engine capability, an API route,
//! or an agent-facing CLI behavior means updating this matrix in the same
//! PR. `Deferred` is an honest and expected answer — the point is that a
//! human wrote the answer down where a test can keep it true, not that every
//! feature ships everywhere at once.
//!
//! The embedding story this matrix serves — what "the API surface ships with
//! parity" means for hosts dropping Stella into their own applications — is
//! `docs/design/engine-embedding.md`.

/// How one capability ships on one surface.
#[derive(Debug)]
pub enum SurfacePosture {
    /// Wired on this surface, with the named witness test proving it —
    /// checked for existence by this crate's tests, exactly as
    /// `provider_parity` checks adapter witnesses.
    Shipped {
        /// The surface mechanism, for humans reading the matrix. For API
        /// rows this names the route template(s) verbatim — the route sweep
        /// matches [`stella_serve::observe::Route::ALL`] against these
        /// strings, so a claimed route must be spelled exactly.
        mechanism: &'static str,
        /// Name of the test function proving the wiring on this surface.
        witness: &'static str,
    },
    /// Wired on this surface, but no test pins the wiring — a debt this
    /// matrix counts rather than hides. Bounded by
    /// [`UNWITNESSED_BASELINE`], a ratchet that only goes down: writing the
    /// missing witness promotes the row to `Shipped` and lowers the
    /// baseline in the same PR.
    ShippedUnwitnessed {
        mechanism: &'static str,
        /// What a witness for this wiring would pin.
        missing: &'static str,
    },
    /// Not on this surface yet, deliberately and visibly — the surface-level
    /// sibling of `stella-store`'s declared-gap `DRAIN_FORMATS`. `waiting_on`
    /// names the decision or dependency, so a reviewer can tell a parked
    /// feature from a forgotten one.
    Deferred { waiting_on: &'static str },
    /// This capability is not meant to exist on this surface, with the
    /// design reason a reviewer can check.
    NotApplicable { reason: &'static str },
}

/// One engine capability and how it ships on each surface.
#[derive(Debug)]
pub struct Capability {
    /// Stable id, `area.name`.
    pub id: &'static str,
    /// Where the capability lives in the engine, as prose for the reader.
    pub engine_home: &'static str,
    /// The public `Engine` function names this row claims — the engine-side
    /// completeness sweep requires every swept entry point to be claimed by
    /// exactly this field (or the [`COMPOSITION_SEAMS`] allowlist).
    pub engine_entries: &'static [&'static str],
    /// How the community CLI ships it.
    pub cli: SurfacePosture,
    /// How the embeddable API ships it.
    pub api: SurfacePosture,
}

/// Public `Engine` methods that are composition seams, not customer-facing
/// capabilities: hosts use them to assemble a stack, and no surface would
/// ever expose them directly. Anything swept from the engine sources that is
/// neither claimed by a row nor listed here fails the completeness test.
pub const COMPOSITION_SEAMS: &[&str] = &[
    "with_sleeper",
    "with_call_role",
    "with_turn_instance",
    "max_steps",
];

/// How many `ShippedUnwitnessed` postures the matrix currently carries. A
/// ratchet, not a budget: the test pins exact equality, so witnessing a row
/// forces this DOWN in the same PR (the win is recorded), and adding a new
/// unwitnessed claim forces it UP — a visible review decision instead of a
/// silent one.
pub const UNWITNESSED_BASELINE: usize = 2;

/// The matrix. Ordered by area; ids are stable and unique.
pub static CAPABILITIES: &[Capability] = &[
    Capability {
        id: "turn.run",
        engine_home: "stella-core driver: one bounded agent turn (run_turn as a loop over run_step)",
        engine_entries: &["run_turn", "run_turn_with_sender", "run_step", "new_turn"],
        cli: SurfacePosture::Shipped {
            mechanism: "`stella run` / deck turns via agent::run_turn and the pipeline drivers",
            witness: "non_tty_text_run_wiring_stays_headless_and_json_run_wiring_never_bypasses_scope_review",
        },
        api: SurfacePosture::Shipped {
            mechanism: "POST /v1/turns — a stateless turn driven as a step loop on a session thread",
            witness: "full_turn_round_trips_over_http",
        },
    },
    Capability {
        id: "turn.checkpoint",
        engine_home: "stella-core step: the CheckpointSink seam, written at the one step boundary \
                      where the transcript is guaranteed well-paired",
        engine_entries: &["persist_checkpoint", "discard_checkpoint"],
        cli: SurfacePosture::Shipped {
            mechanism: "Config's SessionDurability handle, attached at agent::tuned_engine_config \
                        so every role gets it; the sink writes the work journal's CHECKPOINT_BLOB",
            witness: "a_bound_session_checkpoints_from_every_role",
        },
        // The plumbing is present and correct on this surface — `drive_turn`
        // calls both entry points at the same seams the CLI driver does — but
        // nothing sets `checkpoint_sink`, so a served turn is not durable.
        // That is a design decision, not an omission: in the reverse-RPC model
        // the workspace lives on the HOST side, so serve has no filesystem
        // location it could honestly checkpoint against (and no `stella-store`
        // dependency, and no workspace root or session id in `SessionSpec`).
        // `SessionSpec::config` is `pub`, so an embedder that owns a workspace
        // supplies its own sink today.
        api: SurfacePosture::Deferred {
            waiting_on: "who owns the workspace for a served session — the host supplies a sink \
                         through the public EngineConfig today; a server-side one needs the \
                         portable-session design (transportable state between machines) first",
        },
    },
    Capability {
        id: "turn.stream_events",
        engine_home: "stella-protocol AgentEvent over the engine's EventSender",
        engine_entries: &[],
        cli: SurfacePosture::Shipped {
            mechanism: "TUI transcript rendering and --output-format json event stream",
            witness: "non_tty_text_output_is_headless_without_losing_text_rendering",
        },
        api: SurfacePosture::Shipped {
            mechanism: "GET /v1/turns/{id}/events — SSE ServerFrames with monotonic seq, \
                        resumable via ?after= / Last-Event-ID",
            witness: "every_frame_carries_a_monotonic_seq_in_the_payload_and_the_sse_id",
        },
    },
    Capability {
        id: "turn.cancel",
        engine_home: "stella-engine CancelToken — clean stop at the next step boundary",
        engine_entries: &[],
        cli: SurfacePosture::Shipped {
            mechanism: "deck turn controls and signal handling; parent cancel reaches children",
            witness: "cancelling_the_parent_stops_the_child_at_its_next_boundary",
        },
        api: SurfacePosture::Shipped {
            mechanism: "POST /v1/turns/{id}/cancel — unwinds at the next step boundary, \
                        completed steps kept",
            witness: "a_cancel_unwinds_at_the_next_step_boundary",
        },
    },
    Capability {
        id: "turn.soft_stop",
        engine_home: "stella-core SOFT_STOP_REASON via the TurnSteering port — user-initiated, keeps history valid",
        engine_entries: &[],
        cli: SurfacePosture::Shipped {
            mechanism: "deck Esc soft stop through the SteeringTap; propagates to dispatched children",
            witness: "a_turns_soft_stop_reaches_the_children_it_dispatched",
        },
        api: SurfacePosture::Deferred {
            waiting_on: "a soft flag on POST /v1/turns/{id}/cancel (or a distinct verb): only the \
                         hard step-boundary cancel is on the wire, so an API host cannot express \
                         'stop but keep this turn's completed work as an ordinary result'",
        },
    },
    Capability {
        id: "turn.steer",
        engine_home: "stella-core TurnSteering port — mid-turn user messages at step boundaries",
        engine_entries: &["with_steering"],
        cli: SurfacePosture::Shipped {
            mechanism: "deck mid-turn input via the SteeringTap",
            witness: "a_live_turn_still_steers_on_the_marker_and_spawns_otherwise",
        },
        api: SurfacePosture::Shipped {
            mechanism: "POST /v1/turns/{id}/steer — injected at the next step boundary, echoed as \
                        a steered event",
            witness: "a_steer_lands_in_the_next_model_request_and_echoes_on_the_stream",
        },
    },
    Capability {
        id: "turn.pause_resume",
        engine_home: "stella-core TurnGate port — park at a step boundary, never mid-tool",
        engine_entries: &["with_gate"],
        cli: SurfacePosture::Shipped {
            mechanism: "deck pause/resume controls; a paused turn parks its sub-agent children too",
            witness: "a_paused_turn_parks_its_children_and_resume_releases_them",
        },
        api: SurfacePosture::Shipped {
            mechanism: "POST /v1/turns/{id}/pause and POST /v1/turns/{id}/resume — idempotent, \
                        held at the boundary",
            witness: "a_paused_turn_holds_at_the_boundary_until_resumed",
        },
    },
    Capability {
        id: "session.persistent",
        engine_home: "caller-owned message history and budget across turns (the engine borrows, never owns)",
        engine_entries: &[],
        // Two stores, deliberately not converged, and split by instant rather
        // than by content: the sidecar (journal.jsonl / history.json /
        // queue.json) is canonical for the session and for the conversation
        // BETWEEN turns; the workspace's git-backed work journal is canonical
        // for the agent's file changes and for the conversation INSIDE an
        // interrupted turn. They cannot both describe one moment, because a
        // checkpoint exists only while a turn is in flight. The table in
        // `stella_store::journal`'s module docs is the full statement.
        cli: SurfacePosture::Shipped {
            mechanism: "`stella chat` / `stella resume` over the session_persist journal, plus \
                        the work journal for in-turn state (see turn.checkpoint_resume)",
            witness: "journal_then_replay_is_identity_on_the_fold_relevant_stream",
        },
        api: SurfacePosture::Shipped {
            mechanism: "POST /v1/sessions, GET|DELETE /v1/sessions/{id}, \
                        POST /v1/sessions/{id}/turns — server-owned history on a byte-stable \
                        prompt prefix",
            witness: "a_session_threads_history_across_turns_on_a_stable_prefix",
        },
    },
    Capability {
        id: "turn.checkpoint_resume",
        engine_home: "stella-engine Checkpoint — versioned serde snapshot at a step boundary, resumable in another process",
        engine_entries: &["resume_turn"],
        // Shipped at TRANSCRIPT granularity, not as a rebuilt `TurnState`. The
        // deck prefers the work journal's CHECKPOINT_BLOB over the sidecar's
        // turn-boundary history whenever one exists — and because every
        // terminal path discards, one existing means a turn was interrupted —
        // so a resumed session reopens with the completed steps' work already
        // in the conversation and does not re-run it. What it does NOT do is
        // call `resume_turn`: CLI turns are dispatched through stella-pipeline,
        // which owns turn framing and builds its own TurnState, so handing the
        // engine a resumed one would mean threading a checkpoint through the
        // whole verification ladder. See `Engine::resume_turn`'s own docs for
        // that gap, declared where a caller reads it.
        cli: SurfacePosture::Shipped {
            mechanism: "`stella resume` / the SESSIONS overlay via \
                        session_persist::restore_conversation, which prefers the work journal's \
                        CHECKPOINT_BLOB over history.json and degrades to the turn boundary, \
                        visibly, on a version it cannot read",
            witness: "an_interrupted_turn_resumes_at_the_step_boundary_not_the_turn_boundary",
        },
        // Deliberately phrased to AGREE with `turn.checkpoint` above rather
        // than restate it differently: serve does reach both write seams, so
        // the gap here is not the seam and not the writing. It is that there is
        // nowhere to read one back FROM, and nothing to key it on.
        api: SurfacePosture::Deferred {
            waiting_on: "somewhere for a served session to read a resume point back from: serve \
                         reaches the write seams (see turn.checkpoint) but has no store, and \
                         SessionSpec carries no session identity to key one on — so nothing \
                         survives the process to resume. Note stella-serve/tests/resume.rs is SSE \
                         stream resumption, a different resume entirely",
        },
    },
    Capability {
        id: "goal.loop",
        engine_home: "stella-core goal: judged rounds until an independent judge assesses the goal met",
        engine_entries: &["run_goal", "assess"],
        cli: SurfacePosture::Shipped {
            mechanism: "`stella goal` / `stella monitor`, with a cross-family judge resolved by \
                        default",
            witness: "distinct_families_route_a_cross_family_judge",
        },
        api: SurfacePosture::Deferred {
            waiting_on: "a goals resource (or a mode flag on turns): judged multi-round runs — a \
                         defining capability for agent-app hosts — cannot be requested over the \
                         wire at all",
        },
    },
    Capability {
        id: "agent.subagents",
        engine_home: "stella-core subagent: bounded child turns with carved budgets and forwarded metering",
        engine_entries: &[],
        cli: SurfacePosture::Shipped {
            mechanism: "the task tool via SessionSubAgents; deck lanes; fleet workers",
            witness: "the_production_tool_stack_forwards_sub_agent_spend",
        },
        api: SurfacePosture::Deferred {
            waiting_on: "sub-agent wiring in serve's session assembly: the engine machinery and \
                         the SubAgent event vocabulary are wire-ready, but a served turn's tool \
                         stack installs no task tool, so children cannot exist",
        },
    },
    Capability {
        id: "hooks.lifecycle",
        engine_home: "stella-core hooks (PreToolUse/PostToolUse/SessionStart) and the observer-only HookBus",
        engine_entries: &["with_hooks", "run_session_start_hooks", "with_bus"],
        cli: SurfacePosture::ShippedUnwitnessed {
            mechanism: "workspace hooks wired via with_session_hook_context on every driver path",
            missing: "a CLI-side test pinning that a configured workspace hook actually fires \
                      through agent wiring (core has hook tests; the CLI wiring has none)",
        },
        api: SurfacePosture::Deferred {
            waiting_on: "attaching the HookBus in serve's session assembly: shell hooks are \
                         deliberately not run server-side (a host must not be able to make the \
                         sidecar spawn shells), and with_bus is the server-shaped observer seam \
                         built for exactly this — but serve never attaches it, so an API host \
                         gets no lifecycle boundary events",
        },
    },
    Capability {
        id: "budget.enforce",
        engine_home: "stella-core BudgetGuard: turn and session USD axes, enforced at step boundaries",
        engine_entries: &[],
        cli: SurfacePosture::Shipped {
            mechanism: "--budget / --turn-budget on the session axis, surviving across turns",
            witness: "budget_cap_holds_across_turns_rather_than_resetting_each_one",
        },
        api: SurfacePosture::Shipped {
            mechanism: "budget {mode, turn_limit_usd, session_limit_usd} on turn and session \
                        create; aborted turns report incurred cost",
            witness: "aborted_outcome_preserves_incurred_cost_on_wire",
        },
    },
    Capability {
        id: "calibration.drift",
        engine_home: "stella-core estimator CalibrationMap: per-model token-drift correction feeding compaction",
        engine_entries: &["with_calibration"],
        cli: SurfacePosture::ShippedUnwitnessed {
            mechanism: "seed_calibration from the store plus with_calibration on every driver path",
            missing: "a CLI-side test pinning that persisted drift samples reach the engine's \
                      calibration on session start (stella-core and stella-store each test their \
                      half; the CLI seam between them has no witness)",
        },
        api: SurfacePosture::Deferred {
            waiting_on: "serve attaching a CalibrationMap (and somewhere to persist samples — \
                         serve deliberately has no store): every served turn estimates uncorrected",
        },
    },
    Capability {
        id: "config.tuning",
        engine_home: "stella-core EngineConfig: effort, reasoning, output caps, timeouts, loop/compaction tuning",
        engine_entries: &[],
        cli: SurfacePosture::Shipped {
            mechanism: "settings + flags resolved field-by-field into EngineConfig",
            witness: "configured_settings_beat_the_baseline_field_by_field",
        },
        api: SurfacePosture::Deferred {
            waiting_on: "an engine-config block on turn/session create: of EngineConfig's ~15 \
                         knobs only max_steps is settable over the wire — an embedding host \
                         cannot pin effort, reasoning, output caps, timeouts, or budgets-in-time \
                         from the API",
        },
    },
    Capability {
        id: "pipeline.verified_run",
        engine_home: "stella-pipeline: the plan/witness/verify/judge ladder behind `verified done, not claimed done`",
        engine_entries: &[],
        cli: SurfacePosture::Shipped {
            mechanism: "`stella run` (default pipeline path) with the scope-review approval gate",
            witness: "non_tty_text_run_wiring_stays_headless_and_json_run_wiring_never_bypasses_scope_review",
        },
        api: SurfacePosture::Deferred {
            waiting_on: "a runs resource (or moving the approval boundary into the engine): serve \
                         does not link stella-pipeline, so the product's defining verification \
                         ladder — and the approval gate with it — is structurally unreachable \
                         from the API",
        },
    },
    Capability {
        id: "tools.local_execution",
        engine_home: "stella-core ToolExecutor port over stella-tools' sandboxed registry",
        engine_entries: &[],
        cli: SurfacePosture::Shipped {
            mechanism: "the full local registry (bash, edit, read, MCP, skills) under policy \
                        layering",
            witness: "a_settings_entry_hides_and_refuses_bash_in_the_real_stack",
        },
        api: SurfacePosture::NotApplicable {
            reason: "deliberate bring-your-own-tools posture: serve remotes every tool call to \
                     the host (STELLA_SERVE_TOOLS=remote is the only mode) so the sidecar never \
                     executes host-supplied work itself. A server-side tool mode would be a new \
                     capability row, not a change to this one",
        },
    },
    Capability {
        id: "provider.calls",
        engine_home: "stella-protocol Provider port — model calls, streamed and observed",
        engine_entries: &[],
        cli: SurfacePosture::Shipped {
            mechanism: "BYOK adapters resolved from config (stella-model), one per provider id",
            witness: "existing_providers_still_route_to_their_current_adapter",
        },
        api: SurfacePosture::Shipped {
            mechanism: "reverse RPC: provider_request frames answered on \
                        POST /v1/turns/{id}/provider-result, tool_request frames on \
                        POST /v1/turns/{id}/tool-result — the host owns keys and execution",
            witness: "an_unanswered_provider_request_fails_on_the_deadline",
        },
    },
    Capability {
        id: "ops.health",
        engine_home: "surface-level operability: liveness, readiness, counters",
        engine_entries: &[],
        cli: SurfacePosture::Shipped {
            mechanism: "`stella doctor` plus --log-level/--log-file diagnostics",
            witness: "doctor_parses_bare_and_with_repair",
        },
        api: SurfacePosture::Shipped {
            mechanism: "GET /healthz, GET /readyz (unauthenticated), GET /v1/metrics \
                        (authenticated, pull-only)",
            witness: "readyz_reports_ready_while_serving",
        },
    },
];

/// The row for `id`, or `None` — unknown ids are the caller's bug, and the
/// tests below guarantee every declared id resolves.
#[must_use]
pub fn capability(id: &str) -> Option<&'static Capability> {
    CAPABILITIES.iter().find(|c| c.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_serve::observe::Route;

    /// CLI sources a witness may live in. Source text, not the module tree —
    /// the same trade `provider_parity` documents: a witness that moves to a
    /// file outside this list fails loudly (a false alarm to fix by extending
    /// the list), never silently (the rotted proof this exists to catch).
    fn cli_sources() -> [&'static str; 8] {
        [
            include_str!("../../stella-cli/src/agent_tests.rs"),
            // `agent_tests.rs` is split into submodules by the file-size
            // ratchet, so its children have to be listed too — a witness that
            // moved into one of them is not missing, it is one `include_str!`
            // away from being invisible to this sweep.
            include_str!("../../stella-cli/src/agent_tests/engine_wiring.rs"),
            include_str!("../../stella-cli/src/subagent/tests.rs"),
            include_str!("../../stella-cli/src/subsession.rs"),
            include_str!("../../stella-cli/src/command_deck/tests.rs"),
            include_str!("../../stella-cli/src/session_persist.rs"),
            include_str!("../../stella-cli/src/engine_config.rs"),
            include_str!("../../stella-cli/src/main_tests.rs"),
        ]
    }

    /// API sources a witness may live in: the serve crate's unit tests plus
    /// its end-to-end suites.
    fn api_sources() -> [&'static str; 9] {
        [
            include_str!("../../stella-serve/src/server.rs"),
            include_str!("../../stella-serve/tests/bridge.rs"),
            include_str!("../../stella-serve/tests/control.rs"),
            include_str!("../../stella-serve/tests/sessions.rs"),
            include_str!("../../stella-serve/tests/resume.rs"),
            include_str!("../../stella-serve/tests/shutdown.rs"),
            include_str!("../../stella-serve/tests/step_cancel.rs"),
            include_str!("../../stella-serve/tests/http.rs"),
            include_str!("../../stella-serve/tests/hostguard.rs"),
        ]
    }

    fn witness_exists(sources: &[&str], witness: &str) -> bool {
        let needle = format!("fn {witness}(");
        sources.iter().any(|source| source.contains(&needle))
    }

    #[test]
    fn capability_ids_are_unique_and_resolvable() {
        let mut seen = std::collections::BTreeSet::new();
        for row in CAPABILITIES {
            assert!(seen.insert(row.id), "duplicate capability row: {}", row.id);
            assert!(capability(row.id).is_some());
        }
        assert!(capability("no.such.capability").is_none());
    }

    /// Every `Shipped` CLI posture's witness must exist in the CLI sources.
    #[test]
    fn every_cli_witness_exists() {
        let sources = cli_sources();
        for row in CAPABILITIES {
            if let SurfacePosture::Shipped { witness, .. } = row.cli {
                assert!(
                    witness_exists(&sources, witness),
                    "cli witness for `{}` not found: {witness}",
                    row.id
                );
            }
        }
    }

    /// Every `Shipped` API posture's witness must exist in the serve sources.
    #[test]
    fn every_api_witness_exists() {
        let sources = api_sources();
        for row in CAPABILITIES {
            if let SurfacePosture::Shipped { witness, .. } = row.api {
                assert!(
                    witness_exists(&sources, witness),
                    "api witness for `{}` not found: {witness}",
                    row.id
                );
            }
        }
    }

    /// The unwitnessed-claims ratchet: exact equality, so witnessing a row
    /// forces the baseline DOWN in the same PR and a new unwitnessed claim
    /// forces it UP — both visible review decisions.
    #[test]
    fn unwitnessed_claims_match_the_declared_baseline_exactly() {
        let count = CAPABILITIES
            .iter()
            .flat_map(|row| [&row.cli, &row.api])
            .filter(|posture| matches!(posture, SurfacePosture::ShippedUnwitnessed { .. }))
            .count();
        assert_eq!(
            count, UNWITNESSED_BASELINE,
            "ShippedUnwitnessed count moved: promote rows (baseline down) or declare the new \
             debt (baseline up) — in this same change"
        );
    }

    /// API-side completeness: every real route the server dispatches must be
    /// claimed by some row's API mechanism, spelled as its exact template. A
    /// new route added to `stella-serve` without a matrix decision fails
    /// here, in the adding PR.
    #[test]
    fn every_api_route_is_claimed_by_a_capability_row() {
        for route in Route::ALL {
            let template = route.template();
            let claimed = CAPABILITIES.iter().any(|row| match row.api {
                SurfacePosture::Shipped { mechanism, .. }
                | SurfacePosture::ShippedUnwitnessed { mechanism, .. } => {
                    mechanism.contains(template)
                }
                _ => false,
            });
            assert!(
                claimed,
                "route {template} is not claimed by any capability row — add it to an existing \
                 row's mechanism or declare a new capability"
            );
        }
    }

    /// Engine-side completeness: every public `Engine` entry point in the
    /// driver and goal modules must be claimed by a row's `engine_entries`
    /// or by [`COMPOSITION_SEAMS`]. A new engine capability added without a
    /// matrix decision fails here, in the adding PR.
    ///
    /// The sweep reads source text for four-space-indented `pub fn` /
    /// `pub async fn` lines — `impl` items — which deliberately skips
    /// `pub(crate)` internals and module-level free functions.
    #[test]
    fn every_public_engine_entry_point_is_claimed() {
        let sources = [
            include_str!("../../stella-core/src/driver.rs"),
            include_str!("../../stella-core/src/goal.rs"),
        ];
        let mut swept: Vec<&str> = Vec::new();
        for source in sources {
            for line in source.lines() {
                let Some(rest) = line
                    .strip_prefix("    pub fn ")
                    .or_else(|| line.strip_prefix("    pub async fn "))
                else {
                    continue;
                };
                let name = rest.split(['(', '<']).next().unwrap_or("").trim();
                if !name.is_empty() {
                    swept.push(name);
                }
            }
        }
        assert!(
            swept.len() >= 10,
            "the sweep found implausibly few entry points ({}) — did the driver sources move?",
            swept.len()
        );
        for name in swept {
            let claimed = COMPOSITION_SEAMS.contains(&name)
                || CAPABILITIES
                    .iter()
                    .any(|row| row.engine_entries.contains(&name));
            assert!(
                claimed,
                "public Engine entry point `{name}` is not claimed by any capability row or by \
                 COMPOSITION_SEAMS — decide where it ships (Deferred is a legal answer) and \
                 record it"
            );
        }
    }

    /// Every claimed engine entry actually exists in the swept sources —
    /// the mirror of the sweep, so a renamed engine method cannot leave a
    /// row pointing at nothing.
    #[test]
    fn every_claimed_engine_entry_exists() {
        let sources = [
            include_str!("../../stella-core/src/driver.rs"),
            include_str!("../../stella-core/src/goal.rs"),
        ];
        for row in CAPABILITIES {
            for entry in row.engine_entries {
                let plain = format!("pub fn {entry}");
                let asynk = format!("pub async fn {entry}");
                assert!(
                    sources
                        .iter()
                        .any(|s| s.contains(&plain) || s.contains(&asynk)),
                    "row `{}` claims engine entry `{entry}` which no swept source defines",
                    row.id
                );
            }
        }
    }
}
