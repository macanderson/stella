//! The one list every per-variant fact about [`AgentEvent`] expands from.
//!
//! [`AgentEvent::type_tag`], [`KNOWN_TYPE_TAGS`] and [`SIGNAL_CONSUMERS`] are
//! all generated here from a single table. Keeping them in one place is
//! not tidiness — it is what makes two whole classes of bug unrepresentable
//! rather than merely tested for:
//!
//! - A tag missing from [`KNOWN_TYPE_TAGS`] would silently demote every event
//!   carrying it to [`AgentEvent::Unknown`] — data loss with no error.
//! - A variant with no declared consumer would be a signal nobody reads, the
//!   shape that cost four `solved_then_timeout` runs before #2661 (#2701).
//!
//! Both are now `E0004`: the generated `match` is exhaustive, so adding a
//! variant without adding a row here fails `cargo build -p stella-protocol`.
//!
//! # Why this is its own module (#2730)
//!
//! It used to live in `event.rs`, and the consumer postures could not be added
//! there: that file sat 35 lines under the 1500-line ratchet with no baseline
//! entry, and the postures are worth roughly a hundred. The ledger therefore
//! shipped as a hand-maintained table in [`consumers`] enforced by a *test*
//! ([`super::consumers::audit_ledger`]), which was strictly weaker. Splitting
//! the table out is what let the enforcement move from `cargo test` to
//! `cargo build`.
//!
//! [`consumers`]: super::consumers
//!
//! # Adding a variant
//!
//! Add a row below. The compiler will not let you skip it, and it will not let
//! you skip the posture either — but a posture is a **claim**, so read
//! [`ConsumerPosture`] before choosing one. There is no value meaning "not my
//! problem": [`ConsumerPosture::Unclassified`] is a debt under a down-only
//! ratchet ([`super::consumers::MAX_UNCLASSIFIED`]), and it will refuse to grow.
//!
//! Then propagate the variant to every downstream matcher — the table alone is
//! not enough:
//!
//! **Compile-enforced** — also exhaustive, so they will not build until you add
//! an arm; but each break surfaces one crate at a time (CI stops at the first
//! failing crate), which is exactly how #415's variant reached `main` before
//! breaking two crates on separate days — the then-existing `stella-pipeline`
//! (#421; that crate was deleted from the workspace in #3865, and its
//! `replay::event_signature` matcher went with it) then `stella-tui` (#422):
//!   - `stella-tui` `model::Model::apply`
//!   - `stella-tui` `textline::event_line`
//!   - `stella-tui` `deck::trace_of`
//!   - `stella-cli` `diag_bridge::DomainBridge::observe` — a record, a tally,
//!     or deliberately nothing. Listed since #3616, which found it by being
//!     stopped by the compiler rather than by reading this list.
//!
//! **Silent** — wildcard / `matches!` arms the compiler CANNOT catch, so a new
//! variant falls through to a default and is wrong only at runtime. These are
//! the real trap; audit them by hand:
//!   - `stella-tui` `deck::event_intensity` and `deck::status_from_event`: give
//!     the variant an intensity / agent status if it should register on the
//!     fleet deck.
//!
//! The same duty applies to the other exhaustively-matched cross-crate enums
//! this pattern warns about (`ToolOutput`, `BudgetOutcome`).
//!
//! Note that none of this is about *wire* safety any more — an older reader
//! survives your new variant via [`AgentEvent::Unknown`]. It is about this
//! workspace's own renderers staying complete.

use super::AgentEvent;
use super::consumers::{ConsumerPosture, SignalConsumers, Surface};

/// Expands the variant table into the tag mapping, the known-tag list, and the
/// signal-consumer ledger.
///
/// Each row is `Variant => "wire_tag", <posture>, <surfaces>;`. The `expr`
/// fragments are evaluated in const context, so a posture that is not
/// const-constructible is a compile error rather than a lazily-initialized
/// surprise.
macro_rules! agent_event_tags {
    ($($variant:ident => $tag:literal, $posture:expr, $surfaces:expr;)*) => {
        impl AgentEvent {
            /// The stable discriminant tag for this event — identical to the
            /// string `serde` writes as the `"type"` field on the stream-json
            /// wire. Allocation-free, so logs, metrics, and tests can name an
            /// event without serializing it.
            ///
            /// For [`AgentEvent::Unknown`] this returns the *preserved
            /// original* tag, not a placeholder — an unrecognized event still
            /// reports truthfully what it was on the wire.
            ///
            /// Which means the return value is no longer drawn from a closed
            /// set: an `Unknown` tag is arbitrary, unbounded, externally
            /// authored text. Grouping, indexing, or labelling a metric by
            /// this string is safe only against [`KNOWN_TYPE_TAGS`] — bucket
            /// anything outside it as one `unknown` cohort rather than letting
            /// a foreign stream drive cardinality.
            #[must_use]
            pub fn type_tag(&self) -> &str {
                match self {
                    $(AgentEvent::$variant { .. } => $tag,)*
                    AgentEvent::Unknown { event_type, .. } => event_type.as_str(),
                }
            }
        }

        /// Every `"type"` tag this build decodes into a typed variant.
        ///
        /// The deserializer's forward-compat fallback keys off exactly this
        /// list: a tag present here must parse into its variant or fail loudly;
        /// a tag absent from it becomes [`AgentEvent::Unknown`]. Consumers can
        /// also use it to detect that a stream came from a newer stella.
        pub const KNOWN_TYPE_TAGS: &[&str] = &[$($tag,)*];

        /// The signal-consumer ledger: what reads each event.
        ///
        /// Generated from the same table as [`KNOWN_TYPE_TAGS`], so it is total
        /// over the variants by construction. Re-exported as
        /// [`super::consumers::SIGNAL_CONSUMERS`], which is where its types and
        /// the remaining semantic checks live.
        pub const SIGNAL_CONSUMERS: &[SignalConsumers] = &[
            $(SignalConsumers {
                type_tag: $tag,
                posture: $posture,
                surfaces: $surfaces,
            },)*
        ];
    };
}

// The #4501 census, which drove [`ConsumerPosture::Unclassified`] to zero by
// reading each remaining row's consumers instead of leaving them unaudited.
// Three consumers recur below and are described once here instead of in
// thirty rows:
//
// - The **Observatory** selects `event_type` by name in four places, and a
//   row claiming `Surface::Observatory` appears in at least one:
//   `journal.rs::entries` (the transcript), `sessions.rs`'s
//   `TENDENCY_EVENT_TYPES` (the behavioural fold), `db.rs::recall_timings`,
//   and `sent_context.rs::journal_payloads`.
// - **`stella-serve`**'s `observe/tally.rs::TallyFold::observe` folds ten
//   variants into the `TurnTally` on each settle record — the counts a host
//   reads to tell a wedged turn from a waiting one. That is the arm
//   `Surface::Serve` names.
// - **`stella-cli/src/diag_bridge.rs`** writes a diagnostic record for nearly
//   every variant. Recording is not deciding, so it earns no posture on its
//   own; a row whose only readers are the bridge and the exhaustive TUI
//   renderers is `RecordedOnly`.
agent_event_tags! {
    // The run owner's stage boundaries. `Surfaced` on both axes: the
    // Observatory's journal query names the tag, and serve's tally counts
    // `StageScope::Run` boundaries as its progress axis. Nothing branches on
    // it — the stage has already changed by the time this announces it.
    Stage => "stage",
        ConsumerPosture::Surfaced,
        &[Surface::Observatory, Surface::Serve];
    // Exemplar — `Surfaced`. Assistant prose has no behavioral consumer by
    // design: the engine does not branch on what the model said in English.
    Text => "text",
        ConsumerPosture::Surfaced,
        &[Surface::Observatory];
    // The streaming preview. `Behavioral`: both durable writers refuse it —
    // `spawn_renderer` skips the store write and `stella-store`'s
    // `SessionJournal::write` skips the journal line — because the step's
    // `Text` event carries the same bytes in full. Delete either branch and
    // every answer doubles on disk, and the delta-coalescing runs tear into
    // per-token flushes.
    TextDelta => "text_delta",
        ConsumerPosture::Behavioral {
            site: "stella-cli/src/agent/persistence.rs::spawn_renderer",
        },
        &[];
    // `Behavioral`: `ReasoningRun::admit` folds consecutive fragments into one
    // block before the store write, so a page of thought costs one row instead
    // of tens of thousands (#3969). The Observatory selects the tag and
    // re-joins whatever fragments did land (`journal.rs::coalesce_reasoning`).
    Reasoning => "reasoning",
        ConsumerPosture::Behavioral {
            site: "stella-cli/src/agent/persistence/reasoning_run.rs::ReasoningRun::admit",
        },
        &[Surface::Observatory];
    // The other half of the `tool_result` row below: `project_event` opens the
    // `tool_calls` row that the result settles, so severing this leaves every
    // call unrecorded and every result orphaned. Also folded into the turn
    // facts a wrapper plugin judges (`stella-cli/src/turn_facts.rs`).
    ToolStart => "tool_start",
        ConsumerPosture::Behavioral {
            site: "stella-store/src/tool_calls.rs::project_event",
        },
        &[Surface::Observatory, Surface::Serve];
    // Exemplar — `Behavioral`. Projected into the store's `tool_calls` table
    // in the same transaction that records the event.
    //
    // Worth stating what is *not* the consumer here, because the intuitive
    // answer is wrong and the mistake is instructive: loop detection does not
    // read this signal. It reads the transcript
    // (`stella-core/src/driver/loop_evidence.rs::recent_call_records` over
    // `CompletionMessage`s), which is a different plane that happens to carry
    // the same facts. Citing it here would have described a dependency that
    // does not exist — and severing the event would not have been caught.
    //
    // `Surface::Serve` joined the row in the #4501 census: the tally reads
    // `output.is_error()` off this event for its `tools_failed` count, which
    // makes it a selecting arm the same way `tool_start` is.
    ToolResult => "tool_result",
        ConsumerPosture::Behavioral {
            site: "stella-store/src/tool_calls.rs::project_event",
        },
        &[Surface::Observatory, Surface::Serve];
    // `Behavioral`: the CGP trace recorder resolves the discarded call as
    // `Rejected`, so its tool pairing stays truthful — the I/O ran and the
    // model never saw it. Both Observatory lists select the tag, and serve's
    // tally counts it rather than re-deriving discarded work.
    SpeculationDiscarded => "speculation_discarded",
        ConsumerPosture::Behavioral {
            site: "stella-cli/src/arena.rs::observe",
        },
        &[Surface::Observatory, Surface::Serve];
    // `Behavioral`: the reflection digest's friction fold records each attempt
    // and its reason, which is what `Priority::Friction` selects on when a
    // lesson is mined into memory. The retry itself is the engine's decision;
    // `site` names the consumer that keeps it.
    Retry => "retry",
        ConsumerPosture::Behavioral {
            site: "stella-cli/src/memory/reflection/digest.rs::TurnFriction::observe",
        },
        &[Surface::Observatory, Surface::Serve];
    // `Surfaced`: the message was already injected at the step boundary by the
    // time this echoes it, so a host learns *when* it landed and nothing
    // decides on it. The Observatory's tendencies fold names the tag.
    Steered => "steered",
        ConsumerPosture::Surfaced,
        &[Surface::Observatory];
    // The park pair (#1471, #1857). `Surfaced`: serve's tally counts spans,
    // wakes, polls and licensed seconds, which is what keeps its `stages`
    // progress axis honest — a host reading a stalled turn can tell a
    // deliberate wait from a hang. Both tags sit in the Observatory's journal
    // query. The park loop itself runs on `stella-core::driver::waiting`, so
    // these events report it and never drive it.
    TurnParked => "turn_parked",
        ConsumerPosture::Surfaced,
        &[Surface::Observatory, Surface::Serve];
    TurnWoken => "turn_woken",
        ConsumerPosture::Surfaced,
        &[Surface::Observatory, Surface::Serve];
    // `Behavioral`: the friction fold records the pattern, its repeat count and
    // whether the loop aborted the turn, which is what a mined lesson cites.
    // The engine's own abort is decided in `stella-core`'s loop detection, so
    // `site` names the reflection consumer.
    LoopDetected => "loop_detected",
        ConsumerPosture::Behavioral {
            site: "stella-cli/src/memory/reflection/digest.rs::TurnFriction::observe",
        },
        &[Surface::Observatory, Surface::Serve];
    // `Surfaced`: the budget guard has already refused the call by the time
    // this reports the refusal (`stella-core/src/driver.rs`). The Observatory's
    // tendencies fold names the tag.
    BudgetDenied => "budget_denied",
        ConsumerPosture::Surfaced,
        &[Surface::Observatory];
    // `Behavioral`, same fold as `retry`: a doomed sequence reports its whole
    // run at once, and the friction entry carries the last reason — which is
    // the sentence a mined lesson quotes when a turn died on retries.
    RetriesExhausted => "retries_exhausted",
        ConsumerPosture::Behavioral {
            site: "stella-cli/src/memory/reflection/digest.rs::TurnFriction::observe",
        },
        &[Surface::Observatory, Surface::Serve];
    // `Surfaced`: the authorization bus emits a content-free record after the
    // decision is made (`stella-core/src/bus.rs`), so the deny has already
    // happened. The Observatory's tendencies fold names the tag.
    PolicyDecision => "policy_decision",
        ConsumerPosture::Surfaced,
        &[Surface::Observatory];
    // `Behavioral`: `journal_preimages` indexes each rewrite by content digest,
    // which is how a reconstructed transcript resolves a compacted block back
    // to its bytes and how the digest check over it can pass. Selected by the
    // Observatory's tendencies fold and by its sent-context view.
    Compaction => "compaction",
        ConsumerPosture::Behavioral {
            site: "stella-store/src/reconstruct.rs::journal_preimages",
        },
        &[Surface::Observatory];
    // `Behavioral`: reflection settlement strips the sub-turn's own ticks and
    // re-emits one from the session guard, so the remaining budget on the wire
    // is the session's. Drop that consumer and a nested report's ceiling
    // reaches the caller as if it were the session's.
    BudgetTick => "budget_tick",
        ConsumerPosture::Behavioral {
            site: "stella-cli/src/agent/budget.rs::settle_reflection_budget",
        },
        &[];
    // The metering record. `Behavioral`: `persist_event_detailed` writes the
    // per-call usage row every cost, cache and token figure is later read
    // from, and carries `complete` up into the execution's accounting bit.
    // Serve's tally counts it as the turn's model calls.
    StepUsage => "step_usage",
        ConsumerPosture::Behavioral {
            site: "stella-cli/src/agent/persistence.rs::persist_event_detailed",
        },
        &[Surface::Serve];
    // An attempt that died and produced no usage frame, which is a different
    // fact from a `step_usage` with `complete: false` (#4147). `Behavioral`:
    // the same persistence hop carries it into `usage_complete`, and
    // `settle_reflection_budget` counts it as accounting having happened. The
    // Observatory's tendencies fold names the tag.
    UsageIncomplete => "usage_incomplete",
        ConsumerPosture::Behavioral {
            site: "stella-cli/src/agent/persistence.rs::persist_event_detailed",
        },
        &[Surface::Observatory];
    // Audited out of the #2703 backlog by #3977. `Behavioral`: the reflection
    // digest splits a goal arc's journal at each verdict and labels every
    // segment from this event's own `round`, so deleting the consumer would
    // make the ledger reflection mines into memory attribute round 1's failed
    // tool to the last round — a change in what the engine does with its
    // evidence, not in what a human sees. The engine's own halt is decided by
    // the typed `GoalVerifierVerdict` (`stella-core/src/goal.rs`), never by
    // this event, so `site` names the reflection consumer rather than the loop.
    // `surfaces` stays empty and that is not a weaker claim: the TUI renders
    // every variant by construction (see `Surface`'s doc), the offline HTML
    // export in `stella-cli/src/export/transcript.rs` reads the recorded
    // stream rather than selecting variants, and no `Surface` chooses this tag
    // — the Observatory's journal query (`journal.rs`) and its
    // `TENDENCY_EVENT_TYPES` (`sessions.rs`) both name explicit `event_type`
    // lists that omit `goal_verdict`. `stella-cli/src/diag_bridge.rs` emits a
    // diagnostic record, which is recording, not deciding.
    GoalVerdict => "goal_verdict",
        ConsumerPosture::Behavioral {
            site: "stella-cli/src/memory/reflection/digest.rs::TurnFriction::per_goal_round",
        },
        &[];
    // Audited out of the #2703 backlog by #3916. `Surfaced` rather than
    // `Behavioral`: the swap has already happened in
    // `stella-core/src/driver/model_fallback.rs::attempt_provider_fallback`
    // by the time this is sent — the event announces the decision, it does
    // not carry it, so deleting every consumer would change what a human
    // sees and nothing the engine does. The Observatory is a genuinely
    // selecting surface: `sessions.rs`'s journal query names
    // `provider_fallback` in its explicit `event_type` list, counts it, and
    // renders it as a warn chip. `stella-cli/src/diag_bridge.rs` also
    // branches to a diagnostic record, which is recording, not deciding.
    ProviderFallback => "provider_fallback",
        ConsumerPosture::Surfaced,
        &[Surface::Observatory];
    // `Behavioral`: `TurnFacts::observe` folds these into the `changed_files`
    // a wrapper plugin's verdict rule reads (#3552). Before that tap existed
    // the field was always empty, and a wrapper gating on "did this turn touch
    // anything" graded every run as untouched. The Observatory's journal query
    // names the tag. The event's contract is unchanged by the posture:
    // observability, never evidence (#2873).
    FileChange => "file_change",
        ConsumerPosture::Behavioral {
            site: "stella-cli/src/turn_facts.rs::TurnFacts::observe",
        },
        &[Surface::Observatory];
    // `Behavioral`: the deck's forwarder holds the run's `Stage(Execute)`
    // boundary until the first event that is not a recall, so a turn's recall
    // renders ahead of the stage it opens. The Observatory's recall-timings
    // view reads latency, frame count and token cost off the same event.
    ContextRecall => "context_recall",
        ConsumerPosture::Behavioral {
            site: "stella-cli/src/command_deck/forwarder.rs::spawn_forwarder",
        },
        &[Surface::Observatory];
    // `RecordedOnly`: a memory write announces itself and nothing reads the
    // announcement — the diagnostic bridge records one, the TUI's textline
    // renders a line by exhaustive match, and no Observatory query names the
    // tag. Wiring a real consumer (a memory-pressure view, or a reflection
    // input) is what #4501 leaves open here.
    ContextWrite => "context_write",
        ConsumerPosture::RecordedOnly { issue: "#4501" },
        &[];
    // A context receipt. `Behavioral`: `persist_event_detailed` writes the
    // `context_blocks` row that the Observatory's block registry and
    // `stella-store`'s preimage reconstruction both resolve against — and the
    // gap preimage that only ever exists in that row.
    BlockRegistered => "block_registered",
        ConsumerPosture::Behavioral {
            site: "stella-cli/src/agent/persistence.rs::persist_event_detailed",
        },
        &[];
    // What one model call was compiled from. `Behavioral`: the same
    // persistence hop writes the manifest row, which is what a step's receipts
    // and its parallel tool calls join against by `(turn_instance, step,
    // call_seq)`.
    StepManifest => "step_manifest",
        ConsumerPosture::Behavioral {
            site: "stella-cli/src/agent/persistence.rs::persist_event_detailed",
        },
        &[];
    // The verification pair, post-rail (#3790). Neither row is `Unclassified`
    // debt: the consumers are known and named below; what remains undecided —
    // whether a verification plugin re-emits these variants — is a producer
    // question the plugin wire contract (#3511) settles, not an audit gap.
    //
    // `Proof`'s only production emitter was `stella-pipeline`, and that crate
    // has since left the tree (#3865), so no plugin-less run produces it —
    // confirmed rather than predicted by #3881, which decided all three of the
    // extraction's producerless surfaces together. What consumes
    // it today is the deck's traces tab
    // (`stella-tui/src/deck/classify.rs::proof_trace`), the offline transcript
    // export (`stella-cli/src/export/transcript.rs`), and a debug-level diag
    // record — all readers of the recorded stream, none of them a selecting
    // surface in [`Surface`]'s sense (the TUI renders every variant by
    // construction, so it is deliberately not listable).
    Proof => "proof",
        ConsumerPosture::RecordedOnly { issue: "#3790" },
        &[];
    // `Verdict` additionally folds the session model's verification state
    // (`stella-tui/src/model.rs`) into the one-line textline verdict — still
    // rendering, still no branch, still no selecting surface. It stays on the
    // wire because it is the natural event a verification plugin re-emits;
    // #3790 confirms that assumption when the wire contract is settled.
    Verdict => "verdict",
        ConsumerPosture::RecordedOnly { issue: "#3790" },
        &[];
    // `Behavioral`: the deck's forwarder seeds the task board from the
    // proposal's steps as the gate is reached, so the ids already resolve when
    // the first `task_start` arrives. Seeded on the proposal rather than on
    // approval, and `seed_from_plan` is a no-op on a board that already has
    // rows, so a declined or revised plan cannot clobber live work.
    ScopeReview => "scope_review",
        ConsumerPosture::Behavioral {
            site: "stella-cli/src/command_deck/forwarder.rs::spawn_forwarder",
        },
        &[];
    // `RecordedOnly`: the hunk gate's decision is already made when this
    // reports it, and the readers are the diagnostic bridge and the TUI's
    // exhaustive textline. No Observatory query names the tag. Whether a
    // review surface should select it is what #4501 leaves open here.
    HunkReview => "hunk_review",
        ConsumerPosture::RecordedOnly { issue: "#4501" },
        &[];
    // `Behavioral`, and the one row where the TUI is the consumer rather than
    // a renderer. `Model::apply` latches `pending_ask_user`, and
    // `stella-tui/src/deck_ui/gates.rs::handle_focused_gates` reads that latch
    // to claim the next digit or `⏎` for the answer card. Sever the arm and
    // the question is unanswerable: the gate's channel
    // (`stella-cli/src/command_deck/mid_turn_ask.rs`, which blocks on the
    // answer rather than on this event) never receives a reply and the turn
    // sits there until the session is killed.
    AskUser => "ask_user",
        ConsumerPosture::Behavioral {
            site: "stella-tui/src/model.rs::Model::apply (latches pending_ask_user)",
        },
        &[];
    // The media pair (#4454). `RecordedOnly` because #4448 removed
    // `stella-media` and left these with zero producers in this tree: they are
    // kept as the wire contract an out-of-tree media MCP surface would speak,
    // and removing them is a protocol break for anyone replaying a recording
    // that carries the tag. The TUI's textline still renders both by
    // exhaustive match. #4454 tracks the retire-or-keep decision at the next
    // `PROTOCOL_VERSION` bump.
    MediaProgress => "media_progress",
        ConsumerPosture::RecordedOnly { issue: "#4454" },
        &[];
    MediaComplete => "media_complete",
        ConsumerPosture::RecordedOnly { issue: "#4454" },
        &[];
    // The delivery artifacts. `Behavioral`: the calibration fold attaches every
    // commit and PR a session records after a pass to that pass, and settles
    // the unproven ones when the PR carries a terminal CI status — an
    // unproven pass whose PR failed CI is counted as a false positive. That
    // number is what the verifier's calibration report is.
    Commit => "commit",
        ConsumerPosture::Behavioral {
            site: "stella-cli/src/calibration.rs::calibration",
        },
        &[];
    Pr => "pr",
        ConsumerPosture::Behavioral {
            site: "stella-cli/src/calibration.rs::calibration",
        },
        &[];
    // `RecordedOnly`: the board's authority is `stella-core`'s `TaskBoard`,
    // which `command_deck/task_tap.rs` snapshots into this event after each
    // `task_*` call, and the durable `tasks` rows are written from that same
    // board rather than from the stream. What reads the event is the deck —
    // a transcript entry and the active-task elapsed/cost anchor
    // (`stella-tui/src/deck.rs`) — which is rendering, by exhaustive match. A
    // cross-session consumer (replay rebuilding a board from the journal) is
    // what #4501 leaves open here.
    TaskUpdate => "task_update",
        ConsumerPosture::RecordedOnly { issue: "#4501" },
        &[];
    // `RecordedOnly`: the delegation itself is driven by
    // `stella-core::subagent`'s dispatcher and this reports its phases. The
    // deck stamps a start/finish bracket from them (`deck.rs`), which is the
    // elapsed a human reads; nothing else consumes them. The controllable
    // sub-session lanes are a separate mechanism and do not ride this variant.
    SubAgent => "sub_agent",
        ConsumerPosture::RecordedOnly { issue: "#4501" },
        &[];
    // The delivery decision (#2942). `Surfaced` rather than `RecordedOnly`
    // because the observatory's journal query was widened to select it in the
    // same change — a reader asking "did this candidate's work land?" gets the
    // answer in the transcript instead of joining `verdict` timestamps against
    // a `file_change` burst by hand.
    //
    // Deliberately NOT `Behavioral`. Nothing branches on it, and claiming
    // otherwise would put a false statement in the ledger: the pipeline's own
    // decision is the typed `pipeline::delivery::Delivery` value this event is
    // a projection of, so severing the event changes what a reader sees and
    // nothing about what the engine does.
    //
    // The posture is about consumption and stays true; the *producer* is gone.
    // `Pipeline::deliver_winner` left with `stella-pipeline` (#3865), so no run
    // in this workspace emits this variant. #3881 decided to keep it rather
    // than retire it — recorded journals carry the tag, and a best-of-N wrapper
    // plugin reporting a delivery over the socket is what a re-homed producer
    // would look like. The variant's own doc carries the argument.
    CandidateDelivery => "candidate_delivery",
        ConsumerPosture::Surfaced,
        &[Surface::Observatory];
    // `Behavioral`, twice over in the session journal. `unsettled_prompts`
    // treats a non-retryable error as a settle, so resume does not return an
    // already-failed prompt to the front of the queue; `is_transition` fsyncs
    // the record, because a power cut must not take back the fact that the
    // turn failed. A retryable error settles nothing, which is what the
    // `retryable: false` pattern is doing there.
    Error => "error",
        ConsumerPosture::Behavioral {
            site: "stella-store/src/journal.rs::unsettled_prompts",
        },
        &[];
    // The engine's per-turn ending (#3379). `Behavioral`: the diagnostic
    // bridge drains its in-flight tool-call retention map on it, because that
    // is a *turn* obligation — a call still awaiting a result when a turn ends
    // will never get one — and doing it only on the run's `RunComplete` carried
    // one turn's stale entries into the next.
    //
    // The session journal is a second behavioral consumer: it classifies this
    // as a transition, which is what makes it flush durably instead of
    // buffering — the signal decides persistence timing, not just content.
    //
    // Deliberately not claiming the run owners as a third behavioral site:
    // `RunEnding` observes this event to author `RunComplete`, but it branches on
    // nothing — severing the signal would change what it reports, not what it
    // does.
    TurnComplete => "turn_complete",
        ConsumerPosture::Behavioral {
            site: "stella-cli/src/diag_bridge.rs::DomainBridge::observe",
        },
        &[Surface::Observatory];
    // The run's terminator — the only event that means "nothing more is
    // coming" (#3379). Behavioral rather than merely surfaced: it is what
    // ends a recording, settles the deck's terminal state, and closes the
    // stream-json evidence file.
    RunComplete => "run_complete",
        ConsumerPosture::Behavioral {
            site: "stella-cli/src/arena.rs::observe (run terminator, latches SessionOutcome::Completed)",
        },
        &[Surface::Observatory];
    // What the trust gate held back (#2302's harness half, #3616). `Surfaced`
    // and not `Behavioral`: nothing in the engine branches on it — the
    // steering was already withheld by the time this says so — and rendering
    // is exactly the realized value. The observatory journal names it in its
    // own whitelist, so the claim below is checkable rather than decorative,
    // and the TUI renders it the way it renders every variant.
    SteeringWithheld => "steering_withheld",
        ConsumerPosture::Surfaced,
        &[Surface::Observatory];
}
