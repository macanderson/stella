//! Witnesses for the stagnation rung's origin exemption.
//!
//! A server may ack every call the same way. That is its design. A built-in
//! that says the same thing every time is a turn that has stopped.

use proptest::prelude::*;

use super::*;

/// One call to a server that acks every success the same way. The arguments
/// differ. The bytes back do not. That is what the server was built to do.
fn mcp_ack(issue: &str) -> CallRecord<'static> {
    call(
        "mcp__tracker__create_issue",
        serde_json::json!({ "title": issue }),
        "ok",
    )
}

/// **Witness, direction one.** Six calls to an MCP tool. Six sets of
/// arguments. One ack. This was real work, and the rung killed it.
///
/// The mark is the whole difference. The same records with no mark still
/// stagnate, and the second check pins that. So this test cannot pass by
/// the fixture missing the rung.
#[test]
fn an_mcp_tool_acking_every_call_the_same_way_is_not_stagnation() {
    let unmarked: Vec<CallRecord<'static>> = (0..6)
        .map(|i| mcp_ack(&format!("flaky test {i}")))
        .collect();
    let marked: Vec<CallRecord<'static>> = unmarked
        .iter()
        .cloned()
        .map(|record| from(record, ToolOrigin::Mcp))
        .collect();

    assert_eq!(
        detect_loop(&marked, LoopDetectionConfig::default()),
        LoopVerdict::NoLoop,
        "a constant ack is the server's design, not evidence the turn stalled"
    );
    assert!(
        matches!(
            detect_loop(&unmarked, LoopDetectionConfig::default()),
            LoopVerdict::Stagnant { .. }
        ),
        "anti-vacuity: with the origin unrecorded these exact records still trip \
         the rung, so the exemption is what the first assertion measures"
    );
}

/// **Witness, direction two.** The exemption is only for arguments that
/// moved. Six *identical* calls to one MCP tool are a repeat, whatever the
/// bytes mean. Exact-repeat needs no claim about the bytes to say so.
#[test]
fn an_mcp_tool_called_identically_over_and_over_is_still_a_loop() {
    let records: Vec<CallRecord<'static>> = vec![mcp_ack("one issue"); 6]
        .into_iter()
        .map(|record| from(record, ToolOrigin::Mcp))
        .collect();

    match detect_loop(&records, LoopDetectionConfig::default()) {
        LoopVerdict::ExactRepeat { tool, count, .. } => {
            assert_eq!(tool, "mcp__tracker__create_issue");
            assert_eq!(count, 6);
        }
        other => panic!("expected ExactRepeat, got {other:?}"),
    }
}

/// **Witness, direction three.** The exemption reaches only tools from
/// outside the binary. A built-in reports what it saw. The same bytes for
/// six sets of arguments still mean the arguments reached nothing new.
#[test]
fn a_builtin_answering_every_new_argument_the_same_way_still_stagnates() {
    for origin in [None, Some(ToolOrigin::Builtin)] {
        let records: Vec<CallRecord<'static>> = (0..6)
            .map(|i| {
                let record = grep_variant(&format!("cache_write|savings_{i}"));
                match origin {
                    Some(origin) => from(record, origin),
                    None => record,
                }
            })
            .collect();

        match detect_loop(&records, LoopDetectionConfig::default()) {
            LoopVerdict::Stagnant { tool, count } => {
                assert_eq!(tool, "grep");
                assert_eq!(count, 6, "origin {origin:?}");
            }
            other => panic!("expected Stagnant for origin {origin:?}, got {other:?}"),
        }
    }
}

/// A script that prints one line on success is the MCP ack one byte away.
/// The stamp a silent success gets fires on *empty* stdout only. So
/// `echo done` gives the same bytes every call.
#[test]
fn a_custom_tool_printing_a_constant_line_is_not_stagnation() {
    let records: Vec<CallRecord<'static>> = (0..8)
        .map(|i| {
            from(
                call(
                    "deploy_preview",
                    serde_json::json!({ "branch": format!("topic-{i}") }),
                    "done",
                ),
                ToolOrigin::Custom,
            )
        })
        .collect();

    assert_eq!(
        detect_loop(&records, LoopDetectionConfig::default()),
        LoopVerdict::NoLoop
    );
}

proptest! {
    /// Property: a history of records from outside the binary never
    /// reaches `Stagnant`. Not for any arguments, outputs or thresholds.
    ///
    /// The rung rests on one claim: the same bytes mean nothing was
    /// learned. That claim is false for a tool that acks by design. So the
    /// verdict must never be reached for one.
    ///
    /// The other rungs stay armed. An exact repeat of a server call is
    /// still a loop, and this says nothing against that.
    #[test]
    fn a_tool_from_outside_the_binary_never_stagnates(
        records in proptest::collection::vec(arb_call_record(), 0..16),
        exact_repeat_threshold in 0usize..8,
        short_cycle_repeats in 0usize..8,
        stagnation_threshold in 0usize..8,
        interleaved_repeat_threshold in 0usize..8,
        monotonic_sweep_threshold in 0usize..8,
        custom in proptest::bool::ANY,
    ) {
        let origin = if custom { ToolOrigin::Custom } else { ToolOrigin::Mcp };
        let records: Vec<CallRecord<'static>> = records
            .into_iter()
            .map(|record| from(record, origin))
            .collect();
        let config = LoopDetectionConfig { exact_repeat_threshold, short_cycle_repeats, stagnation_threshold, interleaved_repeat_threshold, monotonic_sweep_threshold, stall_steer_threshold_secs: 0 };
        prop_assert!(!matches!(detect_loop(&records, config), LoopVerdict::Stagnant { .. }));
    }

    /// Property: marking every record `Builtin` changes no verdict. The
    /// rung always assumed as much, so writing it down must change nothing.
    ///
    /// A record with no origin set behaves the same way. A caller that does
    /// not stamp yet keeps the detector it has today.
    #[test]
    fn marking_a_history_builtin_changes_nothing(
        records in proptest::collection::vec(arb_call_record(), 0..16),
        exact_repeat_threshold in 0usize..8,
        short_cycle_repeats in 0usize..8,
        stagnation_threshold in 0usize..8,
        interleaved_repeat_threshold in 0usize..8,
        monotonic_sweep_threshold in 0usize..8,
    ) {
        let marked: Vec<CallRecord<'static>> = records
            .iter()
            .cloned()
            .map(|record| from(record, ToolOrigin::Builtin))
            .collect();
        let config = LoopDetectionConfig { exact_repeat_threshold, short_cycle_repeats, stagnation_threshold, interleaved_repeat_threshold, monotonic_sweep_threshold, stall_steer_threshold_secs: 0 };
        prop_assert_eq!(detect_loop(&marked, config), detect_loop(&records, config));
    }
}
