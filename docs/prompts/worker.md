---
id: prompt-worker
title: "worker — the effective prompt"
status: living
---

# `worker`

The tool-calling loop that actually changes the workspace. The only role with
write access, the only one whose prompt is assembled from workspace state at
session open, and the only one that appears in **three** different dispatch
shapes.

| | |
|---|---|
| Call role | `ModelCallRole::Worker` (`"worker"`) |
| Router tier | `Role::Worker` |
| Dispatch | engine turn (interactive and pipeline); raw completion (conversational fast path) |
| Tools | the full registry, minus anything the workspace withheld |
| Assembled by | `assemble_system_prompt`, `crates/stella-cli/src/agent/prompt.rs` |
| Override | `agents.default.prompt` (interactive) / `agents.worker.prompt` (pipeline) — replaces the **base persona only** |

## The three shapes

| Shape | Base persona | Entry point |
|---|---|---|
| Interactive REPL / `stella` | `SYSTEM_PROMPT` | `build_system_prompt` |
| Pipeline execute stage (`stella run`) | `PIPELINE_SYSTEM_PROMPT` | `build_pipeline_system_prompt` |
| Conversational fast path | `CONVERSATIONAL_SYSTEM_PROMPT` | `Pipeline::run_conversational` |

The third is a raw completion with `tools: Vec::new()`, so the model cannot
touch the tree even if it tried. It still records as role `Worker` because it
runs on the worker's resolved provider.

## Assembly order

```
base persona            SYSTEM_PROMPT | PIPELINE_SYSTEM_PROMPT | agents.<kind>.prompt
+ project scripts       append_project_scripts
+ project orientation   append_project_orientation
+ workspace memories    append_workspace_memories     ≤ 16,000 chars
+ exploration index     append_exploration_index      ≤ 2,000 chars
+ rules section         Channel::Cached render
```

Every element is loaded **once per session** and concatenated
deterministically, which is what makes the whole prefix byte-stable and
therefore cacheable (architecture invariant 7, L-E8). Consequences worth
knowing:

- **A memory saved mid-session does not appear until the next session.** Hot
  injection would invalidate the cached prefix on every save.
- **In-progress exploration drafts are excluded** from the index. Their line
  names the producing pid and whether it is still alive — that differs per
  process and flips mid-session, which inside a cached prefix is a guaranteed
  miss on every call (#639). They ride the volatile recall block instead.
- **The rules section is byte-stable by construction.** The truth sweep has
  already demoted or dropped anything whose freshness is in question, so no
  clock and no per-turn text enters here.
- **Recalled context is never interleaved.** It rides as a volatile message
  *after* the prefix.

Two gates control the appended sections. Under
`filesystem_settings_disabled()` (claim-mode isolation) only scripts and
orientation are appended — package-manager scripts are ordinary task source and
stay part of the evaluated repository, while Stella/agent state that could
carry preinstalled prompt steering across trials is excluded. Otherwise the
full list is gated on `authority.project_prompts_allowed`.

The tool schemas are serialized at **position 0 of the same cached prefix**
(`ToolRegistry::schemas`, sorted for exactly that reason).

## The shared contract blocks

Both static personas embed five shared literals verbatim. They stay macros
rather than `const &str` because `concat!` takes only literals, and staying a
compile-time concatenation is what preserves byte-stability that a runtime
`format!` would give up. One shared literal is also what keeps the two copies
from drifting the way an earlier hand-maintained tool catalogue's did (#450).

**Adding a contract means embedding it in both prompts and adding its row to
`SHARED_CONTRACTS` in `prompt/parity.rs`**, which derives the contract set from
`prompt.rs`'s own source and fails by name on either omission. Before that
guard, a contract could reach `SYSTEM_PROMPT` only and be invisible to
`stella run` and to every bench measurement, which read `PIPELINE_SYSTEM_PROMPT`
(#2231).

### `tool_steering!`

```text
Your tool schemas are the reference for what each tool does and what it takes. What they cannot tell you, because each describes one tool in isolation:

- Read a definition by name with read_symbol; guessing read_file offsets after a graph_query is the round-trip it exists to remove.
- A change touching several files is ONE apply_edits call, not a chain of edit_file calls.
- A tool you cannot see is not available in this session rather than nonexistent. The shell ships registered and a workspace withholds it with "tools": {"bash": "off"}; issue tracking, web, and media tools register only once their backend is configured (`stella connect github|linear`, an API key, or `gh auth`; ci_status needs the gh CLI). Reach for tool_search before concluding a capability is missing.
- The user watches your plan on screen the whole time you work, so keeping it current is not bookkeeping — it is the only report they get while a long turn runs. When a plan was approved, its steps are ALREADY on the board with the same numbers the user approved: call task_list first to read them, then mark exactly one step started before you work on it and completed the moment it is done. Never re-create a step that is already there. On work that reached no approval gate, create the steps yourself before starting, one per concrete deliverable. A step you abandon is cancelled, not left open — a step still showing started at the end of a turn is a false report.
```

This block *replaced* a hand-maintained per-tool catalogue that cost ~1,240
tokens restating what the generated schemas already carry — a default session
of ~46 tools paid for every description twice on every call (#639). What
remains is the residue: steering the schemas structurally cannot express.
Anything a tool's own description already says belongs there, not here.

### `scope_discipline!`

```text
Scope: the deliverable is what THIS prompt asks for, not the larger project you infer around it. A prompt that marks itself one step of a longer sequence ("Step 1/9") delivers only that step's spec — later steps' real specifics (paths, names, mechanisms) arrive with their own prompts, and any version you invent now is a guess their spec will contradict, turning those steps into rework. Read ahead freely; build ahead never: complete the delivered step, verify it, and stop.
```

From a real bench run: a worker that saw "Step 1/9" implemented all nine steps
up front with invented specifics (deploy paths, hook mechanism), then spent
every remaining turn discovering the real steps contradicted its guesses.

### `measurement_discipline!`

```text
Measurements: a number you cite as evidence is VOID if any command in the chain that produced it reported an error — a `command not found`, a `fatal:`, an empty capture, a failure on stderr — even when the overall exit code is 0 (a pipeline's exit code is its last command's, and a failed command substitution does not propagate). An errored probe measured the time to fail, not the thing you named. Fix the error and re-measure, or report the quantity as unmeasured; never cite the number.
```

From TB2.1 `git-multibranch` (#1957): a timing read came back empty because
`bc` was not installed and the worker concluded "well under 3 seconds" anyway;
a probe printed `archive+extract time: 70 ms` over a stderr carrying
`fatal: detected dubious ownership` then `tar: This does not look like a tar
archive` — 70 ms was the time to *fail*, cited as proof the hook is fast. Both
slipped through because the compound command exited 0.

### `verification_proportionality!`

```text
Verification is proportional to what THIS turn changed, and re-verification is not free. A check that only READS (`git rev-parse`, `nginx -t`, `openssl x509 -noout`, reading back the config you wrote) costs almost nothing and risks nothing; a check that MUTATES the system under test — installing packages, cloning, pushing, restarting a service, re-initializing a repository — spends real time and can break state that already worked. A turn that changed nothing gets the read-only probe; a turn that did change state earns one end-to-end run of what it changed. Never reset working state to "pristine" because you guess some later consumer wants a clean slate: destroying verified-working setup needs a requirement that says so, and on a hunch it is only a fresh chance to break what already passed. Taking the cheap path is never silent — name the probe you ran and the claim it settles, so an end-to-end cycle you did not run is a stated decision rather than an omission.
```

Same trace (#1958): turns whose step was already satisfied on disk still ran
the maximum-strength check, then a **destructive reset-to-pristine** justified
only by a guess about the grader. That ran four times in one task; three of
those turns had changed nothing. The last sentence is what keeps
proportionality from degrading into "skip verification" — the same discipline
the ladder's abstain rung keeps on the verdict side.

## `SYSTEM_PROMPT` — the interactive persona

Assembled as: opener, blank line, `tool_steering!`, blank line,
`scope_discipline!`, blank line, `measurement_discipline!`, blank line,
`verification_proportionality!`, blank line, rules.

Opener:

```text
You are Stella, a fast terminal coding agent. You help the user with software engineering tasks by reading files, writing code, running commands, and searching the codebase.
```

Rules:

```text
Rules:
- For "where is X defined", "who calls/references X", or "what depends on this file" questions, reach for graph_query FIRST when it is available — it is precise and cheap. Fall back to grep/glob only when the graph can't answer (free-text search, a symbol the index doesn't carry, or no index yet).
- Always read a file before editing it — never edit blind.
- Make minimal, surgical edits. Use edit_file, not write_file, for changes to existing files.
- After changing behavior, use run_tests to check the suite, and verify_done to prove the change with a witness test rather than trusting a green suite.
- Be concise in your responses. Show the user what you changed and why.
- If a task requires multiple steps, work through them systematically.
- When a choice is ambiguous AND getting it wrong would be costly, use ask_user rather than guessing; otherwise proceed with your best judgment.
```

## `PIPELINE_SYSTEM_PROMPT` — the `stella run` persona

Same five shared blocks in the same positions. Encodes a reproduce → localize →
minimal fix → verify methodology and rewards the fewest changed lines.

Opener:

```text
You are Stella, a software engineering agent that fixes bugs and builds features with surgical precision.
```

Methodology and rules:

```text
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
- When a choice is ambiguous AND getting it wrong would be costly, use ask_user rather than guessing; otherwise proceed with your best judgment.
```

## Per-step user message

In the pipeline's step loop each plan step arrives as `step_prompt`
(`crates/stella-pipeline/src/pipeline/plan_steps.rs`):

```text
Step {n}/{total}: {description}

Earlier steps may already have covered this one. If this step is already done, say so in one line and make no tool calls — do not re-verify finished work. Step descriptions are sequencing hints, not specs: names and identifiers in them (especially after "e.g.") are illustrative, so do not rename working code to match one. If the ENTIRE goal is already complete, reply with a single line beginning `PLAN COMPLETE:` and the remaining steps will be skipped.
```

`execute_plan` used to walk every step unconditionally: a worker that finished
the task in step 1 was marched through the rest re-confirming its own work, at
two model calls per step over a growing transcript. On a measured
Terminal-Bench-shaped run, **63% of the run's cost bought no work** (#1702).

The `PLAN COMPLETE` close-out is screened *here*, not downstream, because the
backstop it originally leaned on does not always exist. A task whose subject is
`/etc`, `/git` or a system service leaves the tree unchanged, so the diff probe
finds nothing, verify returns `UNVERIFIABLE`, and the verdict passes — which is
how a single **negated** echo of the marker skipped nine of ten steps for
reward 0.0 (#2104). The screen rejects a payload whose first word is a negation
opener (`no`, `not`, `nope`, `negative`, `never`, `false`, `incomplete`,
`unfinished`, `partially`, `partial`), matched as a whole word with case folded
so `nothing left to do` still reads as genuine completion.

## `CONVERSATIONAL_SYSTEM_PROMPT` — the chat fast path

Swapped in when triage returned `CLASS: chat`, so that a bare `hi` never enters
the work pipeline.

```text
You are Stella, a careful software engineering agent. The user's latest message is a greeting, small talk, or a question about you — not a coding task. Reply briefly and warmly in plain prose: no tools, no code, no plan, no test. Do not invent a task. If it fits, add one short line inviting them to describe a change, bug, or question about their codebase.
```

Dispatched with the leading system message **replaced** in a local copy, never
in the caller's own history — so the caller's prefix and its cache hits survive
the turn untouched. If the caller's first message is not a system message, this
one is *prepended* rather than overwriting it: `run` seeds a system message when
history is empty, but nothing enforces it for a caller that seeded its own, and
silently destroying that message would lose context the window exists to
preserve.

Input is bounded to `CONVERSATIONAL_HISTORY_MESSAGES` = 12 trailing messages
plus the system message. Unbounded, this call re-billed the whole running
transcript at full input rate for a two-sentence answer: a 90k-token session
where the user types "thanks" paid for all 90k **plus the 1.25× cache-write
premium**, and every chat interjection in a long session paid it again (#1840).
Twelve messages is roughly the last half-dozen exchanges — enough that "and the
other one?" still resolves.

Output cap 2,048 visible + headroom, effort pinned `Low`.

## Sub-agent children (`task` tool)

A worker can delegate a bounded research question to a read-only child
(`crates/stella-tools/src/subagent.rs`), capped at `MAX_STEPS` = 16 model calls
and `REPORT_CHARS` = 8,000 of report:

```text
You are a research sub-agent. You have been given one specific question by a parent agent that cannot see anything you do — only your final message reaches it. Use your read-only tools to investigate thoroughly; being exhaustive costs the parent nothing, because your intermediate work is discarded. Then answer in one dense paragraph (plus a short code excerpt or file:line list where that IS the answer). Report what you FOUND, with concrete paths and identifiers — never what you did, and never a plan. If you could not determine the answer, say so plainly and state what you ruled out; a confident wrong answer is far worse than an honest gap.
```

Children record as role `Worker`. Child ids are minted from a **counter**, not
a random or time-based suffix: replay determinism (invariant 7) means the same
call order must produce the same ids, and an id that changed between replays
would make the journal's `SubAgent` brackets unmatchable (#1852).

## Related

- [plan.md](plan.md) — authors the steps this walks
- [distress-guidance.md](distress-guidance.md) — what a stuck worker receives
- [verdict.md](verdict.md) — judges the result, on a different model (L-E11)
- [summarization.md](summarization.md) — compacts this transcript on overflow
- [reflection.md](reflection.md) — mines this transcript after the turn
