//! The selection's mechanics: what it keeps, what it says it dropped, what it
//! costs, and that it is a function of the turn alone.
//!
//! The end-to-end witnesses live one level up, in
//! `memory::reflection::tests` — they drive `reflect_on_turn` and assert against
//! the prompt that actually went on the wire, which is what makes them
//! witnesses. These are the unit tests underneath them.

use super::*;
use stella_protocol::{ModelCallRole, ToolCall, ToolResult};

fn user(text: &str) -> CompletionMessage {
    CompletionMessage::user(text)
}

fn assistant(text: &str) -> CompletionMessage {
    CompletionMessage::assistant(text)
}

/// An assistant message that calls one tool.
fn calls(call_id: &str, name: &str, input: serde_json::Value) -> CompletionMessage {
    CompletionMessage {
        role: MessageRole::Assistant,
        content: String::new(),
        tool_calls: vec![ToolCall {
            call_id: call_id.into(),
            name: name.into(),
            input,
        }],
        tool_results: Vec::new(),
        attachments: Vec::new(),
    }
}

/// The `Tool` message answering it — built the way the engine builds it, with
/// `content` empty and the payload in `tool_results`.
fn answers(call_id: &str, output: ToolOutput) -> CompletionMessage {
    CompletionMessage {
        role: MessageRole::Tool,
        content: String::new(),
        tool_calls: Vec::new(),
        tool_results: vec![ToolResult {
            call_id: call_id.into(),
            output,
        }],
        attachments: Vec::new(),
    }
}

fn ok(content: &str) -> ToolOutput {
    ToolOutput::Ok {
        content: content.into(),
        data: None,
    }
}

fn failed(message: &str) -> ToolOutput {
    ToolOutput::error(message)
}

/// `steps` routine tool exchanges around one that failed at `at`.
///
/// Each routine result is deliberately longer than [`FILLER_CHARS`], so a turn
/// of any real length actually exhausts the budget — a fixture of terse
/// one-liners fits inside it and elides nothing, which tests the wrong thing.
fn long_turn(steps: usize, at: usize, failure: &str) -> Vec<CompletionMessage> {
    let mut transcript = vec![user("make the suite green")];
    for step in 0..steps {
        let id = format!("call_{step}");
        transcript.push(calls(&id, "bash", serde_json::json!({"cmd": "cargo test"})));
        transcript.push(answers(
            &id,
            if step == at {
                failed(failure)
            } else {
                ok(&format!(
                    "step {step}: {}",
                    "running 412 tests ... ok ".repeat(30)
                ))
            },
        ));
    }
    transcript.push(assistant("gave up"));
    transcript
}

#[test]
fn a_tool_result_is_rendered_from_tool_results_not_from_content() {
    let transcript = vec![
        user("read the config"),
        calls("c1", "read_file", serde_json::json!({"path": "Cargo.toml"})),
        answers("c1", ok("[workspace]\nmembers = [\"crates/*\"]")),
    ];
    let out = build(TurnEvidence::from_transcript(&transcript, true));
    assert!(
        out.contains("read_file ok: [workspace]"),
        "a `Tool` message's payload is in `tool_results` and its `content` is \
         empty, so reading `content` renders every tool result as nothing — the \
         defect this module exists to fix.\n\n{out}"
    );
    assert!(
        !out.lines().any(|line| line.trim() == "tool:"),
        "a bare `tool:` line with no payload is the old rendering\n\n{out}"
    );
}

#[test]
fn a_failed_call_survives_the_middle_of_a_long_turn() {
    let transcript = long_turn(40, 6, "linker error: undefined symbol _stella_main");
    let out = build(TurnEvidence::from_transcript(&transcript, false));
    assert!(
        out.contains("linker error: undefined symbol _stella_main"),
        "the failure is the point of the turn and it is 60 messages from the \
         end\n\n{out}"
    );
    assert!(
        out.contains("bash FAILED:"),
        "a failure must be labelled as one, not left for the model to sniff out \
         of prose\n\n{out}"
    );
}

#[test]
fn the_goal_and_the_outcome_are_never_dropped() {
    let transcript = long_turn(60, 3, "boom");
    let out = build(TurnEvidence::from_transcript(&transcript, false));
    assert!(
        out.contains("make the suite green"),
        "the first user message is the goal; judging a turn without it is \
         judging work whose point is invisible\n\n{out}"
    );
    assert!(
        out.contains("gave up"),
        "the outcome is pinned too\n\n{out}"
    );
}

#[test]
fn elisions_are_declared_and_counted() {
    let transcript = long_turn(60, 3, "boom");
    let out = build(TurnEvidence::from_transcript(&transcript, false));
    assert!(
        out.contains("is a SELECTION of"),
        "a model reading a selection must be told it is reading one, or it will \
         reason confidently across a gap\n\n{out}"
    );
    assert!(
        out.contains("messages elided …"),
        "every gap is marked in place, not merely counted in the header\n\n{out}"
    );
}

/// Nothing is elided when nothing needs to be: a short turn renders whole, with
/// no selection header and no gap markers to explain away.
#[test]
fn a_short_turn_carries_no_selection_notice() {
    let transcript = vec![
        user("bump the version"),
        calls("c1", "edit_file", serde_json::json!({"path": "Cargo.toml"})),
        answers("c1", ok("1 replacement")),
        assistant("done"),
    ];
    let out = build(TurnEvidence::from_transcript(&transcript, true));
    assert!(
        !out.contains("SELECTION") && !out.contains("elided"),
        "a turn that fits needs no apology\n\n{out}"
    );
}

/// The whole rendering, spelled out — a golden a reviewer can read.
///
/// Every other test here asserts one property and would pass over a digest that
/// was correct and unreadable. This one is the shape itself: the friction section
/// first, then the transcript, one line per message part. If it fails, read the
/// diff and decide whether the new shape is better; do not re-bless it unread.
///
/// The elided form is `elisions_are_declared_and_counted`; this turn fits, and a
/// turn that fits carries no selection notice.
#[test]
fn the_whole_rendering_reads_like_this() {
    let mut friction = TurnFriction::default();
    friction.observe(&step_usage(3, ModelCallRole::Worker, 0.0812, 31_400));
    friction.observe(&AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "c2".into(),
            name: "bash".into(),
            input: serde_json::json!({}),
        },
        sub_agent_id: None,
    });
    friction.observe(&AgentEvent::ToolResult {
        call_id: "c2".into(),
        output: failed("error[E0599]: no method named `parse_amount`"),
        duration_ms: 12_800,
        speculated: false,
        sub_agent_id: None,
    });

    let transcript = vec![
        user("make the money test pass"),
        calls(
            "c0",
            "read_file",
            serde_json::json!({"path": "src/money.rs"}),
        ),
        answers(
            "c0",
            ok("pub fn to_cents(v: f64) -> i64 { (v * 100.0) as i64 }"),
        ),
        calls("c1", "read_file", serde_json::json!({"path": "src/lib.rs"})),
        answers("c1", ok("pub mod money;")),
        calls(
            "c2",
            "bash",
            serde_json::json!({"cmd": "cargo test -p money"}),
        ),
        answers("c2", failed("error[E0599]: no method named `parse_amount`")),
        calls(
            "c3",
            "edit_file",
            serde_json::json!({"path": "src/money.rs"}),
        ),
        answers("c3", ok("1 replacement")),
        assistant("used money::parse_amount; the test is green"),
    ];

    let out = build(TurnEvidence {
        transcript: &transcript,
        friction: std::slice::from_ref(&friction),
        succeeded: true,
    });
    // Ten messages under a 6k budget: nothing is elided, so there is no
    // selection notice and no gap marker. That is the point of the pairing with
    // `elisions_are_declared_and_counted` — the notice appears when, and only
    // when, something was actually dropped.
    //
    // Tool arguments render as the JSON the model produced, not as a bare value.
    // The key is the readable half: `{"cmd": …}` and `{"path": …}` say what kind
    // of mistake an argument was, which is the whole reason the arguments are
    // here instead of the old `[called: bash]`.
    let expected = "\
Where this turn spent itself (from its event stream):
- 1 model calls, $0.0812, 31.4s of model wall clock
- costliest: step 3 (worker) $0.0812, 31.4s, 2 tool calls
- slowest tool: bash (c2) took 12.8s
- 1 tool calls FAILED:
  - bash (c2): error[E0599]: no method named `parse_amount`

user: make the money test pass
assistant: → calls read_file({\"path\":\"src/money.rs\"})
tool: read_file ok: pub fn to_cents(v: f64) -> i64 { (v * 100.0) as i64 }
assistant: → calls read_file({\"path\":\"src/lib.rs\"})
tool: read_file ok: pub mod money;
assistant: → calls bash({\"cmd\":\"cargo test -p money\"})
tool: bash FAILED: error[E0599]: no method named `parse_amount`
assistant: → calls edit_file({\"path\":\"src/money.rs\"})
tool: edit_file ok: 1 replacement
assistant: used money::parse_amount; the test is green";
    assert_eq!(out, expected, "\n--- actual ---\n{out}\n--- end ---");
}

/// A credential in a tool argument or a tool result never reaches the digest.
///
/// This is not inherited caution. The old tail digest carried tool *names* and no
/// tool output at all, so it could not leak a token; this one renders arguments
/// and results, which is precisely where one appears — a `read_file` of `.env`, a
/// `bash` that printed `env`. Reflection also dispatches on the triage route,
/// which may be a different vendor than the worker, so "that provider has seen it
/// already" does not hold. Mirrors the dataset exporter's own planted-key test.
#[test]
fn a_planted_credential_never_reaches_the_digest() {
    const PLANTED: &str = "sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let transcript = vec![
        user("why is auth failing"),
        calls("c1", "read_file", serde_json::json!({"path": ".env"})),
        answers("c1", ok(&format!("ANTHROPIC_API_KEY={PLANTED}"))),
        calls(
            "c2",
            "bash",
            serde_json::json!({"cmd": format!("curl -H 'x-api-key: {PLANTED}' https://api")}),
        ),
        answers("c2", failed(&format!("401 unauthorized for {PLANTED}"))),
        assistant("the key is revoked"),
    ];
    let out = build(TurnEvidence::from_transcript(&transcript, false));
    assert!(
        !out.contains(PLANTED),
        "a credential reached the reflection prompt, which routes to the triage \
         model and not necessarily to the provider that already saw it\n\n{out}"
    );
    assert!(
        out.contains(stella_core::redact::PLACEHOLDER),
        "the redaction must be visible where it happened, so a reader knows the \
         digest was edited rather than that the turn said nothing\n\n{out}"
    );
    assert!(
        out.contains("ANTHROPIC_API_KEY=") && out.contains("401 unauthorized for"),
        "only the secret is replaced — the surrounding evidence is the whole \
         point of showing the tool result\n\n{out}"
    );
}

/// The budget is the cost control, and it binds. Measured on a turn far larger
/// than any real one so the ceiling is the thing under test, not the fixture.
#[test]
fn the_digest_stays_inside_its_character_budget() {
    let mut transcript = vec![user("port the whole module")];
    for step in 0..300 {
        let id = format!("call_{step}");
        transcript.push(calls(
            &id,
            "read_file",
            serde_json::json!({"path": format!("crates/stella-core/src/file_{step}.rs")}),
        ));
        transcript.push(answers(&id, ok(&"x".repeat(20_000))));
    }
    transcript.push(assistant("done"));
    let out = build(TurnEvidence::from_transcript(&transcript, true));
    let spent = out.chars().count();
    // Pinned messages are admitted outside the budget by design — one goal plus
    // OUTCOME_TAIL_MESSAGES, each capped at PINNED_CHARS — so the ceiling is the
    // budget plus that bounded allowance, never unbounded.
    let ceiling = DIGEST_BUDGET_CHARS + (OUTCOME_TAIL_MESSAGES + 1) * PINNED_CHARS;
    assert!(
        spent <= ceiling,
        "the digest spent {spent} characters against a {ceiling} ceiling; the \
         budget is what keeps reflection's per-turn cost arguable"
    );
}

/// The measurement #2460's definition of done asks for, as a test rather than a
/// sentence in a PR description: what the old tail digest and the new selection
/// each spend on the same turn.
///
/// It asserts only the direction and the bound. The exact numbers move with every
/// prompt edit, and a test that pinned them would fail for reasons that are not
/// about cost.
#[test]
fn the_selection_costs_more_than_the_tail_it_replaces_and_says_how_much() {
    let transcript = long_turn(40, 6, "linker error: undefined symbol _stella_main");

    // The digest as it was: the last twelve messages, each `content` truncated
    // to 300 characters. Reproduced here to make the comparison a measurement
    // instead of a recollection.
    let old: String = transcript
        .iter()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|message| {
            let role = match message.role {
                MessageRole::System => "system",
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
            };
            let content: String = message.content.chars().take(300).collect();
            let tools = if message.tool_calls.is_empty() {
                String::new()
            } else {
                format!(
                    " [called: {}]",
                    message
                        .tool_calls
                        .iter()
                        .map(|call| call.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            format!("{role}: {content}{tools}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let new = build(TurnEvidence::from_transcript(&transcript, false));

    let (before, after) = (old.chars().count(), new.chars().count());
    // Reported, not just asserted: `cargo test -- --nocapture
    // the_selection_costs_more` is how the number in the PR description is
    // obtained, so it can be re-derived instead of trusted.
    println!(
        "#2460 digest cost on an {}-message tool-heavy turn: tail={before} chars \
         (~{} tokens) → selection={after} chars (~{} tokens)",
        transcript.len(),
        before / 4,
        after / 4
    );
    assert!(
        before < 400,
        "the old digest of an 82-message tool-heavy turn was {before} \
         characters. If this ever grows, the premise of the comparison has \
         changed — a tool message rendered as the six characters `tool: ` is \
         why it was so small:\n\n{old}"
    );
    assert!(
        after > before,
        "the selection costs {after} characters against the tail's {before}. It \
         is SUPPOSED to cost more — it carries evidence the tail did not — and a \
         version that costs the same is one that stopped reading tool results."
    );
    assert!(
        after <= DIGEST_BUDGET_CHARS + (OUTCOME_TAIL_MESSAGES + 1) * PINNED_CHARS,
        "the extra spend is bounded by the budget, not open-ended: {after}"
    );
}

/// A `System` message is the byte-stable prompt prefix (invariant 7), identical
/// on every turn of the session. Spending digest budget on it buys nothing.
#[test]
fn the_system_prompt_is_not_turn_evidence() {
    let transcript = vec![
        CompletionMessage::system("You are Stella. NEVER_IN_A_DIGEST"),
        user("do it"),
        assistant("done"),
    ];
    let out = build(TurnEvidence::from_transcript(&transcript, true));
    assert!(
        !out.contains("NEVER_IN_A_DIGEST"),
        "the system prompt is the same every turn — reflection already knows \
         it\n\n{out}"
    );
}

#[test]
fn a_tool_calls_arguments_are_shown_because_the_argument_is_often_the_mistake() {
    let transcript = vec![
        user("run the tests"),
        calls(
            "c1",
            "bash",
            serde_json::json!({"cmd": "cargo test --workspace --release"}),
        ),
        answers("c1", ok("finished in 41m")),
    ];
    let out = build(TurnEvidence::from_transcript(&transcript, true));
    assert!(
        out.contains("cargo test --workspace --release"),
        "`[called: bash]` cannot tell a reflection that the turn spent forty \
         minutes because the command was wrong\n\n{out}"
    );
}

fn step_usage(step: usize, role: ModelCallRole, cost_usd: f64, duration_ms: u64) -> AgentEvent {
    AgentEvent::StepUsage {
        upstream_provider: None,
        step,
        role,
        provider: "anthropic".into(),
        output_text: None,
        model: "claude".into(),
        input_tokens: 1_000,
        output_tokens: 100,
        cached_input_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: None,
        estimated_input_tokens: 0,
        cost_usd,
        duration_ms,
        retries: 0,
        tool_calls: 2,
        complete: true,
        finish_reason: None,
        effort: None,
        max_output_tokens: None,
        temperature: None,
        params: None,
        sub_agent_id: None,
    }
}

#[test]
fn the_friction_section_names_the_costliest_step_by_its_wire_role() {
    let mut friction = TurnFriction::default();
    friction.observe(&step_usage(1, ModelCallRole::Triage, 0.001, 900));
    friction.observe(&step_usage(7, ModelCallRole::WitnessAuthor, 0.31, 61_000));
    let section = friction.section(1);
    assert!(
        section.contains("step 7 (witness_author) $0.3100"),
        "the role is spelled as the wire spells it, so a reflection's account of \
         a turn greps against stella-events.jsonl — the record anyone checking \
         the claim will open\n\n{section}"
    );
    assert!(
        section.contains("1m01s"),
        "wall clock reads as a duration, not as a count of \
         milliseconds\n\n{section}"
    );
}

#[test]
fn the_friction_section_names_every_failed_tool_and_the_loop_detector() {
    let mut friction = TurnFriction::default();
    friction.observe(&AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "c1".into(),
            name: "bash".into(),
            input: serde_json::json!({}),
        },
        sub_agent_id: None,
    });
    friction.observe(&AgentEvent::ToolResult {
        call_id: "c1".into(),
        output: failed("exit 101: could not compile stella-core"),
        duration_ms: 96_000,
        speculated: false,
        sub_agent_id: None,
    });
    friction.observe(&AgentEvent::Retry {
        attempt: 1,
        reason: "overloaded_error".into(),
    });
    friction.observe(&AgentEvent::LoopDetected {
        turn_instance: 0,
        kind: "short_cycle".into(),
        pattern: vec!["read_file".into(), "bash".into()],
        repeats: 3,
        evidence: "same two calls, three times".into(),
        aborted: true,
    });
    let section = friction.section(1);
    for expected in [
        "bash (c1): exit 101: could not compile stella-core",
        "slowest tool: bash (c1) took 1m36s",
        "attempt 1: overloaded_error",
        "short_cycle ×3 on [read_file → bash] — aborted",
    ] {
        assert!(
            section.contains(expected),
            "{expected:?} missing from the friction section\n\n{section}"
        );
    }
}

/// A result whose `ToolStart` was never folded still records — the duration and
/// the failure are the signal, and the name is recoverable from the transcript.
#[test]
fn a_result_without_its_start_is_recorded_under_its_call_id() {
    let mut friction = TurnFriction::default();
    friction.observe(&AgentEvent::ToolResult {
        call_id: "orphan".into(),
        output: failed("no such file"),
        duration_ms: 5,
        speculated: false,
        sub_agent_id: None,
    });
    assert!(
        friction.section(1).contains("orphan: no such file"),
        "dropping it would lose a failure to bookkeeping"
    );
    let transcript = vec![
        user("read it"),
        calls("orphan", "read_file", serde_json::json!({"path": "x"})),
        answers("orphan", failed("no such file")),
    ];
    let out = build(TurnEvidence {
        transcript: &transcript,
        friction: std::slice::from_ref(&friction),
        succeeded: false,
    });
    assert!(
        out.contains("read_file FAILED: no such file"),
        "the transcript supplies the name the ledger lacked\n\n{out}"
    );
}

/// An empty ledger renders no section — a surface with no tap wired must not get
/// a header promising evidence it has none of (#2483).
#[test]
fn an_empty_ledger_renders_no_friction_section() {
    assert!(TurnFriction::default().section(1).is_empty());
    let transcript = vec![user("hi"), assistant("hello")];
    let out = build(TurnEvidence::from_transcript(&transcript, true));
    assert!(
        !out.contains("Where this turn spent itself"),
        "no ledger, no section\n\n{out}"
    );
}

/// The property replay rests on: the digest is a function of the turn.
///
/// `reflect_on_turn` reads no clock and assigns no instant (#2320), and the
/// mining log it feeds is reproducible only while that holds. A digest that
/// varied — iteration order over the ledger's map, a tie between two equally
/// slow tools breaking differently — would break replay with nothing failing.
#[test]
fn the_digest_is_a_function_of_the_turn() {
    let build_once = || {
        let mut friction = TurnFriction::default();
        for step in 0..8 {
            let id = format!("c{step}");
            friction.observe(&AgentEvent::ToolStart {
                call: ToolCall {
                    call_id: id.clone(),
                    name: "bash".into(),
                    input: serde_json::json!({}),
                },
                sub_agent_id: None,
            });
            friction.observe(&AgentEvent::ToolResult {
                call_id: id,
                // Every duration identical, so the tie-break is what orders them.
                output: ok("fine"),
                duration_ms: 1_000,
                speculated: false,
                sub_agent_id: None,
            });
            friction.observe(&step_usage(step, ModelCallRole::Worker, 0.01, 1_000));
        }
        let transcript = long_turn(30, 4, "boom");
        build(TurnEvidence {
            transcript: &transcript,
            friction: std::slice::from_ref(&friction),
            succeeded: false,
        })
    };
    let first = build_once();
    for _ in 0..12 {
        assert_eq!(
            build_once(),
            first,
            "the digest must not vary between builds of the same turn"
        );
    }
}

/// Multibyte content must not panic the clip, and must not be counted in bytes
/// where the contract says characters.
#[test]
fn clipping_respects_character_boundaries() {
    let text = "日本語のテキスト".repeat(200);
    let clipped = clip(&text, 10);
    assert_eq!(clipped.chars().take(10).collect::<String>(), text[..30]);
    assert!(
        clipped.contains("more characters]"),
        "a truncation says how much it cut, so the model can report that the \
         evidence it needed was outside the cut"
    );
}

#[test]
fn durations_read_as_durations() {
    assert_eq!(human_ms(0), "0ms");
    assert_eq!(human_ms(999), "999ms");
    assert_eq!(human_ms(1_000), "1.0s");
    assert_eq!(human_ms(1_250), "1.2s");
    assert_eq!(human_ms(59_999), "59.9s");
    assert_eq!(human_ms(60_000), "1m00s");
    assert_eq!(human_ms(3_678_000), "61m18s");
}

/// The fold is bounded: a turn cannot make the ledger grow without limit, and
/// what it refuses is counted rather than silently dropped.
#[test]
fn the_ledger_is_bounded_and_reports_what_it_refused() {
    let mut friction = TurnFriction::default();
    for step in 0..(FRICTION_ENTRY_CAP + 25) {
        friction.observe(&AgentEvent::Retry {
            attempt: 1,
            reason: format!("reason {step}"),
        });
    }
    assert_eq!(friction.retries.len(), FRICTION_ENTRY_CAP);
    assert_eq!(friction.dropped, 25);
    assert!(
        friction
            .section(1)
            .contains("25 further friction records were not retained"),
        "a silent truncation reads as \"this was everything\"\n\n{}",
        friction.section(1)
    );
}
