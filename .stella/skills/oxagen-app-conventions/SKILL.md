---
name: oxagen-app-conventions
description: apps/app conventions — server/client component boundary, the actions.ts pattern, the @/components/ui re-export layer, the single chat SSE transport, the generative UI component registry, and proxy.ts (not middleware.ts). Use when writing or reviewing any Next.js page, Server Action, or chat-surface code in apps/app.
---

# Oxagen `apps/app` conventions

Next.js App Router, RSC, streaming. Default is Server Component; `"use client"` is the first line of any file needing hooks/state. A Server Component must **never** import a function from a `"use client"` file directly as a callable — only render it as a component.

## `actions.ts` pattern

One `actions.ts` per route segment directory, containing all Server Actions (mutations) for that page. Always named exactly `actions.ts` — never pluralized or suffixed differently. Examples: `apps/app/src/app/[orgSlug]/[workspaceSlug]/knowledge/memories/actions.ts`, `apps/app/src/app/(auth)/reset-password/actions.ts`.

Since `apps/app` does not bootstrap IAM (see `oxagen-tenancy`), a Server Action calling `invoke()` must add explicit `assertBillingManager`/`assertOrgMember`-style gates at the call site — do not assume the kernel enforces authorization from this surface.

## `@/components/ui/*` re-export layer

**Never import `@oxagen/ui/components/*` directly in app code.** All Next.js apps import UI components from their local re-export layer:

```ts
// ✅ import { Button } from "@/components/ui/button"
// ❌ import { Button } from "@oxagen/ui/components/button"
```

`apps/app/src/components/ui/*.tsx` (`button.tsx`, `dialog.tsx`, ...) is the *only* place allowed to import `@oxagen/ui/components/*` directly — it's a cheap override escape hatch. Enforced via `no-restricted-imports` in `eslint.next.mjs`; the ESLint config has a dedicated override block for `**/src/components/ui/*.tsx` files that relaxes the rule just for the re-export layer itself. Exceptions to the rule elsewhere: `@oxagen/ui` barrel, `@oxagen/ui/styles/*`, `@oxagen/ui/lib/*`.

## Chat transport — one SSE pipe, do not add a second

Single stream: `POST /api/v1/chat/stream`, consumed client-side by `apps/app/src/components/chat/use-tool-stream.ts` (a `"use client"` hook). It defines a rich discriminated-union event vocabulary and reducer-style live state — `LiveToolCall`, `LiveReasoning`, `LiveStep`, `LiveTextSegment`, `LivePendingApproval`, `LivePendingConsent`, `LivePlan`, `LiveFanout`, `LiveMemoryRecall` — each keyed by a stable id (`toolCallId`, `reasoningId`, `stepIndex`, `approvalId`, `fanoutId`) so partial-stream updates merge in place. **Do not add a second transport** for chat; extend this one.

## Generative UI registry (`apps/app/src/components/chat/chat-component-registry.tsx`)

Maps a stable string `componentId` (e.g. `"file-attachment"`, `"svg-preview"`, `"pr-stats"`, `"code-diff"`, `"terminal-trace"`, `"file-tree"`) to a `React.lazy()`-loaded component. A handler's output ends with `render: { componentId, props }` (see `oxagen-capability-contracts`); the message bubble dispatches on `componentId`.

```ts
/**
 * IDs are stable contracts. Never rename a key without a migration —
 * persisted content_blocks rows reference them by string.
 */
```

Unknown `componentId`s render a visible fallback (`UnknownComponentCard`) instead of silently returning `null`, plus a `logUnknownComponent()` warning — the "no silent failure" rule applied to UI. Generative UI is `generateObject` structured output only, mapped client-side via this registry — never `ai/rsc` (see `oxagen-ai-calls`), never server-rendered React trees sent as generative output.

## `proxy.ts`, not `middleware.ts`

Request interception lives in `apps/app/src/proxy.ts` — `middleware.ts` is **no longer recognized** by this Next.js version. `proxy.ts` must stay edge-safe only: cookies, URL rewrites, redirects. No Node built-ins, DB calls, or secrets.

## Violations to avoid

- `import { Button } from "@oxagen/ui/components/button"` anywhere outside `apps/app/src/components/ui/*.tsx`.
- Creating `apps/app/src/middleware.ts` — it is silently ignored; use `proxy.ts`.
- Adding a second streaming transport for chat instead of extending `use-tool-stream.ts` and its event vocabulary.
- Renaming a `componentId` key in `chat-component-registry.tsx` without a data migration for persisted `content_blocks` rows.
- Returning `null` for an unrecognized `componentId` instead of the `UnknownComponentCard` fallback + `logUnknownComponent()`.
- Calling a `"use client"` exported function directly from a Server Component instead of rendering it as a component.
- Assuming `invoke()` enforces IAM inside a Server Action in `apps/app` — it does not; gate explicitly.
- Naming a route segment's Server Actions file anything other than exactly `actions.ts`.
