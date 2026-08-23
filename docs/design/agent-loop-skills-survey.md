# Agent-loop survey: skills, tools, tasks, and the skills-first question

Survey date: 2026-08-10. A ten-question audit of what the agent loop actually
exposed on that date — what existed, what did not, and whether "skills drive
everything" is a direction worth taking. Every claim carries the file and line
where it was read. Line numbers are as of this date and will drift; read
every "exists today" below as "existed on 2026-08-10".

## 1. Skills: no invocation tool, no functions, no prompt text

**There is no skill-execution tool.** No `skill`, `use_skill`, or `run_skill`
exists anywhere in `stella-tools`. Three skill-*adjacent* tools exist, all
registered at the CLI session layer (`crates/stella-tools/src/catalog.rs:227-230`),
never in the native registry:

| Tool | Purpose | Where |
|---|---|---|
| `skill_search` | Rank installed skills by fit; `include_body: true` returns the top match's full text | `crates/stella-cli/src/discovery.rs:51`, impl `:416-503` |
| `search_skills` | Query the public registry (`npx skills find`) | `crates/stella-cli/src/interactive.rs:259-272` |
| `install_skill` | `npx skills add` into `.stella/skills/`, asks the user first | `crates/stella-cli/src/interactive.rs:273-289` |

The model can find and read a skill; nothing executes one. The primary
delivery path is host-side context injection:
`select_skills` → `render_skills_section` → volatile recall block
(`crates/stella-cli/src/memory/recall.rs:138-146`).

**Skills have no named functions.** The parser
(`crates/stella-core/src/skills.rs:244-291`) accepts exactly four frontmatter
keys — `name`, `description`, `domains`, `origin` — plus one opaque markdown
body. The `Skill` struct (`skills.rs:94-110`) has no entry-point map, no
`scripts:`, no `allowed-tools`. The filesystem source reads only `<slug>.md`
or `<slug>/SKILL.md` (`crates/stella-cli/src/memory/skill_files.rs:15-42`);
anything else in a skill directory is invisible.

**The system prompt never mentions skills.** `SYSTEM_PROMPT` and
`PIPELINE_SYSTEM_PROMPT` (`crates/stella-cli/src/agent/prompt.rs:110-140`,
`:142+`) contain zero skill guidance. Specifically:

- No instruction to consult a skill tool before executing work when the user
  requests a skill.
- No instruction to detect relevant skills from the user's prompt. Relevance
  detection exists but is host-side and deterministic: `select_skills`
  (`skills.rs:418-473`) does lexical Jaccard over name+description with a
  0.5 domain boost, min score 0.08, top-5; winners are injected under the
  header `## Applicable skills (selected for this task — apply the relevant
  ones)` (`skills.rs:546-547`). That header is the entire extent of
  skill-related prompt text the model ever sees.
- No instruction about recognizing named skill requests — that is handled
  deterministically before the model sees anything (§8 below).

The only "use this FIRST" language lives inside `skill_search`'s own tool
description, and it means "before `search_skills`", not "before the work".

## 2. Agent dispatch: yes, the `task` tool

> **Renamed since this survey was written.** The spawn tool is now
> **`delegate`** (#3192): its catalog row's `group` was `task` while its
> `name` was also `task`, so one settings key addressed both the tool and
> the six-row board. `task` remains the *group* key and still withholds
> all seven coordination tools. This section is left as written because
> it is a record of the surface at the time; read `task` below as
> `delegate`.

The model-visible spawn tool is **`task`**
(`crates/stella-tools/src/subagent.rs:215`, registered at
`crates/stella-tools/src/registry.rs:449-453`). `parallel_safe() -> true`
(`subagent.rs:261-263`); the description tells the model to issue several
`task` calls in one step for fan-out. Children are read-only by construction
(`read_only: false` on the spawn tool means children behind `ReadOnlyTools`
never see it — `catalog.rs:200-203`), which is what caps nesting.

A second path, `task_assign` (`crates/stella-tools/src/tasks.rs:374`),
delegates a board task to a sub-agent via the `SpawnQueue`.

`stella-fleet` is **not** reachable as a tool — nothing in `stella-tools`
depends on it; it is exposed only through the `stella fleet` CLI command
(`crates/stella-cli/src/cli.rs:705-708`).

## 3. Sleep: no tool; parked waits instead

No `sleep`/`wait`/`schedule_wakeup` tool exists. The mechanism is the
**parked-wait** seam: a tool surfaces a wait request
(`Tool::take_wait_request`, `registry.rs:73-75`; aggregated by
`ToolRegistry::drain_wait_request`, `registry.rs:2144-2153`) and the engine
parks the turn at a step boundary (#1471) rather than sleeping in-tool. The
only shipped producer is `ci_status` with `wait: true`
(`crates/stella-tools/src/ci.rs:222-255`).

## 4. Plans and tasks: yes, a six-tool board

`crates/stella-tools/src/tasks.rs` — the board is "the session's visible
plan"; there is no separate plan-mode or todo tool:

- `task_create` (:85) — accepts a `tasks` array to lay down a whole plan in
  one call
- `task_list` (:196)
- `task_start` (:236)
- `task_complete` (:281) — completed is terminal
- `task_cancel` (:325)
- `task_assign` (:374) — delegate to a sub-agent

Snapshots surface as `AgentEvent::TaskUpdate`
(`crates/stella-protocol/src/event.rs:944`).

## 5. Skill-for-creating-skills: no; it is a product feature

No shipped meta-skill exists. Skill authoring is first-class in the Command
Deck: `SKILL_AUTHOR_SYSTEM` and `create_skill_llm`
(`crates/stella-cli/src/command_deck/skills.rs:308-445`) — registry search for
prior art, one model call labeled `skill_author`, validation via
`skill_from_file`, write through `skill_manager::create`. Separately, skills
auto-promote from recurring reflection lessons (`auto_create_skills`,
`crates/stella-cli/src/memory/learning.rs:464-507`; mining in
`crates/stella-core/src/skills.rs:669+`; session cap 2, no-clobber,
`origin: auto`).

## 6. MCP server listing: a tool, not a skill

**`mcp_search`** (`crates/stella-cli/src/discovery.rs:52`, schema
`:296-315`): scope `workspace` lists servers from `.stella/mcp.toml` merged
with servers actually connected this session, plus their
`mcp__<server>__<tool>` names; scope `registry` queries the public MCP
registry. Session-layer only (`catalog.rs:232`). CLI equivalent:
`stella mcp list|search|install|remove|login|logout|usage`
(`crates/stella-cli/src/cli.rs:1247-1285`).

## 7. Agent-to-agent messaging: none

No `send_message`, teammate, or inbox concept exists in `stella-tools`,
`stella-fleet`, or `stella-protocol`. Communication is strictly
parent-mediated and one-directional: parent → child via the spawn `prompt`
(`subagent.rs:236-241`) or `briefing` (`tasks.rs:383`); child → parent via
final findings only. Siblings cannot communicate. `ask_user`
(`crates/stella-cli/src/interactive.rs:389`) is the only "message someone"
tool, and its target is the human.

## 8. Slash-invoking a skill: text expansion, never dispatch

`/skill-slug args` works in both front-ends. `CustomExtensions::lookup` /
`expand` (`crates/stella-cli/src/extensions.rs:689-731`; commands shadow
skills shadow agents) expands to:

```text
Apply the following skill.

# Skill: {name}
{description}

{body}

## Task
{args}
```

(`extensions.rs:774-784`). Call sites: plain REPL
(`crates/stella-cli/src/agent.rs:1052-1060`) and the Command Deck
(`crates/stella-cli/src/command_deck.rs:4087-4098`).

Consequences:

- No code path invokes a named function within a skill — there are no
  functions to invoke (§1).
- Writing "invoke function X with args Y" as the slash arguments merely lands
  that sentence under `## Task` as prose the model interprets. There is no
  function resolution, no argument binding (`$ARGUMENTS`/`$1..$9`
  substitution is commands-only,
  `crates/stella-core/src/extensions.rs:448-507`), and no execution
  guarantee. A `/skill` invocation is prompt-text expansion, not a tool call.

## 9. "Skills drive everything": assessment

**Effort: moderate-to-large.** Three layers would have to change:

1. **Schema** — add entry points to `Skill`: a `functions:` map in
   frontmatter (name, description, arg schema, prompt template or
   executable). Parser and file source both extend easily; this part is
   small.
2. **Invocation** — a real `skill` tool in the registry taking
   `{slug, function, args}` that expands a template or runs a script. The
   `run_script`/`list_scripts` machinery is the natural substrate — an
   executable skill function is essentially a namespaced custom tool, which
   custom tool configs already model. Medium effort; execution must land
   in `stella-tools` and selection stays pure in `stella-core` (invariants
   #1/#2).
3. **The loop itself** — the hard part, and the part this survey recommends
   against.

**Recommendation: do not make skills drive the control loop.**

- The loop's spine is deliberately deterministic and typed: the staged
  pipeline, the flip oracle, the budget guard, the roster — property-tested
  pure functions, with the verification ladder terminal-by-construction
  (`ladder_decision`). Reframing those as model-interpreted markdown trades
  compile-time guarantees for prompt adherence — the failure class the
  verifier-ablation incident (#2569/#2570) already demonstrated.
- Skills are advice by design ("never enforced — selected and injected as
  volatile context", AGENTS.md), which is what keeps a bad auto-promoted
  skill from breaking the agent. Making them required removes that
  safety property.
- Cache economics: the system prompt is byte-stable (invariant #7); skills
  ride the volatile suffix. Moving orchestration into skill text moves
  driving logic into the uncached, model-interpreted zone.

**Where skills-first is right — the capability plane, not the control
plane:**

1. **Prompt gap (highest value, lowest cost):** the system prompt says
   nothing about skills. Add a paragraph: skills exist, check `skill_search`
   before nontrivial work, honor explicit skill requests.
2. **Executable skill functions** as sugar over custom script tools —
   additive, breaks no invariant.
3. **`/slug function args`** resolving a named entry point instead of
   dumping raw args under `## Task`.

## Appendix: default tool inventory

Canonical table: `crates/stella-tools/src/catalog.rs:128-231`; construction in
`ToolRegistry::with_backends_and_options` (`registry.rs:404-548`).

- **file:** `read_file`, `read_symbol`, `write_file`, `edit_file`,
  `apply_edits`, `delete_file`
- **search:** `grep`, `glob`, `graph_query`
- **context:** `project_overview`, `gather_context`, `explorations`,
  `save_exploration`, `save_memory`, `cite_memory`
- **build:** `verify_done`, `build_project`, `run_tests`, `diagnostics`,
  `run_lint`, `format_code`
- **scripts:** `list_scripts`, `run_script`
- **process:** `start_process`, `read_output`, `send_stdin`, `stop_process`
- **repo:** `repo_status`, `repo_diff`, `repo_history`, `repo_recover`,
  `repo_commit`, `repo_push`, `repo_pull`, `repo_rollback`
- **ci/media:** `ci_status`, `screenshot`, `generate_svg`
- **task:** `task_create`, `task_list`, `task_start`, `task_complete`,
  `task_cancel`, `task_assign`, `task`
- **bash/web:** `bash`, `web_fetch`, `web_extract_assets`, `web_download`
- **Conditional:** `web_search` (BYOK key), `generate_image` /
  `generate_video` / `poll_video` (media backend + approving host), the
  issue family (`create_issue` … `start_work_on_issue`; resolved
  Linear/GitHub backend) — `catalog.rs:209-224`
- **Session layer only:** `ask_user`, `search_skills`, `install_skill`,
  `tool_search`, `skill_search`, `mcp_search` — `catalog.rs:226-232`
- **Dynamic:** MCP tools as `mcp__<server>__<tool>`
  (`crates/stella-mcp/src/toolset.rs:61`)
