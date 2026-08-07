//! Triage: classify the prompt so the pipeline can skip work it doesn't need
//! (L-E2). The *decision* logic here is pure and synchronous — the actual
//! Role::Triage model call (under `RetryPolicy::deterministic()` and a
//! latency ceiling, L-M4) lives in [`crate::pipeline`]; this module owns only
//! the classification vocabulary, the response parser, and the deterministic
//! single-task/multi-step pattern floor.
//!
//! # Fast paths and correct downgrade (L-E2)
//!
//! - [`TaskClass::SimpleLookup`] skips both the planner and the verifier — but
//!   that verifier-skip *self-revokes* if execution unexpectedly touches files
//!   (the zero-diff guard, enforced in [`crate::pipeline`]).
//! - [`TaskClass::SingleTask`] skips DAG planning (one execute turn) but is
//!   still verified.
//! - [`TaskClass::MultiStep`] takes the full plan → scope-review → execute →
//!   verify → verdict path.
//!
//! Every fast path must **downgrade correctly**: a complex task *misclassified*
//! as simple must still complete (just without the planning/verification
//! scaffolding, more slowly) — never fail outright. Two mechanisms guarantee
//! that: the deterministic floor ([`deterministic_floor`]) can only *raise*
//! the class toward more planning (it errs toward planning, never away), and
//! the execute engine's own tool loop can still drive a complex task to
//! completion inside a single turn.

use crate::management_prompt::ManagementPrompt;

/// How much orchestration a prompt needs. Ordered least → most work; the
/// derived `Ord` is load-bearing (`deterministic_floor` takes the `max` of
/// the model's class and the pattern floor, so the floor can only ever add
/// planning, never remove it — L-E2 "errs toward planning").
// Serde joined for the resume frame (#1671): the class crosses to the host's
// durable record and back. Variant names are the wire form — renaming one is
// a frame-format change and needs the same care as a checkpoint field.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum TaskClass {
    /// A lookup/read/explain prompt: no plan, no verifier. Self-revoking on
    /// unexpected file mutations (zero-diff guard).
    SimpleLookup,
    /// One concrete change: skip DAG planning, one execute turn, still
    /// verified.
    SingleTask,
    /// Genuinely multi-step: full plan → scope review → execute → verify →
    /// verifier.
    MultiStep,
}

impl TaskClass {
    /// Whether DAG planning runs for this class. Only [`MultiStep`] plans;
    /// simple and single-task goals skip it (L-E2).
    ///
    /// [`MultiStep`]: TaskClass::MultiStep
    pub fn plans(self) -> bool {
        matches!(self, TaskClass::MultiStep)
    }

    /// Whether the verification ladder runs *unconditionally* for this class.
    /// [`SimpleLookup`] skips it (subject to the zero-diff guard, applied
    /// separately in the pipeline); the other two always verify.
    ///
    /// [`SimpleLookup`]: TaskClass::SimpleLookup
    pub fn verifies_unconditionally(self) -> bool {
        !matches!(self, TaskClass::SimpleLookup)
    }

    /// One step down the ceremony ladder, saturating at the cheapest class.
    /// The bound on how far a triage classification may lower the
    /// deterministic floor — see [`resolve_task_class`].
    pub fn one_level_cheaper(self) -> Self {
        match self {
            TaskClass::MultiStep => TaskClass::SingleTask,
            TaskClass::SingleTask | TaskClass::SimpleLookup => TaskClass::SimpleLookup,
        }
    }
}

/// What triage concluded about a goal: how much orchestration it needs, and
/// what assurance the result actually warrants.
///
/// Splitting the two is the point. "How big is this" and "what proof does it
/// need" are different axes that [`TaskClass`] alone conflates: a sweeping
/// mechanical rename needs no independent test, while a one-line change to an
/// auth check might. Triage read the goal, so triage decides — a `None` flag
/// simply means it expressed no opinion and the class-derived default stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskAssessment {
    pub class: TaskClass,
    /// Whether the input is conversational — a greeting, small talk, or a
    /// question about the agent rather than a software task at all. When set,
    /// the pipeline answers in one plain completion and skips plan → execute →
    /// witness → verify entirely (the escape hatch a bare `hi` needs so it
    /// never gets forced into authoring a witness test). Orthogonal to
    /// [`TaskClass`]: "is this even a task" is a different axis from "how big
    /// is the task", so it rides its own field instead of a bottom variant of
    /// the ordered ceremony ladder. See [`resolve_conversational`].
    pub conversational: bool,
    /// Whether an independently authored witness test is warranted.
    pub require_witness: Option<bool>,
    /// Whether a separate model reviewing the result is warranted.
    pub require_verifier: Option<bool>,
}

impl TaskAssessment {
    /// An assessment carrying only a class — what a legacy bare-token triage
    /// response, or a caller with no model answer, produces.
    pub fn from_class(class: TaskClass) -> Self {
        Self {
            class,
            conversational: false,
            require_witness: None,
            require_verifier: None,
        }
    }

    /// Whether an authored witness is warranted, honoring triage's explicit
    /// call and otherwise falling back to what the class implies.
    pub fn wants_witness(&self) -> bool {
        self.require_witness
            .unwrap_or_else(|| self.class.verifies_unconditionally())
    }

    /// Whether a model verifier is warranted.
    ///
    /// Defaults to `true`, unlike [`Self::wants_witness`]: this is only ever
    /// consulted once the verification ladder has already run and come back
    /// *inconclusive*, so the class has had its say. Skipping the verifier there
    /// has to be an explicit call from triage — a class-derived default would
    /// silently drop the verifier on exactly the path the zero-diff guard exists
    /// to catch (a "lookup" that turned out to touch files).
    ///
    /// An explicit `no` is a *request*, not a decision: by the time it is
    /// consulted the ladder is inconclusive, which falsifies the premise the
    /// triage prompt offered for saying no ("success is self-evident or a
    /// test already proves it"). So the pipeline honors the waiver only when
    /// the post-execution warrant agrees the change has nothing a reviewer
    /// could catch (`Pipeline::verifier_waiver_stands`) — the same
    /// evidence-over-prediction rule `resolve_witness`'s ceiling applies in
    /// the other direction.
    pub fn wants_verifier(&self) -> bool {
        self.require_verifier.unwrap_or(true)
    }
}

/// Parse one `KEY: yes|no` line out of a triage response, `None` when no line
/// carries the key with a recognized boolean value.
fn parse_flag(lower: &str, key: &str) -> Option<bool> {
    for line in lower.lines() {
        let line = line.trim().trim_start_matches(['-', '*', ' ']);
        let Some(rest) = line.strip_prefix(key) else {
            continue;
        };
        // A word boundary must follow the key: `judgement …` is not the
        // `verifier` line, and treating it as one used to end the scan before a
        // real `VERIFIER: no` further down was ever read.
        if rest
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        {
            continue;
        }
        let rest = rest.trim_start().trim_start_matches(':').trim();
        let Some(word) = rest
            .split(|c: char| !c.is_ascii_alphanumeric())
            .find(|w| !w.is_empty())
        else {
            continue;
        };
        match word {
            "yes" | "true" | "required" | "y" => return Some(true),
            "no" | "false" | "skip" | "none" | "n" => return Some(false),
            // Not a recognized boolean — keep scanning; a later line may
            // still carry the real answer.
            _ => continue,
        }
    }
    None
}

/// One recognized classification token: the non-task escape (`chat`) or one
/// of the three orchestration classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClassToken {
    Chat,
    Class(TaskClass),
}

/// The classification vocabulary — the single place a response word maps to a
/// token, shared by the class parser and the conversational check so the two
/// can never disagree about which token a response led with.
fn class_token(word: &str) -> Option<ClassToken> {
    match word {
        "chat" | "greeting" | "smalltalk" | "small_talk" => Some(ClassToken::Chat),
        "lookup" | "simple" | "simple_lookup" | "read" | "explain" => {
            Some(ClassToken::Class(TaskClass::SimpleLookup))
        }
        "single" | "single_task" | "task" | "fix" => Some(ClassToken::Class(TaskClass::SingleTask)),
        "multi" | "multi_step" | "multistep" | "complex" | "plan" => {
            Some(ClassToken::Class(TaskClass::MultiStep))
        }
        _ => None,
    }
}

/// The first recognized class token, read from the labeled `CLASS:` line when
/// the response has one, and from a whole-text scan only when it does not.
///
/// The labeled line is authoritative because the rest of the response is not
/// a classification: models pad the format with justification prose, and that
/// prose is written in exactly the vocabulary the scan matches. `This task
/// spans several files.` before a `CLASS: multi` used to classify as SINGLE —
/// the word "task" outran the answer — and `No plan needed.` before a
/// `CLASS: single` classified as MULTI off the word "plan". A response with
/// no labeled line at all (the legacy bare-token contract) keeps the
/// first-keyword scan unchanged.
fn first_class_token(text: &str) -> Option<ClassToken> {
    let lower = text.to_ascii_lowercase();
    let scan = |segment: &str| {
        segment
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .find_map(class_token)
    };
    for line in lower.lines() {
        let line = line
            .trim_start()
            .trim_start_matches(['-', '*', '`', '#', ' ']);
        if let Some(rest) = line.strip_prefix("class") {
            // Accept any spelling of the label itself (`class`,
            // `classification`) but require the colon, so prose that merely
            // opens with "classifying…" is not mistaken for the answer line.
            let after_label = rest.trim_start_matches(|c: char| c.is_ascii_alphanumeric());
            if let Some(value) = after_label.trim_start().strip_prefix(':') {
                // The labeled line IS the answer: an unrecognized value there
                // is an unparseable response, not a cue to go mine the prose.
                return scan(value);
            }
        }
    }
    scan(&lower)
}

/// Parse a triage response into a full [`TaskAssessment`].
///
/// Tolerant by construction: the class comes from `first_class_token` — the
/// labeled `CLASS:` line when the model followed the format, the legacy
/// first-keyword scan when it answered with a bare token — so a model that
/// ignores the structured format still classifies correctly and simply
/// expresses no assurance opinion. A `chat` answer sets
/// [`TaskAssessment::conversational`] (see [`classify_conversational`]).
/// Returns `None` only when neither a class keyword nor a chat signal
/// appears at all.
pub fn parse_triage_response(text: &str) -> Option<TaskAssessment> {
    let (class, conversational) = match first_class_token(text)? {
        ClassToken::Class(class) => (class, false),
        // A pure `chat` answer carries no orchestration keyword. Park the
        // class at the cheapest tier — it is never consulted, since the
        // conversational path skips plan/execute/witness/verify.
        ClassToken::Chat => (TaskClass::SimpleLookup, true),
    };
    let lower = text.to_ascii_lowercase();
    Some(TaskAssessment {
        class,
        conversational,
        require_witness: parse_flag(&lower, "witness"),
        require_verifier: parse_flag(&lower, "verifier"),
    })
}

/// Whether a triage response classifies the input as conversational — a `chat`
/// CLASS answer (a greeting, small talk, a question about the agent), not a
/// software task.
///
/// Reads the same token [`classify_triage_response`] reads (one scan, shared
/// via `first_class_token`), but resolves to a boolean: `true` only when
/// that token is a chat token. The labeled-line precedence is load-bearing —
/// a justification that mentions "chat" beside a real class answer
/// (`CLASS: lookup — just answering a chat about the code`) must not misfire,
/// because routing to chat is terminal no-work and a false positive silently
/// drops a real task.
pub fn classify_conversational(text: &str) -> bool {
    matches!(first_class_token(text), Some(ClassToken::Chat))
}

/// Whether the input should take the conversational (no-work) path.
///
/// Two independent routes, because betting the whole fix on a cheap triage
/// model choosing `chat` is fragile — the model that shipped this bug
/// *recognized* `hi` was odd and forced a task class anyway:
///
/// 1. **Deterministic** (`is_bare_greeting`): an unmistakable bare greeting
///    (`hi`, `hello`, `thanks`, an empty submit) routes to chat with no model
///    opinion needed. EXACT whole-message match only — never prefix/substring
///    — so `hey, fix the login bug` can never be swallowed.
/// 2. **Model**: triage said `chat`, and neither deterministic veto found a
///    positive task signal to overrule it. Same asymmetry
///    [`resolve_task_class`] relies on: the deterministic layer can only ever
///    *add* work, so it may **veto** "this is just chat" (raising a real task
///    back onto the work path) but never manufacture it. An over-eager `chat`
///    on a goal carrying an enumeration, conjoined imperatives, or
///    cross-cutting scope is overruled ([`deterministic_floor`]), and so is one
///    on a goal that plainly asks for something to be done to the workspace
///    (`has_action_request` — private, like `is_bare_greeting` above).
///
/// The second veto exists because the floor alone reads only *code-change*
/// vocabulary, and the observed failure carried none of it: `i want you to
/// organize my documents folder ... so can you explore and help me develop a
/// convention for my files` classified as `chat`, took the tool-less path, and
/// answered "Let me first check what operating system you're on" — with no
/// tools bound to check anything with. The turn then reported complete. Every
/// class but `chat` used to be phrased in terms of *code*, so a real request
/// about a folder of documents had nowhere else to land; the prompt now says
/// otherwise ([`triage_prompt`]) and this veto backstops it, because routing to
/// chat is terminal no-work and a false positive silently drops the task.
///
/// The third veto is length (`CHAT_ROUTE_MAX_CHARS`): vocabulary vetoes can
/// only ever name the task shapes someone has already seen fail, and a long
/// prompt whose verbs and nouns happen to fall outside both lists would still
/// be silently dropped on one cheap model's `chat`. Genuine small talk is
/// short; past the cap the model's opinion stops being sufficient. Like the
/// other vetoes this can only push a message *onto* the work path — a
/// rambling greeting degrades to a lookup turn, never the reverse.
pub fn resolve_conversational(model_says_chat: bool, goal: &str) -> bool {
    is_bare_greeting(goal)
        || (model_says_chat
            && goal.trim().chars().count() <= CHAT_ROUTE_MAX_CHARS
            && deterministic_floor(goal) == TaskClass::SimpleLookup
            && !has_action_request(goal))
}

/// The longest message the model-opinion arm of [`resolve_conversational`]
/// may route to chat. The deterministic greeting arm is unaffected (its
/// matches are all a few words), and so is the veto direction — length can
/// only push a message toward the work path, never onto chat.
const CHAT_ROUTE_MAX_CHARS: usize = 280;

/// Whether the whole message is nothing but a greeting / acknowledgement — the
/// deterministic half of [`resolve_conversational`]. Normalizes (trim,
/// lowercase, strip surrounding punctuation and quotes) then requires an EXACT
/// match against a small fixed set. Whole-message only by construction: a real
/// task that merely *opens* with a greeting (`hey, fix the login bug`) keeps
/// its trailing words and so never matches. An empty/whitespace-only submit
/// counts too — it is not a task.
fn is_bare_greeting(goal: &str) -> bool {
    let normalized = goal
        .trim()
        .trim_matches(|c: char| {
            c.is_whitespace() || matches!(c, '.' | '!' | '?' | ',' | '"' | '\'' | '`' | ':')
        })
        .to_ascii_lowercase();
    if normalized.is_empty() {
        return true;
    }
    const GREETINGS: &[&str] = &[
        "hi",
        "hii",
        "hiya",
        "hello",
        "hey",
        "heya",
        "yo",
        "sup",
        "hey there",
        "hi there",
        "hello there",
        "howdy",
        "greetings",
        "hi stella",
        "hey stella",
        "hello stella",
        "gm",
        "good morning",
        "good afternoon",
        "good evening",
        "thanks",
        "thank you",
        "thankyou",
        "thx",
        "ty",
        "cheers",
        "ok thanks",
        "ok thank you",
        "nice",
        "cool",
    ];
    GREETINGS.contains(&normalized.as_str())
}

/// Whether an authored witness test is warranted, resolving triage's opinion
/// against the deterministic ceiling.
///
/// The mirror of [`resolve_task_class`], and deliberately the only thing in
/// this module that may move assurance *down*. [`deterministic_floor`] may only
/// ever add ceremony because its evidence is weak — keyword guesses about how
/// big a task is. This may only ever remove ceremony, and only for the single
/// shape where the evidence is not a guess at all: see `is_pure_deletion`.
///
/// Everything else keeps the existing contract exactly — triage's explicit call
/// if it made one, otherwise what the class implies.
pub fn resolve_witness(model_opinion: Option<bool>, class: TaskClass, goal: &str) -> bool {
    if is_pure_deletion(goal) {
        return false;
    }
    model_opinion.unwrap_or_else(|| class.verifies_unconditionally())
}

/// Whether the goal is an unmistakable removal of a named code artifact —
/// `remove the witness tests`, `delete stella-cli/src/foo.rs`, `get rid of the
/// old bench/ directory`.
///
/// This overrules an explicit `WITNESS: yes` from triage, which no other
/// deterministic check here is allowed to do. The asymmetry is the point: a
/// witness test must *fail on the old code and pass on the new*, and there is
/// nothing to write that against when the change is "this artifact stops
/// existing". The author is not merely doing unnecessary work — it is cornered
/// into inventing something vacuous. The real observed failure was a request to
/// remove the witness tests producing a test file whose whole body was
/// `panic!("witness tests must be removed")`, which proves nothing about
/// anything. A deletion's proof is its diff.
///
/// Narrow by construction, because the cost of a false positive is a real
/// behavior change shipping without an authored witness. Three conditions must
/// all hold: the message must *lead* with a removal verb (after stripping
/// politeness), it must name a **file-shaped** object rather than a behavior
/// (`remove the rate limiter` is a behavior change and keeps its witness), and
/// it must carry no second clause, replacement, or preserved-behavior
/// qualifier. Anything short of all three falls through to the normal path.
///
/// Note this only turns off the *authored witness*. The verification ladder and
/// the verifier still run — deleting the wrong file stays a reviewable mistake.
///
/// This is the **pre-execution** half of the question, and it has to guess from
/// wording because no diff exists yet. [`crate::witness::warrant`] answers the
/// same question **after** execution, from the change itself, where the answer
/// is evidence rather than a guess. The two are complementary: this one can
/// spare the authoring call, that one can spare the review call.
fn is_pure_deletion(goal: &str) -> bool {
    let lower = goal.to_ascii_lowercase();

    // A second clause, a replacement, or a claim about surviving behavior means
    // this is not a bare removal — hand it back to the normal path.
    const DISQUALIFIERS: &[&str] = &[
        "instead",
        "replace",
        "so that",
        "make sure",
        "ensure",
        "but keep",
        "without breaking",
        "refactor",
        "rewrite",
        "migrate",
        " but ",
        " if ",
        " then ",
    ];
    if DISQUALIFIERS.iter().any(|m| lower.contains(m)) {
        return false;
    }
    // Sweeping or enumerated work is not the trivial case, whatever verb opens
    // it: `remove X, then update every caller` is real multi-step work.
    if deterministic_floor(&lower) == TaskClass::MultiStep || conjoined_imperative(&lower) {
        return false;
    }

    // Politeness and framing are not part of the instruction. Stripped as whole
    // leading words so `deleted` or `removal` in prose can never promote a
    // non-imperative sentence into a deletion.
    const PREFIXES: &[&str] = &[
        "hey", "hi", "ok", "okay", "so", "now", "please", "pls", "can", "could", "would", "will",
        "you", "just", "go", "ahead", "and", "i", "want", "need", "like", "lets", "let's", "us",
        "to", "the",
    ];
    let mut words = lower
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|w| !w.is_empty())
        .skip_while(|w| PREFIXES.contains(&w.trim_matches(|c: char| !c.is_ascii_alphanumeric())));

    const REMOVAL_VERBS: &[&str] = &["remove", "delete", "rm", "drop", "kill", "purge"];
    let leads_with_removal = words
        .next()
        .map(|w| {
            let w = w.trim_matches(|c: char| !c.is_ascii_alphanumeric());
            REMOVAL_VERBS.contains(&w) || w == "get" // "get rid of"
        })
        .unwrap_or(false);
    if !leads_with_removal {
        return false;
    }

    names_a_file_shaped_object(&lower)
}

/// Whether the goal names a concrete code artifact rather than a behavior — the
/// second of [`is_pure_deletion`]'s three conditions, and the one doing the
/// real work of keeping `remove the retry backoff` on the witness path.
///
/// Either an explicit path/filename, or one of a small set of nouns that only
/// ever describe artifacts. Deliberately excludes behavior-flavored nouns
/// (`check`, `handler`, `limiter`, `flag`) — those are changes that a witness
/// can genuinely pin.
fn names_a_file_shaped_object(lower: &str) -> bool {
    const SOURCE_EXTENSIONS: &[&str] = &[
        ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".java", ".rb", ".c", ".h", ".cpp",
        ".md", ".toml", ".json", ".yaml", ".yml", ".sh", ".sql",
    ];
    let has_path_token = lower.split_whitespace().any(|w| {
        let w = w.trim_matches(|c: char| matches!(c, '`' | '"' | '\'' | '(' | ')' | '.' | ','));
        w.contains('/') || SOURCE_EXTENSIONS.iter().any(|ext| w.ends_with(ext))
    });
    if has_path_token {
        return true;
    }
    const ARTIFACT_NOUNS: &[&str] = &[
        "file",
        "files",
        "test",
        "tests",
        "testcase",
        "test case",
        "test cases",
        "directory",
        "directories",
        "folder",
        "dir",
        "crate",
        "dead code",
        "unused import",
        "unused imports",
        "commented-out",
        "commented out",
    ];
    lower
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-' && c != ' ')
        .any(|seg| {
            seg.split_whitespace().any(|w| ARTIFACT_NOUNS.contains(&w))
                || ARTIFACT_NOUNS
                    .iter()
                    .any(|n| n.contains(' ') && seg.contains(n))
        })
}

/// Parse a triage model's classification response into a [`TaskClass`].
///
/// Reads the labeled `CLASS:` line when the model followed the structured
/// format; only a response with no such line falls back to the legacy scan
/// for the first recognized keyword anywhere in the text (whole tokens, not
/// substrings, so "multi_step" inside a longer word never matches and the
/// model may wrap the token in punctuation). Returns `None` when nothing
/// matches — the caller treats an unparseable triage response the same as a
/// failed call: fall through to the full path (never guess a fast path off
/// ambiguous output).
pub fn classify_triage_response(text: &str) -> Option<TaskClass> {
    match first_class_token(text)? {
        ClassToken::Class(class) => Some(class),
        // A chat token carries no orchestration class.
        ClassToken::Chat => None,
    }
}

/// The deterministic pattern floor on task class (L-E2: "single-task goals
/// (deterministic pattern check that errs toward planning)"). Independent of
/// any model call, this inspects the goal text for signals that the work is
/// *at least* single-task or multi-step, and returns the minimum class those
/// signals justify. It can only ever *raise* the class (the caller takes the
/// `max` with the model's classification), so a flaky or over-eager triage
/// model can't talk the pipeline out of planning genuinely multi-step work.
///
/// Deliberately conservative: only strong, unambiguous multi-step signals
/// (explicit enumeration, conjoined imperatives, cross-cutting scope words)
/// raise to [`TaskClass::MultiStep`]. Everything else floors at
/// [`TaskClass::SimpleLookup`] and lets the model's own classification stand.
pub fn deterministic_floor(goal: &str) -> TaskClass {
    let lower = goal.to_ascii_lowercase();

    // Strong multi-step signals: cross-cutting scope, explicit sequencing, or
    // an enumerated list of asks.
    const MULTI_STEP_MARKERS: &[&str] = &[
        " and then ",
        " after that ",
        "step 1",
        "step 2",
        "first,",
        "firstly",
        "refactor across",
        "across the codebase",
        "every file",
        "all files",
        "all the files",
        "each of the",
        "migrate all",
        "rename all",
    ];
    if MULTI_STEP_MARKERS.iter().any(|m| lower.contains(m)) {
        return TaskClass::MultiStep;
    }

    // A numbered/bulleted list of three or more items reads as multi-step.
    if enumerated_item_count(&lower) >= 3 {
        return TaskClass::MultiStep;
    }

    // Two or more conjoined imperative clauses ("do X and do Y", "add … and
    // update …") read as at least single-task work with a real change, and
    // often multi-step — floor conservatively at single-task and let the
    // model raise it further if it saw more.
    if conjoined_imperative(&lower) {
        return TaskClass::SingleTask;
    }

    TaskClass::SimpleLookup
}

/// Combine the model's triage classification with the deterministic floor,
/// taking the more-planning-heavy of the two — except that the model may
/// lower the floor by at most one level (L-E2 "errs toward planning":
/// raising is unbounded, lowering is capped at one rung; see the body).
/// When the model call failed or its response didn't parse (`model_class`
/// is `None`), the floor stands alone.
pub fn resolve_task_class(model_class: Option<TaskClass>, goal: &str) -> TaskClass {
    let floor = deterministic_floor(goal);
    let Some(model) = model_class else {
        // No usable classification: the floor stands alone, erring toward
        // planning exactly as it always has.
        return floor;
    };
    // Triage read the goal; the floor only pattern-matched it, so triage may
    // route work onto a cheaper path than the keywords guessed — otherwise a
    // one-line fix whose description contains "and then" buys a full
    // plan/witness/verifier pipeline forever.
    //
    // But only one level cheaper. Triage is meant to be the weakest, fastest
    // tier, and the floor's keyword evidence is weak rather than worthless;
    // letting a cheap model strip planning, witness, and verifier in a single
    // step is a bigger bet than the speed is worth. Raising stays unbounded —
    // erring toward more work was never the risk.
    model.max(floor.one_level_cheaper())
}

/// Count enumerated list items ("1.", "2)", "- ", "* ") whose text reads as
/// an instruction — a crude but deterministic proxy for "the user listed
/// several asks".
///
/// An item counts only when it *leads* with a work verb (after any
/// sequencing/politeness words). That requirement is what keeps specification
/// bullets from counting: a task description that enumerates *requirements*
/// (`- listens on port 8080`, `- output is JSON`) is one task with three
/// constraints, not three tasks, and flooring it at multi-step bought a
/// planner pass for work one execute turn handles. Only items that ask for
/// something (`- add a field`) still raise the floor.
fn enumerated_item_count(lower: &str) -> usize {
    let mut count = 0usize;
    for line in lower.lines() {
        let t = line.trim_start();
        let body = if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            rest
        } else {
            let mut chars = t.chars();
            match (chars.next(), chars.next()) {
                (Some(d), Some('.' | ')')) if d.is_ascii_digit() => chars.as_str(),
                _ => continue,
            }
        };
        if leads_with_work_verb(body) {
            count += 1;
        }
    }
    count
}

/// Whether `text` opens with a work verb, skipping the sequencing and
/// politeness words that commonly front an enumerated instruction
/// (`- then update the schema`, `1. please add a field`).
fn leads_with_work_verb(text: &str) -> bool {
    const LEAD_INS: &[&str] = &[
        "then", "also", "and", "now", "next", "finally", "first", "second", "third", "please",
        "pls", "you", "should", "must",
    ];
    text.split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_ascii_alphanumeric()))
        .filter(|w| !w.is_empty())
        .find(|w| !LEAD_INS.contains(w))
        .is_some_and(|w| WORK_VERBS.contains(&w))
}

/// Verbs that ask for work to be done — the shared vocabulary behind
/// [`has_action_request`] and the enumerated-item floor
/// ([`enumerated_item_count`]). A superset of [`conjoined_imperative`]'s
/// CHANGE_VERBS (which stays mutation-only on purpose: two *mutating* clauses
/// are its evidence), widened with the inspect/arrange verbs that are real
/// work over files, and with the build/install/run operations vocabulary
/// terminal tasks are phrased in. Matching everywhere is whole-word and
/// unstemmed — see [`has_action_request`] for why.
const WORK_VERBS: &[&str] = &[
    "add",
    "audit",
    "benchmark",
    "build",
    "categorise",
    "categorize",
    "check",
    "clean",
    "compile",
    "compress",
    "configure",
    "convert",
    "create",
    "debug",
    "decrypt",
    "delete",
    "deploy",
    "download",
    "encrypt",
    "explore",
    "extract",
    "find",
    "fix",
    "generate",
    "group",
    "implement",
    "insert",
    "inspect",
    "install",
    "launch",
    "list",
    "look",
    "migrate",
    "move",
    "organise",
    "organize",
    "parse",
    "patch",
    "read",
    "rearrange",
    "refactor",
    "remove",
    "rename",
    "replace",
    "restore",
    "restructure",
    "review",
    "run",
    "search",
    "setup",
    "sort",
    "start",
    "summarise",
    "summarize",
    "test",
    "tidy",
    "train",
    "uncompress",
    "unzip",
    "update",
    "upgrade",
    "wire",
    "write",
    "zip",
];

/// Detect two or more imperative clauses joined by "and"/"then" — "add a
/// field and update the migration". Heuristic and intentionally narrow: it
/// only fires when an "and"/"then" is followed (soon) by a known change verb,
/// so a descriptive "the parser and the lexer" doesn't trip it.
fn conjoined_imperative(lower: &str) -> bool {
    const CHANGE_VERBS: &[&str] = &[
        "add",
        "update",
        "remove",
        "delete",
        "rename",
        "create",
        "write",
        "fix",
        "refactor",
        "implement",
        "wire",
        "move",
        "replace",
        "migrate",
        "extract",
    ];
    // A leading change verb establishes the first clause. `split_whitespace`
    // already skips leading whitespace, so no separate trim is needed.
    let has_leading_verb = lower
        .split_whitespace()
        .next()
        .map(|w| CHANGE_VERBS.contains(&w))
        .unwrap_or(false);
    if !has_leading_verb {
        return false;
    }
    // A second change verb after an " and "/" then " establishes the second.
    for sep in [" and ", " then "] {
        if let Some(idx) = lower.find(sep) {
            let rest = &lower[idx + sep.len()..];
            if let Some(word) = rest.split_whitespace().next()
                && CHANGE_VERBS.contains(&word)
            {
                return true;
            }
        }
    }
    false
}

/// Whether the message plainly asks for something to be *done* to the
/// workspace — the second deterministic veto in [`resolve_conversational`].
///
/// [`conjoined_imperative`] cannot answer this: it requires the message's very
/// first word to be a change verb, so every politely-framed request (`i want
/// you to organize ...`, `can you explore ...`, `could you take a look at ...`)
/// is invisible to it, as is every non-code verb. That is correct for its own
/// job — floor a *class* — but far too narrow to decide whether a message is a
/// task at all.
///
/// The test here is deliberately two-part: an action verb **and** a noun naming
/// something in the workspace. Either alone is too loose — `nice, thanks for
/// fixing the file` names a file, and plenty of small talk carries a stray
/// verb. Requiring both keeps the false-positive rate near zero, which is what
/// matters: this veto can only push a message onto the work path, and pushing a
/// genuine `hi` there is the exact failure the conversational route was built
/// to prevent (`hi` authoring a witness test).
///
/// Matching is whole-word and unstemmed, both on purpose. Whole-word so `dir`
/// does not fire on `direct` and `code` does not fire on `encode`. Unstemmed so
/// the bare imperative (`organize`, `explore`, `review`) fires while the past
/// and progressive forms that characterize *acknowledgements* (`thanks for
/// fixing the file`, `nice, you renamed the folder`) do not — those are chat,
/// and the work they describe already happened.
fn has_action_request(goal: &str) -> bool {
    // Nouns naming something the agent could act on. Not code-specific: the
    // workspace is whatever directory Stella was pointed at, and pointing it at
    // a folder of documents is a supported thing to do. The ops rows (server,
    // database, dataset, container, …) exist for the same reason the ops verbs
    // do — a terminal task phrased entirely in that vocabulary used to be
    // invisible to this veto, and routing it to chat is terminal no-work.
    const WORKSPACE_NOUNS: &[&str] = &[
        "app",
        "application",
        "binary",
        "branch",
        "code",
        "codebase",
        "config",
        "container",
        "csv",
        "data",
        "database",
        "dataset",
        "db",
        "dependencies",
        "dependency",
        "desktop",
        "dir",
        "directories",
        "directory",
        "docs",
        "document",
        "documents",
        "downloads",
        "endpoint",
        "env",
        "environment",
        "file",
        "files",
        "folder",
        "folders",
        "function",
        "image",
        "json",
        "log",
        "logs",
        "module",
        "notebook",
        "notes",
        "package",
        "photos",
        "pictures",
        "pipeline",
        "program",
        "project",
        "repo",
        "repository",
        "schema",
        "screenshots",
        "script",
        "scripts",
        "server",
        "service",
        "spreadsheet",
        "spreadsheets",
        "table",
        "test",
        "tests",
        "tree",
        "url",
        "website",
        "workspace",
        "yaml",
    ];

    let words: Vec<&str> = goal
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let has = |set: &[&str]| {
        words
            .iter()
            .any(|w| set.contains(&w.to_ascii_lowercase().as_str()))
    };
    has(WORK_VERBS) && has(WORKSPACE_NOUNS)
}

/// The prompt handed to the Role::Triage model to classify a user message. A
/// tiny, fixed instruction for a cheap/fast tier: it asks for three labeled
/// lines (`CLASS`/`WITNESS`/`VERIFIER`), but the parser tolerates a bare class
/// token too ([`parse_triage_response`]). The first line offers `chat` so a
/// non-task (a greeting, small talk) has somewhere to land instead of being
/// forced onto the work path.
///
/// The class descriptions are deliberately **not** phrased in terms of code.
/// They were, and it misrouted real work: every alternative to `chat` said
/// "code", so `organize my documents folder` had no bucket that fit and a cheap
/// model reasonably picked the only one that admitted non-software input —
/// `chat`, which is tool-less and terminal. The workspace is whatever directory
/// Stella was pointed at, so the split that matters is *does this ask for
/// anything to be done*, not *is this about source files*. See
/// [`resolve_conversational`] for the deterministic backstop.
///
/// `structure` is the workspace listing the `RepoStructurePort` already
/// supplies to the planner and the witness author — bounded here to
/// `TRIAGE_STRUCTURE_CHARS` and rendered *before* the task so the
/// `Task:`/`Answer:` pair stays adjacent at the end. Triage decides the
/// highest-leverage questions in the pipeline (how much orchestration, whether
/// a witness is warranted, whether this is a task at all) and until this
/// section existed it was the only stage deciding anything from the goal
/// string alone: `WITNESS: no` was guessed with no way to see whether the
/// workspace even has a test harness, and the chat-vs-task call had no
/// evidence of what the workspace *is*. Empty structure (no git, port
/// unavailable) renders the legacy goal-only payload byte-for-byte — the
/// listing is evidence when it exists, never a requirement. It rides in the
/// volatile payload half, so the cacheable instruction block (#1434) is
/// untouched.
pub fn triage_prompt(goal: &str, structure: &str) -> ManagementPrompt {
    let structure = bounded_structure(structure);
    let payload = if structure.is_empty() {
        format!("Task:\n{goal}\n\nAnswer:")
    } else {
        format!("Workspace listing (truncated):\n{structure}\n\nTask:\n{goal}\n\nAnswer:")
    };
    ManagementPrompt {
        instructions: TRIAGE_INSTRUCTIONS,
        payload,
    }
}

/// Ceiling on the workspace-listing characters the triage payload may carry —
/// the same order as one recalled frame (`RECALL_FRAME_CHARS` in
/// `crate::pipeline`). Triage is the cheap, latency-capped tier: the listing's
/// job is to show *what kind of workspace this is* (code with a test tree, a
/// folder of documents, a monorepo) and roughly how big, which the head of a
/// sorted listing answers; past this point more paths add cost per call
/// without changing any triage decision. Counted in `chars`, not bytes, so
/// multi-byte paths are not over-charged.
const TRIAGE_STRUCTURE_CHARS: usize = 4_000;

/// Clamp a structure summary to [`TRIAGE_STRUCTURE_CHARS`], head-kept, with an
/// explicit marker when anything was dropped — a truncated listing must never
/// read as the whole workspace.
fn bounded_structure(structure: &str) -> String {
    let trimmed = structure.trim();
    if trimmed.chars().count() <= TRIAGE_STRUCTURE_CHARS {
        return trimmed.to_string();
    }
    let kept: String = trimmed.chars().take(TRIAGE_STRUCTURE_CHARS).collect();
    format!("{kept}\n[… listing truncated at the triage budget …]")
}

/// The triage protocol block (#1434): fully literal, so it is a plain
/// `&'static str` — byte-identical on every triage call, riding as the
/// system message the provider adapters can cache-mark.
const TRIAGE_INSTRUCTIONS: &str = "Classify the following user message, and decide what assurance its \
     result actually warrants. Do NOT assume it is a software task — it may \
     just be conversation. Answer with EXACTLY these three lines and \
     nothing else:\n\
     CLASS: chat|lookup|single|multi\n\
     WITNESS: yes|no\n\
     VERIFIER: yes|no\n\n\
     CLASS is what the message needs:\n\
     - `chat`    — asks for NOTHING to be done: a greeting (`hi`), thanks, \
     small talk, or a question about you. Reply conversationally; touch no \
     files, write no plan, no test.\n\
     - `lookup`  — a read/explain/search/explore question about the \
     workspace that changes no files\n\
     - `single`  — one concrete change\n\
     - `multi`   — genuinely multi-step work spanning several changes\n\n\
     The workspace is not always code. Exploring, organizing, renaming, \
     sorting, cleaning up or summarizing ANY files — documents, notes, \
     data, photos — is real work: classify it `lookup`, `single` or `multi` \
     like any other task. Use `chat` ONLY when the message asks you to do \
     nothing at all. If it asks you to look at, decide about, or change \
     anything, it is never `chat`.\n\n\
     A truncated listing of the workspace's files may precede the task. It \
     is evidence, not the ask: use it to judge what kind of workspace this \
     is, how far the task could reach, and whether a test could even run \
     here — a workspace with no test files or build manifest usually \
     warrants WITNESS: no. Classify the message, never the listing.\n\n\
     WITNESS is whether a failing test should be written first to pin the \
     intended behavior. Say no when the change is mechanical, when \
     correctness is already obvious from the diff, or when the project \
     has no way to run such a test. Always say no when the ask is to \
     DELETE something — a witness must fail on the old code and pass on \
     the new, and a removal leaves nothing to write that against. Prefer \
     `no` on small, self-evident work — ceremony that proves nothing \
     costs the user time and money.\n\
     VERIFIER is whether a separate model should review the result. It is \
     only consulted when no test settled the outcome, so say no ONLY \
     when a test will already prove the change or the result is \
     trivially checkable from its diff; when unsure, say yes.";

#[cfg(test)]
mod tests;
