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

// `TurnFriction` is re-exported again as of #3946. It was withheld while its
// only production consumer, `FrictionTap`, lay deleted with the staged
// pipeline it wrapped (#3865) — a period in which every shipped digest's
// friction section rendered empty. The raw loop's doors now fold their own
// journal into it (`TurnFriction::from_events`), so the type is once more part
// of this module's public shape rather than an internal of `digest`.
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
pub fn should_reflect_on<T, E: std::fmt::Display>(result: &Result<T, E>) -> bool {
    match result {
        Ok(_) => true,
        Err(reason) => !reason.to_string().contains(stella_core::SOFT_STOP_REASON),
    }
}

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
///
/// The **written-output contract** this cap implies — how many tokens eight
/// three-field lessons plus a five-field self-review actually need — is not
/// stated here. It is declared once for every standalone role at
/// `accounted_call::standalone_bounds`, so that a role added later has to
/// decide its bounds rather than inherit whatever number its own call site
/// picked (#2444). Raising this constant without revisiting that arm buys room
/// the wire will not carry.
pub(crate) const MAX_LESSONS_PER_TURN: usize = 8;

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
        // Two independent things have to fit under one wire number, and each
        // was sent alone once. The written contract is what reflection is asked
        // to WRITE — three prose fields per lesson, up to MAX_LESSONS_PER_TURN
        // of them; the headroom on top is what a reasoning model spends before
        // it writes any of them. Sending the contract alone is what froze this
        // workspace's learning plane for nine days: execution 63 came back at
        // exactly 2,048 output tokens with `finish_reason: length` and no
        // visible text, which `extract_lesson_array` reads as zero lessons —
        // indistinguishable, from every surface, from a turn that genuinely
        // taught nothing (#2174). Undersizing either half has that same
        // invisible symptom, so both are sized together — and neither is sized
        // here. They are declared once at the standalone chokepoint
        // (`accounted_call::standalone_bounds`), because reflection was the
        // only one of the four standalone roles that had ever been given
        // headroom, and leaving the number at this call site is what let the
        // other three each pick one in isolation (#2444).
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
