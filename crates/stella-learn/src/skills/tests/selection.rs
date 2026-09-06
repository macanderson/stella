//! Selection tests for [`crate::skills`] that the parent module has no room
//! for. `skills/tests.rs` is close to the file-size ceiling, and the rule is
//! to split rather than grow.

use super::super::*;
use super::skill;

/// The panic `floor_char_boundary` exists to stop: a body of CJK or emoji cut
/// at a byte offset inside a character.
///
/// The other truncation tests are ASCII. Every byte there is a boundary, so
/// the walk-back never runs. The result must be a valid `str`, since a bad
/// slice would have panicked. It must also be a *prefix* of the input, so the
/// cut goes back and never forward.
#[test]
fn a_multi_byte_body_is_cut_at_a_character_boundary() {
    for glyph in ["漢", "🙂", "é"] {
        let text: String = std::iter::repeat_n(glyph, 4000).collect();
        // A budget the byte cut cannot land on cleanly for a 3-byte or
        // 4-byte glyph. That is the case the walk-back is for.
        let cut = truncate_to_tokens(&text, 100);
        let body = cut
            .strip_suffix(SKILL_BODY_TRUNCATION_MARKER)
            .expect("a body over budget is marked as truncated");
        assert!(
            text.starts_with(body),
            "{glyph} truncation moved forward past the budget: {body:?}"
        );
        assert!(
            !body.is_empty(),
            "{glyph} truncation walked all the way back to nothing"
        );
    }

    let ascii = "a".repeat(4000);
    assert!(
        truncate_to_tokens(&ascii, 100).starts_with(&ascii[..10]),
        "the ASCII path is unchanged by the boundary walk"
    );
}

/// Two skills that score the same are ordered by name, low to high.
///
/// `auto_created_skill_wins_the_tie_break` asserts unequal scores, so it never
/// reaches `then_with`. A real workspace makes equal scores all the time: two
/// skills over one vocabulary. With no total order the output is stable at
/// best, which means the input order decides it.
#[test]
fn skills_with_the_same_score_are_ordered_by_name() {
    // Same description and same body. The only difference is the one term
    // each name adds. Neither term is in the prompt, and there is one apiece,
    // so both coverage sums divide by the same number.
    let twin = |name: &str| Skill {
        name: name.to_string(),
        description: "prefer tables for comparisons".to_string(),
        domains: Vec::new(),
        body: "prefer tables for comparisons in every answer".to_string(),
        source_path: format!("{name}.md"),
        origin: SkillOrigin::Workspace,
        contributed_by: None,
    };

    for order in [["zulu", "alpha"], ["alpha", "zulu"]] {
        let skills = vec![twin(order[0]), twin(order[1])];
        let selected = select_skills(
            &skills,
            "prefer tables for comparisons please",
            &[],
            &SelectionConfig::default(),
        );
        assert_eq!(selected.len(), 2, "both skills clear the floor");
        assert_eq!(
            selected[0].score, selected[1].score,
            "the fixture really is a tie"
        );
        assert_eq!(
            [
                selected[0].skill.name.as_str(),
                selected[1].skill.name.as_str()
            ],
            ["alpha", "zulu"],
            "input order {order:?} must not decide the output order"
        );
    }
}

/// `SkillSelection::over_top_k` holds what `max_skills` cut, in the same
/// order, and `selected` matches `select_skills` exactly.
///
/// `steering::adapt` reads that tail to name an eviction. Nothing pinned the
/// split, so a `split_off` at the wrong index would move a survivor into the
/// tail and nothing would notice.
#[test]
fn over_top_k_holds_exactly_what_max_skills_cut() {
    let skills: Vec<Skill> = ["alpha", "bravo", "charlie", "delta"]
        .iter()
        .map(|name| skill(name, "format sql queries", &["sql"], SkillOrigin::Workspace))
        .collect();
    let config = SelectionConfig {
        max_skills: 2,
        ..SelectionConfig::default()
    };
    let reported = select_skills_reporting(&skills, "format sql", &["sql".to_string()], &config);

    assert_eq!(reported.selected.len(), 2, "top-k is the cut, not a hint");
    assert_eq!(
        reported.over_top_k.len(),
        2,
        "every skill that cleared the floor and lost a seat is kept"
    );
    assert_eq!(
        reported.selected,
        select_skills(&skills, "format sql", &["sql".to_string()], &config),
        "the survivors are what the plain call returns"
    );
    let tail_scores: Vec<f64> = reported.over_top_k.iter().map(|s| s.score).collect();
    assert!(
        tail_scores.windows(2).all(|w| w[0] >= w[1]),
        "the tail keeps the descending order: {tail_scores:?}"
    );
    assert!(
        reported.selected.last().unwrap().score >= reported.over_top_k[0].score,
        "nothing in the tail outranks a survivor"
    );

    let roomy = SelectionConfig {
        max_skills: 10,
        ..SelectionConfig::default()
    };
    assert!(
        select_skills_reporting(&skills, "format sql", &["sql".to_string()], &roomy)
            .over_top_k
            .is_empty(),
        "a cut that removes nothing reports nothing"
    );
}
