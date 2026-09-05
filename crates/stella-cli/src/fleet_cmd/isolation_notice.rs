// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One line in the run header. It says where this run's workers will write.
//!
//! A fleet task gets a git worktree of its own. A plan can ask for the shared
//! root instead (ADR 0027). Each answer is right for some plan.
//!
//! The wrong one costs, and nothing says so later. Two workers in one
//! checkout can wipe out each other's edits. Two workers in their own trees
//! each pay for a cold build cache and a merge.
//!
//! So the run says which it is up front. Reading the plan file is not the
//! same. A plan may name nothing at all. Then a default picks, and nobody
//! reads a default.

use stella_fleet::{Isolation, Plan};

/// How this plan's workers are housed, in one line for the run header.
pub(super) fn line(plan: &Plan) -> String {
    let isolated = plan
        .tasks
        .iter()
        .filter(|t| t.isolation == Isolation::Isolated)
        .count();
    let total = plan.tasks.len();
    let shared = total - isolated;
    match (isolated, shared) {
        (_, 0) => "a git worktree per task — no two workers share a checkout".to_string(),
        (0, _) => "every task in this checkout — the plan asked for the shared tree".to_string(),
        _ => format!(
            "{isolated} task(s) in a worktree of their own, {shared} in this checkout \
             — the plan asked for the shared tree"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_fleet::Task;

    /// **Witness.** A plan that names no isolation reports worktrees. That
    /// is what it gets (ADR 0027).
    #[test]
    fn a_plan_naming_nothing_reports_a_worktree_per_task() {
        let plan = Plan::new(vec![Task::new("a", "a", "p"), Task::new("b", "b", "p")]);
        assert!(line(&plan).contains("a git worktree per task"));
    }

    #[test]
    fn a_shared_tree_plan_says_the_plan_asked_for_it() {
        let plan = Plan::new(vec![Task::new("a", "a", "p").shared_tree()]);
        let said = line(&plan);
        assert!(said.contains("this checkout"), "{said}");
        assert!(said.contains("the plan asked"), "{said}");
    }

    #[test]
    fn a_mixed_plan_counts_both_sides() {
        let plan = Plan::new(vec![
            Task::new("a", "a", "p"),
            Task::new("b", "b", "p").shared_tree(),
            Task::new("c", "c", "p").shared_tree(),
        ]);
        let said = line(&plan);
        assert!(said.starts_with("1 task(s) in a worktree"), "{said}");
        assert!(said.contains("2 in this checkout"), "{said}");
    }

    /// An empty plan never reaches the header. `load_plan` refuses one. The
    /// arm still has to be total, not a panic.
    #[test]
    fn an_empty_plan_does_not_panic() {
        assert!(!line(&Plan::new(vec![])).is_empty());
    }
}
