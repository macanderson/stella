---
name: oxagen-error-handling
description: Error-handling conventions in Oxagen — typed error classes with a stable `code` field, the centralized Hono onError mapping, log-then-rethrow for risky async work, and the ban on swallowed catches. Use when adding a new error type, writing a try/catch, or wiring error handling in a new route or handler.
---

# Oxagen error-handling conventions

## Typed error classes, each carrying a stable `code`

Never just throw a bare `Error` with a message for anything an API surface needs to branch on — carry a stable `code` field so centralized HTTP mapping is possible without string-matching messages.

```ts
// packages/tenancy/src/errors.ts
export class TenantScopeError extends Error {
  readonly code = "no_tenant_scope" as const;
  constructor(message: string) { super(message); this.name = "TenantScopeError"; }
}

// packages/billing/src/metering.ts
export class InsufficientCreditsError extends Error {
  readonly code = "insufficient_credits" as const;
  constructor() { super("Insufficient credits: your balance is empty..."); this.name = "InsufficientCreditsError"; }
}

// packages/oxagen/src/kernel.ts
export class CapabilityError extends Error {
  constructor(readonly capability: string, readonly code: CapabilityErrorCode, message: string) {
    super(message); this.name = "CapabilityError";
  }
}
```

`CapabilityErrorCode` is a closed union: `"unknown_capability" | "no_handler" | "surface_denied" | "authz_denied" | "invalid_input" | "invalid_output" | "capability_not_installed"`.

## Centralized mapping at the transport edge — never per-route try/catch

`apps/api/src/middleware/error.ts`, wired via `app.onError(errorMiddleware)` in `apps/api/src/app.ts`:

- `HTTPException` → its own status, logged `warn`.
- `ZodError` → 400 `validation_error` with `.issues` detail.
- `CapabilityError` → switches on `.code`: `authz_denied`/`surface_denied` → 403, `unknown_capability`/`no_handler` → 404, `invalid_input` → 400. `invalid_output` falls through to the generic 500 — a contract-output mismatch is a **server bug**, not a client error.
- Billing errors → 402 Payment Required. Duck-typed via a `code` field check (`isBillingError()`) specifically to avoid a direct `@oxagen/billing` dependency from the API's error middleware:

```ts
type BillingErrorCode = "insufficient_credits" | "billing_suspended";
function isBillingError(err: unknown): err is BillingError {
  if (typeof err !== "object" || err === null) return false;
  const code = (err as Record<string, unknown>).code;
  return code === "insufficient_credits" || code === "billing_suspended";
}
```

- True catch-all (unhandled/unexpected) → `logger.error`, fire-and-forget `captureError({ error, source: "api", severity: "error", orgId, workspaceId, requestId })` to ClickHouse (plus a Slack-compatible alert if `ALERT_WEBHOOK_URL` is set), then a generic `{ code: "internal_error" }` 500 — **never leak the raw error message to the client** for unclassified errors, unlike the typed branches above which do surface `.message`.
- Every response shares one envelope shape: `{ error: { code, message, details? }, requestId }`.

Do not write a route-level `try/catch` that duplicates this mapping — throw the typed error and let `app.onError` handle it.

## Inside handlers — fail fast, log before rethrow, never swallow

```ts
// packages/handlers/src/document.generate.ts
if (!ctx.userId) throw new Error("documents.generate: userId is required...");
```

Fail fast with a plain `throw new Error(...)` for precondition violations that should never happen. For genuinely-risky async work (file encoding, external API calls):

```ts
try {
  // ...
} catch (err) {
  logger.error({ err, ...context }, "operation: failed");
  throw err; // log with structured context, then rethrow — never swallow
}
```

## The one sanctioned exception — fire-and-forget observability

The kernel's own emitters (`emitSecurityEvent`, `emitTraceEvent`) are explicitly wrapped in their own `try { ... } catch { /* never let a broken emitter crash a capability invocation */ }`. This pattern is reserved for **side-channel telemetry only** — never for the primary business-logic error path. Do not copy this pattern to justify swallowing an error anywhere else.

## Violations to avoid

- Swallowing a `catch` block with no rethrow and no logging — silent failure.
- Throwing a bare `Error` where a typed error with a `.code` is expected by the surface's error middleware.
- Writing a per-route `try/catch` that reimplements the centralized `errorMiddleware` mapping instead of letting `app.onError` handle it.
- Leaking a raw unclassified error `.message` to the client on a 500 — only the typed branches (`HTTPException`, `ZodError`, `CapabilityError`, billing errors) are allowed to surface `.message`.
- Treating `invalid_output` as a client error (4xx) instead of the server bug it is (500).
- Copying the fire-and-forget "swallow in a bare catch" pattern from the telemetry emitters into real business logic.
- Adding a direct `@oxagen/billing` import to the API's error middleware instead of the duck-typed `isBillingError()` check.
