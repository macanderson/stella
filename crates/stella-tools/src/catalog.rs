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
    /// directions, which is why they are separate columns — `delegate`
    /// mutates nothing in the workspace and spends real money, while
    /// `task_create` mutates a board that dies with the session.
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
    /// Today's built-ins populate all four grades, and the working
    /// surface is what spreads them: `read_file` and `search` observe
    /// (`Low`), `write_file` and `edit_file` are bounded and locally undoable
    /// (`Medium`), `delete_file` is the one thing the agent cannot undo from
    /// inside the turn (`Destructive`), and `bash` runs a command nobody
    /// bounded (`High`, which it shares with `delegate` for the same reason
    /// — neither's cost is bounded by anything this table can see). The
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
    /// What a human surface calls this tool — the verb, not the identifier.
    ///
    /// A tool has three names now, answering three different questions.
    /// [`Self::name`] is what the **model** must spell in a call; the
    /// description is what the model reads to decide *whether* to call. This is
    /// what a **reader** sees after it happened: `read`, `edit`, `write`,
    /// `delete`, `run`.
    ///
    /// They diverge on purpose. `read_file` is a good wire identifier — unique,
    /// unambiguous, obviously a file operation — and a poor transcript row,
    /// because a transcript is a log of *actions* and the reader's first
    /// question of any row is what kind of thing happened. `edit_file ·
    /// edit_file · bash` spends three columns saying what `edit · edit · run`
    /// says in one, and the underscore is an implementation detail in prose.
    ///
    /// Lowercase, deliberately: these sit inline in a sentence-shaped row
    /// (`edit …/lifecycle.rs +3 -1`), never as a column heading.
    ///
    /// It does **not** reach the model. Every provider adapter builds its own
    /// tool JSON from `name`/`description`/`input_schema` explicitly, so a field
    /// here cannot perturb the prompt-cache prefix (invariant 7) — which is why
    /// a display concern may live beside the dispatch ones at all.
    pub label: &'static str,
}

/// Declares the canonical table once and derives every flat name list from it,
/// so the two can never disagree.
macro_rules! catalog {
    ($($name:literal => ($read_only:expr, $speculation_safe:expr, $risk:expr, $availability:expr, $group:literal, $label:literal)),* $(,)?) => {
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
                label: $label,
            }),*
        ];

        /// Every name in [`CATALOG`], in the same order. Backs
        /// [`crate::custom::RESERVED_NAMES`].
        pub const ALL_NAMES: &[&str] = &[$($name),*];
    };
}

use Availability::Always;
use RiskLevel::{Destructive, High, Low, Medium};

// Column order: (read_only, speculation_safe, risk, availability, group, label).
// The
// second column only ever narrows the first: `true` means the read is pure
// enough to run twice per step (speculation + a stream retry — #923). The
// third is the governance grade — see `ToolEntry::risk` for the rubric.
catalog! {
    // The working surface: one shell, file CRUD, one search. These are the
    // rows that touch the world outside the process, and they are where the
    // risk column earns its keep — `read_file` and `search` observe, the
    // three writers are bounded and locally undoable, and `bash` runs a
    // command nobody bounded at all.
    "bash"                => (false, false, High, Always, "shell", "run"),
    "read_file"           => (true, true, Low, Always, "file", "read"),
    "write_file"          => (false, false, Medium, Always, "file", "write"),
    "edit_file"           => (false, false, Medium, Always, "file", "edit"),
    // `Destructive` is the honest grade and the only one in the table: an
    // unlinked file is the one thing on this surface the agent cannot undo
    // from inside the turn.
    "delete_file"         => (false, false, Destructive, Always, "file", "delete"),
    // Read-only, but NOT speculation-safe: the semantic rung writes
    // embeddings through into `codegraph.db` while it ranks.
    "search"              => (true, false, Low, Always, "search", "search"),
    // The session task board. In-memory and session-scoped: every row here is
    // `Low` because what it mutates cannot outlive the process, which is the
    // clearest demonstration that `risk` is not `read_only` spelled twice.
    "task_create"         => (false, false, Low, Always, "task", "task"),
    "task_list"           => (true, true, Low, Always, "task", "tasks"),
    "task_start"          => (false, false, Low, Always, "task", "task"),
    "task_complete"       => (false, false, Low, Always, "task", "task"),
    "task_cancel"         => (false, false, Low, Always, "task", "task"),
    "task_assign"         => (false, false, Low, Always, "task", "task"),
    // Sub-agent delegation (#922). NOT read_only — it spends money, and that
    // flag is also what caps nesting: children run behind `ReadOnlyTools`, so
    // a read-only `delegate` would let them spawn children of their own.
    // `High` for the same reason it is not read-only, and it is the one
    // built-in that a risk ceiling meaningfully separates from the rest: it
    // spends real money, and the child it spawns wields a whole tool surface
    // of its own.
    //
    // It shares the board's `task` group — delegation and the board are one
    // coordination family, and `"tools": {"task": "off"}` withholds all
    // seven exactly as it always did. What it may NOT be is *named* `task`:
    // a switch key resolves exact-name-first, so a tool named after its own
    // group makes one key address two surfaces (#3192, the #3120 shape).
    "delegate"            => (false, false, High, Always, "task", "delegate"),
    // Session scratch state (tempfile::TempDir, self-deleting) — the plane
    // dies with the session, `delete_state` included, so nothing here is
    // irreversible in any sense that outlives the run.
    "save_state"          => (false, false, Low, Always, "scratch", "save_state"),
    "get_state"           => (true, true, Low, Always, "scratch", "get_state"),
    "list_state"          => (true, true, Low, Always, "scratch", "list_state"),
    "delete_state"        => (false, false, Low, Always, "scratch", "delete_state"),
    // One-shot environment report: workspace root, git/worktree bit,
    // platform, OS release, shell dialect, scratch dir (#2697).
    "get_environment"     => (true, true, Low, Always, "environment", "get_environment"),
    // Put a decision back to whoever is driving this agent (#4212).
    //
    // `read_only` is honest — it changes nothing — and load-bearing twice:
    // `ReadOnlyTools` filters on exactly this flag, so a delegated child can
    // still ask its dispatcher (the whole agent-to-agent path, by
    // construction rather than by a special case), and the engine
    // parallelizes on it, which is why the broker holds a fairness gate.
    // Deliberately NOT `speculation_safe`: a speculated call would ask a
    // human the same question twice.
    //
    // `Low` on the same rubric as the board: what it touches cannot outlive
    // the process. It spends a human's attention, which no column here
    // grades — the ceiling that matters for this tool is the driver's
    // patience, and the schema's own description is where that is defended.
    //
    // Its own group rather than `task`, so an operator can withhold
    // questions without withholding the coordination family: an unattended
    // fleet worker wants `"question": "off"` and its task board intact.
    "ask_question"        => (true, false, Low, Always, "question", "ask_question"),
}

/// Names Stella once dispatched, or once told the model it dispatched, and no
/// longer does.
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
/// # "or once told the model it dispatched"
///
/// `wait_for` is the reason that clause is in the first sentence. It never
/// carried a `ToolSchema` and appears in no revision of this file — it entered
/// the list because the advisory above *named* it, so the model carries its
/// priors exactly as if it had existed. The set worth reserving is what the
/// model was told about, which is a superset of what shipped.
///
/// # How the rest of it was recovered
///
/// #3237 reserved fifteen names, and #3853 found that was not the history:
/// `web_fetch` — the name #3237's own problem statement uses as its example —
/// was in neither list, so a manifest could claim it. The gap was invisible,
/// because acceptance meant "not on our two lists" rather than "never a
/// built-in".
///
/// The rest were reconstructed from git rather than from memory. Every name
/// Stella ever dispatched was declared in this file (or its pre-move path)
/// since `6d7483c50` made adding a tool a one-place change, so the universe is
/// the union of every `"name" =>` row across every revision of it:
///
/// ```text
/// git log --all -p -- crates/stella-tools/src/catalog.rs stella-tools/src/catalog.rs \
///   | rg -o '^[+-] *"[a-z_0-9]+" +=>' | rg -o '"[a-z_0-9]+"' | sort -u
/// ```
///
/// That yields 81 names; subtracting [`ALL_NAMES`] — which includes the six
/// `6365cf330` restored — leaves the retired set. `54233fd68` (#3244) is where
/// most of them went in one commit. One name predates the table and is not in
/// that union: `code_graph`, which `33ac2f31a` shipped and `326cf4917`
/// renamed to `graph_query`.
///
/// The command is deliberately not a test. It reads every revision on every
/// ref, which is neither hermetic nor fast, and a shallow CI clone would make
/// it answer differently — a guard that is wrong on the machine it runs on is
/// worse than a documented procedure. Re-run it when adding a name; the
/// disjointness half *is* enforced, by `no_retired_name_is_also_a_live_tool`,
/// so restoring a tool means deleting its line here in the same change.
pub const RETIRED_TOOL_NAMES: &[&str] = &[
    "apply_edits",
    "ask_user",
    "build_project",
    "ci_status",
    "cite_memory",
    "clear_output",
    "close_issue",
    "code_graph",
    "create_issue",
    "format_code",
    "gather_context",
    "generate_image",
    "generate_svg",
    "generate_video",
    "get_issue",
    "graph_query",
    "install_skill",
    "invoke_skill",
    "list_labels",
    "list_members",
    "list_scripts",
    "mcp_search",
    "poll_video",
    "probe_capability",
    "project_overview",
    "read_output",
    "read_symbol",
    "recall_context",
    "repo_commit",
    "repo_diff",
    "repo_history",
    "repo_pull",
    "repo_push",
    "repo_recover",
    "repo_rollback",
    "repo_status",
    "restart_process",
    "run_lint",
    "run_script",
    "run_tests",
    "save_exploration",
    "save_memory",
    "search_issues",
    "search_skills",
    "semantic_code_search",
    "send_stdin",
    "skill_search",
    "start_process",
    "start_work_on_issue",
    "stop_process",
    "tool_search",
    "update_issue",
    "verify_done",
    "wait_for",
    "web_download",
    "web_extract_assets",
    "web_fetch",
    "web_search",
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
/// sentence.
///
/// `task` joins them for the same reason (#3192): it was the delegation
/// tool's dispatch name until that name was moved to `delegate`, and it is
/// still the *group* key for the seven-tool coordination family — as well as
/// the commonest noun in every prompt this repository ships. It stays
/// reserved ([`is_reserved`]) so a `.stella/tools/*.toml` manifest cannot
/// claim a name an operator's `"tools": {"task": …}` entry already addresses
/// as a group; it is simply not greppable.
///
/// `diagnostics`, `explorations` and `screenshot` joined in #3853 on the same
/// test. All three were real dispatch names deleted by #3244, and all three
/// are also words this repository's own runtime prose uses about something
/// else — the diagnostic plane, an agent exploring a tree, an image someone
/// took. The scan requires a backticked whole token, which is most of what
/// keeps it off English, but these three are exactly the ones a correct
/// sentence would still backtick.
///
/// # The split still earns its keep, and it is about one thing
///
/// #3853 asked whether two lists are still worth having. They are, and the
/// distinction is **only** about prose scanning: [`is_retired`] is their
/// union, so a manifest naming anything on either list is refused
/// identically, and `is_reserved` follows. Nothing a caller asks about a
/// `name` field can tell the two apart. What the split buys is a scanner that
/// does not cry wolf — and a guard that cries wolf gets deleted, which costs
/// the whole check rather than one name.
pub const RETIRED_NAMES_TOO_AMBIGUOUS_TO_SCAN: &[&str] = &[
    "diagnostics",
    "explorations",
    "glob",
    "grep",
    "screenshot",
    "task",
];

/// The names a live tool dispatched under before its current one — a ledger
/// for reading a **dated measurement** that recorded a tool under its
/// then-name.
///
/// Each pair is `(the live dispatch name, the name the wire carried then)`,
/// and a tool may hold several rows: #3120 folded `grep` and `glob` into one
/// `search`, and #3192 moved the delegation tool from `task` to `delegate`.
///
/// **Not a compatibility alias.** A former name here must not dispatch — it
/// stays retired ([`is_retired`]) and reserved ([`is_reserved`]), and
/// `former_names_point_from_a_live_tool_to_a_dead_name` pins both halves. The
/// ledger's readers are the surfaces that join a dated record to today's tool
/// table, and today that is one: `stella-cli`'s `tool_docs` generator, whose
/// pages carry a census captured 2026-08-11. Without the ledger a renamed
/// tool's page took the no-row branch and printed *"the census enumerates the
/// schemas those runs advertised, and this tool was not among them"* — a
/// statement the census itself contradicts, since `delegate` was advertised
/// and called twice under `task` (#3846).
///
/// A number read through this ledger keeps its provenance. The reader is told
/// which name it was recorded under, because a rename can move a schema too —
/// `search` does more than `grep` did — and how far the old figure carries is
/// the reader's judgement to make, not the generator's to hide.
pub const FORMER_TOOL_NAMES: &[(&str, &str)] =
    &[("delegate", "task"), ("search", "grep"), ("search", "glob")];

/// Whether `name` was a dispatch name Stella has since deleted — the union of
/// [`RETIRED_TOOL_NAMES`] and [`RETIRED_NAMES_TOO_AMBIGUOUS_TO_SCAN`].
///
/// The split between those two lists is a fact about **prose scanning**: one
/// is safe to grep runtime strings for, the other false-fires on ordinary
/// English. Neither half of that distinction survives an exact-match question
/// about a manifest's `name` field, so a caller asking "did Stella once
/// dispatch this?" reads both (#3237).
pub fn is_retired(name: &str) -> bool {
    RETIRED_TOOL_NAMES.contains(&name) || RETIRED_NAMES_TOO_AMBIGUOUS_TO_SCAN.contains(&name)
}

/// Whether Stella claims `name` for itself, so a `.stella/tools/*.toml`
/// manifest or a foundry-authored tool may not register it.
///
/// Two disjoint reasons, both of them "the model already has priors about
/// this name and they are not the manifest's":
///
/// - a live [`CATALOG`] row, where shadowing would route the wrong executor
///   *and* hand a third party a built-in's reviewed
///   [`stella_protocol::ToolContract`] (see [`crate::contracts`]);
/// - a name Stella dispatched and retired ([`is_retired`]), where nothing is
///   shadowed today but the two sharp edges of #3237 remain: a custom `bash`
///   that is not a shell is called as one, and an operator's
///   `"tools": {"run_tests": "off"}` — written when that built-in existed —
///   silently addresses the manifest instead of nothing.
///
/// The asymmetry is deliberate: reserving a name is reversible by deleting a
/// line, while releasing one is not reversible once a shipped manifest
/// depends on it.
pub fn is_reserved(name: &str) -> bool {
    ALL_NAMES.contains(&name) || is_retired(name)
}

/// Look up a tool's canonical row by dispatch name.
pub fn get(name: &str) -> Option<&'static ToolEntry> {
    CATALOG.iter().find(|entry| entry.name == name)
}

/// The groups that have no fixed member list, sorted.
///
/// Every other group is a column of [`CATALOG`], so [`names_in_group`] can
/// enumerate it. These two are assigned by *origin* instead — [`group_for`]
/// answers `"mcp"` for anything named `mcp__<server>__<tool>` and `"custom"`
/// for every other name the table has never heard of — so their membership is
/// knowable only against a live session's tool list, and [`names_in_group`]
/// answers empty for both.
///
/// Named as a constant because a caller that expands group keys into tool
/// names ([`crate::policy::ToolPolicy::narrow_with`]) has to tell these apart
/// from a plain tool name: both give an empty [`names_in_group`], and
/// treating `"custom"` as a tool name is exactly the misreading #2800 was.
pub const DYNAMIC_GROUPS: &[&str] = &["custom", "mcp"];

/// The group an operator switches off to disable a whole family.
///
/// Built-ins answer from [`CATALOG`]. Everything else is grouped by where it
/// came from, so a policy can address tools this table has never heard of:
/// MCP tools (`mcp__<server>__<tool>`) are `"mcp"`, and anything else — a
/// customer's own manifest tool — is `"custom"`. Those two are
/// [`DYNAMIC_GROUPS`]. That is what makes `{"custom": "off"}` mean "none of my
/// registered tools" without enumerating them.
pub fn group_for(name: &str) -> &'static str {
    if let Some(entry) = get(name) {
        return entry.group;
    }
    if name.starts_with("mcp__") {
        return "mcp";
    }
    "custom"
}

/// What a human surface calls `name` — the verb, not the identifier.
///
/// Built-ins answer from [`CATALOG`]. Everything else answers with the most
/// readable thing that is still *true*: an MCP tool's own trailing segment
/// (`mcp__fs__read_file` → `read_file`), and anything else its bare name.
///
/// Deliberately not a guess dressed as knowledge. A server's `read_file` is not
/// necessarily this workspace's `read`, and mapping it to one would put a
/// familiar word on a row that did something else — the one failure a
/// transcript cannot afford, since its whole job is to say what happened.
/// Stripping the routing prefix is the most a caller can honestly do without
/// asking the server, so that is where it stops.
pub fn label_for(name: &str) -> &str {
    if let Some(entry) = get(name) {
        return entry.label;
    }
    name.rsplit("__").next().unwrap_or(name)
}

/// Every group name in the catalog plus the two dynamic ones, sorted and
/// deduped — what the settings UI lists as its sections and what validation
/// accepts as a group key.
pub fn groups() -> Vec<&'static str> {
    let mut groups: Vec<&'static str> = CATALOG.iter().map(|entry| entry.group).collect();
    groups.extend(DYNAMIC_GROUPS.iter().copied());
    groups.sort_unstable();
    groups.dedup();
    groups
}

/// Names in one group, sorted.
///
/// Empty for a [`DYNAMIC_GROUPS`] entry and for a name that is no group at
/// all — the caller that needs to tell those apart consults that constant.
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

    /// [`DYNAMIC_GROUPS`] is a premise two other modules build on: that these
    /// are the groups [`group_for`] can answer with off-catalog, and that
    /// [`names_in_group`] cannot enumerate them. `ToolPolicy::narrow_with`
    /// expands every *other* group key into names and falls back to a
    /// group-level answer for these, so a third dynamic group added to
    /// `group_for` without a line here would be silently expanded to nothing.
    #[test]
    fn the_dynamic_groups_are_the_ones_no_row_declares() {
        for group in DYNAMIC_GROUPS {
            assert!(
                names_in_group(group).is_empty(),
                "`{group}` is dynamic and cannot have catalog members"
            );
            assert!(groups().contains(group), "`{group}` must be listed");
        }
        // Both are reachable from `group_for`, which is what makes them keys
        // an operator can write.
        assert_eq!(group_for("mcp__github__create_issue"), "mcp");
        assert_eq!(group_for("deploy_to_staging"), "custom");
        // ...and no catalog row declares either, so nothing is in two groups.
        for entry in CATALOG {
            assert!(
                !DYNAMIC_GROUPS.contains(&entry.group),
                "`{}` declares the dynamic group `{}`",
                entry.name,
                entry.group
            );
        }
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
    /// No exemptions: every row is asked.
    #[test]
    fn a_group_key_never_doubles_as_another_tools_switch() {
        for entry in CATALOG {
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

    /// [`FORMER_TOOL_NAMES`] reads dated measurements under a tool's
    /// then-name, so both ends of every pair are asserted: the live end must
    /// be a real row, or it names a page that does not exist; the former end
    /// must be dead and retired, or the ledger has quietly given a live tool
    /// a second dispatch name. A duplicated pair is rejected too — its only
    /// effect would be to report one measured row twice.
    #[test]
    fn former_names_point_from_a_live_tool_to_a_dead_name() {
        let mut seen = HashSet::new();
        for (current, former) in FORMER_TOOL_NAMES {
            assert!(
                seen.insert((current, former)),
                "`{current}` lists `{former}` twice; one census row would be \
                 reported as two measurements"
            );
            assert!(
                get(current).is_some(),
                "`{current}` has a former name but is not a live catalog row — \
                 delete the ledger line with the tool"
            );
            assert!(
                !ALL_NAMES.contains(former),
                "`{former}` is recorded as `{current}`'s former name but is itself \
                 live; a ledger entry reads a dated measurement and must never \
                 make a name dispatch"
            );
            assert!(
                is_retired(former),
                "`{former}` is a former dispatch name and must be retired, so \
                 nothing may register it again"
            );
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
