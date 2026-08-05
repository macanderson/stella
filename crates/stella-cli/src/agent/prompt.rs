//! System-prompt assembly and file-tree rendering.
//!
//! The base personas plus the workspace context that appends after them —
//! exploration index, project scripts, memories — under the byte-stable
//! prefix discipline (L-E8): the stable base is what the prompt cache keys
//! on, so nothing nondeterministic may enter here (recalled context rides as
//! a volatile message after the prefix, never interleaved into it).

use super::*;

// Both static prompts used to open with a hand-maintained catalogue: one
// bulleted line per tool, ~1,240 tokens, restating what the generated tool
// schemas already carry. That was pure duplication with a recurring price.
// The schema list is serialized at position 0 of the same cached prefix these
// prompts sit in (`ToolRegistry::schemas`, sorted for exactly that reason), so
// a default session (~46 tools) was paying for every description twice on
// every single call (#639).
//
// What replaces it is the residue: the steering the schemas structurally
// cannot express. A schema describes one tool in isolation, so it can say what
// `apply_edits` does but not that it beats a chain of `edit_file` calls, and it
// can only ever describe a tool that IS registered — never why a capability is
// absent or how to turn it on. Anything a tool's own description already says
// belongs there, not here: the schemas are the reference, this block is policy.
//
// It stays a macro rather than a `const &str` because `concat!` takes only
// literals, and staying a compile-time concatenation is what preserves the
// byte-stable-prefix property (L-E8) a runtime `format!` would give up. One
// shared literal, embedded verbatim by both prompts, is also what keeps the two
// copies from drifting the way the catalogue's did (#450).

/// The cross-tool steering shared by both static prompts — what the generated
/// schemas cannot say. No trailing newline: each prompt continues with its own
/// blank line and section header.
macro_rules! tool_steering {
    () => {
        r#"Your tool schemas are the reference for what each tool does and what it takes. What they cannot tell you, because each describes one tool in isolation:

- Read a definition by name with read_symbol; guessing read_file offsets after a graph_query is the round-trip it exists to remove.
- A change touching several files is ONE apply_edits call, not a chain of edit_file calls.
- A tool you cannot see is not available in this session rather than nonexistent. The shell ships registered and a workspace withholds it with "tools": {"bash": "off"}; issue tracking, web, and media tools register only once their backend is configured (`stella connect github|linear`, an API key, or `gh auth`; ci_status needs the gh CLI). Reach for tool_search before concluding a capability is missing."#
    };
}

pub(crate) const SYSTEM_PROMPT: &str = concat!(
    r#"You are Stella, a fast terminal coding agent. You help the user with software engineering tasks by reading files, writing code, running commands, and searching the codebase.

"#,
    tool_steering!(),
    r#"

Rules:
- For "where is X defined", "who calls/references X", or "what depends on this file" questions, reach for graph_query FIRST when it is available — it is precise and cheap. Fall back to grep/glob only when the graph can't answer (free-text search, a symbol the index doesn't carry, or no index yet).
- Always read a file before editing it — never edit blind.
- Make minimal, surgical edits. Use edit_file, not write_file, for changes to existing files.
- After changing behavior, use run_tests to check the suite, and verify_done to prove the change with a witness test rather than trusting a green suite.
- Be concise in your responses. Show the user what you changed and why.
- If a task requires multiple steps, work through them systematically.
- When a choice is ambiguous AND getting it wrong would be costly, use ask_user rather than guessing; otherwise proceed with your best judgment."#
);

/// The pipeline-mode system prompt: encodes a reproduce, localize, minimal
/// fix, verify methodology and rewards the fewest changed lines. Static
/// text so it rides the prompt cache (L-E8).
pub(crate) const PIPELINE_SYSTEM_PROMPT: &str = concat!(
    r#"You are Stella, a software engineering agent that fixes bugs and builds features with surgical precision.

"#,
    tool_steering!(),
    r#"

Methodology (always follow in order):
1. ORIENT: On an unfamiliar repository, call project_overview FIRST — before any glob, grep, or read_file. It is one call that tells you the language, how the project builds and tests, and where its entry points are. You cannot reproduce a failure or run the right test until you know these, and guessing them by hand is the 10-30 call exploration this exists to replace. Skip it only when you already know the project cold.
2. REPRODUCE: Run the failing test or reproduce the bug before touching any file. If no test captures the task — a new feature, or a bug nothing covers — WRITE the failing test first and run it to watch it fail; that test is the contract the rest of your work must satisfy. Never edit blind, you must see the actual error first.
3. LOCALIZE: Trace the error to its root cause. Read the failing code path. When graph_query is available, use it FIRST to find definitions, references, and import edges — it is precise and cheap; fall back to grep and glob for free-text search or when the graph has no answer.
4. MINIMAL FIX: Make the smallest change that resolves the issue. No refactoring. No style changes. No "while I'm here" edits. One logical change.
5. VERIFY: Run the target test. If it passes, use verify_done to witness the change. If it fails, read the error and adjust.

Rules:
- Never modify existing tests to make them pass. Adding a NEW test that pins the task's expected behavior is required by step 2; weakening one that exists is forbidden.
- Never create backup files, scratch files, or debug artifacts.
- Prefer edit_file (surgical) over write_file (full rewrite).
- Always read a file before editing it — never edit blind.
- If you are editing more than 3 files for a single-task fix, you are overcomplicating it.
- Be concise in your responses. Show the user what you changed and why.
- When a choice is ambiguous AND getting it wrong would be costly, use ask_user rather than guessing; otherwise proceed with your best judgment."#
);

/// Cap on memory characters appended to the system prompt — memories ride
/// the prompt cache on every call, so they must stay dense.
const MEMORY_PROMPT_BUDGET_CHARS: usize = 16_000;

/// Cap on the workspace-maps index appended to the system prompt
/// (`docs/design/exploration-sharing.md` §4a): metadata only — slice,
/// title, freshness verdict, age — never map bodies, which stay one cheap
/// `explorations` tool call away.
const EXPLORATION_INDEX_BUDGET_CHARS: usize = 2_000;

// The A/B recall measurement rate lived here as a `pub(crate)` constant every
// driver had to pass by hand, and exactly one of them did. It is now
// `context.retrieval.ab_recall_rate` (`crate::settings`), read once at session
// open and applied by `SessionMemory::arm_recall_control` — one door, no
// per-driver copy of the schedule, and a workspace can turn the control off
// without editing this file (#1221).

/// Assemble the session's system prompt from a `base` instruction set plus
/// the workspace's saved memories and the workspace rules section (Tier 1
/// soft adherence, `stella_core::rules`). Both are loaded ONCE per session
/// and concatenated deterministically so the resulting prefix is
/// byte-stable across every model call — that stability is what lets the
/// whole prompt (instructions + memories + rules) ride the provider's
/// prompt cache instead of being re-billed. Memories saved mid-session
/// deliberately do NOT appear until the next session: hot-injecting them
/// would invalidate the cached prefix on every save. This coexists with
/// `SessionMemory`'s per-turn recall block (memory.rs) — the baked prefix
/// carries durable lessons, the recall block carries turn-relevant memories
/// and skills. The rules rendered here are the same set whose Tier-2 guards
/// `crate::rules::enforce_workspace_rules` arms at the tool boundary.
pub(crate) fn assemble_system_prompt(
    base: &str,
    workspace_root: &std::path::Path,
    authority: &crate::settings::AuthorityPolicy,
    active_rules: &crate::rules::ResolvedRules,
) -> String {
    let mut prompt = base.to_string();
    // Package-manager scripts are ordinary task source and remain part of the
    // evaluated repository. Claim-mode isolation excludes only Stella/agent
    // state that can carry preinstalled prompt steering across trials.
    if crate::settings::filesystem_settings_disabled() {
        append_project_scripts(&mut prompt, workspace_root);
        append_project_orientation(&mut prompt, workspace_root);
        return prompt;
    }
    if authority.project_prompts_allowed {
        append_project_scripts(&mut prompt, workspace_root);
        append_project_orientation(&mut prompt, workspace_root);
        append_workspace_memories(&mut prompt, workspace_root);
        append_exploration_index(&mut prompt, workspace_root);
    }
    // The cached channel: `must` and `should` records, grouped by force, each
    // carrying its `^handle` so the model can name what it followed. Byte-stable by
    // construction — the truth sweep already demoted or dropped anything whose
    // freshness is in question, so no clock and no per-turn text enters here
    // (docs/design/adaptive-context/context-record-examples/07-agent-projection.md).
    let rules_section = active_rules
        .registry()
        .render(stella_core::records::Channel::Cached, None)
        .text;
    if !rules_section.is_empty() {
        prompt.push('\n');
        prompt.push_str(&rules_section);
    }
    prompt
}

/// The workspace-maps half of [`assemble_system_prompt`]: the exploration
/// store's index — every COMPLETED map with its per-file freshness verdict —
/// so orientation is pushed at turn 1 instead of waiting for the model to
/// think of pulling it. Computed ONCE per session (freshness verdicts
/// included) for the same prompt-cache byte-stability reason as memories;
/// maps saved mid-session by other sessions surface through the registry's
/// coverage hints instead.
///
/// In-progress drafts are deliberately NOT here. Their line names the
/// producing pid and whether it is still alive, which differs per process
/// and flips mid-session — inside the cached prefix that is a guaranteed
/// miss on every call (#639). They ride the volatile recall block instead,
/// via `stella_tools::exploration::render_draft_claims`.
fn append_exploration_index(prompt: &mut String, workspace_root: &std::path::Path) {
    let summaries = stella_tools::exploration::summaries_sync(workspace_root);
    if let Some(index) =
        stella_tools::exploration::render_index(&summaries, EXPLORATION_INDEX_BUDGET_CHARS)
    {
        prompt.push('\n');
        prompt.push_str(&index);
    }
}

/// The project-scripts section of [`assemble_system_prompt`]: the scripts
/// index's canonical verb → command bindings, rendered once at session
/// start right after the base instructions (project ground truth before
/// recalled lessons). Detection is static manifest parsing
/// (`stella_tools::scripts`, docs/design/scripts-index.md) and the section
/// is byte-stable for the same workspace state, so "install this project"
/// costs one `run_script` call and zero discovery turns. Empty workspaces
/// render nothing.
fn append_project_scripts(prompt: &mut String, workspace_root: &std::path::Path) {
    let index = stella_tools::scripts::ScriptIndex::detect_blocking(workspace_root);
    if let Some(section) = index.render_prompt_section() {
        prompt.push_str("\n\n");
        prompt.push_str(&section);
    }
}

/// The project-map section of [`assemble_system_prompt`]: the graph-derived
/// languages, top-level layout, entry points, and storage — the complement
/// of the scripts section above, and bounded by construction so it stays
/// useful on monorepos far past a few hundred files (issue #328). Read-only
/// (`stella_tools::overview::render_orientation_block`
/// opens an existing index and never builds one), so it adds nothing to
/// first-response latency; it appears once the session's background index
/// build has completed (or immediately when the workspace was pre-indexed,
/// as the benchmark adapter does). Byte-stable for a given index state, so it
/// keeps the cache-stable system prefix stable. The point is fewer
/// grep/glob/read_file discovery turns: the model starts knowing the shape of
/// the code.
fn append_project_orientation(prompt: &mut String, workspace_root: &std::path::Path) {
    if let Some(section) = stella_tools::overview::render_orientation_block(workspace_root) {
        prompt.push_str("\n\n");
        prompt.push_str(&section);
    }
}

/// This workspace's workspace-memory tombstones, or why they could not be read.
///
/// A workspace with no store has nothing forgotten — that is an empty filter,
/// not a failure. A store that exists but cannot be read *is* a failure, and
/// the caller fails closed on it.
fn workspace_suppression(
    workspace_root: &std::path::Path,
) -> Result<stella_store::SurfaceSuppression, String> {
    match stella_store::existing_workspace_private_sqlite_path(workspace_root, "store.db") {
        Ok(None) => return Ok(stella_store::SurfaceSuppression::none()),
        Ok(Some(_)) => {}
        Err(e) => return Err(format!("suppression state unavailable: {e}")),
    }
    stella_store::Store::open(workspace_root)
        .and_then(|store| store.suppression_for(stella_store::ContextSurface::WorkspaceMemory))
        .map_err(|e| format!("suppression state unavailable: {e}"))
}

/// The memories half of [`assemble_system_prompt`]: append the workspace's
/// saved memories (filename order, budget-capped, **tombstone-filtered**) to
/// `prompt`, or leave it untouched when there are none.
///
/// # Forgetting one has to stop it shipping
///
/// These files are pasted into the system prompt, and until #712 they were the
/// one context surface with no suppression filter of any kind in front of them.
/// `stella memory forget --surface workspace-memory <name>` wrote a tombstone
/// that nothing here read, so the memory kept arriving in every prompt — a hole
/// in a guarantee the product had already made.
///
/// The filter is [`stella_store::SurfaceSuppression`], the same one every other
/// surface uses, with this surface's own policy resolved inside it: id match
/// always, restatement match only where the surface allows it, which for
/// authored files is never (a person re-writing a memory by hand means it).
///
/// **Fail-closed.** If the suppression state cannot be read, no workspace
/// memories are appended and the omission is stated in the prompt rather than
/// left for the model to not notice. Shipping a memory someone forgot is worse
/// than shipping none: the forget is the explicit instruction, and the file is
/// still on disk for a later turn.
fn append_workspace_memories(prompt: &mut String, workspace_root: &std::path::Path) {
    let dir = workspace_root.join(".stella/memories");
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
                .collect()
        })
        .unwrap_or_default();
    if files.is_empty() {
        return;
    }
    files.sort();

    let suppression = match workspace_suppression(workspace_root) {
        Ok(suppression) => suppression,
        Err(error) => {
            prompt.push_str(&format!(
                "

Workspace memories were omitted from this prompt: {error}. They are still on disk in .stella/memories/ and will return once the suppression state is readable."
            ));
            return;
        }
    };

    let mut memories = String::new();
    let mut used = 0usize;
    let mut dropped = 0usize;
    for file in &files {
        let Ok(body) = std::fs::read_to_string(file) else {
            continue;
        };
        let name = file
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("memory");
        // The tombstone is keyed by filename stem, which is what
        // `ContextSurface::WorkspaceMemory` records.
        // No count of these is reported. A budget omission is worth telling
        // the model about, because it can be fixed by consolidating files; a
        // forget is an instruction that this memory is gone, and announcing
        // "three memories are being withheld" invites exactly the asking-about
        // -it that forgetting exists to end.
        if suppression.suppresses(name, &body) {
            continue;
        }
        let entry = format!(
            "
### {name}
{}
",
            body.trim()
        );
        let cost = entry.chars().count();
        if used + cost > MEMORY_PROMPT_BUDGET_CHARS {
            dropped += 1;
            continue;
        }
        used += cost;
        memories.push_str(&entry);
    }
    if memories.is_empty() {
        return;
    }
    prompt.push_str(&format!(
        "

Workspace memories (lessons from previous sessions — apply them):
{memories}"
    ));
    if dropped > 0 {
        prompt.push_str(&format!(
            "
({dropped} additional memories exceeded the prompt budget and were omitted — consolidate .stella/memories/ to bring them back)"
        ));
    }
}

/// The `agent_engine_config` custom prompt for `kind`, when one is set —
/// it replaces the built-in BASE instruction set only; workspace memories
/// and rules still append (they are workspace context, not part of the
/// base persona, and a custom prompt should not silently disable them).
fn custom_prompt_base(cfg: &Config, kind: crate::settings::EngineAgentKind) -> Option<String> {
    cfg.engine_settings
        .as_ref()
        .and_then(|e| e.agent(kind))
        .and_then(|a| a.prompt.clone())
        .filter(|p| !p.trim().is_empty())
}

/// The raw step-loop system prompt plus workspace memories (`pub(crate)`:
/// the Command Deck session assembles the same prompt). `workspace_root`
/// is a parameter (not read off `cfg`) because fleet workers assemble the
/// prompt for their own worktree root.
pub(crate) fn build_system_prompt(
    cfg: &Config,
    workspace_root: &std::path::Path,
    active_rules: &crate::rules::ResolvedRules,
) -> String {
    let base = custom_prompt_base(cfg, crate::settings::EngineAgentKind::Default);
    assemble_system_prompt(
        base.as_deref().unwrap_or(SYSTEM_PROMPT),
        workspace_root,
        &cfg.authority,
        active_rules,
    )
}

/// The pipeline-mode system prompt plus workspace memories — the WORKER
/// agent's custom prompt applies here.
pub(crate) fn build_pipeline_system_prompt(
    cfg: &Config,
    workspace_root: &std::path::Path,
    active_rules: &crate::rules::ResolvedRules,
) -> String {
    let base = custom_prompt_base(cfg, crate::settings::EngineAgentKind::Worker);
    assemble_system_prompt(
        base.as_deref().unwrap_or(PIPELINE_SYSTEM_PROMPT),
        workspace_root,
        &cfg.authority,
        active_rules,
    )
}

pub(crate) fn render_file_tree(files: &str, max_lines: usize) -> String {
    let mut paths: Vec<&str> = files.lines().filter(|l| !l.is_empty()).collect();
    paths.sort_unstable();
    if paths.is_empty() {
        return String::new();
    }
    let total = paths.len();
    let mut out: String = paths
        .iter()
        .take(max_lines)
        .cloned()
        .collect::<Vec<_>>()
        .join(
            "
",
        );
    if total > max_lines {
        out.push_str(&format!(
            "
... ({} more files)",
            total - max_lines
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{PIPELINE_SYSTEM_PROMPT, SYSTEM_PROMPT, append_workspace_memories};

    /// Every static prompt, labelled.
    const PROMPTS: &[(&str, &str)] = &[
        ("SYSTEM_PROMPT", SYSTEM_PROMPT),
        ("PIPELINE_SYSTEM_PROMPT", PIPELINE_SYSTEM_PROMPT),
    ];

    /// The catalogue-shaped lines of `prompt`: `- <tool_name>: …`, where the
    /// lead-in is a bare canonical tool name or a `/`-joined run of them.
    /// This is the exact shape the hand-maintained catalogue took.
    fn schema_restating_lines(prompt: &str) -> Vec<&str> {
        prompt
            .lines()
            .filter(|line| {
                let Some(rest) = line.strip_prefix("- ") else {
                    return false;
                };
                let Some((lead, _)) = rest.split_once(':') else {
                    return false;
                };
                lead.split('/')
                    .map(str::trim)
                    .all(|name| stella_tools::catalog::get(name).is_some())
            })
            .collect()
    }

    /// The #639 regression guard, and the acceptance criterion for it.
    ///
    /// Both prompts opened with a per-tool catalogue that restated the
    /// generated schemas. Those schemas are serialized at position 0 of the
    /// SAME cached prefix the system prompt sits in, so a default session
    /// (~46 tools) bought every description twice on every call — ~1,240
    /// tokens of pure duplication, billed for the life of the session.
    ///
    /// A tool's own `description` is the one place its behaviour is written
    /// down. If a new line here would restate one, it belongs in the schema.
    #[test]
    fn neither_prompt_re_enumerates_the_generated_tool_schemas() {
        for (label, prompt) in PROMPTS {
            let restated = schema_restating_lines(prompt);
            assert!(
                restated.is_empty(),
                "{label} restates what the tool schemas already carry — put it \
                 in the tool's own description instead (#639):\n{}",
                restated.join("\n")
            );
            assert!(
                !prompt.contains("You have these tools available"),
                "{label} reopens a hand-maintained tool catalogue (#639)"
            );
        }
    }

    /// What survived the cut is cross-tool steering: composition rules and the
    /// availability model, neither of which a single tool's schema can express.
    /// Pin the three claims so a later trim cannot quietly take them too.
    #[test]
    fn both_prompts_keep_the_steering_the_schemas_cannot_carry() {
        for (label, prompt) in PROMPTS {
            for claim in [
                // Composition, not description: read_symbol beats the
                // graph_query → guessed-offset round-trip (#330, #388).
                "guessing read_file offsets after a graph_query",
                // Composition: one transactional call, not a chain (#333).
                "ONE apply_edits call",
                // A schema can only describe a tool that IS registered — it
                // can never explain an absence or how to lift it.
                "not available in this session",
                // The switch's real polarity. Pinned as `off` on purpose:
                // this sentence read `"bash": "on"` — "there is no shell
                // unless the workspace enables it" — for every release after
                // #710 shipped bash registered-by-default, so the prompt told
                // the model the opposite of the tool surface it had, and this
                // assertion pinned the false claim in place (#615).
                "tools\": {\"bash\": \"off\"}",
                "tool_search",
            ] {
                assert!(
                    prompt.contains(claim),
                    "{label} dropped steering that no tool schema carries: {claim:?}"
                );
            }
        }
    }

    /// The catalogue was copy-pasted into both prompts and had already rotted
    /// — `verify_done` and `ask_user` took comma splices in the pipeline copy
    /// where the base copy took an em dash (#450). The replacement is one
    /// shared literal, so pin that both prompts embed it byte-identically
    /// rather than growing a second copy to drift from.
    #[test]
    fn both_prompts_embed_the_one_shared_steering_literal() {
        let shared = tool_steering!();
        for (label, prompt) in PROMPTS {
            assert!(
                prompt.contains(shared),
                "{label} does not embed the shared steering block verbatim"
            );
        }
    }

    /// Witness for #712 deliverable 6: forgetting a workspace memory stops it
    /// shipping in the system prompt.
    ///
    /// These files are pasted into the prefix, and until this change they were
    /// the one surface with no suppression filter in front of them — so
    /// `stella memory forget` recorded a tombstone that nothing read and the
    /// memory arrived in every prompt regardless. The guarantee had already
    /// been made; only this surface did not keep it.
    #[test]
    fn a_forgotten_workspace_memory_does_not_reach_the_system_prompt() {
        let workspace = tempfile::tempdir().expect("workspace");
        let root = workspace.path();
        let memories = root.join(".stella/memories");
        std::fs::create_dir_all(&memories).unwrap();
        std::fs::write(memories.join("kept.md"), "KEEP_THIS_LESSON").unwrap();
        std::fs::write(memories.join("dropped.md"), "FORGET_THIS_LESSON").unwrap();

        let mut before = String::new();
        append_workspace_memories(&mut before, root);
        assert!(
            before.contains("KEEP_THIS_LESSON") && before.contains("FORGET_THIS_LESSON"),
            "both ship before anything is forgotten: {before}"
        );

        let store = stella_store::Store::open(root).expect("open store");
        store
            .forget(
                stella_store::ContextSurface::WorkspaceMemory,
                "dropped",
                "FORGET_THIS_LESSON",
                "no longer true",
            )
            .expect("forget");

        let mut after = String::new();
        append_workspace_memories(&mut after, root);
        assert!(
            after.contains("KEEP_THIS_LESSON"),
            "forgetting one memory must not withhold the others: {after}"
        );
        assert!(
            !after.contains("FORGET_THIS_LESSON"),
            "a forgotten workspace memory must not appear in the system prompt: {after}"
        );
        // Reversible and singular (spec §5.7).
        assert!(
            store
                .restore(stella_store::ContextSurface::WorkspaceMemory, "dropped")
                .expect("restore")
        );
        let mut restored = String::new();
        append_workspace_memories(&mut restored, root);
        assert!(
            restored.contains("FORGET_THIS_LESSON"),
            "restore brings it back: {restored}"
        );
    }

    /// An authored surface is suppressed by id, never by resemblance.
    ///
    /// `ContextSurface::suppresses_restatements` is false for workspace
    /// memories, and this pins that the shared filter honors that policy rather
    /// than applying the restatement half everywhere. A person re-writing a
    /// memory by hand means it; swallowing it because it resembles something
    /// forgotten months ago would be its own bug.
    #[test]
    fn forgetting_one_authored_memory_does_not_suppress_a_similar_one() {
        let workspace = tempfile::tempdir().expect("workspace");
        let root = workspace.path();
        let memories = root.join(".stella/memories");
        std::fs::create_dir_all(&memories).unwrap();
        let lesson = "always run the migration before the deploy step";
        std::fs::write(memories.join("old.md"), lesson).unwrap();
        std::fs::write(memories.join("rewritten.md"), lesson).unwrap();

        let store = stella_store::Store::open(root).expect("open store");
        store
            .forget(
                stella_store::ContextSurface::WorkspaceMemory,
                "old",
                lesson,
                "superseded",
            )
            .expect("forget");

        let mut prompt = String::new();
        append_workspace_memories(&mut prompt, root);
        assert!(
            prompt.contains("### rewritten"),
            "an identical authored memory under a different name still ships: {prompt}"
        );
        assert!(
            !prompt.contains("### old"),
            "the forgotten one does not: {prompt}"
        );
    }
}
