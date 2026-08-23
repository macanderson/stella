//! The ledger's contract, plus the negative controls that prove the audit can
//! fail.
//!
//! The second half matters as much as the first. `content_free.rs` learned it
//! the expensive way: a harness whose checks pass vacuously reports green
//! forever, and the only way to know it is watching is to hand it something
//! broken and watch it complain.

use super::*;

// ---- the ledger's own contract ----------------------------------------

#[test]
fn the_ledger_is_total_over_known_type_tags() {
    // Adding an `AgentEvent` variant without declaring what consumes it fails
    // here, naming the tag and what to do about it.
    let violations = audit();
    assert!(
        violations.is_empty(),
        "the signal-consumer ledger does not hold:\n{}",
        violations
            .iter()
            .map(|v| format!("  - {v}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn the_ledger_row_count_equals_the_tag_count() {
    // Totality plus no-duplicates implies this, but state it directly: if the
    // audit ever regresses into checking one direction only, this stays red.
    assert_eq!(
        SIGNAL_CONSUMERS.len(),
        KNOWN_TYPE_TAGS.len(),
        "one row per wire tag, no more and no fewer"
    );
}

#[test]
fn unclassified_rows_are_a_down_only_debt() {
    // A ratchet, not a budget. Equality on purpose: lowering MAX_UNCLASSIFIED
    // was the point of #2703 and #4501 and had to show as a diff, and raising
    // it means a variant was filed away unread — the move the ledger exists to
    // prevent.
    let actual = unclassified_count();
    assert_eq!(
        actual, MAX_UNCLASSIFIED,
        "{actual} rows are Unclassified but MAX_UNCLASSIFIED is {MAX_UNCLASSIFIED}. \
         If you classified a signal, lower the constant in the same commit. \
         If you added a variant and reached for Unclassified, classify it \
         instead — the ceiling only moves down."
    );
}

/// #4501: every wire tag names a real consumer, so no shipped row holds
/// [`ConsumerPosture::Unclassified`].
///
/// Distinct from the ratchet above, which only says the count and the constant
/// agree — it was green at 32 and would be green at 32 again. This one names
/// the number the census reached, so a later PR that adds a variant and
/// re-opens the debt has to argue for it here.
#[test]
fn no_shipped_row_is_unclassified() {
    let unaudited: Vec<&str> = SIGNAL_CONSUMERS
        .iter()
        .filter(|row| matches!(row.posture, ConsumerPosture::Unclassified { .. }))
        .map(|row| row.type_tag)
        .collect();
    assert!(
        unaudited.is_empty(),
        "#4501 censused every row; these are unaudited again: {unaudited:?}. \
         Read what consumes the signal and declare Behavioral / Surfaced / \
         RecordedOnly — `scripts/check-consumer-sites.sh` then holds a \
         Behavioral row's site to naming live code."
    );
}

/// The two `AgentEvent` surfaces `stella-pipeline`'s removal (#3865) left with
/// no producer are kept on purpose (#3881), so a journal or a
/// `stella-events.jsonl` written before that removal still decodes into its
/// typed variant instead of demoting to [`AgentEvent::Unknown`].
///
/// Deleting one is a defensible decision and this is where it has to argue:
/// the tag leaves `KNOWN_TYPE_TAGS` in the same edit, and this test names what
/// that costs a reader of an old recording.
#[test]
fn the_producerless_pipeline_surfaces_still_decode() {
    for tag in ["proof", "candidate_delivery"] {
        assert!(
            KNOWN_TYPE_TAGS.contains(&tag),
            "`{tag}` has no producer in this workspace, but recordings that \
             predate #3865 carry it — dropping the tag demotes them to Unknown"
        );
    }
}

#[test]
fn the_exemplar_rows_show_each_posture_in_use() {
    // The three postures a real row can hold are each exercised by the
    // shipped ledger, so none of them is dead code the compiler keeps alive
    // and no test ever reaches.
    //
    // `Unclassified` is deliberately absent: #4501 emptied it, and
    // `unclassified_rows_are_a_down_only_debt` above is what now pins that at
    // zero. The negative controls below still construct the variant, so it is
    // exercised without any shipped row having to hold it.
    let posture = |tag: &str| {
        SIGNAL_CONSUMERS
            .iter()
            .find(|row| row.type_tag == tag)
            .map(|row| row.posture.clone())
            .unwrap_or_else(|| panic!("no ledger row for `{tag}`"))
    };
    assert!(matches!(
        posture("tool_result"),
        ConsumerPosture::Behavioral { .. }
    ));
    assert!(matches!(posture("text"), ConsumerPosture::Surfaced));
    assert!(matches!(
        posture("proof"),
        ConsumerPosture::RecordedOnly { .. }
    ));
}

#[test]
fn the_observatory_view_of_the_ledger_is_a_strict_subset() {
    // #2707 turns this into a two-sided parity test against the observatory's
    // own whitelist. Until then, pin the weaker fact that the ledger's
    // observatory claim is non-empty and every tag in it is real — so that
    // PR compares two live lists rather than one live list and one typo.
    let surfaced = tags_surfaced_by(Surface::Observatory);
    assert!(
        !surfaced.is_empty(),
        "no row claims the observatory renders it, yet its journal query \
         names an explicit list of event types — the ledger has drifted from \
         the surface it describes"
    );
    for tag in &surfaced {
        assert!(
            KNOWN_TYPE_TAGS.contains(tag),
            "`{tag}` is claimed as observatory-surfaced but is not a real wire tag"
        );
    }
}

// ---- negative controls: the audit must be able to fail -----------------

/// A minimal well-formed ledger, so each control below alters exactly one
/// thing and the failure it produces is unambiguous.
fn clean_ledger() -> Vec<SignalConsumers> {
    vec![
        SignalConsumers {
            type_tag: "text",
            posture: ConsumerPosture::Surfaced,
            surfaces: &[Surface::Observatory],
        },
        SignalConsumers {
            type_tag: "error",
            posture: ConsumerPosture::RecordedOnly { issue: "#2701" },
            surfaces: &[],
        },
    ]
}

const CONTROL_TAGS: &[&str] = &["text", "error"];

#[test]
fn the_control_fixture_itself_passes() {
    // Without this, every assertion below could be firing on a fixture that
    // was broken to begin with, and the controls would prove nothing.
    assert_eq!(audit_ledger(&clean_ledger(), CONTROL_TAGS), Vec::new());
}

#[test]
fn the_audit_catches_a_signal_with_no_row() {
    let violations = audit_ledger(&clean_ledger(), &["text", "error", "brand_new_signal"]);
    assert!(
        violations.contains(&LedgerViolation::MissingRow {
            type_tag: "brand_new_signal".to_string(),
        }),
        "a new wire tag with no ledger row must be caught: {violations:?}"
    );
}

#[test]
fn the_audit_catches_a_row_for_a_signal_that_does_not_exist() {
    let mut ledger = clean_ledger();
    ledger.push(SignalConsumers {
        type_tag: "renamed_away",
        posture: ConsumerPosture::Surfaced,
        surfaces: &[Surface::Serve],
    });
    let violations = audit_ledger(&ledger, CONTROL_TAGS);
    assert!(violations.contains(&LedgerViolation::UnknownTag {
        type_tag: "renamed_away".to_string(),
    }));
}

#[test]
fn the_audit_catches_a_duplicated_row() {
    let mut ledger = clean_ledger();
    ledger.push(SignalConsumers {
        type_tag: "text",
        posture: ConsumerPosture::Surfaced,
        surfaces: &[Surface::Serve],
    });
    let violations = audit_ledger(&ledger, CONTROL_TAGS);
    assert!(violations.contains(&LedgerViolation::DuplicateRow {
        type_tag: "text".to_string(),
    }));
}

#[test]
fn the_audit_catches_a_gap_that_cites_no_issue() {
    for posture in [
        ConsumerPosture::RecordedOnly { issue: "" },
        ConsumerPosture::Unclassified { issue: "   " },
    ] {
        let ledger = vec![
            SignalConsumers {
                type_tag: "text",
                posture: ConsumerPosture::Surfaced,
                surfaces: &[Surface::Observatory],
            },
            SignalConsumers {
                type_tag: "error",
                posture,
                surfaces: &[],
            },
        ];
        let violations = audit_ledger(&ledger, CONTROL_TAGS);
        assert!(
            violations
                .iter()
                .any(|v| matches!(v, LedgerViolation::UncitedGap { .. })),
            "an uncited gap is an undeclared gap: {violations:?}"
        );
    }
}

#[test]
fn the_audit_catches_a_citation_that_is_not_an_issue_reference() {
    // Prose where a reference belongs is the comfortable way to leave a gap
    // undeclared while appearing to declare it.
    for citation in ["see the epic", "2701", "#", "#27a1"] {
        let ledger = vec![SignalConsumers {
            type_tag: "text",
            posture: ConsumerPosture::Unclassified { issue: citation },
            surfaces: &[],
        }];
        let violations = audit_ledger(&ledger, &["text"]);
        assert!(
            violations.contains(&LedgerViolation::MalformedIssue {
                type_tag: "text".to_string(),
                issue: citation.to_string(),
            }),
            "`{citation}` must not pass as an issue reference: {violations:?}"
        );
    }
}

#[test]
fn the_audit_catches_a_behavioral_claim_with_nowhere_to_look() {
    let ledger = vec![SignalConsumers {
        type_tag: "text",
        posture: ConsumerPosture::Behavioral { site: "  " },
        surfaces: &[],
    }];
    assert!(
        audit_ledger(&ledger, &["text"]).contains(&LedgerViolation::UnnamedSite {
            type_tag: "text".to_string(),
        })
    );
}

#[test]
fn the_audit_catches_surfaced_nowhere_and_recorded_yet_surfaced() {
    // The two coherence failures, which are each other's mirror: a posture
    // claiming more than `surfaces` supports, and one claiming less.
    let surfaced_nowhere = vec![SignalConsumers {
        type_tag: "text",
        posture: ConsumerPosture::Surfaced,
        surfaces: &[],
    }];
    assert!(audit_ledger(&surfaced_nowhere, &["text"]).contains(
        &LedgerViolation::SurfacedNowhere {
            type_tag: "text".to_string(),
        }
    ));

    let recorded_yet_surfaced = vec![SignalConsumers {
        type_tag: "text",
        posture: ConsumerPosture::RecordedOnly { issue: "#2701" },
        surfaces: &[Surface::Observatory],
    }];
    assert!(audit_ledger(&recorded_yet_surfaced, &["text"]).contains(
        &LedgerViolation::RecordedYetSurfaced {
            type_tag: "text".to_string(),
        }
    ));
}

#[test]
fn every_violation_explains_itself_and_names_its_signal() {
    // A failure message is the whole interface this guard has with the person
    // who trips it. An empty or unspecific one turns a caught bug into a
    // puzzle, so pin that each variant renders prose naming its own tag.
    let violations = vec![
        LedgerViolation::UnknownTag {
            type_tag: "ghost".to_string(),
        },
        LedgerViolation::MissingRow {
            type_tag: "ghost".to_string(),
        },
        LedgerViolation::DuplicateRow {
            type_tag: "ghost".to_string(),
        },
        LedgerViolation::UncitedGap {
            type_tag: "ghost".to_string(),
            posture: "Unclassified",
        },
        LedgerViolation::MalformedIssue {
            type_tag: "ghost".to_string(),
            issue: "nope".to_string(),
        },
        LedgerViolation::UnnamedSite {
            type_tag: "ghost".to_string(),
        },
        LedgerViolation::SurfacedNowhere {
            type_tag: "ghost".to_string(),
        },
        LedgerViolation::RecordedYetSurfaced {
            type_tag: "ghost".to_string(),
        },
    ];
    for violation in &violations {
        let rendered = violation.to_string();
        assert!(
            rendered.contains("ghost"),
            "{violation:?} does not name the signal it is about: {rendered}"
        );
        assert!(
            rendered.len() > 40,
            "{violation:?} renders too tersely to act on: {rendered}"
        );
    }
}
