//! Unit tests for [`super`] — split out of `event.rs` so the wire-contract
//! file itself stays inside the file-size ratchet (the `registry.rs`
//! precedent, #1122); a child module, so the private surface stays reachable
//! via `super::*`.

use super::*;

#[test]
fn agent_event_roundtrips_with_type_tag() {
    let event = AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "call_1".into(),
            name: "read_file".into(),
            input: serde_json::json!({ "path": "src/main.rs" }),
        },
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"type\":\"tool_start\""), "{json}");
    let back: AgentEvent = serde_json::from_str(&json).unwrap();
    match back {
        AgentEvent::ToolStart { call } => assert_eq!(call.name, "read_file"),
        other => panic!("unexpected variant: {other:?}"),
    }
}

// The text / text_delta wire spellings — including the pre-#1886 crossed
// legacy — are witnessed in `tests/text_event_wire.rs`.

#[test]
fn tool_result_roundtrips_and_streams_without_speculated_still_parse() {
    // Round-trip with the flag set.
    let event = AgentEvent::ToolResult {
        call_id: "call_1".into(),
        output: ToolOutput::Ok {
            content: "x".into(),
        },
        duration_ms: 42,
        speculated: true,
    };
    let json = serde_json::to_string(&event).unwrap();
    let back: AgentEvent = serde_json::from_str(&json).unwrap();
    match back {
        AgentEvent::ToolResult { speculated, .. } => assert!(speculated),
        other => panic!("unexpected variant: {other:?}"),
    }

    // A stream recorded BEFORE the field existed must still parse, with
    // the safe default (not speculated).
    let old =
        r#"{"type":"tool_result","call_id":"c","output":{"ok":{"content":""}},"duration_ms":1}"#;
    match serde_json::from_str::<AgentEvent>(old) {
        Ok(AgentEvent::ToolResult { speculated, .. }) => {
            assert!(!speculated, "missing field must default to false")
        }
        other => panic!("old stream must parse: {other:?}"),
    }
}

#[test]
fn speculation_discarded_roundtrips_and_names_the_reason() {
    let event = AgentEvent::SpeculationDiscarded {
        call_id: "c1".into(),
        name: "read_file".into(),
        reason: "attempt_failed".into(),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(
        json.contains("\"type\":\"speculation_discarded\""),
        "{json}"
    );
    let back: AgentEvent = serde_json::from_str(&json).unwrap();
    match back {
        AgentEvent::SpeculationDiscarded {
            call_id,
            name,
            reason,
        } => {
            assert_eq!(call_id, "c1");
            assert_eq!(name, "read_file");
            assert_eq!(reason, "attempt_failed");
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

/// Invariant 4 witness for the parked-wait span (#1857): both events
/// round-trip byte-faithfully, under the tags a consumer keys on.
#[test]
fn parked_wait_events_roundtrip_under_their_own_tags() {
    let parked = AgentEvent::TurnParked {
        description: "CI for branch main settles".into(),
        poll_interval_secs: 5,
        deadline_secs: 600,
    };
    let json = serde_json::to_string(&parked).unwrap();
    assert!(json.contains("\"type\":\"turn_parked\""), "{json}");
    match serde_json::from_str::<AgentEvent>(&json).unwrap() {
        AgentEvent::TurnParked {
            description,
            poll_interval_secs,
            deadline_secs,
        } => {
            assert_eq!(description, "CI for branch main settles");
            assert_eq!(poll_interval_secs, 5);
            assert_eq!(deadline_secs, 600);
        }
        other => panic!("unexpected variant: {other:?}"),
    }

    let woken = AgentEvent::TurnWoken {
        reason: "deadline_expired".into(),
        polls_used: 4,
    };
    let json = serde_json::to_string(&woken).unwrap();
    assert!(json.contains("\"type\":\"turn_woken\""), "{json}");
    match serde_json::from_str::<AgentEvent>(&json).unwrap() {
        AgentEvent::TurnWoken { reason, polls_used } => {
            assert_eq!(reason, "deadline_expired");
            assert_eq!(polls_used, 4);
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn budget_tick_roundtrips_with_session_axis() {
    let event = AgentEvent::BudgetTick {
        spent_usd: 0.42,
        limit_usd: Some(2.5),
        mode: BudgetMode::Enforced,
        session_spent_usd: Some(1.75),
        session_limit_usd: Some(10.0),
    };
    let json = serde_json::to_string(&event).unwrap();
    let back: AgentEvent = serde_json::from_str(&json).unwrap();
    match back {
        AgentEvent::BudgetTick {
            spent_usd,
            limit_usd,
            mode,
            session_spent_usd,
            session_limit_usd,
        } => {
            assert_eq!(spent_usd, 0.42);
            assert_eq!(limit_usd, Some(2.5));
            assert_eq!(mode, BudgetMode::Enforced);
            assert_eq!(session_spent_usd, Some(1.75));
            assert_eq!(session_limit_usd, Some(10.0));
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn budget_tick_without_session_fields_parses_with_none() {
    // A stream recorded BEFORE the session axis existed must still parse,
    // with both new fields defaulting to `None` (not `0.0`, which would
    // read as a real "spent nothing").
    let old = r#"{"type":"budget_tick","spent_usd":0.42,"limit_usd":2.5,"mode":"enforced"}"#;
    match serde_json::from_str::<AgentEvent>(old) {
        Ok(AgentEvent::BudgetTick {
            session_spent_usd,
            session_limit_usd,
            ..
        }) => {
            assert_eq!(session_spent_usd, None);
            assert_eq!(session_limit_usd, None);
        }
        other => panic!("old stream must parse: {other:?}"),
    }
}

#[test]
fn compaction_event_carries_counts_and_block_identities() {
    let event = AgentEvent::Compaction {
        before_tokens: 10_000,
        after_tokens: 4_000,
        evicted: 2,
        deduped: 1,
        superseded: 1,
        aged: 1,
        summarized: 3,
        evicted_blocks: vec!["blk_ev1".into(), "blk_ev2".into()],
        deduped_blocks: vec!["blk_dd1".into()],
        superseded_blocks: vec!["blk_sup".into()],
        aged_blocks: vec!["blk_age".into()],
        // Fewer identities than the `summarized` count: the summary folded
        // three messages but only two were identity-bearing tool results.
        summarized_blocks: vec!["blk_sum1".into(), "blk_sum2".into()],
        rewrites: vec![],
        effective_budget_tokens: 136_363,
        calibration_factor: 1.1,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"type\":\"compaction\""), "{json}");
    let back: AgentEvent = serde_json::from_str(&json).unwrap();
    match back {
        AgentEvent::Compaction {
            before_tokens,
            after_tokens,
            evicted_blocks,
            summarized,
            summarized_blocks,
            effective_budget_tokens,
            ..
        } => {
            assert!(after_tokens < before_tokens);
            // Identities, not just counts — which blocks left context.
            assert_eq!(evicted_blocks, vec!["blk_ev1", "blk_ev2"]);
            // The summary names its folded tool-result blocks, and the vec
            // may be shorter than the message count it replaced.
            assert_eq!(summarized_blocks, vec!["blk_sum1", "blk_sum2"]);
            assert!(summarized_blocks.len() < summarized);
            assert_eq!(effective_budget_tokens, 136_363);
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn legacy_compaction_without_identities_still_parses() {
    // A journal written before §6.2 has counts but no identity fields; it
    // must still deserialize (additive contract), with empty identity vecs.
    let old = r#"{"type":"compaction","before_tokens":9,"after_tokens":4,"evicted":1,"deduped":0}"#;
    match serde_json::from_str::<AgentEvent>(old).unwrap() {
        AgentEvent::Compaction {
            evicted,
            evicted_blocks,
            effective_budget_tokens,
            ..
        } => {
            assert_eq!(evicted, 1);
            assert!(evicted_blocks.is_empty());
            assert_eq!(effective_budget_tokens, 0);
        }
        other => panic!("old compaction must parse: {other:?}"),
    }
}

#[test]
fn provider_fallback_is_never_silent_it_names_both_ends() {
    let event = AgentEvent::ProviderFallback {
        from: "zai".into(),
        to: "anthropic".into(),
        reason: "circuit breaker open after 3 consecutive transport failures".into(),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"from\":\"zai\""), "{json}");
    assert!(json.contains("\"to\":\"anthropic\""), "{json}");
}

#[test]
fn file_change_carries_the_delta_and_the_diff_on_the_single_event_path() {
    let event = AgentEvent::FileChange {
        path: "src/lib.rs".into(),
        kind: FileChangeKind::Modified,
        added: 12,
        removed: 3,
        diff: Some("@@ -1 +1 @@\n-old\n+new".into()),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"type\":\"file_change\""), "{json}");
    let back: AgentEvent = serde_json::from_str(&json).unwrap();
    match back {
        AgentEvent::FileChange {
            kind,
            added,
            removed,
            diff,
            ..
        } => {
            assert_eq!(kind, FileChangeKind::Modified);
            assert_eq!(
                (added, removed),
                (12, 3),
                "the recorder's counts survive the wire — consumers must \
                 not have to recount the diff text"
            );
            assert!(diff.is_some());
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

/// Journals written before the counts existed must still replay. They
/// recorded no delta, so they come back as `0/0` — the honest answer for a
/// stream that never measured one.
#[test]
fn a_file_change_without_counts_still_parses() {
    let old = r#"{"type":"file_change","path":"a.rs","kind":"modified","diff":null}"#;
    match serde_json::from_str::<AgentEvent>(old).unwrap() {
        AgentEvent::FileChange {
            path,
            added,
            removed,
            ..
        } => {
            assert_eq!(path, "a.rs");
            assert_eq!((added, removed), (0, 0));
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn read_kind_serializes_and_is_the_only_non_mutation() {
    assert_eq!(
        serde_json::to_string(&FileChangeKind::Read).unwrap(),
        "\"read\""
    );
    let back: FileChangeKind = serde_json::from_str("\"read\"").unwrap();
    assert_eq!(back, FileChangeKind::Read);
    assert!(!FileChangeKind::Read.is_mutation());
    for kind in [
        FileChangeKind::Created,
        FileChangeKind::Modified,
        FileChangeKind::Deleted,
    ] {
        assert!(kind.is_mutation(), "{kind:?} is a mutation");
    }
}

#[test]
fn context_recall_frames_always_carry_a_citation_label() {
    let event = AgentEvent::ContextRecall {
        frames: vec![ContextFrameRef {
            id: None, // not-yet-materialized frames carry no id (L-C4)
            citation_label: "engine step-driver (driver.rs)".into(),
            provider: "code-graph".into(),
            source: "code-graph".into(),
            kind: "symbol".into(),
            uri: Some("file:///repo/stella-core/src/driver.rs".into()),
            method: Some("tree-sitter/symbol-extract".into()),
            token_cost: 120,
            block_id: None,
            content_digest: None,
        }],
        provider_mix: vec![ProviderShare {
            provider: "code-graph".into(),
            frames: 1,
        }],
        tokens: 120,
        usage: None,
        latency_ms: 0,
        used_ann_index: None,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("citation_label"), "{json}");
    assert!(
        !json.contains("\"id\""),
        "absent id must be omitted: {json}"
    );
    let back: AgentEvent = serde_json::from_str(&json).unwrap();
    match back {
        AgentEvent::ContextRecall { frames, .. } => {
            let frame = &frames[0];
            assert_eq!(frame.provider, "code-graph");
            assert_eq!(frame.source, "code-graph");
            assert_eq!(frame.kind, "symbol");
            assert_eq!(
                frame.uri.as_deref(),
                Some("file:///repo/stella-core/src/driver.rs")
            );
            assert_eq!(frame.method.as_deref(), Some("tree-sitter/symbol-extract"));
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

/// WITNESS (#452): the usage-report envelope rides the recall event and
/// round-trips byte-for-byte (AGENTS.md invariant 4), so context cost is a
/// meterable record rather than a number that dies with the turn.
#[test]
fn context_usage_report_round_trips_and_stays_content_free() {
    let usage = ContextUsage {
        budget_requested: 1200,
        budget_consumed: 210,
        as_of: "2026-07-24T00:00:00Z".into(),
        providers: vec![
            ContextProviderUsage {
                provider_id: "workspace-memory".into(),
                frames_served: 2,
                frames_rejected: 0,
                token_cost: 90,
            },
            ContextProviderUsage {
                provider_id: "code-graph".into(),
                frames_served: 1,
                frames_rejected: 3,
                token_cost: 120,
            },
        ],
    };
    assert!(
        usage.is_consistent(),
        "budget_consumed must re-sum from the per-provider costs"
    );
    assert_eq!(usage.total_frames_served(), 3);

    let event = AgentEvent::ContextRecall {
        frames: vec![],
        provider_mix: vec![],
        tokens: 210,
        usage: Some(usage.clone()),
        latency_ms: 0,
        used_ann_index: None,
    };
    let json = serde_json::to_string(&event).unwrap();
    let back: AgentEvent = serde_json::from_str(&json).unwrap();
    match &back {
        AgentEvent::ContextRecall { usage: round, .. } => {
            assert_eq!(round.as_ref(), Some(&usage), "byte-for-byte round trip")
        }
        other => panic!("unexpected variant: {other:?}"),
    }
    assert_eq!(
        json,
        serde_json::to_string(&back).unwrap(),
        "re-serialization must be byte-identical"
    );

    // Content-free by construction: the envelope carries provider ids,
    // counts, costs, and a timestamp — never frame text, titles, URIs, or
    // query text (AGENTS.md invariant 3, #466).
    let value = serde_json::to_value(&usage).unwrap();
    let mut fields: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
    fields.sort();
    assert_eq!(
        fields,
        ["as_of", "budget_consumed", "budget_requested", "providers"],
        "no field may be added to the usage envelope without a content review"
    );
}

/// An inconsistent total is *checkable*, never a silent misbill (§2).
#[test]
fn a_tampered_usage_total_fails_the_arithmetic_identity() {
    let usage = ContextUsage {
        budget_requested: 1200,
        budget_consumed: 999,
        as_of: "2026-07-24T00:00:00Z".into(),
        providers: vec![ContextProviderUsage {
            provider_id: "workspace-memory".into(),
            frames_served: 1,
            frames_rejected: 0,
            token_cost: 90,
        }],
    };
    assert!(!usage.is_consistent());
}

/// The accounting clock is a bare `String` (#568), so a receipt whose
/// `as_of` is junk sorts wrong in whatever tool eventually reads it. The
/// shape is checkable here, at the type, rather than three crates away.
#[test]
fn as_of_wellformedness_is_checkable_on_the_receipt() {
    let usage_at = |as_of: &str| ContextUsage {
        budget_requested: 1200,
        budget_consumed: 0,
        as_of: as_of.into(),
        providers: vec![],
    };

    // The shape every host in this workspace stamps, plus the variants
    // RFC 3339 also permits — rejecting these would make the predicate a
    // false-alarm generator on valid receipts.
    for good in [
        "2026-07-24T00:00:00Z",
        "2026-07-24t00:00:00z",
        "2026-07-24T00:00:00.123456Z",
        "2026-07-24T00:00:00+00:00",
        "2026-07-24T12:30:59.5-05:00",
    ] {
        assert!(usage_at(good).as_of_is_wellformed(), "{good}");
    }

    // Missing the offset, wrong separators, a truncated prefix, junk, and
    // the empty string a `Default`-ish construction would leave behind.
    for bad in [
        "2026-07-24T00:00:00",
        "2026-07-24 00:00:00Z",
        "2026/07/24T00:00:00Z",
        "2026-07-24T00:00:00.Z",
        "2026-07-24T00:00:00+0000",
        "24 July 2026",
        "",
    ] {
        assert!(!usage_at(bad).as_of_is_wellformed(), "{bad}");
    }
}

/// A recall event recorded before the usage report existed must still
/// deserialize — the additive contract.
#[test]
fn a_recall_event_without_a_usage_report_still_parses() {
    let legacy = r#"{"type":"context_recall","frames":[],"provider_mix":[],"tokens":0}"#;
    match serde_json::from_str::<AgentEvent>(legacy).unwrap() {
        AgentEvent::ContextRecall { usage, .. } => assert!(usage.is_none()),
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn context_recall_from_a_pre_provenance_stream_still_parses() {
    let legacy = r#"{"type":"context_recall","frames":[{"citation_label":"driver.rs","source":"code-graph","token_cost":12}],"provider_mix":[{"provider":"code-graph","frames":1}],"tokens":12}"#;
    match serde_json::from_str::<AgentEvent>(legacy) {
        Ok(AgentEvent::ContextRecall { frames, .. }) => {
            let frame = &frames[0];
            assert!(frame.provider.is_empty());
            assert_eq!(frame.source, "code-graph");
            assert!(frame.kind.is_empty());
            assert_eq!(frame.uri, None);
            assert_eq!(frame.method, None);
        }
        other => panic!("old stream must parse: {other:?}"),
    }
}

#[test]
fn media_job_failure_carries_its_reason_inline() {
    let state = MediaJobState::Failed {
        reason: "provider rejected the prompt".into(),
    };
    let json = serde_json::to_string(&state).unwrap();
    let back: MediaJobState = serde_json::from_str(&json).unwrap();
    assert_eq!(back, state);
}

#[test]
fn verdict_distinguishes_deterministic_from_model_evidence() {
    let event = AgentEvent::Verdict {
        passed: true,
        evidence: VerdictEvidence {
            summary: "flip oracle: fail→pass on `cargo test -p x`".into(),
            deterministic: true,
            evidence_refs: vec!["trace:t1#verify".into()],
            ladder: None,
        },
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(
        !json.contains("ladder"),
        "an absent snapshot must not serialize a null field"
    );
    let back: AgentEvent = serde_json::from_str(&json).unwrap();
    match back {
        AgentEvent::Verdict { evidence, .. } => assert!(evidence.deterministic),
        other => panic!("unexpected variant: {other:?}"),
    }
}

/// #865 wire compatibility, both directions: a verdict recorded before
/// snapshots existed parses (`ladder` absent → `None`), and a snapshot
/// roundtrips with its oracle trace intact.
#[test]
fn ladder_snapshot_is_additive_and_roundtrips() {
    let legacy = r#"{"type":"verdict","passed":true,
        "evidence":{"summary":"ok","deterministic":true}}"#;
    let back: AgentEvent = serde_json::from_str(legacy).unwrap();
    match back {
        AgentEvent::Verdict { evidence, .. } => assert!(evidence.ladder.is_none()),
        other => panic!("unexpected variant: {other:?}"),
    }

    let event = AgentEvent::Verdict {
        passed: true,
        evidence: VerdictEvidence {
            summary: "flip + confirmation".into(),
            deterministic: true,
            evidence_refs: vec![],
            ladder: Some(Box::new(LadderSnapshot {
                rung: Some(crate::LadderRung::SubmitFast),
                tracked_command: Some("cargo test -p x".into()),
                oracle_trace: vec![
                    crate::OracleObservation {
                        tree: ProofTree::Baseline,
                        passed: false,
                    },
                    crate::OracleObservation {
                        tree: ProofTree::Candidate,
                        passed: true,
                    },
                ],
                flip_achieved: true,
                unstable_flip: false,
                flip_refused_different_failure: false,
                touched_tests_passed: Some(true),
                test_infra: None,
                diff_lines: 12,
                diff_budget: 400,
                diff_available: true,
                file_change_events: 2,
                mutating_actions: 3,
                new_diag_errors: 0,
                new_diag_warnings: 0,
                witness_intact: Some(true),
                witness_mutation: None,
                diff_coverage: Some("covered".into()),
                verifier_independent: None,
            })),
        },
    };
    let json = serde_json::to_string(&event).unwrap();
    let back: AgentEvent = serde_json::from_str(&json).unwrap();
    match back {
        AgentEvent::Verdict { evidence, .. } => {
            let snapshot = evidence.ladder.expect("snapshot survives the wire");
            assert_eq!(snapshot.oracle_trace.len(), 2);
            assert!(snapshot.flip_achieved);
            assert_eq!(snapshot.tracked_command.as_deref(), Some("cargo test -p x"));
            assert_eq!(snapshot.rung, Some(crate::LadderRung::SubmitFast));
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn ask_user_roundtrips_and_carries_structured_options() {
    let event = AgentEvent::AskUser {
        id: "call_q1".into(),
        question: "Which database should the migration target?".into(),
        options: vec!["local (5433)".into(), "staging".into()],
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"type\":\"ask_user\""), "{json}");
    let back: AgentEvent = serde_json::from_str(&json).unwrap();
    match back {
        AgentEvent::AskUser { options, .. } => assert_eq!(options.len(), 2),
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn scope_review_and_pr_events_roundtrip() {
    for event in [
        AgentEvent::ScopeReview {
            proposal: ScopeProposal {
                summary: "refactor the auth module".into(),
                steps: vec!["step 1".into(), "step 2".into()],
                estimated_files: 12,
                estimated_cost_usd: Some(1.25),
                ..Default::default()
            },
        },
        AgentEvent::Pr {
            url: "https://github.com/x/y/pull/1".into(),
            status: PrStatus::Open,
            number: Some(1),
            ci: Some(CiStatus::Running),
        },
        AgentEvent::Commit {
            sha: "abc123".into(),
            message: "feat: x".into(),
        },
    ] {
        let json = serde_json::to_string(&event).unwrap();
        let _back: AgentEvent = serde_json::from_str(&json).unwrap();
    }
}

/// Invariant #4: the scope-card facts (repo/branch, globs, shell policy)
/// round-trip byte-for-byte, and a proposal recorded before they existed
/// still parses with every one absent.
#[test]
fn scope_proposal_roundtrips_its_scope_card_facts_and_stays_additive() {
    let full = ScopeProposal {
        summary: "wire the automations API".into(),
        steps: vec!["extract types".into()],
        estimated_files: 4,
        estimated_cost_usd: Some(0.42),
        repo: Some("macanderson/stella".into()),
        branch: Some("feat/automations".into()),
        write_globs: vec!["apps/api/**".into(), "apps/app/automations/**".into()],
        read_globs: vec!["packages/shared/**".into()],
        shell_policy: Some("allowlisted".into()),
    };
    let json = serde_json::to_string(&full).unwrap();
    let back: ScopeProposal = serde_json::from_str(&json).unwrap();
    assert_eq!(full, back);

    // A pre-existing stream: none of the new keys, all default in.
    let legacy = r#"{"summary":"s","steps":["a"],"estimated_files":2}"#;
    let old: ScopeProposal = serde_json::from_str(legacy).unwrap();
    assert_eq!(old.repo, None);
    assert!(old.write_globs.is_empty());
    assert_eq!(old.shell_policy, None);
    // And a proposal that states none of them serializes without the keys.
    let json = serde_json::to_string(&old).unwrap();
    assert!(!json.contains("write_globs"), "{json}");
    assert!(!json.contains("repo"), "{json}");
}

/// Invariant #4: the oracle observation's replay facts (`run`,
/// `runs_required`, `seed`) round-trip byte-for-byte, and an observation
/// recorded before they existed still parses with each absent.
#[test]
fn proof_oracle_roundtrips_replay_facts_and_stays_additive() {
    let step = ProofStep::Oracle {
        command: "cargo test -p x".into(),
        passed: true,
        tree: ProofTree::Candidate,
        run: Some(2),
        runs_required: Some(3),
        seed: Some(7741),
    };
    let event = AgentEvent::Proof { step: step.clone() };
    let json = serde_json::to_string(&event).unwrap();
    let back: AgentEvent = serde_json::from_str(&json).unwrap();
    match back {
        AgentEvent::Proof { step: parsed } => assert_eq!(parsed, step),
        other => panic!("unexpected variant: {other:?}"),
    }

    let legacy = r#"{"type":"proof","step":{"kind":"oracle","command":"cargo test","passed":false,"tree":"baseline"}}"#;
    let old: AgentEvent = serde_json::from_str(legacy).unwrap();
    match old {
        AgentEvent::Proof {
            step:
                ProofStep::Oracle {
                    run,
                    runs_required,
                    seed,
                    ..
                },
        } => {
            assert_eq!(run, None);
            assert_eq!(runs_required, None);
            assert_eq!(seed, None);
        }
        other => panic!("unexpected variant: {other:?}"),
    }
    // A single-run oracle serializes without the replay keys.
    let single = AgentEvent::Proof {
        step: ProofStep::Oracle {
            command: "c".into(),
            passed: false,
            tree: ProofTree::Baseline,
            run: None,
            runs_required: None,
            seed: None,
        },
    };
    let json = serde_json::to_string(&single).unwrap();
    assert!(!json.contains("runs_required"), "{json}");
    assert!(!json.contains("seed"), "{json}");
}

#[test]
fn pr_event_from_a_pre_ci_stream_still_parses() {
    // Backward compatibility: a `pr` line serialized before `number`
    // and `ci` existed must deserialize with both absent — absent ci
    // means "not polled yet", never "passing".
    let legacy = r#"{"type":"pr","url":"https://github.com/x/y/pull/183","status":"open"}"#;
    match serde_json::from_str::<AgentEvent>(legacy) {
        Ok(AgentEvent::Pr { number, ci, .. }) => {
            assert_eq!(number, None);
            assert_eq!(ci, None);
        }
        other => panic!("old stream must parse: {other:?}"),
    }
}

#[test]
fn task_update_roundtrips_a_full_board_snapshot() {
    let event = AgentEvent::TaskUpdate {
        tasks: vec![
            TaskItem {
                id: "1".into(),
                subject: "Map the auth module".into(),
                description: None,
                status: TaskStatus::Completed,
                owner: Some("lead".into()),
            },
            TaskItem {
                id: "2".into(),
                subject: "Fix the redirect loop".into(),
                description: Some("token refresh races the redirect".into()),
                status: TaskStatus::InProgress,
                owner: Some("sub:2".into()),
            },
            TaskItem {
                id: "3".into(),
                subject: "Add a witness test".into(),
                description: None,
                status: TaskStatus::Pending,
                owner: None,
            },
        ],
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"type\":\"task_update\""), "{json}");
    // Absent optionals are omitted, not serialized as null.
    assert!(!json.contains("null"), "{json}");
    let back: AgentEvent = serde_json::from_str(&json).unwrap();
    match back {
        AgentEvent::TaskUpdate { tasks } => {
            assert_eq!(tasks.len(), 3);
            assert_eq!(tasks[1].status, TaskStatus::InProgress);
            assert_eq!(tasks[1].owner.as_deref(), Some("sub:2"));
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn task_status_open_vs_terminal() {
    assert!(TaskStatus::Pending.is_open());
    assert!(TaskStatus::InProgress.is_open());
    assert!(!TaskStatus::Completed.is_open());
    assert!(!TaskStatus::Cancelled.is_open());
}

/// The research stage's two wire tokens (#1778), pinned in both directions
/// (invariant 4): the snake_case token is what a recorded stream carries, and
/// a role addition is one-directional (`ModelCallRole` has no catch-all), so
/// what this build writes is exactly what a future build must read.
#[test]
fn research_stage_and_role_roundtrip_with_snake_case_tokens() {
    let stage = serde_json::to_string(&StageKind::Research).unwrap();
    assert_eq!(stage, "\"research\"");
    assert_eq!(
        serde_json::from_str::<StageKind>(&stage).unwrap(),
        StageKind::Research
    );
    let role = serde_json::to_string(&ModelCallRole::Research).unwrap();
    assert_eq!(role, "\"research\"");
    assert_eq!(
        serde_json::from_str::<ModelCallRole>(&role).unwrap(),
        ModelCallRole::Research
    );
}

#[test]
fn stream_json_is_one_line_per_event() {
    let events = [
        AgentEvent::Stage {
            name: StageKind::Triage,
        },
        AgentEvent::Text { text: "hi".into() },
        AgentEvent::Complete {
            model: "glm-5.2".into(),
            cost_usd: 0.001,
        },
    ];
    let lines: Vec<String> = events
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect();
    assert_eq!(lines.len(), 3);
    for line in &lines {
        assert!(!line.contains('\n'));
    }
}

#[test]
fn step_usage_roundtrips_as_a_complete_metering_record() {
    let event = AgentEvent::StepUsage {
        step: 3,
        role: ModelCallRole::Plan,
        provider: "zai".into(),
        output_text: Some(r#"["inspect", "patch"]"#.into()),
        model: "glm-5.2".into(),
        input_tokens: 12_000,
        output_tokens: 450,
        cached_input_tokens: 9_000,
        cache_write_tokens: 2_500,
        reasoning_tokens: None,
        estimated_input_tokens: 11_200,
        cost_usd: 0.0042,
        duration_ms: 1_830,
        retries: 1,
        tool_calls: 4,
        complete: true,
        finish_reason: None,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"type\":\"step_usage\""), "{json}");
    assert!(json.contains("\"role\":\"plan\""), "{json}");
    assert!(json.contains("\"cache_write_tokens\":2500"), "{json}");
    let back: AgentEvent = serde_json::from_str(&json).unwrap();
    match back {
        AgentEvent::StepUsage {
            step,
            role,
            output_text,
            cached_input_tokens,
            cache_write_tokens,
            estimated_input_tokens,
            retries,
            tool_calls,
            ..
        } => {
            assert_eq!(step, 3);
            assert_eq!(role, ModelCallRole::Plan);
            assert_eq!(output_text.as_deref(), Some(r#"["inspect", "patch"]"#));
            assert_eq!(cached_input_tokens, 9_000);
            assert_eq!(cache_write_tokens, 2_500);
            assert_eq!(estimated_input_tokens, 11_200);
            assert_eq!(retries, 1);
            assert_eq!(tool_calls, 4);
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn step_usage_from_a_pre_drift_stream_still_parses() {
    // Backward compatibility: a `step_usage` line serialized before
    // `estimated_input_tokens` existed must deserialize with the field
    // defaulting to 0 ("no estimate was taken") — the stream-json wire
    // format is versioned by being additive-only.
    let legacy = r#"{"type":"step_usage","step":3,"model":"glm-5.2","input_tokens":12000,
        "output_tokens":450,"cached_input_tokens":9000,"cost_usd":0.0042,
        "duration_ms":1830,"retries":1,"tool_calls":4}"#;
    let back: AgentEvent = serde_json::from_str(legacy).unwrap();
    match back {
        AgentEvent::StepUsage {
            role,
            output_text,
            estimated_input_tokens,
            input_tokens,
            ..
        } => {
            assert_eq!(estimated_input_tokens, 0);
            assert_eq!(role, ModelCallRole::Unknown);
            assert_eq!(output_text, None);
            assert_eq!(input_tokens, 12_000);
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn step_usage_from_a_pre_cache_write_stream_still_parses() {
    // Backward compatibility: a `step_usage` line serialized before
    // `cache_write_tokens` existed (but after `estimated_input_tokens`)
    // must deserialize with the field defaulting to 0 ("provider
    // reported no cache writes") — the additive-only wire contract.
    let legacy = r#"{"type":"step_usage","step":3,"model":"glm-5.2","input_tokens":12000,
        "output_tokens":450,"cached_input_tokens":9000,"estimated_input_tokens":11200,
        "cost_usd":0.0042,"duration_ms":1830,"retries":1,"tool_calls":4}"#;
    let back: AgentEvent = serde_json::from_str(legacy).unwrap();
    match back {
        AgentEvent::StepUsage {
            cache_write_tokens,
            cached_input_tokens,
            estimated_input_tokens,
            ..
        } => {
            assert_eq!(cache_write_tokens, 0);
            assert_eq!(cached_input_tokens, 9_000);
            assert_eq!(estimated_input_tokens, 11_200);
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn goal_verdict_roundtrips_both_outcomes() {
    for met in [true, false] {
        let event = AgentEvent::GoalVerdict {
            round: 2,
            met,
            reasoning: "tests now pass".into(),
            cost_usd: 0.001,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"goal_verdict\""), "{json}");
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            AgentEvent::GoalVerdict { met: b, round, .. } => {
                assert_eq!(b, met);
                assert_eq!(round, 2);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }
}

#[test]
fn step_usage_preserves_call_identity_and_completeness() {
    let json = r#"{"type":"step_usage","step":3,"role":"plan_repair","provider":"anthropic","model":"claude-sonnet-4-5","input_tokens":12000,"output_tokens":300,"cached_input_tokens":9000,"cache_write_tokens":12,"estimated_input_tokens":11000,"cost_usd":0.09,"duration_ms":1400,"retries":1,"tool_calls":0,"complete":true}"#;
    let event: AgentEvent = serde_json::from_str(json).unwrap();
    let roundtrip = serde_json::to_value(event).unwrap();
    assert_eq!(roundtrip["role"], "plan_repair");
    assert_eq!(roundtrip["provider"], "anthropic");
    assert_eq!(roundtrip["complete"], true);
}

#[test]
fn legacy_step_usage_without_completeness_fails_closed() {
    let legacy = r#"{"type":"step_usage","step":1,"model":"old","input_tokens":10,"output_tokens":2,"cached_input_tokens":0,"cost_usd":0.01,"duration_ms":10,"retries":0,"tool_calls":0}"#;
    let event: AgentEvent = serde_json::from_str(legacy).unwrap();
    let roundtrip = serde_json::to_value(event).unwrap();
    assert_eq!(roundtrip["complete"], false);
}

#[test]
fn usage_incomplete_is_a_closed_content_free_signal() {
    // `verdict`, not `verifier`: this field is a `ModelCallRole` — the JOB
    // the call was doing — and the rename mapped the old `judge` role to
    // `Verdict`. `Verifier` is the `Role` (the model slot), a different
    // enum; a blanket judge→verifier sweep conflated the two.
    let json = r#"{"type":"usage_incomplete","role":"verdict","provider":"anthropic","model":"claude-sonnet-4-5","reason":"timeout","duration_ms":2500,"retries":null}"#;
    let event: AgentEvent = serde_json::from_str(json).unwrap();
    let roundtrip = serde_json::to_value(event).unwrap();
    assert_eq!(roundtrip["type"], "usage_incomplete");
    assert_eq!(roundtrip["reason"], "timeout");
    assert_eq!(roundtrip.as_object().unwrap().len(), 7);
}

#[test]
fn block_registered_carries_bytes_only_for_gap_kinds() {
    // Journal-resolvable kinds (tool I/O, assistant text) carry NO content —
    // their preimage is resolved from the originating event, never re-stored.
    let tool = AgentEvent::BlockRegistered {
        block_id: "blk_0123456789abcdef01234567".into(),
        kind: BlockKind::ToolResult,
        origin: BlockOrigin {
            turn_instance: 2,
            step: 5,
            call_id: Some("call_9".into()),
            memory_id: None,
        },
        token_cost: 480,
        content_digest: "sha256:deadbeef".into(),
        citation_label: None,
        content: None,
    };
    let value = serde_json::to_value(&tool).unwrap();
    assert_eq!(value["type"], "block_registered");
    assert_eq!(value["kind"], "tool_result");
    assert_eq!(value["origin"]["call_id"], "call_9");
    assert!(
        value.get("content").is_none() && value.get("output").is_none(),
        "a journal-resolvable block must not carry payload bytes: {value}"
    );

    // Gap kinds the journal cannot resolve (the system prefix, the assembled
    // user message) DO carry their bytes — local-only, stripped on export —
    // so the step stays reconstructable (spec §5.3).
    let system = AgentEvent::BlockRegistered {
        block_id: "blk_sys0000000000000000000".into(),
        kind: BlockKind::SystemPrefix,
        origin: BlockOrigin {
            turn_instance: 0,
            step: 0,
            call_id: None,
            memory_id: None,
        },
        token_cost: 300,
        content_digest: "sha256:beef".into(),
        citation_label: None,
        content: Some("you are a careful engineer".into()),
    };
    let value = serde_json::to_value(&system).unwrap();
    assert_eq!(value["content"], "you are a careful engineer");

    let back: AgentEvent = serde_json::from_str(&value.to_string()).unwrap();
    match back {
        AgentEvent::BlockRegistered { kind, content, .. } => {
            assert_eq!(kind, BlockKind::SystemPrefix);
            assert_eq!(content.as_deref(), Some("you are a careful engineer"));
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn step_manifest_preserves_block_order_and_the_effective_budget() {
    let event = AgentEvent::StepManifest {
        turn_instance: 1,
        step: 3,
        call_seq: 0,
        role: ModelCallRole::Worker,
        provider: "anthropic".into(),
        model: "claude-opus".into(),
        blocks: vec![
            ManifestEntry {
                block_id: "blk_sys".into(),
                cache_zone: CacheZone::StablePrefix,
                token_cost: 1200,
                resident_since_step: 0,
                message_index: 0,
                call_id: None,
            },
            ManifestEntry {
                block_id: "blk_tail".into(),
                cache_zone: CacheZone::Volatile,
                token_cost: 90,
                resident_since_step: 3,
                message_index: 3,
                call_id: Some("call_7".into()),
            },
        ],
        effective_budget_tokens: 136_363,
        calibration_factor: 1.1,
        estimated_input_tokens: 1290,
        compiled_frame: None,
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["type"], "step_manifest");
    // Order is load-bearing — the manifest IS the wire sequence.
    assert_eq!(value["blocks"][0]["block_id"], "blk_sys");
    assert_eq!(value["blocks"][1]["block_id"], "blk_tail");
    assert_eq!(value["effective_budget_tokens"], 136_363);
    // A lifecycle-off manifest keeps the frame off the wire entirely
    // rather than serializing a null: this is the widest event in the
    // stream, it is emitted once per step, and the default state of the
    // switch that produces the frame is off.
    assert!(value.get("compiled_frame").is_none(), "{value}");
    let back: AgentEvent = serde_json::from_str(&value.to_string()).unwrap();
    match back {
        AgentEvent::StepManifest { blocks, .. } => {
            assert_eq!(blocks.len(), 2);
            assert_eq!(blocks[0].cache_zone, CacheZone::StablePrefix);
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn a_manifest_from_a_pre_frame_stream_still_parses() {
    // Phase 2 (#713) added `compiled_frame`. Every `serde(default)` in this
    // crate owes a hand-written pre-field literal that proves the default;
    // this is that literal for the frame. A stored manifest recorded by any
    // build before the compiled frame existed must still decode, because
    // the journal is read back by newer binaries than wrote it.
    let legacy = r#"{"type":"step_manifest","turn_instance":0,"step":0,"role":"worker",
        "provider":"anthropic","model":"opus","blocks":[],
        "effective_budget_tokens":1,"calibration_factor":1.0,
        "estimated_input_tokens":1}"#;
    let event: AgentEvent = serde_json::from_str(legacy).unwrap();
    match event {
        AgentEvent::StepManifest {
            compiled_frame,
            call_seq,
            ..
        } => {
            assert_eq!(compiled_frame, None, "absent frame ⇒ the lifecycle was off");
            assert_eq!(call_seq, 0);
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn a_manifest_carrying_a_compiled_frame_round_trips() {
    let event = AgentEvent::StepManifest {
        turn_instance: 0,
        step: 0,
        call_seq: 0,
        role: ModelCallRole::Worker,
        provider: "anthropic".into(),
        model: "opus".into(),
        blocks: vec![],
        effective_budget_tokens: 1,
        calibration_factor: 1.0,
        estimated_input_tokens: 1,
        compiled_frame: Some(crate::CompiledContextFrameBuilt {
            compiled_frame_id: "cf_abc".into(),
            frame_hash: "sha256:abc".into(),
        }),
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["compiled_frame"]["compiled_frame_id"], "cf_abc");
    assert_eq!(value["compiled_frame"]["frame_hash"], "sha256:abc");
    let back: AgentEvent = serde_json::from_str(&value.to_string()).unwrap();
    match back {
        AgentEvent::StepManifest { compiled_frame, .. } => {
            assert_eq!(compiled_frame.unwrap().compiled_frame_id, "cf_abc");
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn a_manifest_entry_from_a_pre_occurrence_stream_still_parses() {
    // #684 added per-occurrence `call_id`, and #389 `message_index`, to the
    // manifest entry. Every `serde(default)` in this crate owes a
    // hand-written pre-field literal that proves the default — these two
    // landed without one.
    let legacy = r#"{"block_id":"blk_a","token_cost":12,"resident_since_step":0}"#;
    let entry: ManifestEntry = serde_json::from_str(legacy).unwrap();
    assert_eq!(entry.cache_zone, CacheZone::Cacheable);
    assert_eq!(entry.message_index, 0);
    assert_eq!(entry.call_id, None);

    // A non-tool entry keeps the id off the wire entirely rather than
    // serializing a null — the manifest is the widest event in the stream
    // and every omitted key is real tokens on a receipt.
    let wire = serde_json::to_value(&entry).unwrap();
    assert!(wire.get("call_id").is_none(), "{wire}");
}

#[test]
fn context_frame_ref_without_receipt_fields_still_parses() {
    // A recall frame recorded before receipts existed carries no block_id
    // or content_digest — it must still deserialize (additive contract).
    let old = r#"{"citation_label":"auth module","source":"stella-context","token_cost":42}"#;
    let frame: ContextFrameRef = serde_json::from_str(old).unwrap();
    assert_eq!(frame.citation_label, "auth module");
    assert!(frame.block_id.is_none());
    assert!(frame.content_digest.is_none());
}

#[test]
fn unknown_block_kind_degrades_to_other_not_a_parse_error() {
    // A newer emitter may name a block kind this reader has never heard of.
    // The additive contract requires it to degrade, not reject the event.
    let kind: BlockKind = serde_json::from_str("\"some_future_kind\"").unwrap();
    assert_eq!(kind, BlockKind::Other);
    let zone: CacheZone = serde_json::from_str("\"some_future_zone\"").unwrap();
    assert_eq!(zone, CacheZone::Other);
}

#[test]
fn type_tag_matches_the_serde_type_wire_tag() {
    // `type_tag()` must return exactly the `"type"` string serde writes,
    // since both come from the same `snake_case` variant name. The match's
    // exhaustiveness is compiler-enforced (a new variant cannot escape a
    // tag), so this only pins that the hand-written strings are correct —
    // cross-checked against serde for a representative sample, weighted to
    // the recently added variants most prone to a copy-paste tag.
    let sample = vec![
        AgentEvent::Stage {
            name: StageKind::Triage,
        },
        AgentEvent::Text { text: "hi".into() },
        AgentEvent::TextDelta { delta: "h".into() },
        AgentEvent::Reasoning { delta: "r".into() },
        AgentEvent::SpeculationDiscarded {
            call_id: "c".into(),
            name: "n".into(),
            reason: "attempt_failed".into(),
        },
        AgentEvent::Retry {
            attempt: 1,
            reason: "x".into(),
        },
        AgentEvent::Steered { text: "s".into() },
        AgentEvent::LoopDetected {
            turn_instance: 1,
            kind: "exact_repeat".into(),
            pattern: vec!["read".into()],
            repeats: 2,
            evidence: "e".into(),
            aborted: false,
        },
        AgentEvent::BudgetDenied {
            scope: BudgetScope::Turn,
            spent_usd: 1.0,
            limit_usd: 0.5,
            mode: BudgetMode::Enforced,
        },
        AgentEvent::RetriesExhausted {
            turn_instance: 1,
            attempts: 3,
            reasons: vec!["t".into()],
            retryable: true,
        },
        AgentEvent::PolicyDecision {
            kind: PolicyKind::Blocked,
            subject: "write_file".into(),
            outcome: "deny".into(),
        },
        AgentEvent::BudgetTick {
            spent_usd: 0.1,
            limit_usd: None,
            mode: BudgetMode::Off,
            session_spent_usd: None,
            session_limit_usd: None,
        },
        AgentEvent::UsageIncomplete {
            role: ModelCallRole::Worker,
            provider: "z".into(),
            model: "m".into(),
            reason: UsageIncompleteReason::Timeout,
            duration_ms: 1,
            retries: None,
            partial: None,
        },
        AgentEvent::GoalVerdict {
            round: 1,
            met: true,
            reasoning: "ok".into(),
            cost_usd: 0.0,
        },
        AgentEvent::ProviderFallback {
            from: "a".into(),
            to: "b".into(),
            reason: "r".into(),
        },
        AgentEvent::Commit {
            sha: "abc".into(),
            message: "m".into(),
        },
        AgentEvent::Pr {
            url: "u".into(),
            status: PrStatus::Open,
            number: None,
            ci: None,
        },
        AgentEvent::Error {
            message: "m".into(),
            retryable: false,
        },
        AgentEvent::Complete {
            model: "m".into(),
            cost_usd: 0.0,
        },
    ];
    for event in &sample {
        let value = serde_json::to_value(event).unwrap();
        let wire = value
            .get("type")
            .and_then(|tag| tag.as_str())
            .unwrap_or_else(|| panic!("event has no string `type` tag: {event:?}"));
        assert_eq!(
            event.type_tag(),
            wire,
            "type_tag disagrees with the serde wire tag for {event:?}"
        );
    }
    // Pin two exact tags so a wholesale serde-rename change is caught too.
    assert_eq!(
        AgentEvent::TextDelta {
            delta: String::new()
        }
        .type_tag(),
        "text_delta"
    );
    assert_eq!(
        AgentEvent::SpeculationDiscarded {
            call_id: String::new(),
            name: String::new(),
            reason: String::new(),
        }
        .type_tag(),
        "speculation_discarded"
    );
}

// ---- forward compatibility: events from a newer stella ----------------

#[test]
fn an_unrecognized_type_tag_degrades_to_unknown_rather_than_failing() {
    // The whole point of the change. A reader built before this variant
    // existed must not fail the line — it must keep going.
    let line = r#"{"type":"quantum_entangled","turn":7,"nested":{"a":[1,2]}}"#;
    let event: AgentEvent = serde_json::from_str(line).expect("future event must parse");
    let AgentEvent::Unknown {
        event_type,
        payload,
    } = &event
    else {
        panic!("expected Unknown, got {event:?}");
    };
    assert_eq!(event_type, "quantum_entangled");
    assert_eq!(payload["turn"], 7);
    assert_eq!(payload["nested"]["a"][1], 2);
    assert!(event.is_unknown());
}

#[test]
fn an_unknown_event_round_trips_without_losing_data() {
    // A recorder or proxy must be able to pass a future event through
    // without corrupting it. Compare parsed values, not raw bytes: object
    // key order is not preserved (and is not meaningful).
    let line = r#"{"type":"from_the_future","alpha":1,"beta":["x",null,true]}"#;
    let event: AgentEvent = serde_json::from_str(line).unwrap();
    let reserialized = serde_json::to_string(&event).unwrap();

    let before: Value = serde_json::from_str(line).unwrap();
    let after: Value = serde_json::from_str(&reserialized).unwrap();
    assert_eq!(before, after, "round-trip changed the event's content");
    // The tag must survive: it lives only inside `payload`.
    assert_eq!(after["type"], "from_the_future");
}

#[test]
fn a_known_tag_with_a_malformed_body_is_still_a_hard_error() {
    // The load-bearing negative test. Forward compatibility is scoped to
    // *unrecognized tags only*; a recognized tag whose body does not fit
    // is an encoder bug or a corrupt record, and must stay loud. If this
    // ever degrades to Unknown, corruption becomes indistinguishable from
    // a version skew and the store's `skipped` counter starts lying.
    let err = serde_json::from_str::<AgentEvent>(r#"{"type":"text"}"#)
        .expect_err("`text` without `delta` must not parse");
    assert!(
        !format!("{err}").is_empty(),
        "a known tag with a bad body must produce a real error"
    );

    // Same for a wrong field type under a known tag.
    assert!(
        serde_json::from_str::<AgentEvent>(r#"{"type":"retry","attempt":"one","reason":"x"}"#)
            .is_err(),
        "`retry.attempt` is a u32; a string must not be laundered into Unknown"
    );

    // And an object with no tag at all keeps the derived error path.
    assert!(serde_json::from_str::<AgentEvent>(r#"{"delta":"hi"}"#).is_err());
}

#[test]
fn every_known_type_tag_resolves_to_a_typed_variant() {
    // Proves the variant→tag list in `agent_event_tags!` has no typo.
    //
    // Combined with two structural facts this is airtight: the generated
    // match is exhaustive (so every variant is listed) and duplicate arms
    // would be unreachable (so each is listed once) — meaning
    // `KNOWN_TYPE_TAGS.len()` equals the variant count. If every listed tag
    // is additionally a *real* serde name, as asserted below, the mapping is
    // a bijection and no real tag can be missing from the list. A missing
    // one would silently demote all of its events to `Unknown`.
    //
    // The probe is a bare `{"type": tag}`, and today every variant has at
    // least one required field, so it always errors. That is expected — the
    // assertion is on WHICH error. `missing field ...` proves serde routed
    // the tag to a variant; `unknown variant ...` proves it did not, which
    // is exactly the typo this test exists to catch. Asserting only on the
    // `Ok` arm would assert nothing at all.
    for tag in KNOWN_TYPE_TAGS {
        let probe = serde_json::json!({ "type": tag });
        match serde_json::from_value::<AgentEvent>(probe) {
            Ok(event) => assert!(
                !event.is_unknown(),
                "`{tag}` is in KNOWN_TYPE_TAGS but deserialized as Unknown — \
                 the tag string does not match any serde variant name"
            ),
            Err(err) => assert!(
                !err.to_string().contains("unknown variant"),
                "`{tag}` is in KNOWN_TYPE_TAGS but serde has no variant by \
                 that name, so every event carrying it decodes as Unknown: {err}"
            ),
        }
    }

    let mut sorted = KNOWN_TYPE_TAGS.to_vec();
    sorted.sort_unstable();
    let before = sorted.len();
    sorted.dedup();
    assert_eq!(before, sorted.len(), "duplicate tag in KNOWN_TYPE_TAGS");
}

#[test]
fn type_tag_of_an_unknown_event_is_the_preserved_wire_tag() {
    // `type_tag()` tells the truth even for an event it cannot decode, so
    // logs and metrics can name a future event correctly.
    let event: AgentEvent = serde_json::from_str(r#"{"type":"newer_thing"}"#).unwrap();
    assert_eq!(event.type_tag(), "newer_thing");
    assert!(!KNOWN_TYPE_TAGS.contains(&event.type_tag()));
}

#[test]
fn a_literal_unknown_tag_is_not_privileged() {
    // `Unknown` is `serde(skip)`, so it has no wire tag of its own. An
    // event literally tagged `"unknown"` is just another unrecognized tag
    // and must round-trip as one rather than being mistaken for the
    // fallback variant's own encoding.
    let line = r#"{"type":"unknown","event_type":"spoof","payload":{}}"#;
    let event: AgentEvent = serde_json::from_str(line).unwrap();
    assert_eq!(event.type_tag(), "unknown");

    // It round-trips as the opaque object it is — the decoy `event_type`
    // and `payload` keys stay ordinary payload data, not the variant's
    // own fields.
    let after: Value = serde_json::from_str(&serde_json::to_string(&event).unwrap()).unwrap();
    assert_eq!(after, serde_json::from_str::<Value>(line).unwrap());
    assert_eq!(after["event_type"], "spoof");
}

#[test]
fn a_known_event_wire_format_is_unchanged_by_the_fallback() {
    // The hand-written codec must be a pass-through for known variants —
    // no tag rename, no extra wrapper, no reordering.
    let event = AgentEvent::Text {
        text: "hello".into(),
    };
    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        r#"{"type":"text","text":"hello"}"#
    );
    let back: AgentEvent = serde_json::from_str(r#"{"type":"text","text":"hello"}"#).unwrap();
    assert!(matches!(back, AgentEvent::Text { text } if text == "hello"));
}

mod tag_table;
