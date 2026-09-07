//! Witnesses for the self-appending rung (`#5863`). Here is the growing
//! command no other rung can see. Here too is the ordinary work it must
//! never fire on.

use super::*;

/// The `grep` block the measured trial kept adding. It is cut short here so
/// a fixture stays easy to read.
const BLOCK: &str = " && grep -n display_plot a.c b.c c.c";

/// One call of the grind. The base command carries `copies` copies of the
/// block. Its answer is as long as the command that asked for it, so no two
/// answers here ever match.
fn grind(copies: usize) -> CallRecord<'static> {
    let command = format!("ls /usr/local{}", BLOCK.repeat(copies));
    let output = format!("{} matches", command.len());
    call("bash", serde_json::json!({ "command": command }), &output)
}

/// The measured run. Each call sends the last command with one more copy of
/// the block on the end.
fn grind_of(calls: usize) -> Vec<CallRecord<'static>> {
    (0..calls).map(grind).collect()
}

/// **Witness (`#5863`).** The `build-pov-ray__3DYrafv` shape, at the length
/// that trial ground for. The five older rungs are all on at their defaults,
/// and they see nothing. Each input is new, so the three repeat rungs reset.
/// Each output is new, so stagnation resets. There is no number to climb, so
/// the sweep rung has nothing to read.
#[test]
fn a_command_that_re_sends_itself_with_more_added_is_a_loop() {
    let records = grind_of(12);
    let without_the_new_rung = LoopDetectionConfig {
        self_appending_threshold: 0,
        ..LoopDetectionConfig::default()
    };
    assert_eq!(
        detect_loop(&records, without_the_new_rung),
        LoopVerdict::NoLoop,
        "anti-vacuity: no rung before this one sees a growing command"
    );
    match detect_loop(&records, LoopDetectionConfig::default()) {
        LoopVerdict::SelfAppending { tool, base, calls } => {
            assert_eq!(tool, "bash");
            assert_eq!(calls, 12);
            assert_eq!(
                base,
                serde_json::json!({ "command": "ls /usr/local" }),
                "the base is the run's first and shortest input"
            );
        }
        other => panic!("a self-appending run must be a loop: {other:?}"),
    }
}

/// The rung reads the run and says what to do about it. So the steer the
/// driver sends names the shape, and not just "loop detected".
#[test]
fn the_evidence_names_the_growth_and_the_command_it_grew_from() {
    let evidence = detect_loop(&grind_of(12), LoopDetectionConfig::default())
        .evidence()
        .expect("a detected loop carries evidence");
    assert!(evidence.contains("`bash`"), "{evidence}");
    assert!(
        evidence.contains("re-sent the whole of the call"),
        "{evidence}"
    );
    assert!(evidence.contains("ls /usr/local"), "{evidence}");
}

/// Building a pipe over a few calls is how a model works. Each step of it
/// extends the one before. The floor is all that tells that from a grind, so
/// the short run has to come back clean.
#[test]
fn a_command_built_up_over_a_few_calls_is_not_a_loop() {
    assert_eq!(
        detect_loop(&grind_of(4), LoopDetectionConfig::default()),
        LoopVerdict::NoLoop
    );
}

/// **Property (`#5863`), first way.** Inputs that do not nest never get this
/// verdict, however long the run. A model that edits one command in place is
/// changing a flag, a path, a pattern. That is moving its arguments, and the
/// stagnation rung is the one written to judge it.
#[test]
fn a_run_whose_inputs_do_not_nest_never_self_appends() {
    let records: Vec<CallRecord<'static>> = (0..40)
        .map(|i| {
            call(
                "bash",
                serde_json::json!({ "command": format!("grep -n pattern_{i} src/main.rs") }),
                &format!("hit {i}"),
            )
        })
        .collect();
    assert!(
        !matches!(
            detect_loop(&records, LoopDetectionConfig::default()),
            LoopVerdict::SelfAppending { .. }
        ),
        "nothing here extends anything"
    );
}

/// The same way, where nesting is the tempting read. The command shrinks
/// back. So no earlier input is a prefix of a later one.
#[test]
fn a_command_that_shrinks_never_self_appends() {
    let records: Vec<CallRecord<'static>> = (0..12).rev().map(grind).collect();
    assert!(!matches!(
        detect_loop(&records, LoopDetectionConfig::default()),
        LoopVerdict::SelfAppending { .. }
    ));
}

/// **Property (`#5863`), second way.** A growing command whose answers never
/// change is a tool with nothing left to say. The stagnation rung names that,
/// and it keeps it. This rung reads no output, so a window whose output
/// repeats must never be reported as this one.
#[test]
fn a_growing_command_with_identical_outputs_stays_stagnation() {
    let records: Vec<CallRecord<'static>> = (0..12)
        .map(|copies| {
            call(
                "bash",
                serde_json::json!({ "command": format!("ls /usr/local{}", BLOCK.repeat(copies)) }),
                "no such directory",
            )
        })
        .collect();
    match detect_loop(&records, LoopDetectionConfig::default()) {
        LoopVerdict::Stagnant { tool, .. } => assert_eq!(tool, "bash"),
        other => panic!("repeated output is stagnation, not self-appending: {other:?}"),
    }
}

/// Another tool called in the middle ends the run. It ends the run of every
/// rung that reads one. The measured shape has no gap in it, and a rung that
/// let gaps through would claim more than the corpus can show.
#[test]
fn another_tool_interleaved_breaks_the_run() {
    let mut records = grind_of(11);
    records.insert(6, read("src/main.rs"));
    assert_eq!(
        detect_loop(&records, LoopDetectionConfig::default()),
        LoopVerdict::NoLoop,
        "the trailing run is five calls, below the threshold"
    );
}

/// Two commands growing out of different roots are two loops. So a model
/// steered about one still earns a warning about the other. That is the
/// `#1743` rule, read for this rung.
#[test]
fn two_commands_growing_from_different_roots_are_two_loops() {
    let ours = detect_loop(&grind_of(12), LoopDetectionConfig::default())
        .identity()
        .expect("a self-appending run has an identity");
    let theirs: Vec<CallRecord<'static>> = (0..12)
        .map(|copies| {
            call(
                "bash",
                serde_json::json!({ "command": format!("cat README.md{}", BLOCK.repeat(copies)) }),
                &format!("read {copies}"),
            )
        })
        .collect();
    let theirs = detect_loop(&theirs, LoopDetectionConfig::default())
        .identity()
        .expect("a self-appending run has an identity");
    assert_eq!(ours.same_loop_as(&ours), Some(true));
    assert_eq!(ours.same_loop_as(&theirs), Some(false));
}

/// One grind, seen two calls apart, is one loop. The ladder above charges it
/// once. It does not buy a fresh warning for each call the run grows by.
#[test]
fn one_grind_seen_twice_is_the_same_loop() {
    let earlier = detect_loop(&grind_of(12), LoopDetectionConfig::default())
        .identity()
        .expect("a self-appending run has an identity");
    let later = detect_loop(&grind_of(14), LoopDetectionConfig::default())
        .identity()
        .expect("a self-appending run has an identity");
    assert_eq!(earlier.same_loop_as(&later), Some(true));
}

/// The floor is config, as every other rung's is. A `0` or a `1` turns it
/// off.
#[test]
fn the_self_appending_threshold_is_honoured_in_both_directions() {
    let records = grind_of(6);
    assert_eq!(
        detect_loop(&records, LoopDetectionConfig::default()),
        LoopVerdict::NoLoop,
        "six calls are below the default of eight"
    );
    assert!(matches!(
        detect_loop(
            &records,
            LoopDetectionConfig {
                self_appending_threshold: 5,
                ..LoopDetectionConfig::default()
            }
        ),
        LoopVerdict::SelfAppending { .. }
    ));
    for disabling in [0, 1] {
        assert_eq!(
            detect_loop(
                &grind_of(12),
                LoopDetectionConfig {
                    self_appending_threshold: disabling,
                    ..LoopDetectionConfig::default()
                }
            ),
            LoopVerdict::NoLoop,
            "{disabling} disables the check, like every threshold beside it"
        );
    }
}

/// A call that adds a key is a different ask, not a longer one. The key set
/// has to match, or the growth does not mean what this rung says it means.
#[test]
fn a_call_that_grows_by_adding_a_key_does_not_nest() {
    let records: Vec<CallRecord<'static>> = (0..12)
        .map(|i| {
            let mut input = serde_json::Map::new();
            input.insert("command".into(), serde_json::json!("ls /usr/local"));
            for key in 0..i {
                input.insert(format!("extra_{key}"), serde_json::json!(key));
            }
            call(
                "bash",
                serde_json::Value::Object(input),
                &format!("run {i}"),
            )
        })
        .collect();
    assert!(!matches!(
        detect_loop(&records, LoopDetectionConfig::default()),
        LoopVerdict::SelfAppending { .. }
    ));
}
