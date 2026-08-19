//! The one list every per-variant fact about [`AgentEvent`] expands from.
//!
//! Three things are generated here from a single table: [`AgentEvent::type_tag`],
//! [`KNOWN_TYPE_TAGS`], and [`SIGNAL_CONSUMERS`]. Keeping them in one place is
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
//! breaking `stella-pipeline` (#421) then `stella-tui` (#422):
//!   - `stella-pipeline` `replay::event_signature`
//!   - `stella-tui` `model::Model::apply`
//!   - `stella-tui` `textline::event_line`
//!   - `stella-tui` `deck::trace_of`
//!
//! **Silent** — wildcard / `matches!` arms the compiler CANNOT catch, so a new
//! variant falls through to a default and is wrong only at runtime. These are
//! the real trap; audit them by hand:
//!   - `stella-pipeline` `replay::structural_diff` volatile keep-set: add the
//!     variant if it is a run-to-run artifact absent from older golden streams,
//!     or it will shift every aligned position of the diff.
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

agent_event_tags! {
    Stage => "stage",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[Surface::Observatory];
    // Exemplar — `Surfaced`. Assistant prose has no behavioral consumer by
    // design: the engine does not branch on what the model said in English.
    Text => "text",
        ConsumerPosture::Surfaced,
        &[Surface::Observatory];
    TextDelta => "text_delta",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    Reasoning => "reasoning",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[Surface::Observatory];
    ToolStart => "tool_start",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[Surface::Observatory];
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
    ToolResult => "tool_result",
        ConsumerPosture::Behavioral {
            site: "stella-store/src/tool_calls.rs::project_event",
        },
        &[Surface::Observatory];
    SpeculationDiscarded => "speculation_discarded",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[Surface::Observatory];
    Retry => "retry",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    Steered => "steered",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    TurnParked => "turn_parked",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[Surface::Observatory];
    TurnWoken => "turn_woken",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[Surface::Observatory];
    LoopDetected => "loop_detected",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    BudgetDenied => "budget_denied",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    RetriesExhausted => "retries_exhausted",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    PolicyDecision => "policy_decision",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    Compaction => "compaction",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    BudgetTick => "budget_tick",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    StepUsage => "step_usage",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    UsageIncomplete => "usage_incomplete",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    GoalVerdict => "goal_verdict",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    ProviderFallback => "provider_fallback",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    FileChange => "file_change",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    ContextRecall => "context_recall",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    ContextWrite => "context_write",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    BlockRegistered => "block_registered",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    StepManifest => "step_manifest",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    // The verification pair, post-rail (#3790). Neither row is `Unclassified`
    // debt: the consumers are known and named below; what remains undecided —
    // whether a verification plugin re-emits these variants — is a producer
    // question the plugin wire contract (#3511) settles, not an audit gap.
    //
    // `Proof`'s only production emitter is `stella-pipeline`, so once that
    // crate leaves the tree no plugin-less run can produce it; what consumes
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
    ScopeReview => "scope_review",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    HunkReview => "hunk_review",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    AskUser => "ask_user",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    MediaProgress => "media_progress",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    MediaComplete => "media_complete",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    Commit => "commit",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    Pr => "pr",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    TaskUpdate => "task_update",
        ConsumerPosture::Unclassified { issue: "#2703" },
        &[];
    SubAgent => "sub_agent",
        ConsumerPosture::Unclassified { issue: "#2703" },
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
    CandidateDelivery => "candidate_delivery",
        ConsumerPosture::Surfaced,
        &[Surface::Observatory];
    Error => "error",
        ConsumerPosture::Unclassified { issue: "#2703" },
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
            site: "stella-pipeline/src/replay.rs::validate_stream (stream terminator)",
        },
        &[Surface::Observatory];
}
