// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One prompt, one skill file, the same bytes.
//!
//! The string below is not new. It was copied out of `stella-cli`'s golden
//! block test, which pins this exact fixture. That golden predates the
//! move, so the string is what the engine wrote before it. If this test
//! passes, the plane still writes it.
//!
//! Four steps run to get there: parse the file, score it, cut it to the
//! budget, render it. They all crossed a crate line together. If any one of
//! them changed, the model would read something new, and every `contains`
//! check in the suite would still pass.

use stella_learn::skills::{
    SelectionConfig, render_skills_section, select_skills, skill_from_file,
};

/// The fixture `SKILL.md` `golden_block`'s temp directory writes.
const SKILL_FILE: &str =
    "---\nname: reviewer\ndescription: database review\n---\nALWAYS_REVIEW_DATABASES";

/// The prompt `golden_block` recalls with.
const PROMPT: &str = "review the database migrations";

/// The skills section of `golden_block`'s pinned block, byte for byte.
const PINNED_SECTION: &str = "\n## Applicable skills (selected for this task — apply the relevant ones)\n\n### reviewer\ndatabase review\n\nALWAYS_REVIEW_DATABASES\n";

#[test]
fn one_prompt_through_the_moved_plane_renders_the_bytes_the_engine_used_to_render() {
    let skill = skill_from_file(".stella/skills/reviewer/SKILL.md", SKILL_FILE)
        .expect("the fixture skill file parses");

    let selected = select_skills(
        std::slice::from_ref(&skill),
        PROMPT,
        &[],
        &SelectionConfig::default(),
    );

    assert_eq!(
        selected
            .iter()
            .map(|s| s.skill.name.as_str())
            .collect::<Vec<_>>(),
        vec!["reviewer"],
        "the prompt still selects the skill it selected before the move"
    );
    assert_eq!(
        render_skills_section(&selected),
        PINNED_SECTION,
        "the rendered section moved off the bytes stella-cli's golden pinned"
    );
}
