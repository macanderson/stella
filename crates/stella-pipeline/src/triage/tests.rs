//! Unit tests for [`super`] — split out of `triage.rs` to keep the
//! classifier a manageable size; a child module, so the private
//! surface (`class_token`, `parse_flag`, `deterministic_floor`, ...)
//! stays reachable via `super::*`.

use super::*;

#[test]
fn task_class_ordering_is_least_to_most_work() {
    assert!(TaskClass::SimpleLookup < TaskClass::SingleTask);
    assert!(TaskClass::SingleTask < TaskClass::MultiStep);
}

#[test]
fn plans_and_verifies_flags_match_the_fast_path_design() {
    assert!(!TaskClass::SimpleLookup.plans());
    assert!(!TaskClass::SingleTask.plans());
    assert!(TaskClass::MultiStep.plans());

    assert!(!TaskClass::SimpleLookup.verifies_unconditionally());
    assert!(TaskClass::SingleTask.verifies_unconditionally());
    assert!(TaskClass::MultiStep.verifies_unconditionally());
}

#[test]
fn parses_each_class_from_bare_and_decorated_responses() {
    assert_eq!(
        classify_triage_response("lookup"),
        Some(TaskClass::SimpleLookup)
    );
    assert_eq!(
        classify_triage_response("Classification: single."),
        Some(TaskClass::SingleTask)
    );
    assert_eq!(
        classify_triage_response("`multi` — this spans files"),
        Some(TaskClass::MultiStep)
    );
    assert_eq!(
        classify_triage_response("I think this is a SIMPLE read"),
        Some(TaskClass::SimpleLookup)
    );
}

#[test]
fn unparseable_triage_response_is_none_not_a_guess() {
    assert_eq!(classify_triage_response(""), None);
    assert_eq!(classify_triage_response("hmm, not sure"), None);
    assert_eq!(classify_triage_response("42"), None);
}

#[test]
fn deterministic_floor_raises_on_strong_multi_step_markers() {
    assert_eq!(
        deterministic_floor("Add the field and then update the migration"),
        TaskClass::MultiStep
    );
    assert_eq!(
        deterministic_floor("Rename all callers of foo across the codebase"),
        TaskClass::MultiStep
    );
    assert_eq!(
        deterministic_floor("First, add a route. Secondly wire the handler."),
        TaskClass::MultiStep
    );
}

#[test]
fn deterministic_floor_detects_enumerated_lists() {
    let goal = "Please:\n1. add a field\n2. update the schema\n3. add a test";
    assert_eq!(deterministic_floor(goal), TaskClass::MultiStep);

    let bullets = "Do these:\n- add a field\n- update the schema\n- add a test";
    assert_eq!(deterministic_floor(bullets), TaskClass::MultiStep);
}

#[test]
fn deterministic_floor_conjoined_imperatives_reach_single_task() {
    assert_eq!(
        deterministic_floor("Add a field and update the migration"),
        // "and then" isn't present, but two change verbs joined by "and"
        // floor at single-task (the model may raise it to multi).
        TaskClass::SingleTask
    );
}

#[test]
fn deterministic_floor_leaves_plain_prompts_at_lookup() {
    assert_eq!(
        deterministic_floor("What does the retry policy do?"),
        TaskClass::SimpleLookup
    );
    assert_eq!(
        deterministic_floor("Explain the flip oracle"),
        TaskClass::SimpleLookup
    );
    // A descriptive "and" between nouns must NOT trip the imperative
    // detector (no leading change verb).
    assert_eq!(
        deterministic_floor("Where are the parser and the lexer defined?"),
        TaskClass::SimpleLookup
    );
}

#[test]
fn parses_the_structured_assurance_answer() {
    let a = parse_triage_response("CLASS: single\nWITNESS: no\nJUDGE: yes")
        .expect("a well-formed answer parses");
    assert_eq!(a.class, TaskClass::SingleTask);
    assert_eq!(a.require_witness, Some(false));
    assert_eq!(a.require_judge, Some(true));
    assert!(!a.wants_witness(), "an explicit no wins over the class");
    assert!(a.wants_judge());
}

#[test]
fn a_bare_token_answer_still_classifies_and_claims_no_assurance_opinion() {
    // The pre-existing contract: models that ignore the structured format
    // must keep working, falling back to what the class implies.
    let a = parse_triage_response("multi").expect("a bare token parses");
    assert_eq!(a.class, TaskClass::MultiStep);
    assert_eq!(a.require_witness, None);
    assert_eq!(a.require_judge, None);
    assert!(a.wants_witness(), "class-derived default stands");
}

#[test]
fn an_answer_with_no_class_keyword_is_not_a_guess() {
    assert_eq!(parse_triage_response("WITNESS: no\nJUDGE: no"), None);
    assert_eq!(parse_triage_response("hmm, hard to say"), None);
}

#[test]
fn assurance_flags_tolerate_bullets_punctuation_and_case() {
    let a = parse_triage_response("- CLASS: Multi\n- Witness: NO\n* judge : none").expect("parses");
    assert_eq!(a.class, TaskClass::MultiStep);
    assert_eq!(a.require_witness, Some(false));
    assert_eq!(a.require_judge, Some(false));
}

#[test]
fn a_chat_answer_parses_as_conversational_and_parks_the_class() {
    // The whole point: a greeting must have somewhere to land. A `chat`
    // CLASS carries no orchestration keyword, so the class parks at the
    // cheapest tier and the conversational flag is what the pipeline reads.
    let a =
        parse_triage_response("CLASS: chat\nWITNESS: no\nJUDGE: no").expect("a chat answer parses");
    assert!(a.conversational);
    assert_eq!(a.class, TaskClass::SimpleLookup);

    // A bare `chat` token works too (models that ignore the format).
    let bare = parse_triage_response("chat").expect("a bare chat token parses");
    assert!(bare.conversational);
}

#[test]
fn classify_conversational_is_first_class_keyword_wins() {
    assert!(classify_conversational("chat"));
    assert!(classify_conversational("CLASS: chat — greeting, no task"));
    assert!(classify_conversational("greeting"));
    // A real class token appearing FIRST overrules a later "chat": routing
    // to no-work is terminal, so a justification must never flip it.
    assert!(!classify_conversational(
        "CLASS: lookup — just answering a chat about the code"
    ));
    assert!(!classify_conversational("multi"));
    assert!(!classify_conversational(""));
    assert!(!classify_conversational("hmm, not sure"));
}

#[test]
fn a_non_chat_answer_is_not_conversational() {
    assert!(!parse_triage_response("single").unwrap().conversational);
    assert!(
        !parse_triage_response("CLASS: lookup\nWITNESS: no\nJUDGE: no")
            .unwrap()
            .conversational
    );
    // A class carried by `from_class` is never conversational.
    assert!(!TaskAssessment::from_class(TaskClass::SimpleLookup).conversational);
}

#[test]
fn a_bare_greeting_is_conversational_without_any_model_opinion() {
    // The deterministic route: no model answer needed for an unmistakable
    // greeting — the fix must not hinge on a cheap model choosing `chat`.
    assert!(resolve_conversational(false, "hi"));
    assert!(resolve_conversational(false, "  Hi!  "));
    assert!(resolve_conversational(false, "hello"));
    assert!(resolve_conversational(false, "thanks"));
    assert!(resolve_conversational(false, ""));
}

#[test]
fn is_bare_greeting_matches_whole_message_only() {
    for g in [
        "hi",
        "Hi",
        "  hi  ",
        "hi!",
        "hello.",
        "HELLO",
        "hey",
        "yo",
        "hey there",
        "thanks",
        "thank you",
        "thankyou",
        "ty",
        "",
        "   ",
        "hi stella",
        "\"hi\"",
        "gm",
        "good morning",
    ] {
        assert!(is_bare_greeting(g), "{g:?} should be a bare greeting");
    }
    // NOT greetings: a real task that merely opens with one, or anything
    // with trailing content. Whole-message match, never prefix/substring.
    for g in [
        "hey, fix the login bug",
        "hi, can you refactor the parser",
        "fix the parser",
        "hello world function",
        "hey there, add a test",
        "thanks for nothing, delete the file",
        "high", // superstring of "hi"
        "history",
    ] {
        assert!(!is_bare_greeting(g), "{g:?} must NOT be a bare greeting");
    }
}

#[test]
fn the_floor_vetoes_an_over_eager_model_chat_when_there_is_task_signal() {
    // Model says chat, no task signal, not a bare greeting → chat stands.
    assert!(resolve_conversational(true, "what's your favorite color"));
    // Model silent AND not a greeting → not conversational.
    assert!(!resolve_conversational(
        false,
        "what does the retry policy do?"
    ));
    // Positive task signal overrules an over-eager `chat`: an enumeration,
    // conjoined imperatives, or cross-cutting scope is real work no matter
    // what the cheap triage model guessed.
    assert!(!resolve_conversational(
        true,
        "add the field and then update the migration"
    ));
    assert!(!resolve_conversational(
        true,
        "rename all callers of foo across the codebase"
    ));
    assert!(!resolve_conversational(true, "add a field and update it"));
}

#[test]
fn an_action_request_on_non_code_files_is_not_chat() {
    // The observed failure, verbatim from the session journal. Triage
    // answered `CLASS: chat`, the floor saw no code-change keyword, and the
    // task took the tool-less conversational path: the model replied "Let
    // me first check what operating system you're on" with no tools bound,
    // and the turn reported complete. It must reach the work path.
    assert!(!resolve_conversational(
        true,
        "i want you to organize my documents folder but I don't know whats \
         inside so can you explore and help me develop a convention for my \
         files and where they are saved and what i call them"
    ));

    // The shapes the floor is structurally blind to: a polite frame moves
    // the verb off the front of the message, and none of these verbs are
    // code-change verbs.
    assert!(!resolve_conversational(
        true,
        "could you take a look at my documents directory"
    ));
    assert!(!resolve_conversational(
        true,
        "can you explore the downloads folder and tell me what is in there"
    ));
    assert!(!resolve_conversational(
        true,
        "sort out the files on my desktop"
    ));
    assert!(!resolve_conversational(true, "organize this project"));
}

#[test]
fn real_chat_survives_the_action_request_veto() {
    // The negative half, and the one that actually constrains the design:
    // this veto can only ever push a message ONTO the work path, so a false
    // positive resurrects the bug the conversational route exists to fix
    // (`hi` authoring a witness test). Every one of these must stay chat.
    for goal in [
        "what can you do?",
        "who built you?",
        "how are you today",
        // Names a workspace noun but asks for nothing — a verb alone or a
        // noun alone must never be enough.
        "that's a cool project",
        "i really like this codebase",
        // Acknowledgements of work already done. The past/progressive forms
        // are why matching is unstemmed: `fixing`/`renamed` must not read
        // as `fix`/`rename`.
        "nice, thanks for fixing the file",
        "thanks, you renamed the folder correctly",
    ] {
        assert!(
            resolve_conversational(true, goal),
            "{goal:?} must stay conversational"
        );
    }
}

#[test]
fn action_request_matching_is_whole_word() {
    // `encoder` contains "code" and `direct` contains "dir"; a substring
    // test would call this a workspace request. It is not one.
    assert!(!has_action_request("check that the encoder is direct"));
    // Same verb, a real noun → now it is.
    assert!(has_action_request("check that the directory is empty"));
}

#[test]
fn the_triage_prompt_does_not_frame_every_real_class_as_code() {
    let p = triage_prompt("anything");
    // The regression: `lookup`/`single`/`multi` each said "code", leaving a
    // non-code task no bucket but `chat`.
    assert!(
        p.contains("The workspace is not always code"),
        "the prompt must tell triage that non-code work is still work"
    );
    assert!(
        !p.contains("one concrete code change"),
        "`single` must not be scoped to code changes"
    );
    assert!(
        !p.contains("question about the code"),
        "`lookup` must not be scoped to questions about code"
    );
}

#[test]
fn a_judge_opinion_is_required_to_skip_the_judge() {
    // `wants_judge` is only consulted after the ladder came back
    // inconclusive, so silence must never mean "skip".
    for class in [
        TaskClass::SimpleLookup,
        TaskClass::SingleTask,
        TaskClass::MultiStep,
    ] {
        assert!(
            TaskAssessment::from_class(class).wants_judge(),
            "{class:?} with no triage opinion must still reach the judge"
        );
    }
}

#[test]
fn triage_may_lower_the_floor_by_one_level_and_raise_it_without_bound() {
    // Floor says multi (markers present), triage says lookup → lands on
    // single: DAG planning is skipped, verification is kept. Triage read
    // the goal, but it is the weakest tier, so it does not get to strip
    // planning, witness, and judge in one move.
    assert_eq!(
        resolve_task_class(
            Some(TaskClass::SimpleLookup),
            "refactor across the codebase"
        ),
        TaskClass::SingleTask
    );
    // One level down is honored exactly.
    assert_eq!(
        resolve_task_class(Some(TaskClass::SingleTask), "refactor across the codebase"),
        TaskClass::SingleTask
    );
    // A floor of single may be lowered all the way to lookup.
    assert_eq!(
        resolve_task_class(Some(TaskClass::SimpleLookup), "add the field and update it"),
        TaskClass::SimpleLookup
    );
    // Raising stays unbounded: erring toward more work was never the risk.
    assert_eq!(
        resolve_task_class(Some(TaskClass::MultiStep), "explain X"),
        TaskClass::MultiStep
    );
    // Failed/unparseable triage → floor stands alone.
    assert_eq!(
        resolve_task_class(None, "explain X"),
        TaskClass::SimpleLookup
    );
    assert_eq!(
        resolve_task_class(None, "add a field and then update the migration"),
        TaskClass::MultiStep
    );
}

#[test]
fn a_bare_deletion_never_authors_a_witness() {
    // The reported failure, verbatim: this asked for a removal and got a
    // witness author, which produced a test whose entire body was a
    // `panic!("witness tests must be removed")`.
    for goal in [
        "can you remove the witness tests please",
        "remove the witness tests",
        "delete stella-cli/tests/slash_models_witness.rs",
        "please delete the hi_witness.rs file",
        "rm bench/old_harness.rs",
        "get rid of the dead code in stella-graph/src/walk.rs",
        "drop the unused imports",
        "delete the docs/legacy folder",
    ] {
        assert!(
            is_pure_deletion(goal),
            "{goal:?} should be recognized as a bare deletion"
        );
        // ... and the ceiling overrules even an explicit WITNESS: yes.
        assert!(
            !resolve_witness(Some(true), TaskClass::MultiStep, goal),
            "{goal:?} must not author a witness even when triage asked for one"
        );
        assert!(!resolve_witness(None, TaskClass::SingleTask, goal));
    }
}

#[test]
fn the_deletion_ceiling_does_not_swallow_real_work() {
    // The negative half, and the one that matters: a false positive here
    // ships a behavior change with no authored witness. Each of these must
    // keep the normal path.
    for goal in [
        // Behavior removals — nothing file-shaped, a witness can pin these.
        "remove the retry backoff",
        "remove the rate limiter from the auth handler",
        "delete the stale-session check",
        "drop the trailing-slash redirect",
        // Deletion plus a second clause / replacement / preserved behavior.
        "remove the old parser and add the new one",
        "delete config.toml and replace it with config.yaml",
        "remove src/legacy.rs but keep the public API working",
        "delete the cache layer, make sure reads still work",
        "remove the shim then update every caller",
        "delete the adapter and refactor the callers",
        // Sweeping / enumerated work that merely opens with a removal verb.
        "remove the deprecated helper from every file",
        "delete all the files under bench/",
        // Not an imperative removal at all — prose that mentions deletion.
        "why was stella-cli/src/foo.rs deleted?",
        "explain the removal of the witness stage",
        "the tests were deleted, can you tell me what covered auth.rs",
        // A real change that happens to name a file.
        "fix the panic in stella-pipeline/src/verify.rs",
        "add a test to stella-core/src/lib.rs",
    ] {
        assert!(
            !is_pure_deletion(goal),
            "{goal:?} must NOT be treated as a bare deletion"
        );
        assert!(
            resolve_witness(Some(true), TaskClass::SingleTask, goal),
            "{goal:?} must honor an explicit WITNESS: yes"
        );
    }
}

#[test]
fn the_ceiling_leaves_every_other_witness_decision_untouched() {
    // Outside the deletion shape, `resolve_witness` must be exactly the old
    // `wants_witness` contract: triage's call, else the class default.
    let goal = "make the parser handle escaped quotes";
    assert!(!resolve_witness(Some(false), TaskClass::MultiStep, goal));
    assert!(resolve_witness(Some(true), TaskClass::SimpleLookup, goal));
    assert!(!resolve_witness(None, TaskClass::SimpleLookup, goal));
    assert!(resolve_witness(None, TaskClass::SingleTask, goal));
    assert!(resolve_witness(None, TaskClass::MultiStep, goal));
    for class in [
        TaskClass::SimpleLookup,
        TaskClass::SingleTask,
        TaskClass::MultiStep,
    ] {
        assert_eq!(
            resolve_witness(None, class, goal),
            TaskAssessment::from_class(class).wants_witness(),
            "{class:?} must keep the class-derived default"
        );
    }
}

#[test]
fn triage_prompt_names_all_four_class_tokens_and_the_goal() {
    let p = triage_prompt("Fix the failing test");
    assert!(p.contains("chat"));
    assert!(p.contains("lookup"));
    assert!(p.contains("single"));
    assert!(p.contains("multi"));
    assert!(p.contains("Fix the failing test"));
}

#[test]
fn the_labeled_class_line_outranks_justification_prose() {
    // The regression: justification prose is written in the same
    // vocabulary the fallback scan matches, and it used to outrun the
    // answer — "task" here classified this multi answer as SINGLE…
    assert_eq!(
        classify_triage_response(
            "This task spans several files.\nCLASS: multi\nWITNESS: yes\nJUDGE: yes"
        ),
        Some(TaskClass::MultiStep)
    );
    // …and "plan" classified this single answer as MULTI.
    assert_eq!(
        classify_triage_response("No plan needed here.\nCLASS: single\nWITNESS: no\nJUDGE: no"),
        Some(TaskClass::SingleTask)
    );
    // Chat on the labeled line wins over class words in the prose.
    assert!(classify_conversational(
        "The task here is nothing at all.\nCLASS: chat"
    ));
    // The labeled line IS the answer: garbage there is unparseable, not a
    // cue to go mine the prose for the first keyword that turns up.
    assert_eq!(classify_triage_response("CLASS: dunno\nWITNESS: no"), None);
    // No labeled line at all keeps the legacy first-keyword contract.
    assert_eq!(
        classify_triage_response("I think this is a SIMPLE read"),
        Some(TaskClass::SimpleLookup)
    );
}

#[test]
fn flag_scan_survives_near_miss_lines() {
    // `judgement…` is not the judge line (word boundary), and an
    // unrecognized value on an early line must not end the scan before
    // the real answer further down is read.
    let a = parse_triage_response(
        "CLASS: single\njudgement: seems fine\nwitness: probably\nWITNESS: no\nJUDGE: no",
    )
    .expect("parses");
    assert_eq!(a.class, TaskClass::SingleTask);
    assert_eq!(a.require_witness, Some(false));
    assert_eq!(a.require_judge, Some(false));
}

#[test]
fn a_long_message_is_never_routed_to_chat_on_model_opinion() {
    // Chosen to dodge every vocabulary veto — no work verb, no workspace
    // noun, no floor marker — so only the length cap stands between a
    // wrong `chat` and silently dropping the task.
    let goal = "Given the encrypted payload sitting at /tmp/payload.bin and the \
                reference notes reproduced below, produce a small decryption \
                utility, make certain the recovered plaintext round-trips through \
                the encoder, and place the recovered plaintext at /tmp/answer.bin \
                with the exact byte layout the checker expects to see there.";
    assert!(goal.trim().chars().count() > CHAT_ROUTE_MAX_CHARS);
    assert!(
        !resolve_conversational(true, goal),
        "a benchmark-length instruction must stay on the work path"
    );
    // The cap does not disturb genuinely short small talk.
    assert!(resolve_conversational(true, "what's your favorite color"));
}

#[test]
fn terminal_task_vocabulary_defeats_a_chat_call() {
    // Ops-flavored requests carry none of the code-change or
    // document-arranging vocabulary the veto originally knew, and every
    // one of these is real work a `chat` route would silently drop.
    for goal in [
        "install the dependencies and start the server",
        "build the project and run the tests",
        "download the dataset and train on it",
        "configure the service to listen on port 8080",
    ] {
        assert!(
            !resolve_conversational(true, goal),
            "{goal:?} is work, not chat"
        );
    }
}

#[test]
fn requirement_bullets_do_not_floor_to_multi_step() {
    // A task description that enumerates constraints is one task, not a
    // list of asks — it must not buy a planner pass on its own. (The model
    // may still classify it multi; the floor just stops forcing it.)
    let spec = "Build a small HTTP server:\n\
                - listens on port 8080\n\
                - responds with JSON\n\
                - logs each request to /var/log/app.log";
    assert_eq!(deterministic_floor(spec), TaskClass::SimpleLookup);
    // A list of actual instructions still raises the floor, including
    // items fronted by sequencing/politeness words.
    let asks = "Please:\n1. add a field\n2. then update the schema\n3. please write a test";
    assert_eq!(deterministic_floor(asks), TaskClass::MultiStep);
}

#[test]
fn the_judge_nudge_prefers_review_when_unsure() {
    let p = triage_prompt("anything");
    // The witness keeps its skip-nudge; the judge must not share it —
    // the judge is only ever consulted when no test settled the outcome,
    // which is exactly when review has value.
    assert!(p.contains("when unsure, say yes"));
    assert!(!p.contains("Prefer `no` for both"));
}
