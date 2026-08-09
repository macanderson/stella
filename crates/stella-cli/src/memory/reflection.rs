//! Reflection gating, accounted provider dispatch, and response parsing.

/// What the prompt must and must not say. A child module so it can drive
/// [`reflect_on_turn`] directly rather than through the whole record path.
#[cfg(test)]
mod tests;

use std::path::Path;

use stella_model::provider::Provider;
use stella_protocol::{
    AgentEvent, CompletionMessage, CompletionRequest, MessageRole, ReasoningEffort,
};
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
pub fn should_reflect_on<E: std::fmt::Display>(result: &Result<(), E>) -> bool {
    match result {
        Ok(()) => true,
        Err(reason) => !reason.to_string().contains(stella_core::SOFT_STOP_REASON),
    }
}

/// How much of the transcript the reflection digest reads ([`reflect_on_turn`]
/// takes exactly this many messages off the tail).
///
/// This window is the ceiling on how good any reflection prompt can be, and it
/// is currently far too small: twelve messages, each truncated to 300
/// characters, is a few thousand characters of a turn that may have spent
/// hundreds of thousands of tokens. The part of a turn that cost the most is
/// in its middle, and the tail is what survives. Widening it well means
/// *selecting* from the event stream rather than raising these numbers, which
/// is #2460 — filed rather than folded in, because it is a per-turn cost
/// change that deserves its own measurement.
const REFLECTED_TAIL_MESSAGES: usize = 12;

/// How many lessons one turn may contribute.
///
/// Deliberately above the number of genuinely distinct things a turn tends to
/// teach, so that the cap is not what decides — the test stated in the prompt
/// is. The previous limit was 3, which is below that number and therefore
/// *was* the decision on any turn that had more to say: a fourth finding was
/// discarded by arithmetic before anything weighed it.
///
/// It is not unbounded, and the reason is the objective the prompt now names.
/// Reflection memories ride the recall channel, where `max_frames` defaults to
/// 5 — so every stored lesson is a competitor for five slots, and a turn
/// permitted to file twenty does not add recall, it dilutes it. A cap that
/// never binds on an honest answer and does bind on a flood is the shape that
/// serves both halves.
pub(crate) const MAX_LESSONS_PER_TURN: usize = 8;

/// What reflection is asked to **write**: at most [`MAX_LESSONS_PER_TURN`]
/// lesson objects, each three prose fields, plus a five-field self-review.
/// Thinking room is added on top by
/// [`stella_core::starvation::with_reasoning_headroom`] — the number here stays
/// readable against the prompt that justifies it instead of silently encoding
/// someone's guess at a reasoning budget.
///
/// 512 was the original, enough for a model that answers with bare JSON and
/// nothing else. A model that narrates first spends the whole allowance on
/// prose and never reaches the array, so every lesson from every turn is lost
/// — silently, because a truncated response parses to zero lessons exactly
/// like an empty one.
///
/// 2,048 was that repair, and it is no longer the number: it was sized for a
/// one-field lesson capped at three per turn. A lesson now carries `insight`,
/// `trigger` and `saves`, and up to [`MAX_LESSONS_PER_TURN`] of them may come
/// back — call it eight times the ~90 tokens a grounded lesson takes, plus the
/// self-review. Headroom here is only ever spent by a response that was going
/// to be truncated, and a truncation here is invisible.
const LESSONS_OUTPUT_CONTRACT: u32 = 4_096;

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

/// The slice of `messages` worth copying for a reflection transcript.
///
/// A caller that appends turn context (the final answer, a files-changed
/// note) before reflecting should clone this tail, never the whole history:
/// [`reflect_on_turn`] digests only the last [`REFLECTED_TAIL_MESSAGES`]
/// messages, so on a long session everything earlier was memcpy'd to be
/// dropped (#1847's call-site nit).
pub(crate) fn reflection_tail(messages: &[CompletionMessage]) -> Vec<CompletionMessage> {
    messages[messages.len().saturating_sub(REFLECTED_TAIL_MESSAGES)..].to_vec()
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
    transcript: &[CompletionMessage],
    quiet: bool,
    succeeded: bool,
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
        .reflect_and_record(
            provider,
            model_hint,
            transcript,
            quiet,
            succeeded,
            budget_limit,
            posture,
        )
        .await
}

/// Stamps no timestamp of its own (#2320). `occurred_at` is assigned by
/// `SessionMemory::reflect_and_record`, in the same pass that stamps
/// `task_id`, from the session clock — so neither this dispatch nor the parser
/// below ever reads a clock, and the mining log they feed is reproducible.
// Eight parameters, and the lint is wrong here: every one is an independent
// fact about THIS dispatch that the caller alone knows (which provider, which
// model slug, which workspace, which transcript slice, which domain
// vocabulary, whether the turn succeeded, what budget is left, what posture
// the route declared). Bundling them into a struct would rename the arguments
// without reducing what a caller must decide, and this is a leaf dispatch with
// three call sites — the shape the lint exists to catch is a growing function
// that has become several, which this is not.
#[allow(clippy::too_many_arguments)]
pub async fn reflect_on_turn(
    provider: &dyn Provider,
    model_hint: &str,
    workspace_root: &Path,
    transcript: &[CompletionMessage],
    domain_names: &[String],
    succeeded: bool,
    budget_limit: Option<f64>,
    posture: ReflectionPosture,
) -> Result<
    (ReflectionParse, Option<SelfReviewRow>, f64, Vec<AgentEvent>),
    crate::accounted_call::StandaloneCallError,
> {
    let digest = transcript
        .iter()
        .rev()
        .take(REFLECTED_TAIL_MESSAGES)
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
    // The question is the counterfactual — what would you want to have been
    // told, to do this again faster, cheaper and more accurately — and
    // everything else in this prompt exists to keep the answer honest.
    //
    // It has been wrong three times, and every time for the same reason: it
    // asked for a PROXY instead of for the thing itself. The lifecycle
    // downstream was blameless each time — retrieval cannot surface a fact that
    // was never written down, and cannot decline one that was.
    //
    // FIRST (#768): "what should change next time to avoid repeating this
    // failure?" — a question about the agent, which reliably got an answer
    // about the agent. Eight of ten mined lessons were process self-critique;
    // zero recorded a repository convention.
    //
    // SECOND (#944): the repair over-corrected into "where things live", and
    // recorded facts that are free to look up. Measured on a live store: 23
    // memories encoding six facts, every one a single file-read away —
    // "commands are registered in registry.py" held seven times. The proving
    // ground then measured what those memories were worth, and the answer was
    // *negative*: hand-delivering exactly those conventions, perfectly worded,
    // did not improve the pass rate and cost steps, because the agent could
    // read them faster than it could be told them.
    //
    // THIRD (this change): the repair for the second was a rediscovery-cost
    // test — "could a competent engineer find this in under a minute? If YES,
    // DISCARD IT" — operationalized as surprise. That is right about one class
    // of fact and blind to another, because surprise measures NOVELTY and what
    // a memory is worth is SAVINGS. The two come apart exactly where the money
    // is: on a fact that is trivial to look up and expensive not to know.
    // "The whole suite takes twenty minutes; the scoped run takes forty
    // seconds" is one grep into a Makefile, and the rediscovery test orders it
    // discarded — after it has already cost three turns of waiting, and will go
    // on costing them in every future session, because nobody greps for what
    // they do not know to ask.
    //
    // So the test is now the counterfactual itself, settled against the only
    // thing that can settle it: whether knowing the fact would have changed an
    // ACTION. That subsumes the rediscovery rule without inheriting its blind
    // spot — a fact you would have looked up anyway changes no action, and is
    // still discarded — while admitting the class the rediscovery rule threw
    // away.
    //
    // What this prompt deliberately does NOT do is tell the model what a lesson
    // may be ABOUT. There is no topic list, and the "do NOT record / DO record"
    // enumerations are gone. Every one of the three failures above was a topic
    // guess made by whoever wrote the prompt, and the model that just ran the
    // turn is better placed than the author to know what the turn cost it. What
    // is constrained instead is the SHAPE OF THE ARGUMENT — a trigger, and a
    // named moment in the transcript — because those are what make a lesson
    // retrievable later and checkable at all. Free the topic; fix the
    // epistemics. Re-adding a topic list is how this prompt fails a fourth
    // time.
    //
    // THE EXAMPLES ARE LOAD-BEARING, not decoration. Twice the instruction was
    // correct in the abstract and undercut by what it showed, and a model
    // resolving that contradiction resolves it toward the concrete example. Do
    // not add an example whose lesson would not itself survive the test stated
    // in the prompt.
    let task_frame = if succeeded {
        "This turn SUCCEEDED.\n\
         You are about to be handed a task like this one again, in this same \
         repository, with no memory of anything that happened here. What do you \
         want to have been told before you start, so that the next attempt is \
         faster, costs less, and gets more of it right than this one did?"
    } else {
        "This turn FAILED.\n\
         You are about to be handed a task like this one again, in this same \
         repository, with no memory of anything that happened here. A failure \
         is the cheapest evidence there is about what you did not know going \
         in. What do you want to have been told before you start, so that the \
         next attempt gets where this one did not?"
    };
    // `self_review` rides along in this same call rather than costing a second
    // one — the model has the transcript in front of it either way.
    //
    // It is deliberately asked for LAST, and named as explicitly not a
    // substitute for a lesson. The comment above is the reason: this prompt
    // already lost one fight against self-commentary, where asking about the
    // agent got eight process notes and zero codebase facts. A self-review
    // field is exactly the kind of invitation that can re-open that, so the
    // lesson instruction keeps the front of the prompt and the self-review is
    // fenced off as being about THIS turn only — the one place a note about
    // the agent genuinely belongs, because it is stored against this execution
    // and never recalled as a lesson.
    //
    // The fence matters more now, not less. The prompt no longer tells the
    // model what a lesson may be about, so the pressure that produced those
    // eight process notes has nothing topical holding it back — what holds it
    // back instead is `saves`, which a self-critique cannot fill in without
    // naming a moment, and `kind`, which sends anything that does not travel
    // to a deferred recall tier rather than into competition with facts that
    // do.
    let prompt = format!(
        "Review this coding-agent turn transcript. {task_frame}\n\n\
         Respond with ONLY a JSON object:\n\
         {{\"lessons\": [{{\"lesson\": \"...\", \"trigger\": \"...\", \
         \"saves\": \"...\", \"kind\": \"domain\", \"domains\": [\"...\"]}}], \
         \"self_review\": {{\"delivered\": true, \"rating\": 7, \
         \"went_well\": \"...\", \"to_improve\": \"...\", \
         \"critique\": \"...\"}}}}\n\
         There is no approved list of topics and no house style. A command, a \
         constraint, an assumption that turned out to be wrong, an ordering, a \
         dead end not worth walking twice, a number, a name, something the user \
         told you they wanted — if you want it, write it down. You are the only \
         one who watched this turn, and nobody has decided in advance what \
         counts.\n\
         ONE TEST, applied to every candidate before you write it: WOULD \
         KNOWING THIS HAVE CHANGED WHAT YOU ACTUALLY DID? Not whether it is \
         true, interesting, or hard to find — whether it would have changed an \
         action. A fact you would have looked up in three seconds anyway \
         changes nothing and is worth nothing, however true it is. A fact you \
         COULD have looked up in three seconds and did not, and paid twenty \
         minutes for, is worth everything: cheap to find and cheap to be \
         without are not the same thing, and it is the second one that \
         matters.\n\
         `saves` is what knowing it would have bought you HERE, pointing at the \
         moment in this transcript it would have changed — the wrong attempt it \
         prevents, the wait it skips, the wrong answer it stops you shipping. \
         If you cannot name the moment, do not record the lesson: you are \
         guessing at what helps, and guessing is the one thing this is not \
         for.\n\
         `trigger` is what has to be true of a future task for this to matter, \
         written so that a future you can tell at a glance whether it applies. \
         A lesson whose trigger you cannot state is one you have not finished \
         learning.\n\
         `kind` is \"domain\" if it will still be true on a DIFFERENT task in \
         this repository, \"process\" if it only describes how this particular \
         turn went. That is a question about how far it travels, not about what \
         it is about.\n\
         Good: \"util/amounts.to_cents parses through float and loses a cent \
         on values like 1.15; money.parse_amount is the correct one despite \
         both looking current\" — a wrong answer was shipped and then found; \
         nothing short of getting it wrong would have taught it.\n\
         Good: \"the full suite takes ~20 minutes and the scoped run covering \
         these files takes ~40 seconds\" — one grep away, and it still cost \
         three turns of waiting, because nobody greps for what they do not know \
         to ask.\n\
         Bad: \"commands are registered in registry.py\" — you would have \
         grepped that in three seconds. Knowing it in advance changes not one \
         action you took.\n\
         Write as many as pass the test and no more. Padding the list with \
         candidates that failed it is how the list stops being read; an empty \
         list is a complete answer when nothing passes, and at most {max} are \
         kept.\n\
         `self_review` is your account of THIS turn alone and is never a \
         substitute for a lesson — omit it entirely rather than let it crowd \
         out one. `delivered` is whether you actually did what was asked, \
         `rating` is 0-10 for this turn's work, `to_improve` is the one thing \
         you would do differently. One sentence per field. This is shown to the \
         user as your own assessment, so do not flatter yourself: a turn that \
         produced no output or left the work unfinished did not deliver.\n\
         Allowed domain tags (use only these, or []): {}\n\nTranscript:\n{digest}",
        domain_names.join(", "),
        max = MAX_LESSONS_PER_TURN,
    );
    let request = CompletionRequest {
        messages: vec![
            CompletionMessage::system(
                "You are a self-reflection module. Respond with only a JSON object.",
            ),
            CompletionMessage::user(prompt),
        ],
        // Two independent things have to fit under one number, and each was
        // sent alone once. [`LESSONS_OUTPUT_CONTRACT`] is what reflection is
        // asked to WRITE — now three prose fields per lesson, up to
        // [`MAX_LESSONS_PER_TURN`] of them. The headroom on top is what a
        // reasoning model spends before it writes anything, and sending the
        // contract alone is what froze this workspace's learning plane for
        // nine days: execution 63 came back at exactly 2,048 output tokens
        // with `finish_reason: length` and no visible text, which
        // `extract_lesson_array` reads as zero lessons — indistinguishable,
        // from every surface, from a turn that genuinely taught nothing
        // (#2174). Undersizing either half has the same invisible symptom, so
        // both are sized here rather than one being left to chance.
        max_output_tokens: Some(stella_core::starvation::with_reasoning_headroom(
            LESSONS_OUTPUT_CONTRACT,
        )),
        temperature: Some(0.0),
        // Pinned low, like every bounded management call (the pipeline's
        // `management_bounds` pins triage the same way, and the overflow
        // summarizer pins its own pass low): the written output contract is
        // a three-lesson JSON array, and an unset effort leaves the
        // provider's default reasoning allowance in force — unbounded
        // thinking spent deciding, most turns, to return an empty list.
        // Providers that cannot express effort drop it per their declared
        // `ReasoningPosture` (provider-parity invariant #8), the same path
        // every pinned management call rides.
        //
        // The operator's own triage posture wins over both pins where they set
        // one: reflection dispatches on the model the triage pin selected
        // (#1847), and a knob that chooses the model for a call but cannot
        // reach the call is a knob that lies. An `agents.triage.reasoning: off`
        // now actually reaches the wire.
        effort: posture.effort.or(Some(ReasoningEffort::Low)),
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
    lessons.truncate(MAX_LESSONS_PER_TURN);
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
