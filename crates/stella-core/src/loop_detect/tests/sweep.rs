//! Witnesses for the monotonic-sweep rung (#4042) — the shape no output-keyed
//! rung can see, and the legitimate paging it must never fire on.

use super::*;

/// One paging read: a fresh page every call, so no two outputs ever match.
fn page(path: &str, offset: usize) -> CallRecord<'static> {
    call(
        "read_file",
        serde_json::json!({ "path": path, "offset": offset, "limit": 100 }),
        &format!("lines {offset}..{}", offset + 100),
    )
}

/// A sweep of `pages` pages, repeated `passes` times over the same path.
fn sweep_of(path: &str, pages: usize, passes: usize) -> Vec<CallRecord<'static>> {
    (0..passes)
        .flat_map(|_| (0..pages).map(|page_index| page(path, page_index * 100)))
        .collect()
}

/// **Witness (#4042).** A sweep that ran off the end and started again. All
/// four output-keyed rungs are enabled at their defaults and see nothing —
/// every page is a distinct output, so exact-repeat, short-cycle, stagnation
/// and interleaved-repeat all reset on every call. The fifth rung is what
/// notices, and it notices the wrap.
#[test]
fn a_wrapped_paging_sweep_is_a_loop() {
    let records = sweep_of("big.rs", 42, 2);
    let output_keyed_only = LoopDetectionConfig {
        monotonic_sweep_threshold: 0,
        ..LoopDetectionConfig::default()
    };
    assert_eq!(
        detect_loop(&records, output_keyed_only),
        LoopVerdict::NoLoop,
        "anti-vacuity: no existing rung sees a sweep, which is why this one exists"
    );
    match detect_loop(&records, LoopDetectionConfig::default()) {
        LoopVerdict::MonotonicSweep {
            tool,
            calls,
            wraps,
            target,
        } => {
            assert_eq!(tool, "read_file");
            assert_eq!(calls, 84);
            assert_eq!(wraps, 1);
            assert!(
                target.contains("big.rs") && !target.contains("4100"),
                "the target names the file and none of the offsets: {target}"
            );
        }
        other => panic!("a wrapped sweep must be a loop: {other:?}"),
    }
}

/// The counter-test that decides whether the rung is safe to ship: paging a
/// genuinely large file straight through, never going back, is exactly what a
/// model reading a long file should do — and it is far past the call
/// threshold, so only the wrap can be separating the two.
#[test]
fn a_straight_through_paging_sweep_is_not_a_loop() {
    let records = sweep_of("big.rs", 90, 1);
    assert_eq!(
        detect_loop(&records, LoopDetectionConfig::default()),
        LoopVerdict::NoLoop
    );
}

/// A wrap the model has not resumed from is not yet a sweep: going back to
/// the top once may be re-reading a header. One further advancing call is
/// what settles it.
#[test]
fn a_wrap_with_no_advance_after_it_is_not_yet_a_loop() {
    let mut records = sweep_of("big.rs", 20, 1);
    records.push(page("big.rs", 0));
    assert_eq!(
        detect_loop(&records, LoopDetectionConfig::default()),
        LoopVerdict::NoLoop
    );
    records.push(page("big.rs", 100));
    assert!(matches!(
        detect_loop(&records, LoopDetectionConfig::default()),
        LoopVerdict::MonotonicSweep { .. }
    ));
}

/// The rung is not about `read_file`: that tool's own ceiling already covers
/// it (#4041). A `bash` sweep spells its range into the command string, and
/// canonicalizing integers out of the string is what makes the two one shape.
#[test]
fn a_bash_range_sweep_is_the_same_shape() {
    let mut records: Vec<CallRecord<'static>> = Vec::new();
    for pass in 0..2 {
        for chunk in 0..10 {
            let (from, to) = (chunk * 50 + 1, chunk * 50 + 50);
            records.push(call(
                "bash",
                serde_json::json!({ "command": format!("sed -n '{from},{to}p' big.rs") }),
                &format!("pass {pass} chunk {chunk}"),
            ));
        }
    }
    match detect_loop(&records, LoopDetectionConfig::default()) {
        LoopVerdict::MonotonicSweep { tool, wraps, .. } => {
            assert_eq!(tool, "bash");
            assert_eq!(wraps, 1);
        }
        other => panic!("a wrapped `sed -n` sweep must be a loop: {other:?}"),
    }
}

/// Two files swept in parallel are two targets, and neither is a loop: the
/// grouping has to be by what the call addresses, or interleaving any second
/// file would defeat the rung exactly the way interleaving defeats the
/// contiguous scans (#1851).
#[test]
fn sweeps_of_two_targets_do_not_pool_into_one() {
    let mut records: Vec<CallRecord<'static>> = Vec::new();
    for page_index in 0..45 {
        records.push(page("alpha.rs", page_index * 100));
        records.push(page("beta.rs", page_index * 100));
    }
    assert_eq!(
        detect_loop(&records, LoopDetectionConfig::default()),
        LoopVerdict::NoLoop,
        "neither file wrapped, so neither is a sweep"
    );
}

/// A sweep of one file is not a re-detection of a sweep of another, so a
/// model steered about the first earns a warning about the second — the
/// #1743 rule, applied to this rung.
#[test]
fn two_targets_swept_are_two_loops() {
    let alpha = detect_loop(&sweep_of("alpha.rs", 42, 2), LoopDetectionConfig::default())
        .identity()
        .expect("a sweep has an identity");
    let beta = detect_loop(&sweep_of("beta.rs", 42, 2), LoopDetectionConfig::default())
        .identity()
        .expect("a sweep has an identity");
    assert_eq!(alpha.same_loop_as(&alpha), Some(true));
    assert_eq!(alpha.same_loop_as(&beta), Some(false));
}

/// The threshold is configuration like every other rung's, and `0` disables.
#[test]
fn the_sweep_threshold_is_honoured_in_both_directions() {
    let records = sweep_of("big.rs", 5, 2);
    assert_eq!(
        detect_loop(
            &records,
            LoopDetectionConfig {
                monotonic_sweep_threshold: 20,
                ..LoopDetectionConfig::default()
            }
        ),
        LoopVerdict::NoLoop,
        "ten calls are below a threshold of twenty"
    );
    assert!(matches!(
        detect_loop(
            &records,
            LoopDetectionConfig {
                monotonic_sweep_threshold: 4,
                ..LoopDetectionConfig::default()
            }
        ),
        LoopVerdict::MonotonicSweep { .. }
    ));
    assert_eq!(
        detect_loop(
            &records,
            LoopDetectionConfig {
                monotonic_sweep_threshold: 0,
                ..LoopDetectionConfig::default()
            }
        ),
        LoopVerdict::NoLoop,
        "zero disables the check, like every threshold beside it"
    );
}

/// A cursor that goes backwards without restarting is nobody's shape: the
/// model jumped back to re-check something and carried on, which is ordinary
/// navigation. The rung must decline rather than count it as a wrap.
#[test]
fn a_partial_regression_is_not_a_wrap() {
    let mut records: Vec<CallRecord<'static>> = (0..20).map(|i| page("big.rs", i * 100)).collect();
    records.push(page("big.rs", 900));
    records.push(page("big.rs", 2_000));
    assert_eq!(
        detect_loop(&records, LoopDetectionConfig::default()),
        LoopVerdict::NoLoop
    );
}
