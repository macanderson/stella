---
name: oxagen-tenancy
description: Multi-tenant scoping in Oxagen — runInTenantScope, withTenantDb/withSystemDb, the Postgres RLS GUCs, principal attribution, and the raw db() ban. Use when writing any code that reads or writes tenant-scoped Postgres data, or when reviewing a change for cross-tenant isolation risk.
---

# Oxagen tenancy pattern

**Raw `db()` is banned.** Every Postgres access goes through `withTenantDb` (normal, tenant-scoped) or `withSystemDb` (explicit, audited RLS-bypass escape hatch). This is what makes Row-Level Security actually load-bearing instead of decorative.

## Files

- `packages/tenancy/src/scope.ts` — AsyncLocalStorage-based scope + principal attribution.
- `packages/tenancy/src/errors.ts` — `TenantScopeError`.
- `packages/database/src/tenant.ts` — `withTenantDb` / `withSystemDb` / startup guards.

## Entering scope

```ts
// packages/tenancy/src/scope.ts
export function runInTenantScope<T>(scope: TenantScope, fn: () => T): T {
  assertUuid(scope.orgId, "orgId");
  assertUuid(scope.workspaceId, "workspaceId");
  // ...
  return als.run({ orgId: scope.orgId, workspaceId: scope.workspaceId, /* ... */ }, fn);
}
```

`runInTenantScope({orgId, workspaceId, ...}, fn)` validates both ids are UUIDs and **fails closed** (throws `TenantScopeError`) otherwise, then runs `fn` inside an `AsyncLocalStorage` context. `requireScope()` — used by every data accessor — throws if no scope is active; this is the actual enforcement point.

## `withTenantDb` / `withSystemDb` (`packages/database/src/tenant.ts`)

```ts
export async function withTenantDb<T>(fn: (tx: Tx) => Promise<T>): Promise<T> {
  const { orgId, workspaceId } = requireScope();
  const bypass = rlsEnforced() ? "off" : "on";
  return db().transaction(async (tx) => {
    await tx.execute(sql`
      select
        set_config('app.current_org_id', ${orgId}, true),
        set_config('app.current_workspace_id', ${workspaceId}, true),
        set_config('app.rls_bypass', ${bypass}, true)
    `);
    return fn(tx);
  });
}
```

`withTenantDb(fn)` opens a Drizzle transaction, sets three Postgres GUCs (`app.current_org_id`, `app.current_workspace_id`, `app.rls_bypass`) via `set_config(..., true)` (transaction-local), then runs `fn(tx)`. RLS policies read those GUCs. Keep the body focused — do not wrap a long LLM/tool call in one `withTenantDb`; the transaction is held for the callback's lifetime.

`withSystemDb(fn)` is the **explicit, audited RLS-bypass escape hatch** — for identity resolution before a scope exists, webhook org lookups, cron jobs, bootstrap (org/workspace creation), and security-audit writes that must succeed even on a no-scope deny. It is NOT a shortcut for normal handlers. Every call increments an unscoped-call counter (`recordIfUnscoped`) so operators have a metric for "is it safe to flip RLS enforcement on" — when `db.query.unscoped` reads zero during the seeding window, enforcement can flip on safely.

## Startup guards

- `assertRlsEnforcedInProduction()` — refuses to boot in production if RLS enforcement is off (`TENANT_RLS_ENFORCEMENT_ENABLED=false` in prod is a hard failure, not a warning).
- `assertRlsConnectionSafe()` — refuses to boot if the DB connection role is a superuser or has `BYPASSRLS` (which would make RLS silently inert even with `FORCE ROW LEVEL SECURITY`). Requires the `oxagen_app` non-superuser role in production.

## Principal attribution

`PrincipalAttribution` (`principalId`, `principalKind: "human"|"agent"|"service"`, `userId`, `capabilityName`) rides alongside the tenant scope — **attribution metadata only, never an authorization boundary** (IAM decides access; this records who a decision was *for*). Enriched post-IAM-resolution via `runWithPrincipal`, a nested ALS run (scope entry happens before the principal is known):

```ts
export function runWithPrincipal<T>(attribution: Partial<PrincipalAttribution>, fn: () => T): T {
  const current = als.getStore();
  if (!current) return fn();          // no-op passthrough when no scope is active
  return runInTenantScope({ ...current, ...attribution }, fn);
}
```

## Where the kernel enters scope

`packages/oxagen/src/kernel.ts` enters `runInTenantScope` when `cap.scoped !== false` **or** both `ctx.orgId`/`ctx.workspaceId` are valid UUIDs — this dual condition exists because some unscoped-by-data-ownership capabilities (e.g. `plugin.schema.get`) still read through `withTenantDb` when invoked with real tenant ids, and skipping the scope wrap there throws `TenantScopeError` mid-handler.

## Gotcha — `apps/app` does not bootstrap IAM

`invoke()` called from `apps/app` skips IAM role checks. Add explicit `assertBillingManager` / `assertOrgMember` gates at the call site in Server Actions/route handlers; do not rely on the kernel to enforce IAM from that surface.

## Violations to avoid

- Calling the raw `db()` client directly anywhere outside `packages/database`'s own internals — always go through `withTenantDb`/`withSystemDb`.
- Using `withSystemDb` as a convenience shortcut inside a normal tenant-scoped handler — it bypasses RLS and must be reserved for the narrow, documented cases above, each with an obvious comment at the call site.
- Treating `principalId`/`principalKind` as an authorization signal — it is attribution only; IAM makes the actual allow/deny decision.
- Wrapping a long-running LLM/tool call inside a single `withTenantDb` transaction.
- Assuming `apps/app` enforces IAM automatically — it does not; gate explicitly.
