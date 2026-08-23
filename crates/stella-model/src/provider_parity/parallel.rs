//! The parallel-tool-call axis of the provider-parity matrix (#4163).
//!
//! The engine runs consecutive read-only tool calls concurrently, and the
//! system prompt asks the model to use that ("Send independent tool calls
//! together in one response" — `stella-cli`'s `tool_steering!` macro, #2985).
//! Whether a provider will *emit* several tool calls in one assistant
//! message, and whether the request has to ask for it, is therefore a
//! capability the working surface depends on — and it was declared on no
//! axis, tested by nothing, and assumed by both halves.
//!
//! # Why this axis is a pair of facts, not one word
//!
//! The other five axes each name one thing a provider does and one test
//! proving it. This one cannot, because two independent facts decide whether
//! a parallel call ever reaches a tool:
//!
//! 1. **Admission** — does the *request* have to opt in, or does the dialect
//!    admit several tool calls per assistant message by default? This is a
//!    fact about a vendor's live API. It cannot be established from inside
//!    this repository by reading code, and a wiremock test cannot establish
//!    it either: a mock answers with whatever the test author wrote.
//! 2. **Fan-in** — does *this adapter* turn N tool calls in one assistant
//!    message into N [`ToolCall`]s, or does it keep the first and drop the
//!    rest? This is a fact about code in this tree, and a wiremock test
//!    settles it exactly.
//!
//! Collapsing them into one posture word is how a row would come to claim
//! more than its evidence supports — the failure CLAUDE.md's evidence rule
//! exists to prevent. So [`ParallelToolCallPosture`] carries both: an
//! [`ParallelAdmission`] that records **how the admission question was
//! settled, or that it was not**, and a `fan_in_witness` that every row must
//! name, because every row can prove that half.
//!
//! [`ToolCall`]: stella_protocol::ToolCall
//!
//! # What the evidence said, and what it did not
//!
//! #4163 was filed on the reading that nothing sends `parallel_tool_calls`,
//! so parallel calls were presumed not to be happening — measured as 69
//! distinct timestamps across 71 tool calls in one execution. The issue was
//! explicit that this did not distinguish "we never asked" from "it
//! declined", and named settling that as the first job.
//!
//! It is settled, and it went the other way. Grouping `tool_start` events by
//! the `step_manifest` that precedes them — one `step_manifest` is one model
//! call, so one group is one assistant message — over the `store.db` of a
//! workspace that ran the route:
//!
//! | route | tool-bearing messages | tool calls | messages with 2+ | most in one |
//! |---|---|---|---|---|
//! | `openrouter` / `moonshotai/kimi-k3` (executions 140–177) | 648 | 891 | 92 | 9 |
//! | `zai` / `glm-5.2` (all executions) | 382 | 496 | 79 | 5 |
//! | `anthropic` / `claude-fable-5` (executions 25–33) | 132 | 199 | 60 | 3 |
//!
//! All three routes emit parallel tool calls in volume, and none has ever been
//! sent an opt-in — `parallel_tool_calls` appears nowhere in this tree, and
//! Anthropic's control is an opt-*out* this adapter never sends. So **no
//! request field is added by this axis**, on any of the three ids where the
//! question is answered. Adding one would change the wire bytes of every
//! request on the shared adapter, risk a 400 on the local servers that
//! reject unknown keys, and buy a capability the measurement says is already
//! there.
//!
//! The single execution that prompted the issue (178) is an outlier within
//! its own route, not a provider limitation. Its consequence was real —
//! #4151, a model routing 24 of 26 reads through `sed` — but the cause is not
//! a missing request field.
//!
//! Three honest limits on that census. It covers the executions that read
//! cleanly: this repository's own `store.db` has genuine page corruption
//! (`pragma quick_check` reports `btreeInitPage` errors on trees 45 and 46),
//! which is why the `openrouter` row cites an execution range rather than the
//! whole table, and why execution 178 itself could not be re-counted this way.
//! The `anthropic` row is counted in a **different workspace's** store, named
//! in the row, because the two anthropic-direct executions in this repository's
//! own store record no tool use at all — a row's evidence names the store it
//! was read from for exactly that reason. And it is evidence about *routes
//! actually run* — it says those three ids admit parallel calls without being
//! asked, and says nothing about the other seven, which is why they are
//! [`ParallelAdmission::Undetermined`] rather than assumed to match.

/// How a provider's *request* admits more than one tool call per assistant
/// message — the half of this axis that is a fact about a vendor's live API
/// rather than about code in this tree.
///
/// Deliberately not defaulted to an optimistic value. "Every OpenAI-compatible
/// server defaults `parallel_tool_calls` to true" is a plausible sentence that
/// would make eight rows green without a single observation behind them,
/// which is the shape of claim invariant #8 exists to refuse.
#[derive(Debug)]
pub enum ParallelAdmission {
    /// Several tool calls per assistant message are admitted with nothing
    /// sent, and that was *observed* on this provider — not inferred from a
    /// vendor's documentation and not assumed from a sibling dialect.
    DefaultOn {
        /// What was observed and where, in enough detail to re-run. Prose,
        /// checked by a reviewer rather than by a test — the observation is
        /// a fact about a past run, and no test can re-establish it.
        evidence: &'static str,
    },
    /// The adapter must send an explicit opt-in or this provider emits at
    /// most one tool call per assistant message.
    ///
    /// No id is classified here today, and the module doc explains why the
    /// answer for the two settled ids was "nothing to send". The variant
    /// exists so the day one provider *does* need the field, the row can say
    /// so and name the wiremock test asserting it on the request body —
    /// rather than the field being added to the shared adapter for everyone
    /// with nothing recording which provider actually wanted it.
    OptIn {
        /// The request field, for humans reading the matrix.
        field: &'static str,
        /// Name of the test proving the field reaches the request body;
        /// checked for existence by this module's tests.
        witness: &'static str,
    },
    /// This repository has no observation either way for this provider.
    ///
    /// A declared gap, in the sense [`OverflowPosture::BestEffort`] is one:
    /// the adapter's fan-in still works (that is the `fan_in_witness`'s job
    /// and it is proven), so a provider that does volunteer parallel calls is
    /// served correctly today. What is unknown is only whether it volunteers
    /// them, and running one and counting upgrades the row.
    ///
    /// [`OverflowPosture::BestEffort`]: super::OverflowPosture::BestEffort
    Undetermined {
        /// Why it is unknown and what would settle it.
        note: &'static str,
    },
}

/// One provider's parallel-tool-call row: how the request admits several
/// calls, plus the test proving this dialect's adapter actually fans several
/// of them in.
///
/// `fan_in_witness` is a struct field rather than a per-variant one because
/// it is required on *every* row, including a gap. That is the whole point of
/// splitting the axis: an [`ParallelAdmission::Undetermined`] provider still
/// gets its fan-in proven, so an unknown row means "we have not watched this
/// provider volunteer a parallel call", never "we do not know what we would
/// do with one".
#[derive(Debug)]
pub struct ParallelToolCallPosture {
    /// How the request admits more than one tool call per assistant message.
    pub admission: ParallelAdmission,
    /// Name of the test proving this dialect turns several tool calls in one
    /// assistant message into several `ToolCall`s. Checked for existence by
    /// this module's tests, exactly as the other axes check theirs.
    ///
    /// Several ids share one name where they share one adapter — the shared
    /// chat-completions dialect serves five — which is the same reuse
    /// `vertex` already makes of `gemini`'s witnesses.
    pub fan_in_witness: &'static str,
}

/// One parallel-tool-call row per provider id constructible by the CLI — the
/// same completeness contract as the other five axes, enforced from
/// `stella-cli`'s config tests. Settings-defined custom providers inherit the
/// shared OpenAI-compatible adapter and its fan-in, so they need no row.
pub static PARALLEL_TOOL_CALL_POSTURE: &[(&str, ParallelToolCallPosture)] = &[
    (
        "openrouter",
        ParallelToolCallPosture {
            admission: ParallelAdmission::DefaultOn {
                evidence: "observed on this repository's own store.db: grouping tool_start \
                           events by the preceding step_manifest (one step_manifest is one \
                           model call, so one group is one assistant message) over executions \
                           140–177 on moonshotai/kimi-k3 gives 891 tool calls across 648 \
                           tool-bearing messages, 92 of which carry two or more and one of \
                           which carries nine — with parallel_tool_calls never sent, because \
                           it appears nowhere in this tree",
            },
            fan_in_witness: "several_tool_calls_in_one_message_fan_in_as_several_calls",
        },
    ),
    (
        "zai",
        ParallelToolCallPosture {
            admission: ParallelAdmission::DefaultOn {
                evidence: "same census, same store, glm-5.2 across every zai execution: 496 \
                           tool calls across 382 tool-bearing messages, 79 carrying two or \
                           more, most-in-one five — with nothing sent to ask for it",
            },
            fan_in_witness: "several_tool_calls_in_one_message_fan_in_as_several_calls",
        },
    ),
    (
        "xai",
        ParallelToolCallPosture {
            admission: ParallelAdmission::Undetermined {
                note: "shares the chat-completions adapter with the two settled ids, which \
                       makes it likely rather than known; no xai execution exists in this \
                       repository's telemetry to count. One run with tools, censused the way \
                       the module doc describes, settles it",
            },
            fan_in_witness: "several_tool_calls_in_one_message_fan_in_as_several_calls",
        },
    ),
    (
        "deepseek",
        ParallelToolCallPosture {
            admission: ParallelAdmission::Undetermined {
                note: "same shared adapter, same absence of a local run to count. DeepSeek \
                       is the id most worth actually measuring rather than assuming: it is \
                       already the outlier on the reasoning axis (no effort control at all), \
                       so inheriting a sibling's answer here is exactly the inference this \
                       matrix refuses",
            },
            fan_in_witness: "several_tool_calls_in_one_message_fan_in_as_several_calls",
        },
    ),
    (
        "local",
        ParallelToolCallPosture {
            admission: ParallelAdmission::Undetermined {
                note: "not one provider but a family — llama.cpp, vLLM, ollama and whatever \
                       else speaks the dialect — so there is no single answer to observe. \
                       Emitting several tool calls per message is a property of the served \
                       model and the server's tool-call template, and a row claiming one \
                       answer for all of them would be wrong for some of them",
            },
            fan_in_witness: "several_tool_calls_in_one_message_fan_in_as_several_calls",
        },
    ),
    (
        "anthropic",
        ParallelToolCallPosture {
            admission: ParallelAdmission::DefaultOn {
                evidence: "same census, counted in the store of a workspace that ran the \
                           route (~/Projects/oxagen-platform/.stella/private/store.db, \
                           pragma quick_check ok) rather than this repository's own, whose \
                           two anthropic-direct executions record no tool use at all: \
                           claude-fable-5 over executions 25–33 gives 199 tool calls across \
                           132 tool-bearing messages, 60 carrying two or more, most-in-one \
                           three, every step_manifest declaring provider anthropic and role \
                           worker. The request-side control here is an opt-*out* \
                           (tool_choice.disable_parallel_tool_use) which this adapter never \
                           sends, so nothing was asked for and nothing needs adding",
            },
            fan_in_witness: "several_tool_use_blocks_fan_in_as_several_calls",
        },
    ),
    (
        "bedrock",
        ParallelToolCallPosture {
            admission: ParallelAdmission::Undetermined {
                note: "Converse returns toolUse content blocks and the adapter fans every \
                       one of them in; no bedrock execution exists in this repository's \
                       telemetry to count",
            },
            fan_in_witness: "several_tool_use_blocks_fan_in_as_several_calls_on_converse",
        },
    ),
    (
        "openai",
        ParallelToolCallPosture {
            admission: ParallelAdmission::Undetermined {
                note: "the Responses dialect emits one function_call item per call, keyed by \
                       output_index, and the adapter fans them in; no openai execution \
                       exists in this repository's telemetry to count. This is the one id \
                       where a request field of this exact name (parallel_tool_calls) is \
                       documented, so it is the first place to look if a measurement ever \
                       comes back sequential",
            },
            fan_in_witness: "several_function_call_items_fan_in_as_several_calls",
        },
    ),
    (
        "gemini",
        ParallelToolCallPosture {
            admission: ParallelAdmission::Undetermined {
                note: "several functionCall parts may ride one candidate and the adapter \
                       fans every one of them in; no gemini execution exists in this \
                       repository's telemetry to count",
            },
            fan_in_witness: "several_function_call_parts_fan_in_as_several_calls",
        },
    ),
    (
        "vertex",
        ParallelToolCallPosture {
            admission: ParallelAdmission::Undetermined {
                note: "shares gemini's aggregator and therefore its fan-in and its gap",
            },
            fan_in_witness: "several_function_call_parts_fan_in_as_several_calls",
        },
    ),
];

/// The declared parallel-tool-call posture for `provider_id`, or `None` for
/// an id the matrix doesn't know — which the `stella-cli` completeness test
/// turns into a hard failure for any seeded provider.
pub fn parallel_tool_call_posture(provider_id: &str) -> Option<&'static ParallelToolCallPosture> {
    PARALLEL_TOOL_CALL_POSTURE
        .iter()
        .find(|(id, _)| *id == provider_id)
        .map(|(_, posture)| posture)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row's fan-in witness must exist as a test function in the
    /// adapter sources — the proof that this dialect delivers N calls rather
    /// than the first one. Unlike the other axes' witness checks, this one
    /// skips nothing: a gap row declares an *unmeasured admission*, never an
    /// unproven fan-in.
    #[test]
    fn every_fan_in_witness_exists_in_the_adapter_sources() {
        let sources = super::super::adapter_sources();
        for (id, posture) in PARALLEL_TOOL_CALL_POSTURE {
            let needle = format!("fn {}(", posture.fan_in_witness);
            assert!(
                sources.iter().any(|source| source.contains(&needle)),
                "parallel-tool-call fan-in witness for `{id}` not found in adapter sources: {}",
                posture.fan_in_witness
            );
        }
    }

    /// An `OptIn` row must name a test proving the field reaches the request
    /// body. No row is `OptIn` today; this guard is what stops the first one
    /// landing as a bare assertion.
    #[test]
    fn every_opt_in_row_names_a_request_body_witness() {
        let sources = super::super::adapter_sources();
        for (id, posture) in PARALLEL_TOOL_CALL_POSTURE {
            let ParallelAdmission::OptIn { witness, .. } = &posture.admission else {
                continue;
            };
            let needle = format!("fn {witness}(");
            assert!(
                sources.iter().any(|source| source.contains(&needle)),
                "parallel-tool-call opt-in witness for `{id}` not found in adapter \
                 sources: {witness}"
            );
        }
    }

    /// **Witness for #4197's third settled id.** `anthropic` is the row the
    /// issue singled out as unanswerable from this repository's own store —
    /// its two anthropic-direct executions record no tool use — and it is
    /// answered now, from a workspace that ran the route. Pinned because a
    /// measurement that lives only in a prose field is a measurement a clean
    /// merge can drop in silence (#2495, and #2462 is where that happened).
    #[test]
    fn anthropic_admits_parallel_tool_calls_by_default() {
        let posture = parallel_tool_call_posture("anthropic").expect("anthropic has a row");
        assert!(
            matches!(posture.admission, ParallelAdmission::DefaultOn { .. }),
            "anthropic was measured emitting up to three tool calls per assistant \
             message with nothing sent to ask for it; the row must say so: {:?}",
            posture.admission
        );
    }

    #[test]
    fn parallel_provider_ids_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for (id, _) in PARALLEL_TOOL_CALL_POSTURE {
            assert!(
                seen.insert(id),
                "duplicate parallel-tool-call row for `{id}`"
            );
        }
    }

    #[test]
    fn lookup_finds_every_row_and_rejects_unknown_ids() {
        for (id, _) in PARALLEL_TOOL_CALL_POSTURE {
            assert!(parallel_tool_call_posture(id).is_some());
        }
        assert!(parallel_tool_call_posture("no-such-provider").is_none());
    }

    /// A `DefaultOn` row's evidence must actually say something. The variant
    /// is the only one on this axis that makes a positive claim about a
    /// vendor's live behavior, and the claim is unfalsifiable unless the row
    /// says what was counted — so an empty or hand-wavy string is the one
    /// thing that must not compile past review unnoticed.
    #[test]
    fn every_default_on_row_cites_what_was_counted() {
        for (id, posture) in PARALLEL_TOOL_CALL_POSTURE {
            let ParallelAdmission::DefaultOn { evidence } = &posture.admission else {
                continue;
            };
            assert!(
                evidence.len() > 80,
                "`{id}` claims parallel calls are admitted by default but its evidence is \
                 too thin to re-run: {evidence:?}"
            );
        }
    }
}
