---
name: oxagen-ai-calls
description: The @oxagen/ai chokepoint — modelIdOf(), white-labeled model tiers, metering emission, the ban on importing generateText/streamText/generateObject directly from "ai", and the ai/rsc ban. Use whenever writing or reviewing code that calls an LLM, picks a model, or streams a response.
---

# Oxagen AI-call pattern (`@oxagen/ai`)

**Never import `generateText`/`streamText`/`generateObject`/`embed` directly from `ai` inside a handler or route.** Always go through `@oxagen/ai` — it is the only package allowed to import the raw AI SDK, and it is what makes metering, duration tracking, surface tagging, and prompt hashing to ClickHouse enforceable.

## Single chokepoint (`packages/ai/src/index.ts`)

Re-exports everything a caller needs: `generateObjectFor`, `generateImageFor`, `generateVideoFor`, `streamAgentReply`, `embedText`, plus `tool`/`jsonSchema`/`stepCountIs` re-exported *from* `ai` itself (so agent-domain code never imports `"ai"` directly either). This single-chokepoint design is what makes "no direct `ai` import outside `packages/ai`" a lintable/greppable rule (`grep -r 'from "ai"'` outside `packages/ai` should return nothing).

```ts
export { generateObjectFor } from "./generate-object";
export { generateImageFor } from "./generate-image";
export { generateVideoFor, /* ... */ } from "./generate-video";
export { streamAgentReply } from "./stream";
export { embedText } from "./embed";
export { tool, jsonSchema, stepCountIs } from "ai"; // re-exported, not a direct import elsewhere
```

## Model resolution — `modelIdOf()`

```ts
// packages/ai/src/models.ts
export function modelIdOf(model: LanguageModel): string {
  return typeof model === "string" ? model : model.modelId;
}
```

Required because AI SDK v6's `LanguageModel` type is a union that also admits a bare model-id string — `.modelId` is not always safe to access directly. **Never hard-code a gateway slug**; always resolve through `selectModel()`/`modelIdOf()`. Verify any new model against `/v1/models` before using it.

## White-labeled tiers (vendor-neutrality mechanism, not just naming)

```ts
export type OxagenTier = "fast" | "balanced" | "precise";
export const DEFAULT_TIER: OxagenTier = "balanced";
```

`OxagenTier` maps to `OXAGEN_LLM_FAST/BALANCED/PRECISE` env vars → concrete Vercel AI Gateway ids (e.g. `anthropic/Codex-sonnet-5`). Customer-facing tier names stay decoupled from the underlying vendor/model — this is the BYOK/vendor-neutrality moat, not cosmetic. Every call routes through `@ai-sdk/gateway`, the platform's single AI auth boundary (`AI_GATEWAY_API_KEY`), so one seam reaches every vendor with no per-provider SDK or key and no direct-provider fallback.

```ts
export function selectModel(selector: ModelSelector = {}): LanguageModel {
  const env = requireEnv(["OXAGEN_LLM_FAST", "OXAGEN_LLM_BALANCED", "OXAGEN_LLM_PRECISE"] as const);
  const modelId = selector.model ?? tierFromEnv(env, selector.tier ?? DEFAULT_TIER);
  return applyDevtools(gateway.languageModel(modelId));
}
```

Image (`selectImageModel`) and video (`selectVideoModel`) selection follow the identical pattern with their own tier env vars (`OXAGEN_LLM_IMAGE_*`, `OXAGEN_LLM_VIDEO_*`).

## Dev-only devtools middleware

`applyDevtools()` wraps the model with `@ai-sdk/devtools` middleware only when `NODE_ENV === "development"` — tree-shaken/absent in prod (the package is a devDependency). Catches a missing-package error to a silent no-op so a `--production` install never breaks the app.

## Metering emission

Not exposed as a separate call — it's baked into the `generate-*`/`stream*` wrappers themselves (along with `batch.ts` for half-price background inference and `cache.ts` for the layered exact+semantic response cache), so callers get metering "for free" by using `@oxagen/ai`'s functions instead of the raw SDK. This is the enforcement mechanism: bypassing `@oxagen/ai` means silently losing metering, cost tracking, and prompt hashing.

## `ai/rsc` is forbidden

`ai/rsc` (`streamUI`, `createStreamableUI`, `createAI`) is **forbidden** everywhere. `@ai-sdk/react` is permitted only for non-chat client surfaces (e.g. a standalone completion widget) — the main chat path is the SSE stream described in `oxagen-app-conventions`, never a second transport.

## Violations to avoid

- `import { generateText } from "ai"` (or `streamText`/`generateObject`/`embed`) anywhere outside `packages/ai` — always import the `@oxagen/ai` wrapper instead.
- Hard-coding a gateway model id string (`"anthropic/Codex-sonnet-5"`) at a call site instead of resolving through `selectModel()`/tier env vars.
- Using `ai/rsc` (`streamUI`, `createStreamableUI`, `createAI`) anywhere — it is banned outright, not just discouraged.
- Adding a second AI-call transport/pipeline instead of extending the existing `@oxagen/ai` chokepoint.
- Accessing `.modelId` directly on a `LanguageModel` value instead of calling `modelIdOf()`.
