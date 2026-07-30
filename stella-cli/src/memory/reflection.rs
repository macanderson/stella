//! Reflection gating, accounted provider dispatch, and response parsing.

use std::path::Path;

use stella_model::provider::Provider;
use stella_protocol::{AgentEvent, CompletionMessage, CompletionRequest, MessageRole};
use stella_store::reflection::SelfReviewRow;

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
) -> Result<
    (ReflectionParse, Option<SelfReviewRow>, f64, Vec<AgentEvent>),
    crate::accounted_call::StandaloneCallError,
> {
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
    // The previous framing asked, on failure, "what should change next time to
    // avoid repeating this failure?" — a question about the agent. It reliably
    // got an answer about the agent. Measured over ten mined lessons from a
    // real run: eight were process self-critique, and *zero* recorded the
    // repository conventions that decided whether the work actually passed.
    // The lesson corpus was full of "the agent should be more proactive" and
    // empty of "money is integer minor units in this repo".
    //
    // Nothing downstream could fix that. Retrieval cannot surface a fact that
    // was never written down, so the whole lifecycle was faithfully
    // accumulating, ranking and recalling commentary about itself.
    let task_frame = if succeeded {
        "This turn SUCCEEDED.\n\
         FIRST, and most importantly: what did you learn about THIS CODEBASE \
         that will still be true next week, on a completely different task? \
         Conventions it enforces, invariants it relies on, where things live, \
         steps that are required for a change to take effect, types or helpers \
         you are expected to use. Succeeding by following a convention is the \
         clearest evidence that the convention exists — record it as a flat \
         statement of fact about the code, not as a description of what you \
         did.\n\
         SECOND, only if something is genuinely worth it: a note about how you \
         worked. Most successful turns have nothing of this kind worth keeping."
    } else {
        "This turn FAILED.\n\
         FIRST, and most importantly: what did you learn about THIS CODEBASE \
         that will still be true next week, on a completely different task? A \
         failure usually means the code expected something you did not know — \
         a required registration step, a type it insists on, a helper you were \
         supposed to call, a place a thing has to be declared. Record that \
         expectation as a flat statement of fact about the code.\n\
         SECOND: the root cause in terms of your own approach — a wrong \
         assumption, a missed file, a bad plan. If the failure was clearly \
         external (network, timeout), return an empty list."
    };
    // `self_review` rides along in this same call rather than costing a second
    // one — the model has the transcript in front of it either way.
    //
    // It is deliberately asked for LAST, and named as explicitly not a
    // substitute for a lesson. The ordering comment above is the reason: this
    // prompt already lost one fight against self-commentary, where asking about
    // the agent got eight process notes and zero codebase facts. A self-review
    // field is exactly the kind of invitation that can re-open that, so the
    // lesson instruction keeps the front of the prompt and its "prefer domain"
    // rule intact, and the self-review is fenced off as being about THIS turn
    // only — the one place a note about the agent genuinely belongs, because it
    // is stored against this execution and never recalled as a lesson.
    let prompt = format!(
        "Review this coding-agent turn transcript and reflect on the agent's \
         performance. {task_frame}\n\n\
         Respond with ONLY a JSON object:\n\
         {{\"lessons\": [{{\"lesson\": \"...\", \"kind\": \"domain\", \
         \"domains\": [\"...\"]}}], \"self_review\": {{\"delivered\": true, \
         \"rating\": 7, \"went_well\": \"...\", \"to_improve\": \"...\", \
         \"critique\": \"...\"}}}}\n\
         `lessons` holds at most 3, most useful first. \
         `kind` is \"domain\" for a fact about the codebase that holds \
         independent of this turn, or \"process\" for a note about how you \
         worked. Prefer domain. A good domain lesson reads like \
         \"amounts are stored as integer minor units; use money.parse_amount\" \
         — a fact someone could act on without knowing anything about this \
         turn. A lesson that begins \"the agent should\" is a process lesson, \
         and if you cannot state a domain fact, say nothing rather than \
         padding with one.\n\
         `self_review` is your account of THIS turn alone and is never a \
         substitute for a lesson — omit it entirely rather than let it crowd \
         out a codebase fact. `delivered` is whether you actually did what was \
         asked, `rating` is 0-10 for this turn's work, `to_improve` is the one \
         thing you would do differently. One sentence per field. This is shown \
         to the user as your own assessment, so do not flatter yourself: a turn \
         that produced no output or left the work unfinished did not deliver.\n\
         Allowed domain tags (use only these, or []): {}\n\nTranscript:\n{digest}",
        domain_names.join(", ")
    );
    let request = CompletionRequest {
        messages: vec![
            CompletionMessage::system(
                "You are a self-reflection module. Respond with only a JSON object.",
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
        parse_self_review(&accounted.result.text),
        accounted.cost_usd,
        accounted.events,
    ))
}

/// The model's `self_review` object, as it comes off the wire.
///
/// Every field is `#[serde(default)]` because a partial review is still worth
/// keeping — a model that offers only `to_improve` has said the one thing the
/// "what to improve" panel exists to show, and rejecting the whole object over
/// a missing `rating` would discard it.
#[derive(serde::Deserialize)]
struct SelfReviewJson {
    #[serde(default)]
    delivered: Option<bool>,
    #[serde(default)]
    rating: Option<i64>,
    #[serde(default)]
    went_well: String,
    #[serde(default)]
    to_improve: String,
    #[serde(default)]
    critique: String,
}

#[derive(serde::Deserialize)]
struct SelfReviewEnvelope {
    self_review: SelfReviewJson,
}

/// Read the self-review out of a reflection response, if it offered one.
///
/// Scans for a balanced `{...}` span that carries a `self_review` key, for the
/// same reason [`extract_lesson_array`] scans rather than slicing: models
/// narrate, fence their JSON, and add trailing notes. `None` covers every
/// benign case — a model that answered with a bare lesson array (the older
/// format, still parsed for lessons), or one that declined to grade itself.
pub(crate) fn parse_self_review(text: &str) -> Option<SelfReviewRow> {
    let bytes = text.as_bytes();
    for (start, _) in text.char_indices().filter(|(_, c)| *c == '{') {
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
                            serde_json::from_str::<SelfReviewEnvelope>(&text[start..=end])
                        {
                            return Some(parsed.self_review.into_row());
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

impl SelfReviewJson {
    fn into_row(self) -> SelfReviewRow {
        SelfReviewRow {
            delivered: self.delivered,
            // Out of range is dropped, not clamped. A model answering on a
            // different scale (95, or 4.5 rounded to 4 out of 5) is not saying
            // "10/10", and clamping would put a fabricated perfect score under
            // a label that promises the model's own number. `None` reads as
            // "declined to grade", which is true.
            self_rating: self.rating.filter(|r| (0..=10).contains(r)),
            what_went_well: self.went_well,
            what_to_improve: self.to_improve,
            critique: self.critique,
        }
    }
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
