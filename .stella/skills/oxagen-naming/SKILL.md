---
name: oxagen-naming
description: Naming conventions across the Oxagen monorepo — kebab-case files/dirs, FooProps interfaces, SCREAMING_SNAKE constants, is/has booleans, and the ADR-025 verb-first snake_case capability-name standard (with the still-dotted filename legacy note). Use when creating a new file, naming a capability, a type, a constant, or a boolean, or when reviewing a diff for naming consistency.
---

# Oxagen naming conventions

## The one big nuance: capability *names* vs. capability *file* names are intentionally on two different conventions

**ADR-025** (`docs/adr/ADR-025-verb-first-snake-naming.md`, accepted 2026-07-08) retired ADR-022's dotted `domain.subject.action` capability-name form and renamed all registered capabilities to verb-first `snake_case` (`recall_memory`, `create_org`, `list_agent_defs`, `dispatch_subagent`, ...). This is the name that lives in the registry, IAM `role_grants.capability_id`, and ClickHouse `tool_invocations.capability_name` — the actual load-bearing identity, validated by `node tools/scripts/check-naming.mjs` (0 hard violations across all registered capabilities; a handful of non-blocking 4-word warnings like `set_default_sandbox_template` are allowed when global uniqueness truly demands it).

Canonical form per the lint: imperative verb FIRST, then the entity, then an optional disambiguating word — `verb_noun` or `verb_noun_qualifier`, lowercase `[a-z0-9]` joined by `_`, no dots. Global uniqueness is mandatory — two contracts claiming one name is a hard fail.

**But** per ADR-025 itself, contract/route/mcp/handler/docs **file** names and the dotted-import call sites are a *separate, deferred* realignment phase — confirmed in code: `packages/oxagen/src/contracts/agent.memory.recall.ts` registers `name: "recall_memory"`; `apps/api/src/routes/v1/agent.memory.recall.ts` and `apps/mcp/src/tools/agent.memory.recall.ts` still use the old dotted filename; `packages/handlers/src/register.ts` binds `registerHandler("recall_memory", () => import("./agent.memory.recall"))`. `docs/capabilities/*.md` also uses dotted filenames.

**Do not "fix" this piecemeal.** It's an explicitly-tracked, ~895-site, cross-cutting rename that ADR-025 itself defers as a dedicated follow-up phase. A partial rename desyncs `check:manifest`'s file-path heuristic further, not less. If you notice the two-tier state, that's expected — flag a dedicated follow-up PR rather than renaming a handful of files as a drive-by.

## Dominant conventions (high confidence, sampled across the repo)

| Area | Convention | Example |
|---|---|---|
| React component files | kebab-case `.tsx` | `apps/app/src/components/**/*.tsx` |
| Hooks | kebab-case file, `use-` prefix | `use-tool-stream.ts`, `use-media-query.ts` |
| Server actions | always exactly `actions.ts` | never `Actions.ts`/`action.ts` |
| Utilities/libs | kebab-case | `field-fill-transition.ts`, `use-latest-ref.ts` |
| Contract/route/mcp/handler *files* | dotted `domain.subject.action.ts` (ADR-022 legacy, tracked) | see above |
| Directories | kebab-case, except Next.js dynamic-route segments | `[orgSlug]`, `[workspaceSlug]` are camelCase by **framework convention**, not a violation |
| React `Props` types | `interface FooProps` (not `type FooProps =`) | ~99% dominant across the codebase |
| Other types/interfaces | `interface` for object shapes, `type` for unions/aliases | no `IFoo` prefix anywhere |
| Constants | `SCREAMING_SNAKE_CASE` | `A2A_PROTOCOL_VERSION`, `DEFAULT_TIER`, `INTERACTIVE_AGENT_NAME` |
| Booleans (vars/props) | `is`/`has` prefix dominant | `isActive`, `isAdmin`, `hasMore`, `hasError` |
| Booleans (result fields, minority) | bare past-participle adjective, no prefix | `hydrated`, `inferred`, `truncated`, `found` (e.g. `apps/app/src/components/knowledge/graph-explorer/types.ts`) — idiomatic, not a violation |
| Test file location | co-located `*.test.ts(x)` next to source | `__tests__/` is the rare grouped/integration exception |
| Drizzle schema | TS var camelCase, SQL identifier snake_case | `orgSchema.table("organizations", { avatarUrl: text("avatar_url") })` |
| CLI command files | mostly single-word, some dotted `domain` families | `code.ts`, `mcp.ts` vs. `graph.lineage.ts`, `graph.search.ts` — the dotted ones deliberately group a `graph` subcommand family |

## What's NOT a violation (don't "fix" these)

- Next.js dynamic-route segment folders (`[orgSlug]`, `[nodeId]`) using camelCase inside brackets — that's App Router syntax, not a project choice.
- The `type`/`interface` split — clean and consistent as-is.
- Mixed adjective-vs-`is`/`has` booleans in the same file/package (e.g. `hydrated` next to `isActive`) — low confusion cost, reads naturally, not worth a call-site-touching rename.
- Mixed single-word vs. dotted-domain CLI command file names — both are locally understandable; CLI filenames were never claimed to be 1:1 with capability names.
- `__tests__/` directories existing alongside co-located tests — they house genuinely cross-file/integration suites where grouping outside any one source file makes sense.

## Citing nodes/edges in the UI — the naming corollary that IS enforced

**Never display a node's or edge's raw UUID as its primary on-screen identifier.** Cite it by human label (`displayName` + domain `label`), make the citation inspectable (hover/click reveals the full property bag + a copyable id). Use the shared components — `NodeRef` (`apps/app/src/components/knowledge/graph/node-ref.tsx`), `sourceNodeRef(edge)`/`targetNodeRef(edge)`, and the graph-explorer's `PropertyList`/`ConfidenceMeter`/`CopyableId`/`colorForLabel` primitives — never hand-roll a `<span>{id}</span>`. Resolve the label **server-side** in the handler (`{ id, label, displayName, properties }` shape), never ship a bare id and hope the UI has a label for it.

## Violations to avoid

- Renaming a contract/route/mcp/handler file to match its ADR-025 capability name as a drive-by fix — this is a tracked ~895-site follow-up phase, not a piecemeal task.
- Introducing a new capability name that isn't verb-first snake_case, or that collides with an existing name (`node tools/scripts/check-naming.mjs` catches both).
- Adding an `IFoo`-style interface prefix, or using `type FooProps =` instead of `interface FooProps`.
- Naming a new constant in camelCase when it's a true module-level constant (should be `SCREAMING_SNAKE_CASE`).
- Rendering a raw node/edge UUID directly in the UI instead of resolving to `displayName`/`label` server-side and using `NodeRef`.
- Creating a new `__tests__/` directory for a single-file-scoped test instead of co-locating it.
