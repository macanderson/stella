---
name: oxagen-surface-patterns
description: Hono API route conventions in apps/api and xmcp MCP tool conventions in apps/mcp — thin-adapter shape, auth middleware, capabilityContext, and error shaping. Use when writing or reviewing an API route or MCP tool that dispatches a capability.
---

# Oxagen surface patterns (API + MCP)

Both `apps/api` (Hono REST) and `apps/mcp` (xmcp) are **thin adapters**: they parse transport input, build a `CapabilityContext`, call `invoke()`, and return the result. No route or tool does its own IAM/billing check — that is entirely the kernel's job (see `oxagen-capability-contracts`).

## API route (Hono)

Real example, `apps/api/src/routes/v1/document.list.ts`:

```ts
import { Hono } from "hono";
import { documentList } from "@oxagen/oxagen/contracts/document.list";
import { invoke } from "@oxagen/oxagen/kernel";
import { capabilityContext } from "../../lib/context";
import type { AppEnv } from "../../app";

export const documentListRoute = new Hono<AppEnv>();

documentListRoute.get("/", async (c) => {
  const workspace_id = c.req.query("workspace_id");
  const input = documentList.input.parse({ workspace_id });
  const ctx = capabilityContext(c);
  const out = await invoke(documentList.name, input, ctx, { surface: "api" });
  return c.json(out);
});
```

Every route: (1) parses HTTP input with the **contract's own** zod schema — never a bespoke schema, (2) builds `CapabilityContext` via `capabilityContext(c)`, (3) calls `invoke(<contract>.name, input, ctx, { surface: "api" })`, (4) returns `c.json(out)`.

Routes are mounted in `apps/api/src/app.ts` — one `import` + one `.route(...)` per capability domain. `check:manifest`'s file-path heuristic has documented false positives for combined route files (`workflow.ts`, `connection.ts`, `integration.ts`, `repo.ts`, `semantic-edge.ts`, `semantic-relationship.ts`, `schema.ts`, `plugin-schema.ts`) that each cover many capabilities — verify by reading the file before filing a parity gap.

### `capabilityContext()` (`apps/api/src/lib/context.ts`)

Builds the `CapabilityContext` from Hono context vars set by earlier middleware (`orgId`, `workspaceId`, `userId`, `apiKeyId`, `requestId`). Throws `HTTPException(400)` if org/workspace scope is required but missing. Pulls the client IP from `x-forwarded-for`/`x-real-ip` — documented as **IAM-condition-only, never an auth boundary**, since those headers are spoofable.

### Auth middleware (`apps/api/src/middleware/auth.ts`)

Bearer token → API key resolver (`resolveApiKey`); else Better Auth session cookie → `resolveSession()`. Sets `userId`/`apiKeyId`/`orgId`/`workspaceId` context vars for `capabilityContext` to read later. API keys pre-bind org/workspace scope so downstream org/workspace middleware (`middleware/org.ts`, `middleware/workspace.ts`) skips slug resolution when those vars are already set. The middleware is a thin HTTP adapter — it delegates all identity logic to the transport-agnostic resolvers in `@oxagen/auth`, it does not resolve sessions itself.

## MCP tool (xmcp)

Real example, `apps/mcp/src/tools/agent.memory.recall.ts`:

```ts
import { type InferSchema, type ToolMetadata } from "xmcp";
import { headers } from "xmcp/headers";
import { agentMemoryRecall } from "@oxagen/oxagen/contracts/agent.memory.recall";
import { invoke } from "@oxagen/oxagen/kernel";
import { buildContext } from "../context";

export const schema = {
  ...agentMemoryRecall.input.shape,
  query: agentMemoryRecall.input.shape.query.describe("Semantic search query"),
  // re-describe select fields for MCP UX
};

export const metadata: ToolMetadata = {
  name: agentMemoryRecall.name,       // same canonical capability name as the contract
  description: agentMemoryRecall.description,
  annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true },
};

export default async function agentMemoryRecallTool(args: InferSchema<typeof schema>) {
  const ctx = await buildContext(headers());
  const output = await invoke(agentMemoryRecall.name, args, ctx, { surface: "mcp" });
  return agentMemoryRecall.output.parse(output);
}
```

MCP tools: re-export the contract's zod `.shape` (spread, then selectively re-`.describe()` fields for better model-facing UX); declare `readOnlyHint`/`destructiveHint`/`idempotentHint` annotations; the default export is always `invoke(<name>, args, ctx, {surface:"mcp"})` then re-parse through `.output` before returning — this is defense in depth (the kernel already validated output; re-parsing guarantees the MCP wire shape matches exactly).

## Error shaping

Both surfaces rely on the centralized error mapping — see `oxagen-error-handling` for the full `CapabilityError` → HTTP status table. Never `try/catch` a `CapabilityError` in a route to translate it yourself; `app.onError(errorMiddleware)` in `apps/api/src/app.ts` already does this uniformly.

## Violations to avoid

- Writing a bespoke Zod schema in a route instead of `contract.input.parse(...)` — this desyncs the HTTP surface from the capability's real input shape.
- Doing an IAM/org-membership check inline in a route handler instead of trusting the kernel's IAM gate (exception: `apps/app`, which does NOT bootstrap IAM — see `oxagen-tenancy`).
- Skipping the MCP tool's final `agentMemoryRecall.output.parse(output)` re-validation.
- Adding a second API auth mechanism instead of extending `@oxagen/auth`'s resolvers.
- Building an MCP tool schema from scratch instead of spreading the contract's `.input.shape`.
