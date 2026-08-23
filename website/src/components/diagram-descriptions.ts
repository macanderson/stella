/**
 * The one-sentence description of every SVG diagram in `diagrams.tsx`,
 * keyed by the component's name. Two consumers:
 *
 *  - `diagrams.tsx` sets it as the `<svg>`'s `aria-label`, so a screen
 *    reader hears what the picture says;
 *  - `src/lib/page-markdown.ts` emits it as the diagram's stand-in when a
 *    docs page is exported to markdown (Copy page / llms.mdx), where the
 *    SVG itself cannot go.
 *
 * It lives in a plain `.ts` module — not beside the components — because
 * the markdown path is exercised by `node --test`, which strips types but
 * cannot parse JSX. `diagrams.test.ts` asserts the record and the
 * component file stay in lockstep, so a diagram added here without a
 * sentence (or vice versa) fails the suite.
 */
export const DIAGRAM_DESCRIPTIONS: Record<string, string> = {
  HeroFlowDiagram: "Your prompt flows through stella to the provider you chose; telemetry stays on your machine.",
  RecallLoopDiagram: "Memories and the code graph feed the recall block, the model works, citations and reflections feed back into the stores.",
  FleetFanoutDiagram: "A pinned base commit fans out to isolated worktree branches; finished branches converge on your review.",
  QuickstartDiagram: "Four steps: install, authenticate, init, run.",
  CredentialChainDiagram: "Five credential sources tried in order — flag, environment variable, settings.json, credentials.toml, interactive prompt — and the first one that resolves is used.",
  SettingsCascadeDiagram: "Org-managed, project, and user settings merge per key into the effective settings; the org layer is a ceiling that lower scopes can narrow but never re-open.",
  PermissionGateDiagram: "A tool call passes through the PreToolUse hook and the permission rules; allowed calls execute and fire PostToolUse, denied calls come back to the model as a refusal.",
  EngineOwnershipDiagram: "Your app keeps keys, tools, and data; the engine keeps the loop, compaction, and retries. Only requests and results cross between them.",
  EngineTestHarnessDiagram: "One host drives either your real gateway or a scripted reply function; both exercise the identical agent loop, but only one of them spends money.",
  EngineGateDiagram: "One committed task set runs through both Stella and Claude Code; the comparison produces a blocking loop-correctness verdict and an advisory quality delta.",
  LoopVerdictDiagram: "Four verdicts checked in order: solved, silent death, zero work, ran but unsolved. The middle two — a turn that did nothing — are the ones that fail the gate.",
  TelemetryFlowDiagram: "Each session turn writes a receipt into .stella/ on your disk, which stella stats and the observatory read back. No arrow leaves the machine.",
  McpTopologyDiagram: "At session start Stella connects to each configured MCP server — stdio servers as local subprocesses, http servers as remote endpoints — and merges their tools into one namespaced tool set. A server that fails to connect within ten seconds is skipped, and the session continues without its tools.",
  HookLifecycleDiagram: "Three hooks fire across a turn: SessionStart, whose stdout becomes context; PreToolUse, whose non-zero exit blocks the tool; and PostToolUse, whose exit status is ignored. Only PreToolUse can stop anything.",
  SingleThreadDiagram: "A swarm is a coordinator and four agents joined by many edges, every one of them a handoff that summarizes. Stella is one ordered loop — plan, act, observe, compact — with a single return edge and one transcript.",
  EventContractDiagram: "Each line is parsed alone. An unrecognized event type is inert — skip it and keep reading. A recognized type whose body does not fit is a real error and must fail loudly.",
  BudgetGuardDiagram: "A plan meets the scope review first, which stops it before the first edit at zero cost. Steps that get past it are metered between steps and stages, never inside a tool call, so a spend-limit abort never leaves a half-applied edit.",
  CostChainDiagram: "Five commands, each narrowing the question: stats for which model and how much, observe for which run, inspect for which call, inspect with a step for what it was sent, and diff for what changed since the previous call.",
  ClaimLockDiagram: "Two tasks declaring the same path meet a shared claim table. The first holds the path for its attempt and runs; the second fails its dispatch by name, in under a second, with the rival identified.",
  EngineSequenceDiagram: "Your app starts a turn. The engine asks it to run a model call and waits for the result, then asks it to run a tool and waits again, then reports the turn complete. Every outbound call is made by your app; the engine only asks.",
  EnginePathsDiagram: "Six ways a session starts. stella, stella run, stella goal and stella fleet can bind an installed wrapper plugin, which contributes context before the turn and gathers evidence after it before calling the engine's turn loop. stella --plain and stella-serve take no wrapper: the plain REPL drives run_turn, and stella-serve drives run_step itself. Every path ends in the same step loop.",
};
