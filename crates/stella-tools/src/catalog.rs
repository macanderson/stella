//! The canonical tool table — the one place a built-in is declared.
//!
//! Adding a tool used to be a ~6-file edit, and several of those edits were
//! hardcoded counts or duplicated name-lists: registry count pins, the
//! read-only partition, [`crate::custom::RESERVED_NAMES`], and counts in the
//! docs. Parallel PRs each bumped the same integer off the same base, so
//! squash-merging them back-to-back left the count off by N−1 with **no merge
//! conflict** — a plausible-but-wrong number that only CI (or nothing at all,
//! on the docs side) caught after the fact.
//!
//! This module is the fix. Every built-in is declared exactly once in the
//! `catalog!` invocation below, with its read-only flag, its risk grade and
//! its policy group. Everything that used to be duplicated is derived from it:
//!
//! - the registry's expected-name set (`registry.rs` tests),
//! - the read-only partition (same),
//! - [`crate::custom::RESERVED_NAMES`] (aliased straight to [`ALL_NAMES`]),
//! - the per-tool reference pages under `docs/tools/`,
//! - every built-in's [`stella_protocol::ToolContract`] ([`crate::contracts`]).
//!
//! **To add a tool:** register it in
//! [`ToolRegistry::new`](crate::registry::ToolRegistry), add one line here,
//! and introduce it in `stella-cli`'s `tool_steering!()` prose. Nothing else
//! needs a count bumped. Two PRs adding different tools merge to the correct
//! union instead of a wrong integer, and a tool registered but never declared
//! here fails the registry tests by *name*, not by an off-by-one.
//!
//! The third step is enforced too (#3557): the steering block presents each
//! group as a complete list, so a tool it never mentions reads to the model
//! as one that does not exist. `stella-cli`'s `agent::prompt::tool_names`
//! fails by name when a row here goes unintroduced, and both it and
//! `tests/tool_name_liveness_witness.rs` fail when prose points the model at
//! a name in [`RETIRED_TOOL_NAMES`] instead.

use stella_protocol::RiskLevel;

/// What has to be true for a tool to be registered.
///
/// **Availability is not policy.** A variant here names something the
/// *environment* either supplies or does not. Whether a satisfiable tool is
/// *allowed* is [`crate::policy::ToolPolicy`]'s business, driven by
/// `settings.json`.
///
/// Every current row registers unconditionally, so [`Availability::Always`]
/// is the whole enum today. The type survives (rather than collapsing into a
/// boolean or vanishing) because it is the declared seam a conditionally
/// registered tool re-enters through, and because the doc generator
/// (`stella-cli/src/tool_docs.rs`) renders each row's availability from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Availability {
    /// Registers in every session, with no configuration and no prerequisite.
    Always,
}

impl Availability {
    /// Whether the native [`crate::registry::ToolRegistry`] is what registers
    /// this tool. True for every current variant; a future CLI-layered
    /// declaration would answer false.
    pub const fn is_native(self) -> bool {
        matches!(self, Availability::Always)
    }
}

/// One row of the canonical table.
#[derive(Debug, Clone, Copy)]
pub struct ToolEntry {
    /// The exact dispatch name — what `schema().name` returns.
    pub name: &'static str,
    /// The schema's `read_only` flag. The engine parallelizes on this: a
    /// mutating tool marked read-only would race writes.
    pub read_only: bool,
    /// The schema's `speculation_safe` flag — safe to EXECUTE TWICE, the
    /// extra claim speculative execution needs on top of `read_only`
    /// (#923): a failed stream attempt re-announces its prefix on retry.
    /// Meaningless (and kept false) on mutating rows.
    pub speculation_safe: bool,
    /// How bad one honest call is — the governance grade a policy ceiling is
    /// written against (#2716), and a **different question** from
    /// [`Self::read_only`]: that one asks whether the workspace changes, this
    /// one asks what the call costs the world. The two come apart in both
    /// directions, which is why they are separate columns — `task` mutates
    /// nothing in the workspace and spends real money, while `task_create`
    /// mutates a board that dies with the session.
    ///
    /// The rubric, so the column stays consistent as tools are added:
    ///
    /// | Grade | Means |
    /// |---|---|
    /// | [`RiskLevel::Low`] | Observes, or touches only state that dies with the session |
    /// | [`RiskLevel::Medium`] | Bounded and locally undoable: a workspace write, a metered call, a repo-declared command |
    /// | [`RiskLevel::High`] | Leaves the workspace, spends money, or runs something nobody bounded |
    /// | [`RiskLevel::Destructive`] | The agent cannot undo it |
    ///
    /// Today's eighteen built-ins populate all four grades, and the working
    /// surface is what spreads them: `read_file` and `search` observe
    /// (`Low`), `write_file` and `edit_file` are bounded and locally undoable
    /// (`Medium`), `delete_file` is the one thing the agent cannot undo from
    /// inside the turn (`Destructive`), and `bash` runs a command nobody
    /// bounded (`High`, which it shares with `task` for the same reason —
    /// neither's cost is bounded by anything this table can see). The
    /// coordination rows — the board, the scratch plane, the environment
    /// report — are all `Low`, because what they mutate cannot outlive the
    /// process.
    ///
    /// `Medium` also carries a second load: every tool that is
    /// *not* a built-in — an MCP server's, a `.stella/tools/*.toml`
    /// manifest's — is graded [`RiskLevel::High`] by
    /// [`stella_protocol::ToolContract::declared`] for being unreviewed, so a
    /// `Medium` ceiling separates the reviewed surface from everything a
    /// third party supplied without a rule being written about any of them.
    pub risk: RiskLevel,
    /// What has to be true for it to register.
    pub availability: Availability,
    /// The family this tool belongs to, and the name an operator can switch
    /// off to disable the whole family at once
    /// (`"tools": {"scratch": "off"}`).
    ///
    /// Data rather than a section comment, because a per-tool policy needs
    /// "turn off the whole family" to be one line, and because the settings
    /// UI groups its rows by exactly this.
    pub group: &'static str,
}

/// Declares the canonical table once and derives every flat name list from it,
/// so the two can never disagree.
macro_rules! catalog {
    ($($name:literal => ($read_only:expr, $speculation_safe:expr, $risk:expr, $availability:expr, $group:literal)),* $(,)?) => {
        /// Every tool Stella can dispatch by name, sorted, declared once.
        ///
        /// See the [module docs](self) for how to add one.
        pub const CATALOG: &[ToolEntry] = &[
            $(ToolEntry {
                name: $name,
                read_only: $read_only,
                speculation_safe: $speculation_safe,
                risk: $risk,
                availability: $availability,
                group: $group,
            }),*
        ];

        /// Every name in [`CATALOG`], in the same order. Backs
        /// [`crate::custom::RESERVED_NAMES`].
        pub const ALL_NAMES: &[&str] = &[$($name),*];
    };
}

use Availability::Always;
use RiskLevel::{Destructive, High, Low, Medium};

// Column order: (read_only, speculation_safe, risk, availability, group). The
// second column only ever narrows the first: `true` means the read is pure
// enough to run twice per step (speculation + a stream retry — #923). The
// third is the governance grade — see `ToolEntry::risk` for the rubric.
catalog! {
    // The working surface: one shell, file CRUD, one search. These are the
    // rows that touch the world outside the process, and they are where the
    // risk column earns its keep — `read_file` and `search` observe, the
    // three writers are bounded and locally undoable, and `bash` runs a
    // command nobody bounded at all.
    "bash"                => (false, false, High, Always, "shell"),
    "read_file"           => (true, true, Low, Always, "file"),
    "write_file"          => (false, false, Medium, Always, "file"),
    "edit_file"           => (false, false, Medium, Always, "file"),
    // `Destructive` is the honest grade and the only one in the table: an
    // unlinked file is the one thing on this surface the agent cannot undo
    // from inside the turn.
    "delete_file"         => (false, false, Destructive, Always, "file"),
    // Read-only, but NOT speculation-safe: the semantic rung writes
    // embeddings through into `codegraph.db` while it ranks.
    "search"              => (true, false, Low, Always, "search"),
    // The session task board. In-memory and session-scoped: every row here is
    // `Low` because what it mutates cannot outlive the process, which is the
    // clearest demonstration that `risk` is not `read_only` spelled twice.
    "task_create"         => (false, false, Low, Always, "task"),
    "task_list"           => (true, true, Low, Always, "task"),
    "task_start"          => (false, false, Low, Always, "task"),
    "task_complete"       => (false, false, Low, Always, "task"),
    "task_cancel"         => (false, false, Low, Always, "task"),
    "task_assign"         => (false, false, Low, Always, "task"),
    // Sub-agent delegation (#922). NOT read_only — it spends money, and that
    // flag is also what caps nesting: children run behind `ReadOnlyTools`, so
    // a read-only `task` would let them spawn children of their own. `High`
    // for the same reason it is not read-only, and it is the one built-in
    // that a risk ceiling meaningfully separates from the rest: it spends
    // real money, and the child it spawns wields a whole tool surface of its
    // own.
    "task"                => (false, false, High, Always, "task"),
    // Session scratch state (tempfile::TempDir, self-deleting) — the plane
    // dies with the session, `delete_state` included, so nothing here is
    // irreversible in any sense that outlives the run.
    "save_state"          => (false, false, Low, Always, "scratch"),
    "get_state"           => (true, true, Low, Always, "scratch"),
    "list_state"          => (true, true, Low, Always, "scratch"),
    "delete_state"        => (false, false, Low, Always, "scratch"),
    // One-shot environment report: workspace root, git/worktree bit,
    // platform, OS release, shell dialect, scratch dir (#2697).
    "get_environment"     => (true, true, Low, Always, "environment"),
}

/// Tool names Stella once dispatched and no longer does.
///
/// This list exists because #3244 deleted a tool surface that prose kept
/// referring to. `bash`'s long-sleep advisory told the model to *"poll with
/// `read_output`/`wait_for` instead"* for weeks after both tools stopped
/// existing, and the test covering that string asserted it *contained*
/// `read_output` — so the suite held the dead reference green until a manual
/// sweep found it (#3555, #3557).
///
/// A retired name is the one that actually bites: a runtime string naming it
/// hands the model a directive with nothing behind it, which is worse than
/// silence — it teaches the model to discount the next directive too (#3031).
/// So the liveness guards scan for **these** names rather than trying to
/// decide in general which prose token is tool-shaped, and cannot false-fire
/// on ordinary English.
///
/// Every entry must be absent from [`ALL_NAMES`], which
/// `no_retired_name_is_also_a_live_tool` pins: restoring a tool means deleting
/// its line here, in the same change.
pub const RETIRED_TOOL_NAMES: &[&str] = &[
    "apply_edits",
    "ask_user",
    "build_project",
    "format_code",
    "gather_context",
    "graph_query",
    "project_overview",
    "read_output",
    "read_symbol",
    "run_lint",
    "run_script",
    "run_tests",
    "start_process",
    "verify_done",
    "wait_for",
];

/// Retired names that are also ordinary words outside Stella — deliberately
/// **not** in [`RETIRED_TOOL_NAMES`].
///
/// `grep` and `glob` were tool names before #3120 folded them into `search`,
/// but they are also the shell command and the pattern syntax, and the prompt
/// legitimately says "bash with grep only when you need every occurrence of
/// one exact literal string". Scanning for them would fire on correct prose,
/// so they are excluded and recorded here instead of silently omitted — the
/// guards err toward missing a stale reference rather than blocking a true
/// sentence, the same posture `bash.rs`'s `is_symbol_shaped` takes.
pub const RETIRED_NAMES_TOO_AMBIGUOUS_TO_SCAN: &[&str] = &["glob", "grep"];

/// Look up a tool's canonical row by dispatch name.
pub fn get(name: &str) -> Option<&'static ToolEntry> {
    CATALOG.iter().find(|entry| entry.name == name)
}

/// The group an operator switches off to disable a whole family.
///
/// Built-ins answer from [`CATALOG`]. Everything else is grouped by where it
/// came from, so a policy can address tools this table has never heard of:
/// MCP tools (`mcp__<server>__<tool>`) are `"mcp"`, and anything else — a
/// customer's own manifest tool — is `"custom"`. That is what makes
/// `{"custom": "off"}` mean "none of my registered tools" without enumerating
/// them.
pub fn group_for(name: &str) -> &'static str {
    if let Some(entry) = get(name) {
        return entry.group;
    }
    if name.starts_with("mcp__") {
        return "mcp";
    }
    "custom"
}

/// Every group name in the catalog plus the two dynamic ones, sorted and
/// deduped — what the settings UI lists as its sections and what validation
/// accepts as a group key.
pub fn groups() -> Vec<&'static str> {
    let mut groups: Vec<&'static str> = CATALOG.iter().map(|entry| entry.group).collect();
    groups.push("mcp");
    groups.push("custom");
    groups.sort_unstable();
    groups.dedup();
    groups
}

/// Names in one group, sorted.
pub fn names_in_group(group: &str) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = CATALOG
        .iter()
        .filter(|entry| entry.group == group)
        .map(|entry| entry.name)
        .collect();
    names.sort_unstable();
    names
}

/// Names whose [`Availability`] satisfies `pred`, sorted.
///
/// Sorted so callers can compare against a registry's schemas directly —
/// `schemas()` is name-sorted for prompt-cache stability.
pub fn names_where(pred: impl Fn(Availability) -> bool) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = CATALOG
        .iter()
        .filter(|entry| pred(entry.availability))
        .map(|entry| entry.name)
        .collect();
    names.sort_unstable();
    names
}

/// The tools that register in every session, with no prerequisite — the
/// "always-on" count the docs quote.
pub fn always_on() -> Vec<&'static str> {
    names_where(|a| a == Availability::Always)
}

/// Every tool the native registry can register — the ceiling the docs quote.
/// Identical to [`always_on`] while every row is unconditional.
pub fn native() -> Vec<&'static str> {
    names_where(Availability::is_native)
}

/// The read-only partition across the whole catalog, sorted. What dispatch
/// parallelizes and a judging context may call — NOT the speculated set,
/// which is the narrower [`speculation_safe`] (#923).
pub fn read_only() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = CATALOG
        .iter()
        .filter(|entry| entry.read_only)
        .map(|entry| entry.name)
        .collect();
    names.sort_unstable();
    names
}

/// The tools the engine may run before their step commits, sorted — every
/// row claiming both `read_only` and `speculation_safe` (#923). A subset of
/// [`read_only`] by construction: what a stream retry may execute twice.
pub fn speculation_safe() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = CATALOG
        .iter()
        .filter(|entry| entry.read_only && entry.speculation_safe)
        .map(|entry| entry.name)
        .collect();
    names.sort_unstable();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// A duplicated row would make every derived count silently too high and
    /// let two rows disagree on `read_only` for the same dispatch name.
    #[test]
    fn catalog_names_are_unique() {
        let mut seen = HashSet::new();
        for entry in CATALOG {
            assert!(
                seen.insert(entry.name),
                "duplicate catalog entry for `{}`",
                entry.name
            );
        }
        assert_eq!(seen.len(), ALL_NAMES.len());
    }

    /// `ALL_NAMES` is macro-derived from the same rows as `CATALOG`; this pins
    /// that the macro keeps them aligned rather than merely equal in length.
    #[test]
    fn all_names_mirrors_the_catalog() {
        let from_catalog: Vec<&str> = CATALOG.iter().map(|entry| entry.name).collect();
        assert_eq!(from_catalog, ALL_NAMES);
    }

    #[test]
    fn derived_views_partition_the_catalog() {
        assert_eq!(
            always_on().len() + names_where(|a| a != Availability::Always).len(),
            CATALOG.len()
        );
        // The always-on set is a subset of the native set.
        let native_set: HashSet<&str> = native().into_iter().collect();
        for name in always_on() {
            assert!(native_set.contains(name), "{name} must be native");
        }
    }

    /// `speculation_safe` narrows `read_only`; it never widens it. A
    /// mutating row claiming it would let a schema drift toward running a
    /// mutation before its step commits, so the table refuses the shape
    /// outright rather than trusting every consumer to intersect.
    ///
    /// (`search` is the live witness that the two flags are genuinely
    /// different questions: it is `read_only` — it changes no workspace file
    /// — and it is deliberately *not* speculation-safe, because its semantic
    /// rung writes embeddings through into `codegraph.db` while it ranks, so
    /// running it twice is not free. Only the subset direction is enforced
    /// here; the other direction is a per-row judgement.)
    #[test]
    fn speculation_safe_is_a_subset_of_read_only() {
        for entry in CATALOG {
            assert!(
                entry.read_only || !entry.speculation_safe,
                "`{}` claims speculation_safe without read_only — a mutating \
                 tool can never run before its step commits (#923)",
                entry.name
            );
        }
    }

    /// A policy switch key resolves exact-name-first, then by group
    /// (`ToolPolicy::allows`), so a group that shares its name with a tool
    /// while holding *other* tools makes one key address two surfaces (the
    /// #3120 shape). A single-member group named after its only tool is
    /// harmless — both readings resolve identically.
    ///
    /// `task` is the one declared exemption: the delegation tool `task` still
    /// shares its name with the task-board group, and untangling that is a
    /// vocabulary decision tracked in #3192, not a silence.
    #[test]
    fn a_group_key_never_doubles_as_another_tools_switch() {
        for entry in CATALOG {
            if entry.group == "task" {
                continue; // declared gap: #3192
            }
            if get(entry.group).is_some() {
                assert_eq!(
                    names_in_group(entry.group),
                    vec![entry.group],
                    "group `{}` shares its name with a tool but holds other tools — \
                     one policy key would address both surfaces (#3120)",
                    entry.group
                );
            }
        }
    }

    #[test]
    fn lookup_finds_entries_by_dispatch_name() {
        assert!(get("task_list").expect("task_list is canonical").read_only);
        assert!(
            !get("save_state")
                .expect("save_state is canonical")
                .read_only
        );
        assert!(get("no_such_tool").is_none());
    }
}
