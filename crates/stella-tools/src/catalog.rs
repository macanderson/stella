//! The canonical tool table — the one place a new built-in is declared.
//!
//! Adding a tool used to be a ~6-file edit, and several of those edits were
//! hardcoded counts or duplicated name-lists: two registry count pins, the
//! read-only partition, [`crate::custom::RESERVED_NAMES`], and two counts in
//! the docs. Parallel PRs each bumped the same integer off the same base, so
//! squash-merging them back-to-back left the count off by N−1 with **no merge
//! conflict** — a plausible-but-wrong number that only CI (or nothing at all,
//! on the docs side) caught after the fact.
//!
//! This module is the fix. Every built-in — plus the six the CLI layers on
//! top — is declared exactly once in the `catalog!` invocation below, with
//! its read-only flag and what has to be true for it to register. Everything
//! that used to be duplicated is now derived from it:
//!
//! - the registry's expected-name sets and their counts (`registry.rs` tests),
//! - the read-only partition (same),
//! - [`crate::custom::RESERVED_NAMES`] (aliased straight to [`ALL_NAMES`]),
//! - the "N always-on / up to M native" counts and the read-only set listed in
//!   the docs (`tests/docs_in_sync.rs`).
//!
//! **To add a tool:** register it in
//! [`ToolRegistry::with_backends_and_options`](crate::registry::ToolRegistry),
//! then add one line here. Nothing else needs a count bumped. Two PRs adding
//! different tools now merge to the correct union instead of a wrong integer,
//! and a tool registered but never declared here fails the registry tests by
//! *name*, not by an off-by-one.

/// What has to be true for a tool to be registered.
///
/// Everything but [`Availability::Session`] is registered by
/// [`crate::registry::ToolRegistry`] itself; the session tools live in the
/// CLI's interactive and discovery layers and are listed here only so
/// [`ALL_NAMES`] can back `RESERVED_NAMES` — a custom manifest must not shadow
/// them either.
/// **Availability is not policy.** A variant here names something the
/// *environment* either supplies or does not — a provider key, an issue
/// backend. Whether a satisfiable tool is *allowed* is
/// [`crate::policy::ToolPolicy`]'s business, driven by `settings.json`.
///
/// The `Bash` and `Web` variants that used to live here were the exception
/// that proved the rule: neither named a prerequisite, only a default. They
/// are gone — `bash` and the key-free web tools are [`Availability::Always`],
/// and an operator turns them off with `"tools": {"bash": "off"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Availability {
    /// Registers in every session, with no configuration and no prerequisite.
    Always,
    /// A BYOK search key (`BRAVE_API_KEY`/`TAVILY_API_KEY`). The other three
    /// web tools need no key and are [`Availability::Always`].
    WebSearch,
    /// An image-capable BYOK media provider key.
    Media,
    /// A media provider whose key family also has a video adapter.
    Video,
    /// A configured issue backend (Linear/GitHub connection, or ambient `gh`).
    Issue,
    /// Layered on by the CLI rather than the native registry — never appears
    /// in [`crate::registry::ToolRegistry::schemas`].
    Session,
}

impl Availability {
    /// Whether the native [`crate::registry::ToolRegistry`] is what registers
    /// this tool. False only for [`Availability::Session`].
    pub const fn is_native(self) -> bool {
        !matches!(self, Availability::Session)
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
    /// False for anything that leaves the workspace on the read path — a
    /// metered web call, an issue-tracker API, the code-graph tools whose
    /// reads write catch-up state to `codegraph.db`. Meaningless (and kept
    /// false) on mutating rows.
    pub speculation_safe: bool,
    /// What has to be true for it to register.
    pub availability: Availability,
    /// The family this tool belongs to, and the name an operator can switch
    /// off to disable the whole family at once
    /// (`"tools": {"process": "off"}`).
    ///
    /// These were section comments in this table for a long time; they are
    /// data now because a per-tool policy needs "turn off the process group"
    /// to be one line rather than four, and because the settings UI groups its
    /// rows by exactly this.
    pub group: &'static str,
}

/// Declares the canonical table once and derives every flat name list from it,
/// so the two can never disagree.
macro_rules! catalog {
    ($($name:literal => ($read_only:expr, $speculation_safe:expr, $availability:expr, $group:literal)),* $(,)?) => {
        /// Every tool Stella can dispatch by name, sorted, declared once.
        ///
        /// See the [module docs](self) for how to add one.
        pub const CATALOG: &[ToolEntry] = &[
            $(ToolEntry {
                name: $name,
                read_only: $read_only,
                speculation_safe: $speculation_safe,
                availability: $availability,
                group: $group,
            }),*
        ];

        /// Every name in [`CATALOG`], in the same order. Backs
        /// [`crate::custom::RESERVED_NAMES`].
        pub const ALL_NAMES: &[&str] = &[$($name),*];
    };
}

use Availability::{Always, Issue, Media, Session, Video, WebSearch};

// Column order: (read_only, speculation_safe, availability, group). The
// second column only ever narrows the first: `true` means the read is pure
// enough to run twice per step (speculation + a stream retry — #923), which
// no network-backed or graph-catch-up read can claim.
catalog! {
    // ---- Always-on: registered in every session ----
    // File CRUD
    "read_file"           => (true, true, Always, "file"),
    // Graph-resolved span read: `open_or_build` may write codegraph
    // catch-up state on the way to answering, so never speculated.
    "read_symbol"         => (true, false, Always, "file"),
    "write_file"          => (false, false, Always, "file"),
    "edit_file"           => (false, false, Always, "file"),
    "apply_edits"         => (false, false, Always, "file"),
    "delete_file"         => (false, false, Always, "file"),
    // Search
    "grep"                => (true, true, Always, "search"),
    "glob"                => (true, true, Always, "search"),
    // The read that writes: graph queries bootstrap/catch up codegraph.db.
    "graph_query"         => (true, false, Always, "search"),
    // Context & memory. The overview and gather ride the same graph
    // substrate as graph_query; explorations is pure file reads.
    "project_overview"    => (true, false, Always, "context"),
    "gather_context"      => (true, false, Always, "context"),
    "explorations"        => (true, true, Always, "context"),
    "save_exploration"    => (false, false, Always, "context"),
    "save_memory"         => (false, false, Always, "context"),
    "cite_memory"         => (false, false, Always, "context"),
    // The definition of done + build/test
    "verify_done"         => (false, false, Always, "build"),
    "build_project"       => (false, false, Always, "build"),
    "run_tests"           => (false, false, Always, "build"),
    // Runs the project's own checker — a manifest-resolved command no one
    // can promise is free to run twice.
    "diagnostics"         => (true, false, Always, "build"),
    // Manifest-verb execution (argv, no shell)
    "run_lint"            => (false, false, Always, "build"),
    "format_code"         => (false, false, Always, "build"),
    // The project scripts index (docs/spec/scripts-index.md) — static
    // manifest detection, nothing executed.
    "list_scripts"        => (true, true, Always, "scripts"),
    "run_script"          => (false, false, Always, "scripts"),
    // The long-running process group
    "start_process"       => (false, false, Always, "process"),
    "read_output"         => (false, false, Always, "process"),
    "send_stdin"          => (false, false, Always, "process"),
    "stop_process"        => (false, false, Always, "process"),
    // Vendor-neutral repository tools; the two reads are local git queries.
    "repo_status"         => (true, true, Always, "repo"),
    "repo_diff"           => (true, true, Always, "repo"),
    "repo_commit"         => (false, false, Always, "repo"),
    "repo_push"           => (false, false, Always, "repo"),
    "repo_pull"           => (false, false, Always, "repo"),
    "repo_rollback"       => (false, false, Always, "repo"),
    // CI & evidence. ci_status reads through `gh`/the forge API — someone
    // else's rate limit.
    "ci_status"           => (true, false, Always, "ci"),
    "screenshot"          => (false, false, Always, "ci"),
    // generate_svg is client-side, so it needs no media key
    "generate_svg"        => (false, false, Always, "media"),
    // The session task board (in-memory)
    "task_create"         => (false, false, Always, "task"),
    "task_list"           => (true, true, Always, "task"),
    "task_start"          => (false, false, Always, "task"),
    "task_complete"       => (false, false, Always, "task"),
    "task_cancel"         => (false, false, Always, "task"),
    "task_assign"         => (false, false, Always, "task"),
    // Sub-agent delegation (#922). NOT read_only — it spends money, and that
    // flag is also what caps nesting: children run behind `ReadOnlyTools`, so
    // a read-only `task` would let them spawn children of their own.
    "task"                => (false, false, Always, "task"),
    // The shell. No prerequisite — it is on unless `"tools": {"bash": "off"}`
    // says otherwise, exactly like every other row in this block.
    "bash"                => (false, false, Always, "bash"),
    // The key-free web tools: read-only for the workspace, but every run is
    // real traffic against someone's server — never speculated (#923).
    "web_fetch"           => (true, false, Always, "web"),
    "web_extract_assets"  => (true, false, Always, "web"),
    "web_download"        => (false, false, Always, "web"),
    // ---- Conditionally registered: the environment must supply something ----
    // A metered BYOK search key: the canonical read-only-but-billed tool.
    "web_search"          => (true, false, WebSearch, "web"),
    "generate_image"      => (false, false, Media, "media"),
    "generate_video"      => (false, false, Video, "media"),
    "poll_video"          => (false, false, Video, "media"),
    // Issue tracking — every read goes to Linear/GitHub, a rate-limited API.
    "create_issue"        => (false, false, Issue, "issue"),
    "update_issue"        => (false, false, Issue, "issue"),
    "close_issue"         => (false, false, Issue, "issue"),
    "search_issues"       => (true, false, Issue, "issue"),
    "get_issue"           => (true, false, Issue, "issue"),
    "list_labels"         => (true, false, Issue, "issue"),
    "list_members"        => (true, false, Issue, "issue"),
    "start_work_on_issue" => (false, false, Issue, "issue"),
    // ---- CLI session layer (never in the native registry) ----
    // search_skills and mcp_search query public internet registries;
    // tool_search and skill_search read local indexes.
    "ask_user"            => (false, false, Session, "session"),
    "search_skills"       => (true, false, Session, "session"),
    "install_skill"       => (false, false, Session, "session"),
    "tool_search"         => (true, true, Session, "session"),
    "skill_search"        => (true, true, Session, "session"),
    "mcp_search"          => (true, false, Session, "session"),
}

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

/// Every tool the native registry can register, with every opt-in enabled and
/// every backend configured — the "up to M native" ceiling the docs quote.
/// Excludes the CLI session layer.
pub fn native() -> Vec<&'static str> {
    names_where(Availability::is_native)
}

/// The always-on set plus the issue tools — what a registry with a configured
/// issue backend and no opt-ins advertises.
pub fn always_on_with_issues() -> Vec<&'static str> {
    names_where(|a| matches!(a, Availability::Always | Availability::Issue))
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
/// row claiming both `read_only` and `speculation_safe` (#923). Strictly a
/// subset of [`read_only`]: what a stream retry may execute twice.
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

    /// The declaration order is the order a reader scans, and `names_where`
    /// sorts anyway — but an alphabetized block is where a new tool goes
    /// without thinking, so keep the availability blocks contiguous.
    #[test]
    fn availability_blocks_are_contiguous() {
        let mut seen_blocks: Vec<Availability> = vec![];
        for entry in CATALOG {
            if seen_blocks.last() != Some(&entry.availability) {
                assert!(
                    !seen_blocks.contains(&entry.availability),
                    "`{}` reopens the {:?} block — keep each availability \
                     contiguous so a new tool has one obvious home",
                    entry.name,
                    entry.availability
                );
                seen_blocks.push(entry.availability);
            }
        }
    }

    #[test]
    fn derived_views_partition_the_catalog() {
        assert_eq!(
            always_on().len() + names_where(|a| a != Availability::Always).len(),
            CATALOG.len()
        );
        // Native + session is the whole table, with no overlap.
        let native_count = native().len();
        let session_count = names_where(|a| a == Availability::Session).len();
        assert_eq!(native_count + session_count, CATALOG.len());
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
        // And the claim must cost something: at least one read-only row
        // opts out (the web family), or the flag has collapsed back into
        // read_only and #923 has regressed.
        assert!(
            CATALOG
                .iter()
                .any(|entry| entry.read_only && !entry.speculation_safe),
            "every read-only row claims speculation_safe — the two flags \
             must be able to diverge (#923)"
        );
    }

    #[test]
    fn lookup_finds_entries_by_dispatch_name() {
        assert!(get("read_file").expect("read_file is canonical").read_only);
        assert!(
            !get("write_file")
                .expect("write_file is canonical")
                .read_only
        );
        assert!(get("no_such_tool").is_none());
    }
}
