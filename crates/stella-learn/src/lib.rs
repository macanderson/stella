// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What the agent learns, and what steers it.
//!
//! Six trees live here. `skills` is the skill catalog. `rules` is the rule
//! engine. `mining` is the text miner both of them share. `comparison` and
//! `self_tuning` answer whether arm B beat arm A. `holdout` is the schedule
//! that makes the arm they compare against. `redact` strips secrets out of
//! text.
//!
//! No I/O. Every entry point is a plain function. A caller reads the file
//! and hands the text in. `RuleSource` and `SkillSource` are the ports for
//! that, and `stella-cli` implements them.
//!
//! This lived in `stella-core` until the engine turned out not to use it.
//! Only one part stayed: how a skill is invoked, now
//! `stella_core::skill_invocation`. `stella-protocol` is the one workspace
//! crate this uses. Three small parts sit down there so the engine can read
//! them too: the token estimate the skill budget spends, the header parser,
//! and the glob a rule guard matches on.

pub mod comparison;
pub mod holdout;
pub(crate) mod mining;
pub mod redact;
pub mod rules;
pub mod self_tuning;
pub mod skills;

pub use rules::{
    EvidenceSource, MineConfig, RawObservation, RuleCandidate, RuleEvidence, decide_promotion,
    mine_candidates,
};
pub use rules::{
    GuardCheck, LoadRulesOptions, ProposedAction, Rule, RuleGuard, RuleSource, evaluate_guards,
    load_rules,
};
pub use skills::{
    AutoCreateConfig, AutoCreateDecision, AutoCreateSkip, InstallDecision, LoadSkillsOptions,
    SelectedSkill, SelectionConfig, Skill, SkillCandidate, SkillInstallProposal, SkillMineConfig,
    SkillObservation, SkillOrigin, SkillSource, decide_auto_creation, load_skills,
    mine_skill_candidates, render_skills_section, select_skills,
};
