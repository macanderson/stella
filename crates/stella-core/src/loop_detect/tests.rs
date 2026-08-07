use proptest::prelude::*;

use super::*;

fn record(name: &str, input: serde_json::Value, output: Option<ToolOutput>) -> CallRecord<'static> {
    // Distinct `call_id` per invocation, on purpose — providers never
    // reuse call ids, and the detector must not depend on them.
    static NEXT_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    CallRecord {
        call: Cow::Owned(ToolCall {
            call_id: format!("call_{id}"),
            name: name.into(),
            input,
        }),
        output: output.map(Cow::Owned),
        // No snapshot: these fixtures exercise the raw-output
        // comparison. The identity path has its own witness in
        // `driver::tests::audit_fixes`.
        identity: None,
    }
}

/// The same record with a caller-supplied output identity attached —
/// the shape the driver builds once it has a pre-compaction snapshot.
fn with_identity(mut record: CallRecord<'static>, identity: &str) -> CallRecord<'static> {
    record.identity = Some(identity.to_string());
    record
}

fn call(name: &str, input: serde_json::Value, output: &str) -> CallRecord<'static> {
    record(
        name,
        input,
        Some(ToolOutput::Ok {
            content: output.into(),
        }),
    )
}

/// Re-reading an unchanged file: same input, same output — the classic
/// no-progress ingredient.
fn read(path: &str) -> CallRecord<'static> {
    call(
        "read_file",
        serde_json::json!({ "path": path }),
        "fn main() {}",
    )
}

/// An edit that keeps failing the same way: same input, same error.
fn edit(path: &str) -> CallRecord<'static> {
    record(
        "edit_file",
        serde_json::json!({ "path": path, "old": "x", "new": "y" }),
        Some(ToolOutput::Error {
            message: "old text not found".into(),
        }),
    )
}

#[test]
fn empty_history_is_never_a_loop() {
    let verdict = detect_loop(&[], LoopDetectionConfig::default());
    assert_eq!(verdict, LoopVerdict::NoLoop);
    assert!(!verdict.is_loop());
}

#[test]
fn single_call_is_never_a_loop() {
    let records = vec![read("a.rs")];
    assert_eq!(
        detect_loop(&records, LoopDetectionConfig::default()),
        LoopVerdict::NoLoop
    );
}

#[test]
fn history_shorter_than_exact_repeat_threshold_is_not_a_loop() {
    // Two identical calls, but the threshold requires three.
    let records = vec![read("a.rs"), read("a.rs")];
    let config = LoopDetectionConfig {
        exact_repeat_threshold: 3,
        short_cycle_repeats: 100, // disable short-cycle for this test
        stagnation_threshold: 0,
        // Disabled: this case isolates another detector (#1851).
        interleaved_repeat_threshold: 0, // disabled: this test isolates another check
    };
    assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
}

#[test]
fn exact_repeat_at_threshold_is_detected() {
    let records = vec![read("a.rs"), read("a.rs"), read("a.rs")];
    let config = LoopDetectionConfig {
        exact_repeat_threshold: 3,
        short_cycle_repeats: 100,
        stagnation_threshold: 0,
        // Disabled: this case isolates another detector (#1851).
        interleaved_repeat_threshold: 0, // disabled: this test isolates another check
    };
    let verdict = detect_loop(&records, config);
    assert_eq!(
        verdict,
        LoopVerdict::ExactRepeat {
            tool: "read_file".into(),
            input: serde_json::json!({ "path": "a.rs" }),
            count: 3,
        }
    );
    assert!(verdict.is_loop());
}

#[test]
fn exact_repeat_above_threshold_reports_full_count() {
    // Five in a row, threshold 3 — the full count is reported, not
    // capped at the threshold.
    let records = vec![read("a.rs"); 5];
    let config = LoopDetectionConfig {
        exact_repeat_threshold: 3,
        short_cycle_repeats: 100,
        stagnation_threshold: 0,
        // Disabled: this case isolates another detector (#1851).
        interleaved_repeat_threshold: 0, // disabled: this test isolates another check
    };
    match detect_loop(&records, config) {
        LoopVerdict::ExactRepeat { count, .. } => assert_eq!(count, 5),
        other => panic!("expected ExactRepeat, got {other:?}"),
    }
}

#[test]
fn identical_input_with_changing_output_is_not_a_loop() {
    // Polling a still-running process: `read_output` on the same
    // handle, no cursor field — the input is byte-identical every
    // time, but each poll returns new bytes. Visible progress, never a
    // loop, however long it goes on.
    let records: Vec<CallRecord<'static>> = (0..10)
        .map(|i| {
            call(
                "read_output",
                serde_json::json!({ "handle": "proc-5" }),
                &format!("[{i}s] compiling stella-core..."),
            )
        })
        .collect();
    assert_eq!(
        detect_loop(&records, LoopDetectionConfig::default()),
        LoopVerdict::NoLoop
    );
}

#[test]
fn identical_input_with_unresolved_output_is_not_a_loop() {
    // No result observed (the result message is gone from the window,
    // or the call never ran): progress cannot be ruled out, so
    // repetition alone is not evidence.
    let records: Vec<CallRecord<'static>> = (0..5)
        .map(|_| record("read_file", serde_json::json!({ "path": "a.rs" }), None))
        .collect();
    assert_eq!(
        detect_loop(&records, LoopDetectionConfig::default()),
        LoopVerdict::NoLoop
    );
}

#[test]
fn different_arguments_to_the_same_tool_is_not_a_loop() {
    // Same tool name every time, but a different path each call — must
    // compare full input, not just tool name.
    let records = vec![read("a.rs"), read("b.rs"), read("c.rs"), read("d.rs")];
    let config = LoopDetectionConfig {
        exact_repeat_threshold: 3,
        short_cycle_repeats: 100,
        stagnation_threshold: 0,
        // Disabled: this case isolates another detector (#1851).
        interleaved_repeat_threshold: 0, // disabled: this test isolates another check
    };
    assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
}

#[test]
fn call_id_is_ignored_when_comparing_calls() {
    // `record()` assigns a fresh call_id every time; ToolCall's derived
    // PartialEq would see these as all-different. The detector must
    // still catch the repeat.
    let records: Vec<CallRecord<'static>> = (0..3).map(|_| read("a.rs")).collect();
    let ids: std::collections::HashSet<_> =
        records.iter().map(|r| r.call.call_id.clone()).collect();
    assert_eq!(ids.len(), 3, "test fixture sanity: call_ids must differ");

    let config = LoopDetectionConfig {
        exact_repeat_threshold: 3,
        short_cycle_repeats: 100,
        stagnation_threshold: 0,
        // Disabled: this case isolates another detector (#1851).
        interleaved_repeat_threshold: 0, // disabled: this test isolates another check
    };
    assert!(detect_loop(&records, config).is_loop());
}

#[test]
fn matching_identities_outrank_outputs_rewritten_since() {
    // The #554 shape: the two older results were stubbed in place by
    // compaction, so their live outputs no longer match the newest —
    // but all three carry the same pre-compaction identity.
    let stub = |path: &str| {
        with_identity(
            call(
                "read_file",
                serde_json::json!({ "path": path }),
                "[evicted]",
            ),
            "blk_same",
        )
    };
    let records = vec![
        stub("a.rs"),
        stub("a.rs"),
        with_identity(read("a.rs"), "blk_same"),
    ];
    let config = LoopDetectionConfig {
        exact_repeat_threshold: 3,
        short_cycle_repeats: 100,
        stagnation_threshold: 0,
        // Disabled: this case isolates another detector (#1851).
        interleaved_repeat_threshold: 0, // disabled: this test isolates another check
    };
    assert!(
        detect_loop(&records, config).is_loop(),
        "identical identities must count as a repeat even when the live outputs differ"
    );
}

#[test]
fn differing_identities_outrank_outputs_collapsed_since() {
    // The mirror image: three DIFFERENT outputs all rewritten to the
    // same stub. Comparing bytes alone would now call this a loop; the
    // identities prove each call produced different information.
    let stub = |identity: &str| {
        with_identity(
            call(
                "read_file",
                serde_json::json!({ "path": "a.rs" }),
                "[evicted]",
            ),
            identity,
        )
    };
    let records = vec![stub("blk_1"), stub("blk_2"), stub("blk_3")];
    let config = LoopDetectionConfig {
        exact_repeat_threshold: 3,
        short_cycle_repeats: 100,
        stagnation_threshold: 0,
        // Disabled: this case isolates another detector (#1851).
        interleaved_repeat_threshold: 0, // disabled: this test isolates another check
    };
    assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
}

#[test]
fn an_identity_never_resurrects_an_unresolved_output() {
    // An identity is evidence ABOUT an observed output, never a
    // substitute for observing one: `None` still matches nothing.
    let unresolved = || {
        with_identity(
            record("read_file", serde_json::json!({ "path": "a.rs" }), None),
            "blk_same",
        )
    };
    let records = vec![unresolved(), unresolved(), unresolved()];
    let config = LoopDetectionConfig {
        exact_repeat_threshold: 3,
        short_cycle_repeats: 100,
        stagnation_threshold: 0,
        // Disabled: this case isolates another detector (#1851).
        interleaved_repeat_threshold: 0, // disabled: this test isolates another check
    };
    assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
}

#[test]
fn one_sided_identity_falls_back_to_comparing_outputs() {
    // Only some records were snapshotted (a result that entered the
    // window after the snapshot). Comparison degrades to the outputs
    // rather than silently failing to match.
    let records = vec![
        with_identity(read("a.rs"), "blk_same"),
        read("a.rs"),
        read("a.rs"),
    ];
    let config = LoopDetectionConfig {
        exact_repeat_threshold: 3,
        short_cycle_repeats: 100,
        stagnation_threshold: 0,
        // Disabled: this case isolates another detector (#1851).
        interleaved_repeat_threshold: 0, // disabled: this test isolates another check
    };
    assert!(detect_loop(&records, config).is_loop());
}

#[test]
fn short_cycle_below_threshold_is_not_a_loop() {
    // Only two full A-B cycles; threshold requires three.
    let records = vec![read("a.rs"), edit("a.rs"), read("a.rs"), edit("a.rs")];
    let config = LoopDetectionConfig {
        exact_repeat_threshold: 100, // disable exact-repeat for this test
        short_cycle_repeats: 3,
        stagnation_threshold: 0,
        // Disabled: this case isolates another detector (#1851).
        interleaved_repeat_threshold: 0, // disabled: this test isolates another check
    };
    assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
}

#[test]
fn short_cycle_at_threshold_is_detected() {
    // read, edit, read, edit, read, edit — 3 full cycles, the "read
    // file / edit rejected or no-op / read again" failure mode.
    let records = vec![
        read("a.rs"),
        edit("a.rs"),
        read("a.rs"),
        edit("a.rs"),
        read("a.rs"),
        edit("a.rs"),
    ];
    let config = LoopDetectionConfig {
        exact_repeat_threshold: 100,
        short_cycle_repeats: 3,
        stagnation_threshold: 0,
        // Disabled: this case isolates another detector (#1851).
        interleaved_repeat_threshold: 0, // disabled: this test isolates another check
    };
    let verdict = detect_loop(&records, config);
    match &verdict {
        LoopVerdict::ShortCycle { pattern, repeats } => {
            assert_eq!(*repeats, 3);
            let names: Vec<&str> = pattern.iter().map(|c| c.name.as_str()).collect();
            assert_eq!(names, ["read_file", "edit_file"]);
        }
        other => panic!("expected ShortCycle, got {other:?}"),
    }
    assert!(verdict.is_loop());
}

#[test]
fn short_cycle_above_threshold_reports_full_repeat_count() {
    let mut records = Vec::new();
    for _ in 0..5 {
        records.push(read("a.rs"));
        records.push(edit("a.rs"));
    }
    let config = LoopDetectionConfig {
        exact_repeat_threshold: 100,
        short_cycle_repeats: 3,
        stagnation_threshold: 0,
        // Disabled: this case isolates another detector (#1851).
        interleaved_repeat_threshold: 0, // disabled: this test isolates another check
    };
    match detect_loop(&records, config) {
        LoopVerdict::ShortCycle { repeats, .. } => assert_eq!(repeats, 5),
        other => panic!("expected ShortCycle, got {other:?}"),
    }
}

#[test]
fn two_distinct_calls_alternating_with_changing_outputs_is_not_a_loop() {
    // The "correct" polling alternation: read_output / bash sleep,
    // read_output / bash sleep — identical inputs at every position,
    // but each poll returns new bytes. Progress, not a cycle.
    let mut records = Vec::new();
    for i in 0..6 {
        records.push(call(
            "read_output",
            serde_json::json!({ "handle": "proc-5" }),
            &format!("[{i}s] compiling..."),
        ));
        records.push(call("bash", serde_json::json!({ "cmd": "sleep 5" }), ""));
    }
    assert_eq!(
        detect_loop(&records, LoopDetectionConfig::default()),
        LoopVerdict::NoLoop
    );
}

#[test]
fn period_three_cycle_with_identical_outputs_is_detected() {
    // The most common real stuck signature: read → failing edit →
    // failing test, over and over, nothing changing.
    let cycle = || {
        vec![
            read("a.rs"),
            edit("a.rs"),
            call(
                "bash",
                serde_json::json!({ "cmd": "cargo test" }),
                "2 failed",
            ),
        ]
    };
    let records: Vec<CallRecord<'static>> = (0..3).flat_map(|_| cycle()).collect();
    let config = LoopDetectionConfig {
        exact_repeat_threshold: 100,
        short_cycle_repeats: 3,
        stagnation_threshold: 0,
        // Disabled: this case isolates another detector (#1851).
        interleaved_repeat_threshold: 0, // disabled: this test isolates another check
    };
    let verdict = detect_loop(&records, config);
    match &verdict {
        LoopVerdict::ShortCycle { pattern, repeats } => {
            assert_eq!(*repeats, 3);
            let names: Vec<&str> = pattern.iter().map(|c| c.name.as_str()).collect();
            assert_eq!(names, ["read_file", "edit_file", "bash"]);
        }
        other => panic!("expected a period-3 ShortCycle, got {other:?}"),
    }
}

#[test]
fn period_three_cycle_with_differing_outputs_is_not_a_loop() {
    // The same A, B, C shape — but the test output improves every
    // cycle (2 failed → 1 failed → 0 failed). That is a productive
    // fix loop, not a stuck one.
    let cycle = |failures: usize| {
        vec![
            read("a.rs"),
            edit("a.rs"),
            call(
                "bash",
                serde_json::json!({ "cmd": "cargo test" }),
                &format!("{failures} failed"),
            ),
        ]
    };
    let records: Vec<CallRecord<'static>> = (0..3).rev().flat_map(cycle).collect();
    let config = LoopDetectionConfig {
        exact_repeat_threshold: 100,
        short_cycle_repeats: 2,
        stagnation_threshold: 0,
        // Disabled: this case isolates another detector (#1851).
        interleaved_repeat_threshold: 0, // disabled: this test isolates another check
    };
    assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
}

#[test]
fn period_four_cycle_with_identical_outputs_is_detected() {
    let cycle = || {
        vec![
            read("a.rs"),
            read("b.rs"),
            edit("a.rs"),
            call(
                "bash",
                serde_json::json!({ "cmd": "cargo test" }),
                "2 failed",
            ),
        ]
    };
    let records: Vec<CallRecord<'static>> = (0..2).flat_map(|_| cycle()).collect();
    let config = LoopDetectionConfig {
        exact_repeat_threshold: 100,
        short_cycle_repeats: 2,
        stagnation_threshold: 0,
        // Disabled: this case isolates another detector (#1851).
        interleaved_repeat_threshold: 0, // disabled: this test isolates another check
    };
    match detect_loop(&records, config) {
        LoopVerdict::ShortCycle { pattern, repeats } => {
            assert_eq!(repeats, 2);
            assert_eq!(pattern.len(), 4);
        }
        other => panic!("expected a period-4 ShortCycle, got {other:?}"),
    }
}

#[test]
fn period_five_cycle_is_beyond_the_detector() {
    // Five distinct calls repeating: outside MAX_CYCLE_PERIOD, left to
    // the step cap. Pins the k <= 4 bound.
    let cycle = || {
        vec![
            read("a.rs"),
            read("b.rs"),
            read("c.rs"),
            edit("a.rs"),
            call(
                "bash",
                serde_json::json!({ "cmd": "cargo test" }),
                "2 failed",
            ),
        ]
    };
    let records: Vec<CallRecord<'static>> = (0..3).flat_map(|_| cycle()).collect();
    let config = LoopDetectionConfig {
        exact_repeat_threshold: 100,
        short_cycle_repeats: 2,
        stagnation_threshold: 0,
        // Disabled: this case isolates another detector (#1851).
        interleaved_repeat_threshold: 0, // disabled: this test isolates another check
    };
    assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
}

#[test]
fn identical_calls_repeated_are_not_misreported_as_a_short_cycle() {
    // Six identical calls: exact-repeat detection is disabled here so
    // we can prove detect_short_cycle's own all-same guard holds at
    // every period — this must stay NoLoop, not a degenerate
    // ShortCycle{X, X}.
    let records = vec![read("a.rs"); 6];
    let config = LoopDetectionConfig {
        exact_repeat_threshold: 0, // disabled
        short_cycle_repeats: 1,
        stagnation_threshold: 0,
        // Disabled: this case isolates another detector (#1851).
        interleaved_repeat_threshold: 0, // disabled: this test isolates another check
    };
    assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
}

#[test]
fn drift_attributed_edit_recovery_is_not_a_loop() {
    // #331: when `edit_file` attributes a match failure to an
    // out-of-band file change, the error embeds the CURRENT content —
    // so as long as the file keeps changing, the outputs differ and the
    // legitimate read→edit-retry recovery is progress by construction.
    // Only when the file stops changing (and the model keeps replaying
    // the same failing edit verbatim) do outputs repeat byte-identically
    // and detection rightly fires. Pins the contract that the drift echo
    // is what keeps recovery progress-eligible.
    let drift_fail = |content: &str| {
        record(
            "edit_file",
            serde_json::json!({ "path": "a.rs", "old": "x", "new": "y" }),
            Some(ToolOutput::Error {
                message: format!("old_string not found — the file CHANGED; current: {content}"),
            }),
        )
    };
    let read_v = |content: &str| call("read_file", serde_json::json!({ "path": "a.rs" }), content);
    let records = vec![
        read_v("v1"),
        drift_fail("v2"),
        read_v("v2"),
        drift_fail("v3"),
        read_v("v3"),
        drift_fail("v4"),
    ];
    assert_eq!(
        detect_loop(&records, LoopDetectionConfig::default()),
        LoopVerdict::NoLoop
    );
}

#[test]
fn healthy_varied_sequence_is_not_a_loop() {
    // A realistic productive trajectory: no false positives.
    let records = vec![
        read("src/lib.rs"),
        call(
            "grep",
            serde_json::json!({ "pattern": "fn run_turn" }),
            "src/agent.rs:42",
        ),
        read("src/agent.rs"),
        edit("src/agent.rs"),
        call(
            "bash",
            serde_json::json!({ "cmd": "cargo test -p stella-cli" }),
            "2 failed",
        ),
        read("src/agent.rs"),
        edit("src/agent.rs"),
        call(
            "bash",
            serde_json::json!({ "cmd": "cargo test -p stella-cli -- --nocapture" }),
            "1 failed",
        ),
    ];
    assert_eq!(
        detect_loop(&records, LoopDetectionConfig::default()),
        LoopVerdict::NoLoop
    );
}

#[test]
fn exact_repeat_takes_precedence_over_a_would_be_cycle_read() {
    // The trailing three calls are an exact repeat; that a short-cycle
    // check might also find something (given a different config) is
    // irrelevant — exact-repeat is checked first and wins.
    let records = vec![edit("a.rs"), read("a.rs"), read("a.rs"), read("a.rs")];
    let config = LoopDetectionConfig::default(); // 3/3
    match detect_loop(&records, config) {
        LoopVerdict::ExactRepeat { count, tool, .. } => {
            assert_eq!(count, 3);
            assert_eq!(tool, "read_file");
        }
        other => panic!("expected ExactRepeat to win, got {other:?}"),
    }
}

#[test]
fn zero_or_one_exact_repeat_threshold_disables_that_check() {
    let records = vec![read("a.rs"); 10];
    for threshold in [0, 1] {
        let config = LoopDetectionConfig {
            exact_repeat_threshold: threshold,
            short_cycle_repeats: 0, // also disabled, so overall NoLoop
            stagnation_threshold: 0,
            // Disabled: this case isolates another detector (#1851).
            interleaved_repeat_threshold: 0, // ditto
        };
        assert_eq!(
            detect_loop(&records, config),
            LoopVerdict::NoLoop,
            "threshold {threshold} should disable exact-repeat detection"
        );
    }
}

#[test]
fn zero_short_cycle_repeats_disables_that_check() {
    let records = vec![
        read("a.rs"),
        edit("a.rs"),
        read("a.rs"),
        edit("a.rs"),
        read("a.rs"),
        edit("a.rs"),
    ];
    let config = LoopDetectionConfig {
        exact_repeat_threshold: 0,
        short_cycle_repeats: 0,
        stagnation_threshold: 0,
        // Disabled: this case isolates another detector (#1851).
        interleaved_repeat_threshold: 0, // disabled: this test isolates another check
    };
    assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
}

#[test]
fn pathological_thresholds_do_not_overflow_the_cycle_arithmetic() {
    // The "never panics on any input" contract covers the config too: a
    // usize::MAX threshold must saturate, not overflow the
    // `period * repeats_threshold` multiply in a debug build.
    let records = vec![read("a.rs"); 8];
    let config = LoopDetectionConfig {
        exact_repeat_threshold: usize::MAX,
        short_cycle_repeats: usize::MAX,
        stagnation_threshold: 0,
        // Disabled: this case isolates another detector (#1851).
        interleaved_repeat_threshold: 0, // disabled: this test isolates another check
    };
    assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
}

#[test]
fn evidence_is_none_for_no_loop() {
    assert_eq!(LoopVerdict::NoLoop.evidence(), None);
}

#[test]
fn evidence_describes_exact_repeat() {
    let verdict = LoopVerdict::ExactRepeat {
        tool: "read_file".into(),
        input: serde_json::json!({ "path": "a.rs" }),
        count: 4,
    };
    let evidence = verdict.evidence().expect("loop verdict has evidence");
    assert!(evidence.contains("read_file"));
    assert!(evidence.contains('4'));
}

/// An `edit_file` loop's input carries whole old/new file chunks, and this
/// string lands in the TUI, the journal, the store and the abort reason
/// (#1744). Bounded rather than "reasonable": the input is model-produced
/// runtime data with no size a caller controls.
#[test]
fn evidence_bounds_a_multi_kilobyte_edit_file_input() {
    let chunk = "fn f() { let x = 1; }\n".repeat(400);
    assert!(chunk.len() > 8_000, "fixture must be genuinely large");
    let verdict = LoopVerdict::ExactRepeat {
        tool: "edit_file".into(),
        input: serde_json::json!({ "path": "a.rs", "old": chunk, "new": "" }),
        count: 3,
    };
    let evidence = verdict.evidence().expect("loop verdict has evidence");

    // The prose around the input is fixed-size, so the whole sentence is
    // bounded once the input is. The slack covers that prose.
    assert!(
        evidence.chars().count() < MAX_DESCRIBED_INPUT + 200,
        "evidence grew with the input ({} chars)",
        evidence.chars().count()
    );
    assert!(
        evidence.contains('…'),
        "the drop must be marked: {evidence}"
    );
    // Still identifies the loop — a bound that erased the useful half
    // would trade one defect for another.
    assert!(evidence.contains("edit_file"));
    assert!(evidence.contains('3'));
}

/// The common case must read exactly as it did before the bound existed: a
/// `bash` loop is a one-line command, and truncating it would make the
/// reason line worse to fix a problem it never had.
#[test]
fn evidence_leaves_a_short_bash_command_intact() {
    let verdict = LoopVerdict::ExactRepeat {
        tool: "bash".into(),
        input: serde_json::json!({ "command": "cargo test -p stella-core" }),
        count: 4,
    };
    let evidence = verdict.evidence().expect("loop verdict has evidence");
    assert!(
        evidence.contains("cargo test -p stella-core"),
        "the whole command must survive: {evidence}"
    );
    assert!(!evidence.contains('…'), "nothing was dropped: {evidence}");
}

/// The input is model-produced UTF-8, so the cap must land on a char
/// boundary. A byte slice at 160 inside a multi-byte char panics — and
/// this path runs while the turn is already aborting.
#[test]
fn evidence_truncates_multi_byte_input_without_panicking() {
    // Every char is 4 bytes, so a byte-indexed cut at MAX_DESCRIBED_INPUT
    // lands mid-char.
    let verdict = LoopVerdict::ExactRepeat {
        tool: "write_file".into(),
        input: serde_json::json!({ "text": "𝄞".repeat(500) }),
        count: 3,
    };
    let evidence = verdict.evidence().expect("loop verdict has evidence");
    assert!(evidence.chars().count() < MAX_DESCRIBED_INPUT + 200);
    assert!(evidence.contains('…'));
}

#[test]
fn evidence_describes_short_cycle() {
    let verdict = LoopVerdict::ShortCycle {
        pattern: vec![
            read("a.rs").call.into_owned(),
            edit("a.rs").call.into_owned(),
        ],
        repeats: 3,
    };
    let evidence = verdict.evidence().expect("loop verdict has evidence");
    assert!(evidence.contains("read_file"));
    assert!(evidence.contains("edit_file"));
    assert!(evidence.contains('3'));
}

// ---- stagnation: same tool, moving arguments, unmoving answer --------

/// One `grep` whose pattern differs from every other, over one file,
/// always finding the same thing — the field shape from the journal of
/// the turn that motivated this check.
fn grep_variant(pattern: &str) -> CallRecord<'static> {
    call(
        "grep",
        serde_json::json!({ "pattern": pattern, "path": "stella-tui/src/deck.rs" }),
        "118:    /// Cumulative prompt-cache *write* tokens",
    )
}

/// THE regression. A model reshuffling one regex alternation gets a
/// different `input` every call and byte-identical output every call.
/// Neither exact-repeat (inputs never match) nor short-cycle (the
/// arguments never settle into a period) can see it; before stagnation
/// existed this ran 38 calls deep on a live turn.
#[test]
fn a_tool_whose_arguments_move_but_whose_answer_never_does_is_a_loop() {
    let records: Vec<CallRecord<'static>> = (0..6)
        .map(|i| grep_variant(&format!("cache_write|total_cache|savings_{i}")))
        .collect();
    let verdict = detect_loop(&records, LoopDetectionConfig::default());
    match &verdict {
        LoopVerdict::Stagnant { tool, count } => {
            assert_eq!(tool, "grep");
            assert_eq!(*count, 6);
        }
        other => panic!("expected Stagnant, got {other:?}"),
    }
    assert!(verdict.is_loop());
    let evidence = verdict.evidence().expect("stagnation has evidence");
    assert!(evidence.contains("grep"), "evidence names the tool");
    assert!(
        evidence.contains("DIFFERENT arguments"),
        "evidence must say why this is not an exact repeat: {evidence}"
    );
}

#[test]
fn stagnation_below_the_threshold_is_not_a_loop() {
    // Five is exploration; the default only speaks at six.
    let records: Vec<CallRecord<'static>> = (0..5)
        .map(|i| grep_variant(&format!("pattern_{i}")))
        .collect();
    assert_eq!(
        detect_loop(&records, LoopDetectionConfig::default()),
        LoopVerdict::NoLoop
    );
}

#[test]
fn a_tool_that_keeps_answering_differently_never_stagnates() {
    // Six searches, six different answers: the arguments moved AND the
    // answer moved. That is exactly what productive searching looks
    // like, and it must survive however long it goes on.
    let records: Vec<CallRecord<'static>> = (0..6)
        .map(|i| {
            call(
                "grep",
                serde_json::json!({ "pattern": format!("p{i}") }),
                &format!("src/lib.rs:{i}: hit"),
            )
        })
        .collect();
    assert_eq!(
        detect_loop(&records, LoopDetectionConfig::default()),
        LoopVerdict::NoLoop
    );
}

#[test]
fn another_tool_interleaved_breaks_the_stagnation_run() {
    // A model that greps, reads what it found, then greps again is
    // acting on the results. Only an UNBROKEN run of one tool telling
    // us nothing counts, so the run is measured from the tail.
    let mut records: Vec<CallRecord<'static>> =
        (0..5).map(|i| grep_variant(&format!("p{i}"))).collect();
    records.push(read("src/deck.rs"));
    records.extend((5..9).map(|i| grep_variant(&format!("p{i}"))));
    assert_eq!(
        detect_loop(&records, LoopDetectionConfig::default()),
        LoopVerdict::NoLoop,
        "the trailing grep run is 4 — the read reset it"
    );
}

#[test]
fn identical_calls_are_reported_as_an_exact_repeat_not_stagnation() {
    // Stagnation is the vaguer description of the same evidence, so it
    // must never claim a run exact-repeat describes precisely.
    let records = vec![grep_variant("same"); 8];
    match detect_loop(&records, LoopDetectionConfig::default()) {
        LoopVerdict::ExactRepeat { count, .. } => assert_eq!(count, 8),
        other => panic!("expected ExactRepeat to win, got {other:?}"),
    }
    // And with exact-repeat disabled it still refuses to relabel them:
    // the all-same-input guard holds on its own, not by ordering luck.
    let config = LoopDetectionConfig {
        exact_repeat_threshold: 0,
        short_cycle_repeats: 0,
        stagnation_threshold: 3,
        // Disabled: this case isolates another detector (#1851).
        interleaved_repeat_threshold: 0,
    };
    assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
}

#[test]
fn stagnation_needs_observed_outputs_like_every_other_check() {
    // Unresolved results prove nothing, however many there are.
    let records: Vec<CallRecord<'static>> = (0..8)
        .map(|i| record("grep", serde_json::json!({ "pattern": i }), None))
        .collect();
    assert_eq!(
        detect_loop(&records, LoopDetectionConfig::default()),
        LoopVerdict::NoLoop
    );
}

#[test]
fn zero_or_one_stagnation_threshold_disables_that_check() {
    let records: Vec<CallRecord<'static>> =
        (0..10).map(|i| grep_variant(&format!("p{i}"))).collect();
    for threshold in [0, 1] {
        let config = LoopDetectionConfig {
            exact_repeat_threshold: 0,
            short_cycle_repeats: 0,
            stagnation_threshold: threshold,
            // Disabled: this case isolates another detector (#1851).
            interleaved_repeat_threshold: 0,
        };
        assert_eq!(
            detect_loop(&records, config),
            LoopVerdict::NoLoop,
            "threshold {threshold} should disable stagnation detection"
        );
    }
}

#[test]
fn stagnation_compares_identities_when_compaction_rewrote_the_outputs() {
    // The #554 rule applies here too: the older results were stubbed in
    // place, but their pre-compaction identities still match, so the run
    // is still evidence.
    let records: Vec<CallRecord<'static>> = (0..6)
        .map(|i| {
            let stubbed = i < 4;
            with_identity(
                call(
                    "grep",
                    serde_json::json!({ "pattern": format!("p{i}") }),
                    if stubbed { "[evicted]" } else { "118: hit" },
                ),
                "blk_same",
            )
        })
        .collect();
    assert!(
        detect_loop(&records, LoopDetectionConfig::default()).is_loop(),
        "matching identities must survive an in-place rewrite here too"
    );
}

/// Small, deliberately overlapping alphabet of names/inputs/outputs so
/// property-test runs actually exercise repeats and cycles instead of
/// almost always generating trivially-varied (and thus trivially
/// NoLoop) sequences. Includes unresolved (`None`) outputs.
fn arb_call_record() -> impl Strategy<Value = CallRecord<'static>> {
    (0..3usize, 0..2usize, 0..3usize).prop_map(|(name_idx, input_idx, output_idx)| {
        let names = ["read_file", "edit_file", "bash"];
        let inputs = [
            serde_json::json!({ "path": "a.rs" }),
            serde_json::json!({ "path": "b.rs" }),
        ];
        let outputs = [
            Some(ToolOutput::Ok {
                content: "ok".into(),
            }),
            Some(ToolOutput::Error {
                message: "boom".into(),
            }),
            None,
        ];
        record(
            names[name_idx],
            inputs[input_idx].clone(),
            outputs[output_idx].clone(),
        )
    })
}

proptest! {
    /// Property: `detect_loop` never panics or indexes out of bounds,
    /// for any history length and any threshold configuration
    /// (including the degenerate `0` thresholds) — required by the
    /// "runs on live untrusted model output" quality bar.
    #[test]
    fn detect_loop_never_panics(
        records in proptest::collection::vec(arb_call_record(), 0..16),
        exact_repeat_threshold in 0usize..8,
        short_cycle_repeats in 0usize..8,
        stagnation_threshold in 0usize..8,
    ) {
        let config = LoopDetectionConfig { exact_repeat_threshold, short_cycle_repeats, stagnation_threshold, interleaved_repeat_threshold: 0 };
        let verdict = detect_loop(&records, config);
        // Whatever the verdict, `is_loop`/`evidence` must not panic either.
        let _ = verdict.is_loop();
        let _ = verdict.evidence();
    }

    /// Property: history shorter than EVERY threshold is always
    /// `NoLoop` — there's no way for any check to have enough evidence
    /// (the shortest cycle period is 2).
    #[test]
    fn short_history_is_always_no_loop(
        records in proptest::collection::vec(arb_call_record(), 0..12),
        exact_repeat_threshold in 2usize..8,
        short_cycle_repeats in 1usize..8,
        stagnation_threshold in 2usize..8,
    ) {
        if records.len() < exact_repeat_threshold
            && records.len() < 2 * short_cycle_repeats
            && records.len() < stagnation_threshold
        {
            let config = LoopDetectionConfig { exact_repeat_threshold, short_cycle_repeats, stagnation_threshold, interleaved_repeat_threshold: 0 };
            prop_assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
        }
    }

    /// Property: when every output in the history is unique, nothing
    /// is ever flagged (at meaningful thresholds — a threshold below 2
    /// is disabled or degenerate). Unique outputs = every call
    /// produced new information = progress by definition; this is the
    /// class-level guarantee that legitimate polling can never trip
    /// the detector, whatever the inputs look like.
    #[test]
    fn unique_outputs_are_never_a_loop(
        records in proptest::collection::vec(arb_call_record(), 0..16),
        exact_repeat_threshold in 2usize..8,
        short_cycle_repeats in 2usize..8,
        stagnation_threshold in 2usize..8,
    ) {
        let records: Vec<CallRecord<'static>> = records
            .into_iter()
            .enumerate()
            .map(|(i, mut record)| {
                record.output = Some(Cow::Owned(ToolOutput::Ok { content: format!("output {i}") }));
                record
            })
            .collect();
        let config = LoopDetectionConfig { exact_repeat_threshold, short_cycle_repeats, stagnation_threshold, interleaved_repeat_threshold: 0 };
        prop_assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
    }
}

// ---- interleaved repeat: the shape a contiguous scan cannot see ---------

/// **Witness (#1851).** A-B-A-B-A, where A is identical every time and B
/// varies, is a wedge that every contiguous scan misses.
///
/// This is what a real stall looks like: re-run the failing test, peek at a
/// different file, repeat. Exact-repeat's `take_while` resets at offset 1,
/// short-cycle breaks on the differing element, and stagnation's trailing run
/// ends there too — so before this rung the turn spun to `max_steps` with no
/// steer at all.
#[test]
fn an_identical_call_interleaved_with_varying_work_is_a_loop() {
    let failing = || {
        record(
            "bash",
            serde_json::json!({ "command": "cargo test" }),
            Some(ToolOutput::Error {
                message: "1 failed".into(),
            }),
        )
    };
    // Each `read` names a DIFFERENT file, so the interleaved element genuinely
    // varies — nothing here is a contiguous run of anything.
    let records = vec![failing(), read("a.rs"), failing(), read("b.rs"), failing()];

    let contiguous_only = LoopDetectionConfig {
        exact_repeat_threshold: 3,
        short_cycle_repeats: 3,
        stagnation_threshold: 6,
        interleaved_repeat_threshold: 0, // the old behaviour
    };
    assert_eq!(
        detect_loop(&records, contiguous_only),
        LoopVerdict::NoLoop,
        "precondition: every contiguous detector is blind to this"
    );

    let with_rung = LoopDetectionConfig {
        interleaved_repeat_threshold: 3,
        ..contiguous_only
    };
    match detect_loop(&records, with_rung) {
        LoopVerdict::InterleavedRepeat { tool, count, .. } => {
            assert_eq!(tool, "bash");
            assert_eq!(count, 3, "all three occurrences counted, gaps included");
        }
        other => panic!("expected an interleaved repeat, got {other:?}"),
    }
}

/// The other direction, and the one that decides whether this rung is safe to
/// enable: an interleave that is genuinely PROGRESSING must stay silent.
///
/// Same A-B-A-B-A shape, same tool, same arguments — but A's output changes
/// each round, which is exactly what "the test is now failing differently"
/// looks like. A detector that fired here would steer an agent that is
/// working.
#[test]
fn an_interleave_whose_results_keep_changing_is_not_a_loop() {
    let attempt = |n: u32| {
        record(
            "bash",
            serde_json::json!({ "command": "cargo test" }),
            Some(ToolOutput::Error {
                message: format!("{n} failed"),
            }),
        )
    };
    let records = vec![
        attempt(3),
        read("a.rs"),
        attempt(2),
        read("b.rs"),
        attempt(1),
    ];

    let config = LoopDetectionConfig {
        exact_repeat_threshold: 3,
        short_cycle_repeats: 3,
        stagnation_threshold: 6,
        interleaved_repeat_threshold: 3,
    };
    assert_eq!(
        detect_loop(&records, config),
        LoopVerdict::NoLoop,
        "shrinking failures are progress, not a loop"
    );
}

/// A contiguous run is still reported as an EXACT repeat, not as the weaker
/// gap-tolerant claim — the "tightest description of the evidence wins"
/// ordering the rest of `detect_loop` follows.
#[test]
fn a_contiguous_run_still_reports_as_an_exact_repeat() {
    let records = vec![read("a.rs"), read("a.rs"), read("a.rs")];
    let config = LoopDetectionConfig {
        exact_repeat_threshold: 3,
        short_cycle_repeats: 100,
        stagnation_threshold: 0,
        interleaved_repeat_threshold: 3,
    };
    assert!(
        matches!(
            detect_loop(&records, config),
            LoopVerdict::ExactRepeat { .. }
        ),
        "an exact repeat must not be demoted to the gap-tolerant verdict"
    );
}

/// An unresolved trailing output proves nothing, here as everywhere else: a
/// loop is only ever established by outputs that were actually observed.
#[test]
fn an_unresolved_output_never_trips_the_interleaved_rung() {
    let records = vec![
        read("a.rs"),
        read("b.rs"),
        read("a.rs"),
        read("b.rs"),
        record("read_file", serde_json::json!({ "path": "a.rs" }), None),
    ];
    let config = LoopDetectionConfig {
        exact_repeat_threshold: 0,
        short_cycle_repeats: 0,
        stagnation_threshold: 0,
        interleaved_repeat_threshold: 2,
    };
    assert_eq!(detect_loop(&records, config), LoopVerdict::NoLoop);
}

/// The identity of an interleaved repeat must equal the identity of the same
/// loop seen contiguously, or the escalation ladder would treat one wedge as
/// two and steer twice for it.
#[test]
fn the_interleaved_and_contiguous_verdicts_share_one_identity() {
    let interleaved = LoopVerdict::InterleavedRepeat {
        tool: "bash".into(),
        input: serde_json::json!({ "command": "cargo test" }),
        count: 3,
    };
    let contiguous = LoopVerdict::ExactRepeat {
        tool: "bash".into(),
        input: serde_json::json!({ "command": "cargo test" }),
        count: 3,
    };
    assert_eq!(interleaved.identity(), contiguous.identity());
}

/// The false positive this rung shipped with, and the reason
/// `window_is_progressing` exists.
///
/// Correct polling alternates `read_output` (same handle, NEW bytes each
/// time) with `bash sleep 5` (same command, empty output every time). The
/// sleep is a spacer: it repeats identically by design. Counting its
/// occurrences flagged a turn that was streaming a build log perfectly well —
/// caught by `two_distinct_calls_alternating_with_changing_outputs_is_not_a_loop`
/// on the first cut of this detector.
///
/// Asserted here on its own, with only the interleaved rung enabled, so a
/// future change cannot re-introduce it and pass by having some other
/// detector answer first.
#[test]
fn a_repeating_spacer_beside_a_streaming_poll_is_not_a_loop() {
    let mut records = Vec::new();
    for i in 0..6 {
        records.push(call(
            "read_output",
            serde_json::json!({ "handle": "proc-5" }),
            &format!("[{i}s] compiling..."),
        ));
        records.push(call("bash", serde_json::json!({ "cmd": "sleep 5" }), ""));
    }
    let only_the_new_rung = LoopDetectionConfig {
        exact_repeat_threshold: 0,
        short_cycle_repeats: 0,
        stagnation_threshold: 0,
        interleaved_repeat_threshold: 3,
    };
    assert_eq!(
        detect_loop(&records, only_the_new_rung),
        LoopVerdict::NoLoop,
        "the sleep repeats identically six times, but read_output is \
         answering differently every round — the turn is making progress"
    );
}
