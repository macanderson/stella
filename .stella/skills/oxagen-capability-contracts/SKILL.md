---
name: oxagen-capability-contracts
description: Defining a capability contract, the registry, handler binding, and the invoke() dispatch pipeline in the Oxagen monorepo. Use when adding, renaming, or reviewing a capability contract, a handler, or anything that calls invoke() — and to check the capability-parity rule (contract → API route → MCP tool → CLI → UI).
---

# Oxagen capability contracts

Every user-facing action in Oxagen is a **capability**: one declarative contract, dispatched by one kernel function (`invoke()`), bound to one handler. This is the object that carries governance (IAM, billing, entitlements, audit) for free — never re-implement any of that at a call site.

## 1. Define the contract

File: `packages/oxagen/src/contracts/<domain>.<subject>.<action>.ts` (file name stays dotted — see `oxagen-naming` for why). Real example, `packages/oxagen/src/contracts/agent.memory.recall.ts`:

```ts
import { z } from "zod";
import { registerCapability } from "../registry";

export const agentMemoryRecall = registerCapability({
  name: "recall_memory",              // ADR-025 canonical id — verb-first snake_case, globally unique
  domain: "agent",
  description: "Query Neo4j AgentMemory by semantic similarity...",
  mode: "sync",
  surfaces: ["api", "mcp", "agent"],   // which transports may dispatch this
  layers: ["schema", "api", "mcp", "unit", "e2e", "docs"], // checked by check:manifest
  scoped: true,                        // requires org+workspace tenant scope
  agent: { requiresApproval: false, riskLevel: "low", category: "memory" },
  sensitivity: "low",
  defaultEffect: "deny",               // IAM default when no explicit grant
  defaultRoles: {
    org: { Owner: "allow", Admin: "allow" },
    workspace: { Owner: "allow", Member: "allow" },
  },
  input: z.object({ query: z.string().min(1) /* ... */ }),
  output: z.object({ memories: z.array(z.object({ /* ... */ })) }),
});

export type AgentMemoryRecallInput = z.output<typeof agentMemoryRecall.input>;
export type AgentMemoryRecallOutput = z.output<typeof agentMemoryRecall.output>;
```

The contract only **declares** policy fields (`defaultEffect`, `defaultRoles`, `sensitivity`, `scoped`, `noBillingGate`) — it never does IAM/billing/metering itself. All of that lives in the kernel dispatch path.

## 2. Registry (`packages/oxagen/src/registry.ts`)

`registerCapability()` writes into a `Map` anchored on `globalThis` via `Symbol.for("@oxagen/oxagen.capabilityRegistry")`. This is deliberate: Turbopack/webpack can evaluate the same contract module twice (barrel import + subpath import + RSC/SSR graphs), so anchoring on `globalThis` keeps one canonical registry across module instances.

- Same name + same descriptor → deduped silently (returns the first registration).
- Same name + different descriptor → **warns** in dev (assumed bundler/HMR artifact), never throws at runtime. A genuine duplicate-name collision is caught by `pnpm check:manifest` at build time, not at runtime.

## 3. Bind a handler

Handler file: `packages/handlers/src/<domain>.<subject>.<action>.ts`, e.g. `packages/handlers/src/document.generate.ts`:

```ts
export const documentsGenerateHandler: CapabilityHandler<typeof documentsGenerate> = async (input, ctx) => {
  if (!ctx.userId) throw new Error("documents.generate: userId is required...");
  // business logic, structured logger.info/error around risky steps
  return { assetId, publicId, /* ... */, render: { componentId: "file-attachment", props: {...} } };
};
```

Note the `render: { componentId, props }` convention — a handler that wants generative UI output embeds a render directive; the chat client's component registry maps `componentId` to a React component (see `oxagen-app-conventions`).

Registration (`packages/handlers/src/register.ts`), imported once per surface boot:

```ts
import { registerHandler, registerHandlersOnce, type CapabilityHandlerFn } from "@oxagen/oxagen/kernel";

registerHandlersOnce("@oxagen/handlers", () => {
  registerHandler("recall_memory", async () =>
    (await import("./agent.memory.recall")).agentMemoryRecallHandler as CapabilityHandlerFn,
  );
  // one entry per capability, always a dynamic import (lazy-loaded)
});
```

Handlers are lazy dynamic imports so booting a surface doesn't eagerly pull in Stripe/Neo4j/Docker clients until a capability actually fires. `registerHandlersOnce(token, fn)` makes re-evaluation (dev HMR) a no-op instead of tripping `registerHandler`'s duplicate-registration guard, which throws hard on a genuine double-bind.

**Gotcha:** `import "@oxagen/handlers/register"` must run before any `invoke()` call in a given process, or `resolveHandler()` throws a loud `no_handler` `CapabilityError` — this is NOT a silent no-op. IAM/billing/entitlement gates are a *separate* injection (`setKernelIAMRuntime`, `setBillingAdmissionGate`, `setCapabilityEntitlementGate`, called once at surface bootstrap in `apps/api/src/index.ts` and the mcp middleware) — forgetting *those* specifically **does** silently no-op (kernel proceeds with no IAM/billing check, "tests, CLI" fallback).

## 4. Call it — every surface calls the same function

```ts
import { invoke } from "@oxagen/oxagen/kernel";
const out = await invoke(agentMemoryRecall.name, input, ctx, { surface: "api" | "mcp" | "agent" });
```

## 5. The `invoke()` pipeline (`packages/oxagen/src/kernel.ts`, `_invokeCore`), in order

1. Resolve capability from registry (`unknown_capability` if missing).
2. Enforce `surfaces[]` allowlist if `opts.surface` is passed (`surface_denied`).
3. `cap.input.safeParse(rawInput)` (`invalid_input`).
4. Enter `runInTenantScope` (from `@oxagen/tenancy`) when `cap.scoped !== false` OR both org/workspace ids are valid UUIDs — this is what makes `withTenantDb` resolvable inside the handler.
5. IAM check — **always runs and always audits** when a check fn is registered; only **blocks** when `IAM_ENFORCEMENT_ENABLED=true`. A check-fn *throw* (not a deny decision) always fails closed regardless of the enforcement flag (OXA-2056 — a prior bug let a throw fail open when enforcement was off).
6. Billing admission gate — skipped when `cap.noBillingGate === true` or the capability is unscoped.
7. Capability entitlement gate — only fires for capabilities claimed by a plugin manifest.
8. Resolve + call the handler, wrapped with `runWithPrincipal` so downstream ClickHouse/Neo4j writes get `principalId`/`principalKind` attribution for free.
9. `cap.output.safeParse(output)` — output is validated too (`invalid_output` if the handler's return drifts from contract).
10. Fire-and-forget `emitSecurityEvent` / `emitTraceEvent` at every step — never awaited, never allowed to throw into the response path.
11. Whole thing wrapped in an OpenTelemetry span (`kernel.invoke`).

This file (`packages/oxagen/src/kernel.ts`) is the single most information-dense file in the repo for "how does governance actually work."

## 6. Capability parity rule

A new user-facing action needs: contract (`packages/oxagen/src/contracts/`) → API route (`apps/api/src/routes/v1/`) → MCP tool (`apps/mcp/src/tools/`) → CLI command (`apps/cli/src/commands/`) → UI wiring in `apps/app` when a human should operate it. Run `pnpm check:manifest` (or `--json`) to verify. Contract-wiring order is law: never shortcut a UI page straight onto a raw query before the contract exists.

## Violations to avoid

- Doing IAM/billing/metering logic inside a handler or route instead of relying on the kernel pipeline — the whole point of the contract is that governance is uniform and automatic.
- Calling `invoke()` from a fresh process/runtime without first importing the handler-register module (`@oxagen/handlers/register` or equivalent) — this throws `no_handler`, it does not silently skip.
- Forgetting `setKernelIAMRuntime`/`setBillingAdmissionGate`/`setCapabilityEntitlementGate` at a new runtime's bootstrap — this silently no-ops governance instead of throwing, which is worse. Any new runtime that invokes capability-gated handlers must call `bootstrapEntitlementRuntime()` from `@oxagen/plugins` at startup.
- Bypassing `invoke()` and calling a handler function directly — skips input/output validation, IAM, billing, and audit entirely.
- Building a UI page against a raw DB query instead of waiting for contract → API route → MCP tool to exist first.
