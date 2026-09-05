// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One row of the trial ledger, and the key it is filed under.
//!
//! Memory records, skills, and mined rules all change on evidence, and each
//! of them asks one question. Does putting this in front of the model change
//! how the turn goes?
//!
//! [`crate::comparison`] answers it and [`crate::skills::appraisal`] drives
//! it. Neither knows what an arm names, and [`crate::holdout`] does not know
//! what an id names either. That is what lets one engine serve all three.
//!
//! A row is what the answer is built from, so a row has to name the artifact
//! it is about. [`ArtifactKind`] plus an id is a key all three fit.
//!
//! No I/O here. A row is a plain serde shape, and the caller owns the file.

use serde::{Deserialize, Serialize};

use crate::skills::appraisal::SkillTrial;

/// Which of the three surfaces a trial is evidence about.
///
/// Small on purpose. A kind earns a place here when something can be held
/// back and measured, not when it merely exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// A memory the recall query put in front of the model.
    Memory,
    /// A published context record — what a mined rule becomes.
    Rule,
    /// A skill the selector injected.
    Skill,
}

impl ArtifactKind {
    /// The `snake_case` word the ledger writes, and the one a reader sees.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Rule => "rule",
            Self::Skill => "skill",
        }
    }
}

/// One ledger row: which artifact the trial is about, and the trial.
///
/// The trial is flattened, so a row stays one flat JSON object and the
/// fields an older build wrote keep the names it wrote them under.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactTrial {
    /// Which surface [`Self::id`] names.
    ///
    /// A row with no kind on it reads as [`ArtifactKind::Skill`], because a
    /// build that wrote no kind wrote skill rows and nothing else. The ledger
    /// is append-only and a workspace has months of it on disk, so a default
    /// here is what saves a pass over the file.
    #[serde(default = "skill_kind")]
    pub kind: ArtifactKind,
    /// The artifact's stable id: a skill slug, a memory id, a record handle.
    ///
    /// `skill` is what the older build called this field, so it is read as an
    /// alias. New rows are written under `id`.
    #[serde(alias = "skill")]
    pub id: String,
    /// The turn, and whether the artifact was part of it.
    #[serde(flatten)]
    pub trial: SkillTrial,
}

/// What a row with no kind meant. See [`ArtifactTrial::kind`].
fn skill_kind() -> ArtifactKind {
    ArtifactKind::Skill
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_tuning::TaskOutcome;

    fn trial(selected: bool) -> SkillTrial {
        SkillTrial {
            task: "live-window".into(),
            selected,
            outcome: TaskOutcome {
                succeeded: true,
                cost_usd: 0.1,
                tokens: 1_000,
                retries: 0,
            },
            turns: 4,
        }
    }

    /// A row a build before this one wrote still loads, and it loads as the
    /// skill row it was. The ledger is append-only, so this is the only way
    /// a workspace keeps the window it already paid for.
    #[test]
    fn a_row_written_before_kinds_existed_reads_as_a_skill() {
        let old = r#"{"skill":"prefer-tables","task":"live-window","selected":true,
            "outcome":{"succeeded":true,"cost_usd":0.1,"tokens":1000,"retries":0},"turns":4}"#;
        let row: ArtifactTrial = serde_json::from_str(old).expect("a skill-only row still loads");
        assert_eq!(row.kind, ArtifactKind::Skill);
        assert_eq!(row.id, "prefer-tables");
        assert_eq!(row.trial, trial(true));
    }

    /// A new row names its kind and its id, and reads back the same.
    #[test]
    fn a_row_round_trips_through_json() {
        let row = ArtifactTrial {
            kind: ArtifactKind::Memory,
            id: "nod_7f".into(),
            trial: trial(false),
        };
        let raw = serde_json::to_string(&row).expect("a row serializes");
        assert!(raw.contains(r#""kind":"memory""#), "{raw}");
        assert!(raw.contains(r#""id":"nod_7f""#), "{raw}");
        assert_eq!(
            serde_json::from_str::<ArtifactTrial>(&raw).expect("and reads back"),
            row
        );
    }

    /// The word on the wire and the word a reader sees are one word.
    #[test]
    fn every_kind_writes_the_word_it_names() {
        for kind in [
            ArtifactKind::Memory,
            ArtifactKind::Rule,
            ArtifactKind::Skill,
        ] {
            let raw = serde_json::to_string(&kind).expect("a kind serializes");
            assert_eq!(raw, format!("\"{}\"", kind.as_str()));
        }
    }
}
