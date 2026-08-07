//! The steer→abort escalation ladder loop detection drives: warn once, then
//! abort — and say honestly which loop the abort is about.
//!
//! `crate::loop_detect` decides *whether* the turn is looping, and
//! `super::loop_evidence` assembles what it reasons over. This module owns
//! what happens next: the first detection of a turn injects a steering
//! warning through the same seam as user steering and records the
//! [`LoopIdentity`] it warned about; any detection after that is the turn's
//! clean abort. The recorded identity is what keeps the abort message honest
//! — "persisted after a steering warning" is a claim about the *warned*
//! loop, and the nightly loop-bench caught the old unconditional phrasing
//! blaming a fresh `edit_file` loop on a `write_file` warning the model had
//! actually obeyed (#1524). Identity is the loop's tools *and* its
//! arguments, because the first pass recorded tool names alone and every
//! `bash` loop in a turn therefore read as one loop — the same false claim,
//! surviving underneath its own fix for the tool that loops most. Split out
//! of `driver.rs` the same way `super::settlement` was: the parent file sits
//! at its file-size ceiling.

use stella_protocol::{AgentEvent, CompletionMessage};

use crate::event_sender::EventSender;
use crate::loop_detect::{LoopIdentity, LoopVerdict, detect_loop};
use crate::ports::ToolExecutor;
use crate::step::AbortKind;

use super::loop_evidence::{ResultIdentities, recent_call_records};
use super::{EngineConfig, LOOP_STEER_PREFIX, TurnOutcome};

/// Loop detection, before spending a model call on a step that's already
/// stuck. Progress-aware: each call is paired with the output it produced
/// ([`recent_call_records`]), so only repeats and cycles with byte-identical
/// outputs count — legitimate polling (identical input, changing output)
/// never trips it (`crate::loop_detect`). `result_identities` is the turn's
/// pre-compaction snapshot ([`super::loop_evidence::snapshot_result_identities`])
/// — compaction runs immediately before this check and rewrites older
/// results in place, so without it a repeat streak reads as
/// `[stub, stub, real]` and never counts.
///
/// Steer first, abort second: the FIRST detection of a turn injects a
/// warning through the same seam as user steering — a user-role message the
/// model answers this very step, plus its `Steered` transcript event — and
/// lets the turn continue, recording the detected loop's [`LoopIdentity`] in
/// `loop_steered`. Only a detection after that warning is the turn's clean
/// abort (`Some`): the model was told and the turn is looping *again*, so
/// more steps would be pure waste. Whether the abort may claim the warned
/// loop persisted is [`abort_reason`]'s job, not a given — and when the
/// second loop is a genuinely different one, the honest answer is that the
/// steer worked (see #1524 and the identity it now compares).
pub(super) fn check_loop_detection(
    config: &EngineConfig,
    tools: &dyn ToolExecutor,
    messages: &mut Vec<CompletionMessage>,
    result_identities: &ResultIdentities,
    loop_steered: &mut Option<LoopIdentity>,
    total_cost_usd: f64,
    events: &EventSender,
) -> Option<TurnOutcome> {
    let turn_instance = config.turn_instance;
    let records = recent_call_records(messages, result_identities);
    let verdict = detect_loop(&records, config.loop_detection);
    // The typed twin of the prose steer/abort (receipts spec §6.3): a
    // receipt parses this instead of string-matching "stuck-loop
    // detected:" prefixes. Emitted for both outcomes — `aborted` says
    // whether this detection steered or killed the turn. Matching the
    // verdict is also the healthy-turn early return, so there is exactly
    // one place that decides "is this a loop".
    let (kind, pattern, repeats) = match &verdict {
        LoopVerdict::NoLoop => return None,
        LoopVerdict::ExactRepeat { tool, count, .. } => {
            ("exact_repeat", vec![tool.clone()], *count)
        }
        LoopVerdict::ShortCycle { pattern, repeats } => (
            "short_cycle",
            pattern.iter().map(|c| c.name.clone()).collect(),
            *repeats,
        ),
        LoopVerdict::Stagnant { tool, count } => ("stagnation", vec![tool.clone()], *count),
        // A distinct `kind` rather than folding into `exact_repeat`: a
        // receipt reading these should be able to tell "the same call three
        // times in a row" from "three times with other work between them",
        // because they are different pathologies and the second is the one
        // no contiguous scan can see (#1851).
        LoopVerdict::InterleavedRepeat { tool, count, .. } => {
            ("interleaved_repeat", vec![tool.clone()], *count)
        }
    };
    let evidence = verdict
        .evidence()
        .unwrap_or_else(|| "loop detected".to_string());
    // `identity` is `Some` for every verdict but `NoLoop`, which returned
    // above. Propagated rather than unwrapped: nothing here is worth a panic,
    // and `None` here means the same thing it means above — not a loop.
    let identity = verdict.identity()?;
    let _ = events.send(AgentEvent::LoopDetected {
        turn_instance,
        kind: kind.to_string(),
        pattern,
        repeats,
        evidence: evidence.clone(),
        aborted: loop_steered.is_some(),
    });
    let Some(warned) = loop_steered else {
        *loop_steered = Some(identity);
        // Duration priors (#1472) are not wired yet: `None` states the
        // blocking wait without a number, and the composer cites one the
        // moment a caller can supply it.
        let text = steer_text(&evidence, polling_tool(&verdict, tools).as_deref(), None);
        let _ = events.send(AgentEvent::Steered { text: text.clone() });
        messages.push(CompletionMessage::user(text));
        return None;
    };
    let reason = abort_reason(warned, &identity, &evidence);
    let _ = events.send(AgentEvent::Error {
        message: reason.clone(),
        retryable: false,
    });
    Some(TurnOutcome::Aborted {
        reason,
        kind: AbortKind::DeliberateStop,
        cost_usd: total_cost_usd,
    })
}

/// The polling shape (#1473): ONE read-only tool re-issued against state
/// that has not changed. The detector only trips on byte-identical outputs,
/// so by verdict time "nothing changed" is established fact — what remains
/// is whether the repeated call was a status check (read-only, where "vary
/// the arguments / try another tool" is precisely wrong) or an action worth
/// varying. Cycles keep the generic steer: several tools are involved and
/// no single wait replaces them. Pure over the verdict plus the declared
/// schemas (invariant #2) — `schemas()` returns owned declarations, no I/O.
fn polling_tool(verdict: &LoopVerdict, tools: &dyn ToolExecutor) -> Option<String> {
    let tool = match verdict {
        LoopVerdict::ExactRepeat { tool, .. }
        | LoopVerdict::Stagnant { tool, .. }
        | LoopVerdict::InterleavedRepeat { tool, .. } => tool,
        LoopVerdict::NoLoop | LoopVerdict::ShortCycle { .. } => return None,
    };
    tools
        .schemas()
        .iter()
        .any(|schema| schema.name == *tool && schema.read_only)
        .then(|| tool.clone())
}

/// The first-detection steer. Generic by default; for a polling loop the
/// prescription is the ONE move that helps — a single blocking wait —
/// because the generic "vary the arguments, try a different tool" makes a
/// polling model strictly worse (#1473). The session that motivated this
/// obeyed the generic steer into a 600s `gh run watch` that timed out as a
/// tool error and voided the prompt cache, then blind `sleep` calls with no
/// signal at all. The wait cites a duration prior when one is supplied
/// (#1472's data, once a caller carries it) and is stated without a number
/// otherwise. Volatile mid-turn context either way — invariant #7 safe by
/// construction.
fn steer_text(evidence: &str, polling_tool: Option<&str>, prior_secs: Option<u64>) -> String {
    let Some(tool) = polling_tool else {
        return format!(
            "{LOOP_STEER_PREFIX}] you appear to be looping: {evidence}. Repeating the \
             same call cannot produce new information — change strategy: vary the \
             arguments, try a different tool, or report what is blocking you. If you \
             keep looping, the turn will be aborted."
        );
    };
    let sizing = match prior_secs {
        Some(secs) => format!(
            "this operation typically takes ~{secs}s here, so make ONE blocking wait \
             of at least that long (a single call whose timeout exceeds {secs}s)"
        ),
        None => "make ONE blocking wait sized to how long this operation usually \
                 takes — a single call with a generous timeout, not repeated checks"
            .to_string(),
    };
    format!(
        "{LOOP_STEER_PREFIX}] you are polling: {evidence} — `{tool}` is read-only and \
         its result has not changed between your calls. Do NOT vary the arguments or \
         switch tools: the state you are waiting on genuinely has not changed, and \
         checking it differently cannot change it. Instead, {sizing}; then check once. \
         If nothing lets you wait, report what you are blocked on and stop. If you \
         keep polling, the turn will be aborted."
    )
}

/// The abort's reason line, phrased by what the steering warning was about.
///
/// The one-warning-per-turn policy is untouched — a second detection always
/// aborts. What varies is the claim: "persisted" is only true when the
/// re-detected loop *is* the warned one. A different loop means the steer
/// worked on the loop it named and a fresh one formed, and saying so matters
/// precisely because the two read the same in a verdict table — the false
/// "persisted" hid a working steer for months (#1524).
///
/// Which loop is which is [`LoopIdentity`]'s call, not this function's, and
/// that is the whole of the #1524 follow-up: comparing tool *names* made
/// every `bash` loop in a turn the same loop, so the claim stayed false for
/// the tool that loops most. A `None` answer is a turn resumed from a
/// checkpoint written before identities carried arguments — it cannot
/// support either claim, so it makes none.
fn abort_reason(warned: &LoopIdentity, detected: &LoopIdentity, evidence: &str) -> String {
    match warned.same_loop_as(detected) {
        None => format!("stuck-loop detected (after an earlier steering warning): {evidence}"),
        Some(true) => {
            format!("stuck-loop detected (persisted after a steering warning): {evidence}")
        }
        Some(false) => format!(
            "stuck-loop detected (a new loop after a steering warning about {}, which the \
             model did break): {evidence}",
            warned.describe()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{LoopIdentity, LoopVerdict, abort_reason, polling_tool, steer_text};
    use crate::ports::ToolExecutor;
    use stella_protocol::tool::{ToolOutput, ToolSchema};

    fn tools(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    /// A registry with one status-shaped (read-only) tool and one mutating
    /// tool — the classification input, nothing more.
    struct StatusAndBash;

    #[async_trait::async_trait]
    impl ToolExecutor for StatusAndBash {
        fn schemas(&self) -> Vec<ToolSchema> {
            let schema = |name: &str, read_only: bool| ToolSchema {
                name: name.into(),
                description: String::new(),
                input_schema: serde_json::json!({}),
                read_only,
                speculation_safe: read_only,
            };
            vec![schema("ci_status", true), schema("bash", false)]
        }
        async fn execute(&self, _name: &str, _input: &serde_json::Value) -> ToolOutput {
            unreachable!("steer classification never executes a tool")
        }
    }

    fn exact_repeat(tool: &str) -> LoopVerdict {
        LoopVerdict::ExactRepeat {
            tool: tool.to_string(),
            input: serde_json::json!({"wait": true}),
            count: 3,
        }
    }

    /// #1473's witness: a 3-repeat of a read-only status tool with
    /// byte-identical output steers to ONE blocking wait — where the
    /// generic text prescribed "vary the arguments", the exact move that
    /// made the motivating session strictly worse (a 600s watch timeout
    /// that voided the prompt cache, then blind sleeps).
    #[test]
    fn a_polling_loop_is_steered_to_one_blocking_wait_not_variation() {
        let verdict = exact_repeat("ci_status");
        let tool = polling_tool(&verdict, &StatusAndBash);
        assert_eq!(tool.as_deref(), Some("ci_status"));

        let text = steer_text("ci_status repeated 3×", tool.as_deref(), None);
        assert!(text.contains("ONE blocking wait"), "{text}");
        assert!(
            !text.contains("vary the arguments, try a different tool"),
            "the generic prescription is precisely wrong for a poll: {text}"
        );
        assert!(
            text.contains("has not changed"),
            "the steer must say WHY re-checking is futile: {text}"
        );
    }

    /// A supplied duration prior is cited with a concrete number (#1472's
    /// data, once a caller carries it).
    #[test]
    fn a_duration_prior_sizes_the_wait_concretely() {
        let text = steer_text("ci_status repeated 3×", Some("ci_status"), Some(720));
        assert!(text.contains("~720s"), "{text}");
    }

    /// A mutating tool keeps the generic steer: repeating an ACTION is not
    /// polling, and "vary the arguments" is real advice there.
    #[test]
    fn a_mutating_repeat_keeps_the_generic_steer() {
        let verdict = exact_repeat("bash");
        assert_eq!(polling_tool(&verdict, &StatusAndBash), None);
        let text = steer_text("bash repeated 3×", None, None);
        assert!(text.contains("vary the arguments"), "{text}");
    }

    /// A cycle involves several tools; no single wait replaces it.
    #[test]
    fn a_cycle_keeps_the_generic_steer_even_over_read_only_tools() {
        let verdict = LoopVerdict::ShortCycle {
            pattern: Vec::new(),
            repeats: 3,
        };
        assert_eq!(polling_tool(&verdict, &StatusAndBash), None);
    }

    /// An exact-repeat loop: one tool, one set of arguments.
    fn repeat(tool: &str, input: &str) -> LoopIdentity {
        LoopIdentity {
            tools: tools(&[tool]),
            inputs: Some(vec![input.to_string()]),
        }
    }

    /// A `bash` loop on `command`, the shape of nearly every real one.
    fn bash(command: &str) -> LoopIdentity {
        repeat("bash", &format!("{{\"command\":\"{command}\"}}"))
    }

    #[test]
    fn re_detecting_the_warned_pattern_claims_persistence() {
        let reason = abort_reason(&bash("cargo test"), &bash("cargo test"), "bash repeated 4×");
        assert_eq!(
            reason, "stuck-loop detected (persisted after a steering warning): bash repeated 4×",
            "the byte-exact legacy phrasing must survive for the one case it was true of"
        );
    }

    /// #1524 witness: on `prove-plus-comm` the model was warned about a
    /// `write_file` loop, stopped repeating `write_file`, and was then
    /// aborted over a fresh `edit_file` loop by a message claiming the
    /// warned loop had "persisted". The abort stays; the false claim goes.
    #[test]
    fn a_different_pattern_does_not_claim_the_warned_loop_persisted() {
        let reason = abort_reason(
            &repeat("write_file", r#"{"path":"a.rs"}"#),
            &repeat("edit_file", r#"{"path":"a.rs"}"#),
            "edit_file repeated 3×",
        );
        assert!(
            !reason.contains("persisted"),
            "a fresh loop must not be blamed on the warned one: {reason}"
        );
        assert!(
            reason.contains("write_file"),
            "the abort should name what the warning was actually about: {reason}"
        );
        assert!(
            reason.contains("edit_file repeated 3×"),
            "the detector's evidence for the loop that DID abort stays: {reason}"
        );
    }

    #[test]
    fn a_cycle_pattern_only_persists_if_the_whole_cycle_matches() {
        let cycle = LoopIdentity {
            tools: tools(&["read_file", "bash"]),
            inputs: Some(vec![
                r#"{"path":"a.rs"}"#.to_string(),
                r#"{"command":"cargo test"}"#.to_string(),
            ]),
        };
        let reason = abort_reason(&cycle, &bash("cargo test"), "bash repeated 3×");
        assert!(!reason.contains("persisted"), "{reason}");
    }

    #[test]
    fn an_unknown_warned_pattern_makes_no_claim_either_way() {
        let unknown = LoopIdentity {
            tools: Vec::new(),
            inputs: None,
        };
        let reason = abort_reason(&unknown, &bash("cargo test"), "bash repeated 3×");
        assert!(
            !reason.contains("persisted") && !reason.contains("new loop"),
            "a checkpoint-resumed turn with no recorded pattern cannot support \
             either claim: {reason}"
        );
        assert!(reason.contains("steering warning"), "{reason}");
    }

    /// A turn resumed from a checkpoint that recorded the warned *tools* but
    /// not the arguments (#1524's format) knows the tool and not the loop.
    /// Tool-name equality is exactly the inference this change removed, so
    /// it must not sneak back in through the resume path.
    #[test]
    fn a_resumed_turn_that_recorded_only_tools_still_makes_no_claim() {
        let partial = LoopIdentity {
            tools: tools(&["bash"]),
            inputs: None,
        };
        let reason = abort_reason(&partial, &bash("cargo test"), "bash repeated 3×");
        assert!(
            !reason.contains("persisted") && !reason.contains("new loop"),
            "knowing only the tool is not knowing the loop: {reason}"
        );
    }

    /// The witness, taken verbatim from the run that exposed this
    /// (`ses-1785988025124`, worker `moonshotai/kimi-k3`): warned about one
    /// `grep`, the model changed the command as instructed, looped on a
    /// different `grep` over a different file — and the abort told the user
    /// the warned loop had "persisted". Both loops are `bash`, so tool-name
    /// identity called them one loop and #1524's fix could not fire.
    #[test]
    fn two_different_bash_loops_are_not_one_loop() {
        let warned = bash(r#"grep -n \"fn open\\b\" -B 8 crates/stella-graph/src/store.rs"#);
        let detected = bash(r#"grep -n \"fn index_all\" -B 4 crates/stella-graph/src/graph.rs"#);
        assert_eq!(
            warned.same_loop_as(&detected),
            Some(false),
            "same tool, different arguments — the detector keyed on the \
             arguments, so identity must too"
        );

        let reason = abort_reason(
            &warned,
            &detected,
            "the same `bash` call … repeated 3 times",
        );
        assert!(
            !reason.contains("persisted"),
            "the model obeyed the steer and broke the warned loop; the abort \
             must not say otherwise: {reason}"
        );
        assert!(
            reason.contains("fn open"),
            "and it should name the loop the warning was actually about, or \
             a reader cannot tell the two `bash` loops apart: {reason}"
        );
    }

    /// The other half: arguments are what identity is keyed on, so the same
    /// `bash` command re-detected still reports persistence. Without this,
    /// "compare the arguments too" could be satisfied by never claiming
    /// persistence at all.
    #[test]
    fn the_same_bash_command_twice_still_persists() {
        let loop_ = bash("cargo test --workspace");
        assert_eq!(loop_.same_loop_as(&loop_), Some(true));
        assert!(
            abort_reason(&loop_, &loop_, "bash repeated 3×").contains("persisted"),
            "the byte-exact legacy phrasing must survive for the case it is true of"
        );
    }

    /// Stagnation's loop spans *differing* arguments by definition, so its
    /// identity is the tool alone and two stagnation verdicts on one tool
    /// are the same loop. Recorded as an empty input list, which must not be
    /// confused with the `None` of an unrecorded one.
    #[test]
    fn stagnation_identity_is_the_tool_alone() {
        let stagnant = |tool: &str| LoopIdentity {
            tools: tools(&[tool]),
            inputs: Some(Vec::new()),
        };
        assert_eq!(stagnant("bash").same_loop_as(&stagnant("bash")), Some(true));
        assert_eq!(
            stagnant("bash").same_loop_as(&stagnant("read_file")),
            Some(false)
        );
        assert_eq!(
            stagnant("bash").same_loop_as(&bash("cargo test")),
            Some(false),
            "an empty input list is a real identity, not an absent one"
        );
    }
}
