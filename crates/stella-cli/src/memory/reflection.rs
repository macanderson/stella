//! Reflection gating, accounted provider dispatch, and response parsing.

/// How a turn is rendered for the reflecting model: selection under a character
/// budget, never a tail window (#2460).
pub(crate) mod digest;

/// What the prompt must and must not say. A child module so it can drive
/// [`reflect_on_turn`] directly rather than through the whole record path.
#[cfg(test)]
mod tests;

use std::path::Path;

use stella_model::provider::Provider;
use stella_protocol::{AgentEvent, CompletionMessage, CompletionRequest, ReasoningEffort};
use stella_store::reflection::SelfReviewRow;

pub use digest::{TurnEvidence, TurnFriction};

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
pub fn should_reflect_on<E: std::fmt::Display>(result: &Result<(), E>) -> bool {
    match result {
        Ok(()) => true,
        Err(reason) => !reason.to_string().contains(stella_core::SOFT_STOP_REASON),
    }
}

/// The thinking posture one reflection call sends on the wire.
///
/// Reflection dispatches on the model the **triage** pin selected (#1847), so
/// the triage agent's configured posture is the one that governs it. Left
/// unthreaded, `agents.triage.reasoning` chose the model for a call it could
/// not otherwise reach — and an operator who switched thinking off still paid
/// for a reasoning stream billed against the output cap (#2174).
///
/// The default is the worker-ridden case: no route, no triage posture, and the
/// pinned-low effort below stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ReflectionPosture {
    /// `agents.triage.reasoning` — thinking on/off. `None` leaves the
    /// provider's default, exactly as before this was threaded.
    pub(crate) reasoning: Option<bool>,
    /// `agents.triage.effort`. `None` keeps reflection's own low pin, which is
    /// the right default for a bounded JSON contract; an explicit setting wins
    /// because it is the more specific statement about this call.
    pub(crate) effort: Option<ReasoningEffort>,
}

/// Post-turn reflection on the cheap tier (#1847): resolve the reflection
/// route, then run [`super::SessionMemory::reflect_and_record`] on whichever
/// provider it lands on — the configured triage model when routable
/// (`crate::agent::reflection_route`), else the worker this turn already ran
/// on, exactly as every reflection call dispatched before the route existed.
///
/// This is the one seam all four reflecting surfaces (one-shot `run`, the
/// REPL, `/goal`, the Command Deck) dispatch through, so the routing
/// decision cannot be remembered by three drivers and forgotten by the
/// fourth. Provider discovery runs per call rather than per session because
/// reflection is already a post-turn, best-effort model call that opens the
/// store — one credential scan is noise beside it, and it keeps this seam a
/// drop-in for the call sites' previous direct dispatch.
pub(crate) async fn reflect_routed(
    memory: &mut super::SessionMemory,
    cfg: &crate::config::Config,
    worker: &dyn Provider,
    evidence: TurnEvidence<'_>,
    quiet: bool,
    budget_limit: Option<f64>,
) -> ReflectionReport {
    let routed =
        crate::agent::reflection_route(cfg, &crate::config::discover_configured_providers());
    // The posture travels with the route, not with the adapter: a triage pin
    // that resolves to the session's own model builds no second adapter and
    // still governs the call (#2174).
    let posture = routed.as_ref().map(|r| r.posture).unwrap_or_default();
    let (provider, model_hint) = match routed.as_ref().and_then(|r| r.provider.as_ref()) {
        Some((model, provider)) => (provider.as_ref(), model.model_id.as_str()),
        None => (worker, cfg.model_id.as_str()),
    };
    memory
        .reflect_and_record(provider, model_hint, evidence, quiet, budget_limit, posture)
        .await
}

/// Stamps no timestamp of its own (#2320). `occurred_at` is assigned by
/// `SessionMemory::reflect_and_record`, in the same pass that stamps
/// `task_id`, from the session clock — so neither this dispatch nor the parser
/// below ever reads a clock, and the mining log they feed is reproducible.
// The remaining parameters each carry a fact about THIS dispatch that the
// caller alone knows — which provider, which model slug, which workspace, which
// domain vocabulary, what budget is left, what posture the route declared — so
// they stay positional. What the turn *is* does not: `transcript`, `succeeded`
// and the event-derived friction are one question with one answer, and bundling
// them as [`TurnEvidence`] is what keeps this at seven arguments while adding
// evidence rather than at nine with an `#[allow]` (#2460).
pub async fn reflect_on_turn(
    provider: &dyn Provider,
    model_hint: &str,
    workspace_root: &Path,
    evidence: TurnEvidence<'_>,
    domain_names: &[String],
    budget_limit: Option<f64>,
    posture: ReflectionPosture,
) -> Result<
    (ReflectionParse, Option<SelfReviewRow>, f64, Vec<AgentEvent>),
    crate::accounted_call::StandaloneCallError,
> {
    // Selection, not the last twelve messages truncated to 300 characters: the
    // expensive part of a turn is in the middle, and a `Tool` message's payload
    // is not in `content` at all, so the old digest showed reflection the string
    // `"tool: "` for every tool result it had ever produced. See
    // [`digest`]'s module docs for both defects and the budget that replaces
    // them.
    let digest = digest::build(evidence);
    let succeeded = evidence.succeeded;
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
    //
    // That fix landed in two halves, and the gap between them is worth naming.
    // #944 rewrote `task_frame` to ask about surprise but left the body below
    // still offering `money.parse_amount` as its model of a good lesson — so
    // this comment described a test the prompt did not actually apply, and the
    // prompt contradicted itself: the frame said "only what inspection cannot
    // tell you", and the next paragraph held up a one-grep fact as the ideal.
    // A model resolving that resolves it toward the concrete example.
    //
    // So THE EXAMPLES ARE LOAD-BEARING, not decoration. The failure mode here
    // has twice been an instruction that was correct in the abstract and
    // undercut by what it showed. Do not re-add a "a good lesson reads like
    // ..." convenience example unless the fact it names would genuinely cost
    // something to rediscover.
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
         list rather than padding it.\n\
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
        // Both bounds are unstated here and declared once at the standalone
        // chokepoint (`accounted_call::standalone_bounds`), which sends this
        // role's written contract PLUS the thinking room a reasoning model
        // spends before writing any of it. Sending the contract alone is what
        // froze this workspace's learning plane for nine days: execution 63
        // came back at exactly 2,048 output tokens with `finish_reason: length`
        // and no visible text, which `extract_lesson_array` reads as zero
        // lessons — indistinguishable, from every surface, from a turn that
        // genuinely taught nothing (#2174). Reflection was then the only one of
        // the four standalone roles that had been given headroom, which is
        // exactly why the number moved to where a new role has to decide it
        // (#2444).
        max_output_tokens: None,
        temperature: Some(0.0),
        // The chokepoint pins this role's effort `Low`; what this line carries
        // is the operator's own triage posture, which outranks that pin where
        // one is set. Reflection dispatches on the model the triage pin
        // selected (#1847), and a knob that chooses the model for a call but
        // cannot reach the call is a knob that lies — `agents.triage.reasoning:
        // off` reaches the wire through here.
        effort: posture.effort,
        tools: Vec::new(),
        reasoning: posture.reasoning,
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

/// Purely a parser: it reads no clock and assigns no instant. `occurred_at`
/// is the session's to stamp — see [`reflect_on_turn`] (#2320).
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
    lessons.truncate(3);
    for lesson in &mut lessons {
        lesson.domains.retain(|domain| {
            allowed_domains
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(domain))
        });
    }
    lessons.retain(|lesson| !lesson.lesson.trim().is_empty());
    ReflectionParse::Lessons(lessons)
}
