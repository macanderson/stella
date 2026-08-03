//! The behavior-compatibility contract the typed adaptive loop must honor.
//!
//! Adaptive-context spec §8: replacing the shipped lexical skills loop with a
//! typed one is **a migration, not a build**. A typed loop that regresses
//! forget/restore is a worse product than the lexical one it replaces. These
//! tests pin the pure half of that obligation — the half `stella-core` owns —
//! and they were written **against the pre-migration code**, so a green run
//! here after the migration means the observable behavior did not move.
//!
//! Pinned here:
//!
//! * **deterministic, stable candidate identity** — the exact `<slug>-<hash8>`
//!   for a fixed lesson, as a literal. `mining_is_deterministic_across_reruns`
//!   already proves *self*-consistency within one process; that is not the same
//!   guarantee. A tokenizer or slug tweak keeps every rerun agreeing with every
//!   other rerun while quietly renaming every skill file a user already has, and
//!   the rename is invisible until a second file appears alongside the first.
//!   Only a literal catches it.
//! * **byte-identical skill files** — the exact `SKILL.md` bytes for a fixed
//!   candidate. This is the gate criterion "existing skill files are
//!   byte-identical after migration", reduced to the one function that decides
//!   it.
//! * **the per-session cap** and **never clobbering a hand-edited file**,
//!   including the order they are checked in.
//!
//! The wired half — tombstones on every surface, end-to-end no-clobber, failure
//! isolation — is pinned in `stella-cli`'s `memory::learning::guarantees`,
//! because it needs the loop, the store, and a filesystem.
//!
//! One gate criterion is pinned here as a **known gap, not a guarantee**: see
//! `lexical_miner_counts_events_not_distinct_tasks`.

use super::*;

/// A fixed lesson, used wherever an identity or a rendering is pinned. Its
/// wording must never change — it is half of every literal below.
const PINNED_LESSON: &str =
    "Prefer updating witness-test assertions to match the live renderer output.";

fn pinned_observation(reference: &str, occurred_at: u64) -> SkillObservation {
    SkillObservation {
        text: PINNED_LESSON.to_string(),
        occurred_at,
        domains: vec!["testing".into()],
        salient: false,
        reference: reference.into(),
    }
}

/// The identity a lesson mines to, as a literal.
///
/// `crate::mining` warns in prose that widening its tokenizer "changes every
/// mined id"; this is that warning as a failing test. The consequence is not
/// abstract — the id is the *filename*, so a drifted id means the next mining
/// pass writes a second file for a skill the user already has, and the
/// `already_captured` guard is the only thing between them.
#[test]
fn candidate_identity_is_pinned_to_a_literal() {
    let observations = (0..3)
        .map(|i| pinned_observation(&format!("reflection:{i}"), 100 + i))
        .collect();
    let mined = mine_skill_candidates(observations, &[], &SkillMineConfig::default());
    assert_eq!(mined.len(), 1, "one cluster, one candidate: {mined:#?}");
    assert_eq!(
        mined[0].name, "prefer-updating-witness-test-assertions-e2010443",
        "the mined identity moved — every skill file already on disk is now \
         orphaned, and the next mining pass will write a duplicate beside it"
    );
}

/// The exact bytes of a mined `SKILL.md`.
///
/// The gate criterion is "existing skill files are byte-identical after
/// migration". A file already on disk is only byte-identical to a re-render if
/// this function's output has not moved, so the whole criterion reduces to this
/// literal. Frontmatter key order, the `origin: auto` marker, the blank-line
/// placement, and the `## Evidence` line format are all load-bearing: the
/// parser round-trips them, and a user's `git diff` sees every one.
#[test]
fn rendered_skill_bytes_are_pinned() {
    let candidate = SkillCandidate {
        name: "prefer-updating-witness-test-assertions-e2010443".into(),
        description: "Learned from 2 observations.".into(),
        domains: vec!["testing".into(), "cli".into()],
        body: PINNED_LESSON.into(),
        occurrences: 2,
        salient: false,
        evidence: vec![
            SkillEvidence {
                reference: "reflection:101".into(),
                occurred_at: 101,
                snippet: PINNED_LESSON.into(),
            },
            SkillEvidence {
                reference: "reflection:100".into(),
                occurred_at: 100,
                snippet: PINNED_LESSON.into(),
            },
        ],
        score: 20.0,
    };
    assert_eq!(
        render_skill_markdown(&candidate),
        "---\n\
         name: prefer-updating-witness-test-assertions-e2010443\n\
         description: Learned from 2 observations.\n\
         domains: [testing, cli]\n\
         origin: auto\n\
         ---\n\
         \n\
         Prefer updating witness-test assertions to match the live renderer output.\n\
         \n\
         ## Evidence\n\
         \n\
         - `reflection:101` (observed at 101): Prefer updating witness-test assertions to match the live renderer output.\n\
         - `reflection:100` (observed at 100): Prefer updating witness-test assertions to match the live renderer output.\n"
    );
}

/// The per-session cap is checked **before** the no-clobber guard, and the
/// order is observable: a capped session reports `SessionCapReached` even for a
/// candidate whose file already exists. Anything that reorders these two
/// changes which reason a caller logs, and a "file exists" line for a session
/// that never got to look at the filesystem is a confusing lie.
///
/// Every call here passes a candidate that has already cleared #1067's eval
/// gate, which is what keeps this a test about the *ordering* rather than one
/// that would pass for the wrong reason once the gate refuses everything. The
/// gate's own placement — last, after both of these — is pinned separately by
/// `the_eval_gate_is_checked_after_the_cap_and_the_no_clobber_guard`.
#[test]
fn the_session_cap_is_checked_before_the_no_clobber_guard() {
    use super::appraisal::EvalEvidence::MeasuredLift as PROVEN;
    let candidate = SkillCandidate {
        name: "already-there".into(),
        description: "d".into(),
        domains: vec![],
        body: "b".into(),
        occurrences: 3,
        salient: false,
        evidence: vec![],
        score: 30.0,
    };
    let existing = vec![".stella/skills/already-there.md".to_string()];
    let config = AutoCreateConfig::default();
    assert_eq!(config.max_per_session, 2, "the shipped cap is 2");

    // At the cap: capped, even though the file also exists.
    assert_eq!(
        decide_auto_creation(&candidate, ".stella/skills", &existing, 2, PROVEN, &config),
        AutoCreateDecision::Skip {
            reason: AutoCreateSkip::SessionCapReached { cap: 2 }
        }
    );
    // Under the cap: now the no-clobber guard is what stops it.
    assert_eq!(
        decide_auto_creation(&candidate, ".stella/skills", &existing, 0, PROVEN, &config),
        AutoCreateDecision::Skip {
            reason: AutoCreateSkip::FileExists {
                path: ".stella/skills/already-there.md".into()
            }
        }
    );
    // Neither applies: create.
    assert_eq!(
        decide_auto_creation(&candidate, ".stella/skills", &[], 1, PROVEN, &config),
        AutoCreateDecision::Create {
            path: ".stella/skills/already-there.md".into()
        }
    );
}

/// #1067's gate is the **last** rung, so neither of the two refusals above is
/// displaced by it. Pinned here beside the ordering it extends, because a gate
/// that jumped ahead of the cap would change which reason a capped session
/// reports — and that reason is what an operator acts on.
#[test]
fn the_eval_gate_is_checked_after_the_cap_and_the_no_clobber_guard() {
    use super::appraisal::EvalEvidence;

    let candidate = SkillCandidate {
        name: "already-there".into(),
        description: "d".into(),
        domains: vec![],
        body: "b".into(),
        occurrences: 3,
        salient: false,
        evidence: vec![],
        score: 30.0,
    };
    let existing = vec![".stella/skills/already-there.md".to_string()];
    // Armed explicitly: the gate ships off (see
    // `AutoCreateConfig::require_measured_lift`), and what this pins is where
    // it sits in the order once something turns it on.
    let config = AutoCreateConfig {
        require_measured_lift: true,
        ..AutoCreateConfig::default()
    };

    // Unevaluated AND at the cap: the cap still wins.
    assert_eq!(
        decide_auto_creation(
            &candidate,
            ".stella/skills",
            &existing,
            2,
            EvalEvidence::Unevaluated,
            &config
        ),
        AutoCreateDecision::Skip {
            reason: AutoCreateSkip::SessionCapReached { cap: 2 }
        }
    );
    // Unevaluated AND the file exists: no-clobber still wins.
    assert_eq!(
        decide_auto_creation(
            &candidate,
            ".stella/skills",
            &existing,
            0,
            EvalEvidence::Unevaluated,
            &config
        ),
        AutoCreateDecision::Skip {
            reason: AutoCreateSkip::FileExists {
                path: ".stella/skills/already-there.md".into()
            }
        }
    );
    // Only once neither applies does the gate get to speak.
    assert_eq!(
        decide_auto_creation(
            &candidate,
            ".stella/skills",
            &[],
            0,
            EvalEvidence::Unevaluated,
            &config
        ),
        AutoCreateDecision::Skip {
            reason: AutoCreateSkip::AwaitingEvaluation {
                evidence: EvalEvidence::Unevaluated
            }
        }
    );
}

/// **A known gap, pinned as such — not a guarantee being preserved.**
///
/// Spec §7 makes counting *distinct tasks* rather than raw events a load-bearing
/// anti-poisoning rule: "thirty repetitions inside one task must never satisfy a
/// three-task threshold." The shipped lexical miner cannot honor that, because
/// [`SkillObservation`] carries no task identity at all — only a free-text
/// `reference`. So thirty repetitions inside one task mine a candidate today.
///
/// This test asserts the **current, wrong** behavior on purpose. It is the
/// baseline the typed path is measured against, and flipping it is a deliberate
///, visible act rather than a silent fix folded into a migration commit.
#[test]
fn lexical_miner_counts_events_not_distinct_tasks() {
    let thirty_in_one_task: Vec<SkillObservation> = (0..30)
        .map(|i| pinned_observation("trace:the-one-and-only-task", 100 + i))
        .collect();
    let mined = mine_skill_candidates(thirty_in_one_task, &[], &SkillMineConfig::default());
    assert_eq!(
        mined.len(),
        1,
        "documenting the gap: thirty repetitions inside ONE task currently \
         satisfy the three-occurrence threshold, because the lexical \
         observation has no task identity to count"
    );
    assert_eq!(mined[0].occurrences, 30);
}

/// The threshold itself, as shipped. Three occurrences, or one salient
/// observation. Both halves are gate-relevant: the typed path reuses this
/// miner, so a threshold change would move the promotion bar for every user
/// without touching a settings file.
#[test]
fn the_shipped_thresholds_are_three_occurrences_or_one_salient() {
    let config = SkillMineConfig::default();
    assert_eq!(config.min_occurrences, 3);
    assert_eq!(config.min_similarity, 0.5);
    assert_eq!(config.limit, 10);

    let two = (0..2)
        .map(|i| pinned_observation(&format!("reflection:{i}"), 100 + i))
        .collect();
    assert!(
        mine_skill_candidates(two, &[], &config).is_empty(),
        "two occurrences are below the bar"
    );

    let mut one_salient = vec![pinned_observation("reflection:0", 100)];
    one_salient[0].salient = true;
    assert_eq!(
        mine_skill_candidates(one_salient, &[], &config).len(),
        1,
        "one salient observation clears the bar on its own"
    );
}

/// A candidate that restates an existing skill is dropped before it can be
/// written. This is the miner's own duplicate guard, distinct from the
/// no-clobber guard: it works on *text*, so it also catches a re-worded lesson
/// that would mint a different filename and slip past a path check.
#[test]
fn a_candidate_matching_an_existing_skill_is_dropped() {
    let existing = vec![Skill {
        name: "witness-assertions".into(),
        description: "Prefer updating witness-test assertions".into(),
        body: PINNED_LESSON.into(),
        domains: vec!["testing".into()],
        source_path: ".stella/skills/witness-assertions.md".into(),
        origin: SkillOrigin::AutoCreated,
    }];
    let observations = (0..3)
        .map(|i| pinned_observation(&format!("reflection:{i}"), 100 + i))
        .collect();
    assert!(
        mine_skill_candidates(observations, &existing, &SkillMineConfig::default()).is_empty(),
        "the lesson is already captured by a skill on disk"
    );
}
