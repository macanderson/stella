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
//! `docs/spec/engine-embedding.md`.

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
    // The blessed constructor (#3390): required ports plus one
    // `TurnCapabilities` answering every optional seam. Assembly in the same
    // sense `with_sleeper` is — a host builds a stack with it, and no surface
    // exposes it — so it belongs here rather than in a capability row.
    "assemble",
    "with_sleeper",
    "with_call_role",
    // The reader for `with_call_role`, and an assembly detail for the same
    // reason the setter is: a host that drives `run_step` itself owns the turn
    // framing, and the `agent.turn.started` payload names the role — so it
    // reads the engine's rather than keeping a second copy of the default.
    "call_role",
    "with_turn_instance",
    "max_steps",
];

/// How many `ShippedUnwitnessed` postures the matrix currently carries. A
/// ratchet, not a budget: the test pins exact equality, so witnessing a row
/// forces this DOWN in the same PR (the win is recorded), and adding a new
/// unwitnessed claim forces it UP — a visible review decision instead of a
/// silent one.
pub const UNWITNESSED_BASELINE: usize = 5;

/// The matrix. Ordered by area; ids are stable and unique.
pub static CAPABILITIES: &[Capability] = &[
    Capability {
        id: "turn.run",
        engine_home: "stella-core driver: one bounded agent turn (drive as a loop over run_step; \
                      run_turn is drive over an adopted transcript)",
        engine_entries: &[
            "run_turn",
            "run_turn_with_sender",
            "drive",
            "run_step",
            "new_turn",
        ],
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
        // Promoted in #1198, and precisely within ADR 0013's line rather than
        // across it. What that ADR refuses is giving *the server* a
        // filesystem: the workspace stays the host's, so serve must never
        // pick a location to write to. It does not. `ServeConfig::checkpoints`
        // is `None` by default and this crate names no path; what ships is the
        // port (`CheckpointStore`) plus the durable identity to key it
        // (`SessionSpec::checkpoint`), which is the same ADR's other half —
        // "gives an embedder a defined artifact to persist".
        //
        // The reference implementations (`MemoryCheckpointStore`,
        // `FileCheckpointStore`) are library types an embedder may choose.
        // Deliberately NOT exposed as a flag on the `stella-serve` binary:
        // that would be the server choosing, which is the act ADR 0013 lists
        // under "what this does not commit us to", and it is one line to add
        // once that ADR is ratified.
        api: SurfacePosture::Shipped {
            mechanism: "SessionSpec::checkpoint keys an embedder-supplied CheckpointStore \
                        (ServeConfig::with_checkpoint_store); a session turn keys on the session \
                        id and a stateless one on its turn id; \
                        GET|DELETE /v1/sessions/{id}/checkpoint and \
                        GET|DELETE /v1/turns/{id}/checkpoint read one back or reclaim it",
            witness: "a_served_turn_writes_a_resume_point_at_every_step_boundary",
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
        id: "turn.steering_requery",
        engine_home: "stella-core SteeringRequery port — the steering plane re-queried at step \
                      boundaries when TurnSignal drift says the opening prompt no longer \
                      describes the turn (#3243 Phase 3)",
        engine_entries: &["with_requery"],
        cli: SurfacePosture::Shipped {
            mechanism: "agent::run_turn and the deck attach a SessionRequery over SessionMemory, \
                        so a drifted turn re-selects skills/records against what it has become",
            witness: "a_drifted_turn_recalls_the_skill_its_prompt_could_not",
        },
        api: SurfacePosture::Deferred {
            waiting_on: "a serve-side SteeringRequery implementor: the engine port exists, but \
                         the serve stack assembles no requery source, so remote turns still \
                         select context against the opening prompt alone",
        },
    },
    Capability {
        id: "turn.halt_on_goal_met",
        engine_home: "stella-core TurnHalt port — end a turn at a step boundary because the goal \
                      is MET, the one exit that is not a limit being reached",
        engine_entries: &["with_turn_halt"],
        cli: SurfacePosture::ShippedUnwitnessed {
            mechanism: "stella-pipeline arms a FlipHalt from the failing pre-execute test \
                        baseline and feeds it the worker's own shell results, so the execute \
                        turn ends the moment the tracked command goes fail→pass",
            // The engine seam itself IS witnessed, in stella-core
            // (`a_halt_ends_the_turn_at_the_next_step_boundary_as_completed`
            // plus its never-fires control). What no test yet pins is the
            // ARMING: that a candidate whose baseline failed reaches
            // `run_engine_turn` with a halt attached, and that a candidate
            // whose baseline passed does not. That needs a pipeline-level
            // harness (CandidateSurface + ports), and this crate's
            // `cli_sources` does not sweep stella-pipeline — claiming a
            // stella-core test here would report the seam as proof of the
            // wiring, which is the exact substitution this matrix exists to
            // prevent.
            missing: "a pipeline test that a failing baseline arms the halt and a passing one \
                      leaves it unarmed",
        },
        api: SurfacePosture::Deferred {
            waiting_on: "a served turn has no test baseline of its own — the host, not the \
                         engine, would have to say what 'done' means for an embedded turn, and \
                         no route expresses that yet",
        },
    },
    Capability {
        id: "turn.pause_resume",
        engine_home: "stella-core TurnGate port — park at a step boundary, never mid-tool",
        engine_entries: &["with_gate"],
        cli: SurfacePosture::Shipped {
            mechanism: "deck pause/resume controls (`p`), on worker lanes and on the lead's own \
                        turn alike (#1219 — raw and pipeline paths); a paused turn parks its \
                        sub-agent children too",
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
        // than restate it differently. #1198 closed the two things this row
        // used to name — serve had no store, and `SessionSpec` had no identity
        // to key one on — so the gap is no longer the seam, the writing, the
        // durability, or the read-back: a served turn writes the versioned
        // `Checkpoint` at every step boundary and a host reads it back byte
        // for byte. What is missing is the OTHER direction. Nothing accepts
        // one: no route takes a checkpoint, and `Engine::resume_turn` still
        // has zero production callers on either surface.
        //
        // That serve does not re-drive a turn itself is a decision, not an
        // omission (`stella-serve/src/checkpoint.rs`, "Resume is
        // host-initiated"): a resumed turn's first act is a reverse request
        // only a host can answer, so a server that resumed on restart would
        // park a thread on a request nobody is listening for. The declared gap
        // is therefore the *verb*, not the mechanism — an API host holding a
        // valid resume point today can only continue it by driving
        // `stella-engine` in its own process.
        api: SurfacePosture::Deferred {
            waiting_on: "a way to hand a resume point back: serve writes one and returns it (see \
                         turn.checkpoint), and the artifact does reconstitute the turn it came \
                         from — a_served_resume_point_reconstitutes_the_turn_it_came_from — but \
                         no route accepts one and nothing calls Engine::resume_turn, so an API \
                         host cannot ask the server to continue from where it crashed. Note \
                         stella-serve/tests/resume.rs is SSE stream resumption, a different \
                         resume entirely",
        },
    },
    Capability {
        id: "goal.loop",
        engine_home: "stella-core goal: judged rounds until an independent verifier assesses the goal met",
        engine_entries: &["run_goal", "assess"],
        cli: SurfacePosture::Shipped {
            mechanism: "`stella goal` / `stella monitor`, with a cross-family verifier resolved by \
                        default",
            witness: "distinct_families_route_a_cross_family_verifier",
        },
        // Shipped in #1297 as the mode flag this row named as its second
        // acceptable shape, deliberately not as a `/v1/goals` resource:
        // every transport concern a goal run has — streaming its progress,
        // stopping it, keeping the work already done — is a turn concern that
        // already exists and is already witnessed. A parallel resource would
        // restate all of them with its own id space and its own bugs.
        api: SurfacePosture::Shipped {
            mechanism: "a `goal` block on POST /v1/turns (and POST /v1/sessions/{id}/turns): \
                        judged rounds driven as a loop over the step driver, each round's \
                        verdict on the existing GET /v1/turns/{id}/events stream, stopped by \
                        POST /v1/turns/{id}/cancel with completed rounds kept",
            witness: "a_goal_run_is_requestable_over_the_wire_and_streams_its_rounds",
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
        // Shipped in #1297. The engine machinery was ready; what a served
        // turn lacked was the handle (no `task` in a host-advertised tool
        // stack) and the operator's answer to "may this deployment spend on
        // children at all", which is `ServeConfig::sub_agents` and defaults
        // to no.
        api: SurfacePosture::Shipped {
            mechanism: "a `sub_agents` block on POST /v1/turns layers the task tool over the \
                        host's remoted stack; children run on the same reverse-RPC ports \
                        (announcing their own provider_id and role), read-only and one level \
                        deep by construction, with pool/steps/provider clamped by \
                        ServeConfig::sub_agents",
            witness: "a_served_turn_can_delegate_to_a_sub_agent",
        },
    },
    Capability {
        id: "hooks.lifecycle",
        engine_home: "stella-core hooks (PreToolUse/PostToolUse/SessionStart/Stop/PreCompact, \
                      the #2684 stdout-decision plane in hooks::decision) and the observer-only \
                      HookBus",
        engine_entries: &["with_hooks", "with_bus", "with_hook_approval_route"],
        cli: SurfacePosture::ShippedUnwitnessed {
            mechanism: "workspace hooks wired via with_session_hook_context on every driver \
                        path. SessionStart firing is a HOST obligation (#2674): the engine \
                        deliberately has no method for it — a host fires hooks::run_hooks once \
                        while it assembles the system prompt, before any Engine exists, and \
                        owns surfacing the diagnostics the no-I/O engine cannot print",
            missing: "a CLI-side test pinning that a configured workspace hook actually fires \
                      through agent wiring (core has hook tests; the CLI wiring has none)",
        },
        api: SurfacePosture::Shipped {
            mechanism: "operator-installed ServeExtensions on a per-turn HookBus (#1298): \
                        turn/step/model boundaries observable, tool.call.requested \
                        interceptable before the reverse request leaves. Shell hooks stay \
                        deliberately unreachable server-side (a host must not be able to make \
                        the sidecar spawn shells) and NO route registers an extension — the \
                        bearer token authenticates a host, which is not the operator",
            witness: "operator_hooks_fire_across_a_served_turn",
        },
    },
    Capability {
        id: "budget.enforce",
        engine_home: "stella-core BudgetGuard: turn and session USD axes, enforced at step boundaries",
        engine_entries: &[],
        cli: SurfacePosture::Shipped {
            mechanism: "--spend-limit / --turn-timeout on the session axis, surviving across turns",
            witness: "spend_limit_cap_holds_across_turns_rather_than_resetting_each_one",
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
            mechanism: "seed_calibration from the store plus with_calibration on all seven \
                        assembly sites: the interactive, raw one-shot, goal, deck and \
                        sub-session paths hand it to the engine directly, and the default \
                        `stella run` staged-pipeline path and fleet workers lend it to the \
                        Pipeline, which attaches it to every engine it builds (#1595)",
            missing: "a CLI-side test pinning that persisted drift samples reach the engine's \
                      calibration on session start (stella-core and stella-store each test their \
                      half; the CLI seam between them has no witness). The pipeline half of the \
                      #1595 gap now has one — `the_pipeline_path_sizes_its_budget_with_the_\
                      callers_calibration` proves a lent map reaches the engines — but nothing \
                      yet proves `run_pipeline_one_shot` and the fleet worker do the lending",
        },
        api: SurfacePosture::Shipped {
            mechanism: "a process-lifetime CalibrationMap per provider_id, fed by every \
                        committed step and read at GET /v1/calibration (#1298). Samples are \
                        not persisted — serve deliberately has no store — so a redeployed \
                        sidecar re-converges, which is why the report carries `samples`",
            witness: "the_drift_report_is_readable_through_the_api",
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
        api: SurfacePosture::Shipped {
            mechanism: "an `engine` block (#1167) on POST /v1/turns and POST /v1/sessions/{id}/turns: \
                        max_output_tokens, temperature, effort, reasoning, params, \
                        compaction_budget_tokens, summarize_overflow, summarize_keep_recent, \
                        tool_result_horizon_steps and model_timeout_secs each lower onto the server \
                        default for that turn; values past an operator ceiling are clamped and the \
                        clamp is reported in the create response. max_steps and \
                        reverse_request_timeout_ms ride the request top level and pre-date #1167",
            witness: "engine_overrides_reach_the_provider_request_and_clamps_are_reported",
        },
    },
    Capability {
        id: "pipeline.verified_run",
        // Deliberately describes the mechanism rather than the slogan: the
        // ladder's guarantee is that the HOST authors the witness, runs its
        // command, and credits the flip it observed. Verification arriving as
        // a plugin is plugin-reported instead (#3511), so "verified done, not
        // claimed done" is true of this path and not of the binary.
        engine_home: "stella-pipeline: the plan/witness/verify/verdict ladder — the host authors \
                      the witness, runs its command, and credits only the fail→pass flip it \
                      observed itself",
        engine_entries: &[],
        cli: SurfacePosture::Shipped {
            mechanism: "`stella run` (default pipeline path) with the scope-review approval gate",
            witness: "non_tty_text_run_wiring_stays_headless_and_json_run_wiring_never_bypasses_scope_review",
        },
        // Shipped in #1288, as the mode flag this row's `waiting_on` named as
        // the alternative to a `/v1/runs` resource: `pipeline` on `POST
        // /v1/turns` (and `POST /v1/sessions/{id}/turns`) drives the turn
        // through `Pipeline::run` instead of a bare engine step loop, over
        // the SAME transport every other turn already uses — the SSE
        // stream, `POST /v1/turns/{id}/cancel`, and the settlement hook all
        // work unchanged, exactly as `goal.loop` (#1297) established for its
        // own mode flag. The one genuinely new wire primitive is the
        // approval gate: `ServerFrame::ScopeReviewRequest` and `POST
        // /v1/turns/{id}/approve`, symmetric with `tool-result` /
        // `provider-result` (`stella-serve/src/pipeline_run.rs`,
        // `stella-serve/src/remote.rs::RemoteApprovalGate`). Verification's
        // process-launching ports (`TestRunner`, `DiagnosticRunner`) are
        // remoted through the SAME `ToolRequest`/`ToolResultIn` frames every
        // tool call already crosses (`RemoteVerificationRunner`) — not a new
        // containment decision, `tools.local_execution`'s posture below
        // applied to two more typed callers.
        //
        // Declared, not silent, follow-up (`pipeline_run.rs`'s own module
        // docs carry the full list): context recall, repo structure, lint,
        // mutation, coverage, and candidate-workspace isolation are not
        // wired yet (every one of those ports is designed to degrade open
        // when absent, so a served run still verifies — it just cannot
        // isolate best-of-N candidates or measure diff coverage yet); the
        // witness-author role rides the worker's provider rather than
        // getting its own id the way verifier does; and `pipeline` is refused
        // alongside `goal`/`sub_agents` on the same turn rather than
        // composed with them.
        api: SurfacePosture::Shipped {
            mechanism: "a `pipeline` block on POST /v1/turns and POST /v1/sessions/{id}/turns \
                        drives the turn through Pipeline::run; POST /v1/turns/{id}/approve \
                        resolves the scope-review gate",
            witness: "a_pipeline_run_is_requestable_over_the_wire_and_its_approval_gate_round_trips",
        },
    },
    Capability {
        id: "tools.local_execution",
        engine_home: "stella-core ToolExecutor port over stella-tools' registry",
        engine_entries: &[],
        cli: SurfacePosture::Shipped {
            mechanism: "the built-in registry (sub-agent spawn, task board, scratch state, \
                        environment report) plus MCP and custom script tools, under policy \
                        layering — settings scopes plus the `--tools` per-invocation scope \
                        (#1263), which composes by intersection and can only narrow",
            witness: "a_settings_entry_hides_and_refuses_a_tool_in_the_real_stack",
        },
        api: SurfacePosture::NotApplicable {
            reason: "deliberate bring-your-own-tools posture: serve remotes every tool call to \
                     the host (STELLA_SERVE_TOOLS=remote is the only mode) so the sidecar never \
                     executes host-supplied work itself. A server-side tool mode would be a new \
                     capability row, not a change to this one",
        },
    },
    Capability {
        id: "tools.contracts",
        engine_home: "stella-core ports: ToolExecutor::contracts (#3287) and the AuthzGate seam \
                      evaluated ahead of dispatch on both surfaces (#2716); contracts and the \
                      shared verdict fold live in stella-protocol / stella-core::hooks::decision",
        engine_entries: &[],
        cli: SurfacePosture::Shipped {
            mechanism: "GatedToolSet assembled into every session chain by agent::tool_stack \
                        (#3283) — the gate sees each tool's resolved contract before the \
                        registry is reached",
            witness: "a_denying_gate_blocks_a_call_the_default_session_stack_allows",
        },
        api: SurfacePosture::Shipped {
            mechanism: "POST /v1/turns, POST /v1/sessions/{id}/turns — `tools` carries full \
                        ToolContracts (a bare schema upgrades to declared), and \
                        RemoteToolExecutor evaluates the gate before a ToolRequest frame \
                        leaves (#3286)",
            witness: "a_denying_gate_refuses_a_remoted_call_before_any_frame_leaves",
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
                        POST /v1/turns/{id}/provider-result, optionally streamed \
                        incrementally via POST /v1/turns/{id}/provider-delta \
                        (#1165), tool_request frames on \
                        POST /v1/turns/{id}/tool-result — the host owns keys and execution",
            witness: "an_unanswered_provider_request_fails_on_the_deadline",
        },
    },
    Capability {
        id: "provider.breaker_feedback",
        engine_home: "stella-core router CircuitBreaker + the ProviderOutcomes port: every \
                      logical model call's terminal verdict feeds the breaker, so `resolve` \
                      fails over from observed outcomes, not configuration (#2673)",
        engine_entries: &["with_provider_outcomes"],
        cli: SurfacePosture::ShippedUnwitnessed {
            mechanism: "the pipeline paths feed the router in PipelinePorts (attach() wires \
                        every engine; raw_usage records management calls) — witnessed in \
                        stella-pipeline by `pipeline_call_outcomes_reach_the_router_breaker`, \
                        which this sweep cannot see — and the bare loops feed a session-scoped \
                        `session_router` (run_interactive/run_raw_one_shot via run_turn, the \
                        goal loop's round verifier)",
            missing: "a CLI-side test pinning that `run_turn`'s engines actually attach the \
                      session router (the stella-pipeline and stella-core witnesses prove the \
                      layers below; the run_turn attachment itself has no witness because the \
                      engine assembly is inline in a function that drives a full turn)",
        },
        api: SurfacePosture::NotApplicable {
            reason: "a served run remotes every model call to the host, which owns keys and \
                     real provider selection — Stella-side failover between BYOK providers \
                     has nothing to fail over TO (the served router carries one profile per \
                     role). The pipeline still feeds that breaker, so a host failing the \
                     threshold of consecutive calls surfaces as AllProvidersUnavailable until \
                     the cooldown's half-open trial (see stella-serve pipeline_run's WallClock \
                     doc), but cross-provider failover is the host's concern by design",
        },
    },
    Capability {
        id: "provider.midturn_fallback",
        engine_home: "stella-core driver/model_fallback + the FallbackResolver port: a retry \
                      ladder exhausting mid-turn re-resolves the role through the router — \
                      whose breaker the failing calls already fed — and continues the turn on \
                      the replacement, transcript repaired, at most one swap per engine (#2679)",
        engine_entries: &["with_fallback_resolver"],
        cli: SurfacePosture::ShippedUnwitnessed {
            mechanism: "the pipeline's execute/revise engines attach a router-backed \
                        `StageFallback` that re-resolves the engine's own role (attach() wires \
                        every worker-role engine) — witnessed in stella-pipeline by \
                        `an_exhausted_execute_turn_re_resolves_the_worker_and_finishes_on_the_fallback`, \
                        which this sweep cannot see — and the bare loops attach a \
                        `SessionFallback` (agent/engine.rs) beside the session router at both \
                        `run_turn` engine sites, whose seam is witnessed in stella-core by \
                        `exhausted_retries_swap_to_the_resolved_fallback_and_the_turn_completes`",
            missing: "a CLI-side test pinning that `run_turn`'s engines actually attach the \
                      resolver (#2733 tracks the same gap for the router attachment). The \
                      pipeline's witness-author and research engines withhold the swap by \
                      declared posture rather than by omission (#2806), which is a design \
                      choice this row does not claim as coverage",
        },
        api: SurfacePosture::NotApplicable {
            reason: "a served run remotes every model call to the host, which owns keys and \
                     provider selection — the Stella side has nothing to swap TO; the same \
                     posture, for the same reason, as provider.breaker_feedback",
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
    fn cli_sources() -> [&'static str; 10] {
        [
            include_str!("../../stella-cli/src/agent/tests.rs"),
            // The steering-plane selection suite — home of the
            // `turn.steering_requery` witness (#3243 Phase 3).
            include_str!("../../stella-cli/src/memory/tests/steering_selection.rs"),
            // The tool-chain assembly seam (#3283) — home of the
            // `tools.contracts` witness.
            include_str!("../../stella-cli/src/agent/tool_stack.rs"),
            // `agent/tests.rs` is split into submodules by the file-size
            // ratchet, so its children have to be listed too — a witness that
            // moved into one of them is not missing, it is one `include_str!`
            // away from being invisible to this sweep.
            include_str!("../../stella-cli/src/agent/tests/engine_wiring.rs"),
            include_str!("../../stella-cli/src/subagent/tests.rs"),
            include_str!("../../stella-cli/src/subsession.rs"),
            include_str!("../../stella-cli/src/command_deck/tests.rs"),
            include_str!("../../stella-cli/src/session_persist.rs"),
            include_str!("../../stella-cli/src/engine_config.rs"),
            include_str!("../../stella-cli/src/tests.rs"),
        ]
    }

    /// API sources a witness may live in: the serve crate's unit tests plus
    /// its end-to-end suites.
    fn api_sources() -> [&'static str; 16] {
        [
            include_str!("../../stella-serve/src/server.rs"),
            // The remoted ports — home of the `tools.contracts` witness
            // (#3286). The witness tests live in the split-out submodule
            // file, which `include_str!` of the parent does not pull in.
            include_str!("../../stella-serve/src/remote.rs"),
            // `remote.rs` split its tests into a sibling submodule under the
            // file-size gate, so the witnesses live here — the same reason
            // `agent/tests.rs`'s children are listed above.
            include_str!("../../stella-serve/src/remote/tests.rs"),
            include_str!("../../stella-serve/tests/calibration.rs"),
            include_str!("../../stella-serve/tests/checkpoint.rs"),
            include_str!("../../stella-serve/tests/hooks.rs"),
            include_str!("../../stella-serve/tests/bridge.rs"),
            include_str!("../../stella-serve/tests/control.rs"),
            include_str!("../../stella-serve/tests/sessions.rs"),
            include_str!("../../stella-serve/tests/resume.rs"),
            include_str!("../../stella-serve/tests/shutdown.rs"),
            include_str!("../../stella-serve/tests/step_cancel.rs"),
            include_str!("../../stella-serve/tests/http.rs"),
            include_str!("../../stella-serve/tests/hostguard.rs"),
            include_str!("../../stella-serve/tests/goal_and_subagents.rs"),
            include_str!("../../stella-serve/tests/pipeline.rs"),
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

    /// A `Deferred` row may name a test too, and when it does the name is
    /// checked like a witness — because the risk it guards is the one this
    /// row already realized once (#1302).
    ///
    /// `turn.checkpoint_resume`'s API deferral turns on a distinction that is
    /// easy to state backwards: serve *does* write the versioned `Checkpoint`
    /// and *does* hand it back, and the artifact really does reconstitute the
    /// turn — the gap is that nothing accepts one. A reader who takes "not
    /// shipped" to mean "nothing is written" rebuilds what exists, which is
    /// exactly what a stale row costs. So the row cites the test that pins the
    /// resumable half, and this keeps that citation from decaying into a name
    /// nothing answers to.
    #[test]
    fn the_deferred_resume_row_cites_a_test_that_exists() {
        let row = capability("turn.checkpoint_resume").expect("the row is declared");
        let SurfacePosture::Deferred { waiting_on } = row.api else {
            panic!(
                "turn.checkpoint_resume's API posture moved — if serve now accepts a resume point \
                 back, this row is Shipped and needs a witness, not a citation"
            );
        };
        let cited = "a_served_resume_point_reconstitutes_the_turn_it_came_from";
        assert!(
            waiting_on.contains(cited),
            "the deferral no longer cites {cited} — say what proves the artifact is resumable, or \
             the row is back to asserting the gap without evidence"
        );
        assert!(
            witness_exists(&api_sources(), cited),
            "the deferral cites {cited}, which no swept serve source defines"
        );
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

    /// The engine sources both sweeps read: the driver and goal modules, plus
    /// every driver **submodule**.
    ///
    /// `Engine`'s inherent methods are spread across `driver/*.rs` — the turn
    /// loop itself is `driver/drive.rs` — and a fixed list of `include_str!`
    /// paths stops covering the tree the moment a submodule is added. That was
    /// not hypothetical: `drive_restored_turn` was a public `Engine` method
    /// living in `driver/resume.rs`, claimed by no row, and invisible to this
    /// guard for its entire life (#2452). Read from `CARGO_MANIFEST_DIR` at
    /// test time so a new sibling is swept the day it lands rather than the day
    /// someone remembers to add a line here.
    ///
    /// Test sources are excluded: a helper inside `#[cfg(test)] mod tests` is
    /// not an engine entry point, and sweeping them would demand matrix rows
    /// for test scaffolding.
    fn engine_sources() -> Vec<String> {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stella-core/src");
        let read = |path: std::path::PathBuf| {
            std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
        };
        let mut sources = vec![read(src.join("driver.rs")), read(src.join("goal.rs"))];
        let mut submodules: Vec<_> = std::fs::read_dir(src.join("driver"))
            .expect("stella-core/src/driver/ — did the driver sources move?")
            .map(|entry| entry.expect("a readable driver/ entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
            .filter(|path| path.file_stem().is_some_and(|stem| stem != "tests"))
            .collect();
        // Sorted so a failure names the same file on every machine; `read_dir`
        // yields in filesystem order, which differs across them.
        submodules.sort();
        sources.extend(submodules.into_iter().map(read));
        sources
    }

    /// Whether a column-zero `impl` line opens an inherent `Engine` block.
    ///
    /// Handles the three spellings these modules use — `impl<'a> Engine<'a>`,
    /// `impl Engine<'_>`, and `impl super::Engine<'_>` — and answers `false`
    /// for a trait impl (`impl Trait for T`), whose items are not entry
    /// points a caller reaches through `Engine`.
    fn opens_engine_impl(line: &str) -> bool {
        let rest = line.trim_end_matches(" {").trim_end();
        let Some(rest) = rest.strip_prefix("impl") else {
            return false;
        };
        // Skip the generic parameter list, if any: `<'a>` in `impl<'a> …`.
        let mut chars = rest.char_indices();
        let mut start = 0;
        if rest.starts_with('<') {
            let mut depth = 0usize;
            for (idx, ch) in chars.by_ref() {
                match ch {
                    '<' => depth += 1,
                    '>' => {
                        depth -= 1;
                        if depth == 0 {
                            start = idx + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if start == 0 {
                return false;
            }
        }
        let target = rest[start..].trim();
        // A trait impl names the trait first; its items are not `Engine`'s.
        if target.split_whitespace().any(|word| word == "for") {
            return false;
        }
        let path = target.split('<').next().unwrap_or("").trim();
        path.rsplit("::").next() == Some("Engine")
    }

    /// Engine-side completeness: every public `Engine` entry point in the
    /// driver and goal modules must be claimed by a row's `engine_entries`
    /// or by [`COMPOSITION_SEAMS`]. A new engine capability added without a
    /// matrix decision fails here, in the adding PR.
    ///
    /// The sweep reads source text for four-space-indented `pub fn` /
    /// `pub async fn` lines — `impl` items — which deliberately skips
    /// `pub(crate)` internals and module-level free functions.
    ///
    /// It is scoped to `impl … Engine…` blocks, which is what makes the
    /// question the one the doc comment asks. These modules also hold
    /// ordinary helper types, and a constructor on one of those is not an
    /// engine entry point: `TurnCapabilities::none` (#3390) is a parameter
    /// bundle's own `none`, and sweeping it demanded a matrix decision about
    /// a type the matrix does not describe.
    #[test]
    fn every_public_engine_entry_point_is_claimed() {
        let sources = engine_sources();
        let mut swept: Vec<&str> = Vec::new();
        for source in &sources {
            // Column-zero `impl` opens a block and column-zero `}` closes it,
            // which is exactly how rustfmt lays these files out.
            let mut in_engine_impl = false;
            for line in source.lines() {
                if line.starts_with("impl") {
                    in_engine_impl = opens_engine_impl(line);
                    continue;
                }
                if line == "}" {
                    in_engine_impl = false;
                    continue;
                }
                if !in_engine_impl {
                    continue;
                }
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

    /// The sweep's scoping rule, pinned directly.
    ///
    /// Without this the rule is only observable through the sweep's verdict,
    /// where "no entry point is unclaimed" and "no entry point was looked at"
    /// read identically — the failure mode a completeness test cannot afford.
    #[test]
    fn the_sweep_reads_engine_impls_and_skips_every_other_block() {
        for line in [
            "impl<'a> Engine<'a> {",
            "impl Engine<'_> {",
            "impl super::Engine<'_> {",
        ] {
            assert!(opens_engine_impl(line), "{line} opens an Engine block");
        }
        for line in [
            // Helper types sharing these modules — `TurnCapabilities::none`
            // is the one that turned #3390 into a red main.
            "impl<'a> TurnCapabilities<'a> {",
            "impl<'a> HooksHandle<'a> {",
            "impl Continuation {",
            // A trait impl's items are reached through the trait, not Engine.
            "impl ParkSupervisor for RateLimitPark<'_, '_> {",
            "impl Default for EngineConfig {",
        ] {
            assert!(!opens_engine_impl(line), "{line} is not an Engine block");
        }
    }

    /// SessionStart firing is a host obligation, never an engine entry
    /// (#2674). `Engine::run_session_start_hooks` shipped with no production
    /// caller: every CLI driver fires SessionStart while assembling the
    /// system prompt — before any Engine exists (the pipeline and fleet
    /// paths never construct one; `Pipeline::run` builds its own, per
    /// stage) — and must surface the hook diagnostics the no-I/O engine
    /// cannot print (#373), while the serve host deliberately keeps shell
    /// hooks unreachable. Two owners of one obligation is the drift the
    /// consolidated turn loop exists to end, so the dead engine copy was
    /// deleted; this pins both halves so a second owner cannot grow back
    /// silently, and that the row keeps naming who does own the firing.
    #[test]
    fn session_start_is_a_host_obligation_not_an_engine_entry() {
        let row = capability("hooks.lifecycle").expect("the row is declared");
        assert!(
            !row.engine_entries.contains(&"run_session_start_hooks"),
            "hooks.lifecycle claims run_session_start_hooks again — SessionStart firing is a \
             host obligation (#2674); an engine-side owner is the duplication that PR removed"
        );
        let (SurfacePosture::Shipped { mechanism, .. }
        | SurfacePosture::ShippedUnwitnessed { mechanism, .. }) = &row.cli
        else {
            panic!("hooks.lifecycle's CLI posture moved — keep naming the SessionStart owner");
        };
        assert!(
            mechanism.contains("with_session_hook_context") && mechanism.contains("obligation"),
            "the row no longer names the single host owner of SessionStart firing"
        );
        assert!(
            !engine_sources()
                .iter()
                .any(|source| source.contains("fn run_session_start_hooks")),
            "an engine source defines run_session_start_hooks again — the host obligation has \
             grown a second, engine-side owner (#2674)"
        );
    }

    /// Every claimed engine entry actually exists in the swept sources —
    /// the mirror of the sweep, so a renamed engine method cannot leave a
    /// row pointing at nothing.
    #[test]
    fn every_claimed_engine_entry_exists() {
        let sources = engine_sources();
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
