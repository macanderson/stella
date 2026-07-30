//! Reflection gating, accounted provider dispatch, and response parsing.

use std::path::Path;

use stella_model::provider::Provider;
use stella_protocol::{AgentEvent, CompletionMessage, CompletionRequest, MessageRole};

use super::ReflectionLesson;

#[derive(Debug, Clone, Default)]
#[must_use = "reflection cost and usage events must be surfaced by every caller"]
pub struct ReflectionReport {
    pub recorded: usize,
    pub model_error: Option<String>,
    pub cost_usd: f64,
    pub events: Vec<AgentEvent>,
}

pub fn turn_warrants_reflection(turn_messages: &[CompletionMessage]) -> bool {
    turn_messages
        .iter()
        .any(|message| !message.tool_calls.is_empty())
}

/// Whether a finished interactive turn should feed reflection at all.
/// Failures ARE reflected on — a failed turn is a high-value learning
/// signal, and the one-shot pipeline path has always treated it as one —
/// EXCEPT a user-chosen soft stop, which is not a failure: reflecting on
/// it would teach the memory that deliberate interruptions are errors.
/// (`contains`, not `==`: goal paths wrap the turn's abort reason in their
/// own prefix.) Callers still pass `result.is_ok()` as
/// `reflect_and_record`'s `succeeded` flag so a failure is recorded AS a
/// failure.
pub fn should_reflect_on(result: &Result<(), String>) -> bool {
    match result {
        Ok(()) => true,
        Err(reason) => !reason.contains(stella_core::SOFT_STOP_REASON),
    }
}

pub async fn reflect_on_turn(
    provider: &dyn Provider,
    model_hint: &str,
    workspace_root: &Path,
    transcript: &[CompletionMessage],
    domain_names: &[String],
    succeeded: bool,
    budget_limit: Option<f64>,
) -> Result<(ReflectionParse, f64, Vec<AgentEvent>), crate::accounted_call::StandaloneCallError> {
    let digest = transcript
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
    // Ask first for facts about the CODEBASE, and only then for notes about
    // the agent. The order is the point.
    //
    // This prompt has now been wrong twice, in opposite directions, and both
    // times the lifecycle downstream was blameless — retrieval cannot surface
    // a fact that was never written down, and cannot decline one that was.
    //
    // FIRST ERROR (#768): it asked "what should change next time to avoid
    // repeating this failure?" — a question about the agent, which reliably got
    // an answer about the agent. Eight of ten mined lessons were process
    // self-critique; zero recorded a repository convention. Fixed by asking for
    // facts about the codebase instead.
    //
    // SECOND ERROR (this change): the fix over-corrected into recording facts
    // that are free to look up. Measured on a live store: 23 memories encoding
    // six facts, every one of them a single file-read away — "commands are
    // registered in registry.py" held seven times. The proving ground then
    // measured what those memories are worth, and the answer was *negative*:
    // hand-delivering exactly those conventions, perfectly worded, did not
    // improve the pass rate and cost steps, because the agent could already
    // read them faster than it could be told.
    //
    // The old prompt caused this directly. It asked for "where things live",
    // and offered "amounts are stored as integer minor units; use
    // money.parse_amount" as its model of a good lesson — which is exactly the
    // class of fact that is cheaper to grep than to carry. It was teaching the
    // wrong thing by example.
    //
    // The governing principle, now stated in the prompt as a test the model
    // applies before writing anything down: a memory is worth its slot in a
    // future prompt only in proportion to what it costs to rediscover. Surprise
    // is the operational signal — if inspection would have told you, inspection
    // will tell you again next time, for free.
    let task_frame = if succeeded {
        "This turn SUCCEEDED.\n\
         What SURPRISED you? Record only what you could NOT have predicted by \
         reading the code — something that contradicted a reasonable \
         assumption, cost you a wrong attempt, or that you only know because \
         you ran it and watched what happened. If nothing surprised you, \
         return an empty list. Most successful turns teach nothing worth \
         keeping, and saying so is the correct answer."
    } else {
        "This turn FAILED.\n\
         What did the code expect that reading it did not tell you? A failure \
         is the cheapest evidence there is that something was not discoverable \
         by inspection — a helper that looks usable and is not, an ordering \
         that matters and is not stated, a check that fires from somewhere \
         unobvious. Record that, as a flat statement of fact.\n\
         If the failure was your own carelessness on something the code stated \
         plainly, there is no lesson: return an empty list."
    };
    let prompt = format!(
        "Review this coding-agent turn transcript and reflect on the agent's \
         performance. {task_frame}\n\n\
         Respond with ONLY a JSON array (max 3), most useful first:\n\
         [{{\"lesson\": \"...\", \"kind\": \"domain\", \"domains\": [\"...\"]}}]\n\
         `kind` is \"domain\" for a fact about the codebase that holds \
         independent of this turn, or \"process\" for a note about how you \
         worked. Prefer domain.\n\
         THE TEST, applied to every candidate before you write it: could a \
         competent engineer find this in under a minute by reading the code or \
         grepping? If YES, DISCARD IT. It is cheaper to look up than to carry, \
         and every remembered fact costs room in a future prompt.\n\
         So do NOT record: where files live, what a module is called, a \
         function's signature, the directory layout, which helper exists, or \
         anything a README or a type definition already states. These are the \
         most tempting lessons and the most worthless.\n\
         DO record what inspection cannot reveal: a helper that looks correct \
         and is subtly wrong, an ordering that matters but is not written down, \
         a step that silently does nothing if skipped, a check that fires from \
         somewhere unrelated, a stated rule that the code does not actually \
         follow, or an explicit preference the user expressed.\n\
         Good: \"util/amounts.to_cents parses through float and loses a cent \
         on values like 1.15; money.parse_amount is the correct one despite \
         both looking current\" — you can only know that by getting it wrong.\n\
         Bad: \"commands are registered in registry.py\" — one grep away, \
         worthless to carry.\n\
         A lesson that begins \"the agent should\" is a process lesson, and if \
         you cannot state something that survives the test, return an empty \
         array rather than padding it.\n\
         Allowed domain tags (use only these, or []): {}\n\nTranscript:\n{digest}",
        domain_names.join(", ")
    );
    let request = CompletionRequest {
        messages: vec![
            CompletionMessage::system(
                "You are a self-reflection module. Respond with only a JSON array.",
            ),
            CompletionMessage::user(prompt),
        ],
        // 512 was enough for a model that answers with bare JSON and nothing
        // else. A model that narrates first spends the whole allowance on
        // prose and is cut off before it reaches the array, so every lesson
        // from every turn is lost — silently, because a truncated response
        // parses to zero lessons exactly like an empty one. The array itself
        // is at most three short objects; the extra headroom is only ever
        // spent by models that were going to be cut off.
        max_output_tokens: Some(2048),
        temperature: Some(0.0),
        effort: None,
        tools: Vec::new(),
        reasoning: None,
        params: None,
    };
    let accounted = crate::accounted_call::complete_standalone(
        workspace_root,
        provider,
        stella_protocol::ModelCallRole::Reflection,
        "reflection",
        model_hint,
        budget_limit,
        request,
    )
    .await?;
    Ok((
        parse_lessons_checked(&accounted.result.text, domain_names),
        accounted.cost_usd,
        accounted.events,
    ))
}

/// The outcome of reading a reflection response.
///
/// `Vec::new()` used to mean two very different things — "the model considered
/// the turn and found nothing worth keeping", which is the common and correct
/// answer, and "the model said something we could not read", which starves the
/// entire context lifecycle. They are separated here so the second can be
/// reported instead of silently looking like the first.
pub enum ReflectionParse {
    Lessons(Vec<ReflectionLesson>),
    /// The model produced text, but no JSON array of lessons could be read out
    /// of it. Carries a short excerpt for the operator.
    Unreadable(String),
}

/// Find the first balanced `[...]` span that parses as a lesson array.
///
/// The previous rule — first `[` to last `]` — assumed the response was bare
/// JSON. Models that narrate before answering break it in both directions: a
/// bracket inside prose moves `start`, and a bracket in a trailing note moves
/// `end`, so the slice between them is not JSON at all and the whole turn's
/// lessons are dropped. Scanning for a balanced span and taking the first one
/// that actually deserializes tolerates prose, markdown fences, and trailing
/// commentary without loosening what counts as a lesson.
fn extract_lesson_array(text: &str) -> Option<Vec<ReflectionLesson>> {
    let bytes = text.as_bytes();
    for (start, _) in text.char_indices().filter(|(_, c)| *c == '[') {
        let mut depth = 0i32;
        let mut in_string = false;
        let mut escaped = false;
        for (offset, byte) in bytes[start..].iter().enumerate() {
            if in_string {
                match byte {
                    _ if escaped => escaped = false,
                    b'\\' => escaped = true,
                    b'"' => in_string = false,
                    _ => {}
                }
                continue;
            }
            match byte {
                b'"' => in_string = true,
                b'[' | b'{' => depth += 1,
                b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        let end = start + offset;
                        if let Ok(parsed) =
                            serde_json::from_str::<Vec<ReflectionLesson>>(&text[start..=end])
                        {
                            return Some(parsed);
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    None
}

pub fn parse_lessons_checked(text: &str, allowed_domains: &[String]) -> ReflectionParse {
    let Some(mut lessons) = extract_lesson_array(text) else {
        // An empty response is a legitimate "nothing to record". Anything else
        // is a response we failed to read, and the caller should say so.
        return if text.trim().is_empty() {
            ReflectionParse::Lessons(Vec::new())
        } else {
            ReflectionParse::Unreadable(text.chars().take(180).collect())
        };
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    lessons.truncate(3);
    for lesson in &mut lessons {
        lesson.occurred_at = now;
        lesson.domains.retain(|domain| {
            allowed_domains
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(domain))
        });
    }
    lessons.retain(|lesson| !lesson.lesson.trim().is_empty());
    ReflectionParse::Lessons(lessons)
}
