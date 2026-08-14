---
name: "source-command-audit-tenancy"
description: "Audit multi-tenancy enforcement — RLS policies, withTenantDb usage, query-time tenant filters, and cross-tenant data isolation gaps. Outputs a scored HTML report and files Linear tickets for gaps."
---

# source-command-audit-tenancy

Use this skill when the user asks to run the migrated source command `audit-tenancy`.

## Command Template

# /audit-tenancy

Audit multi-tenancy isolation across the full stack. Work autonomously. Fix what can be fixed in code immediately; file Linear tickets for gaps requiring schema migrations or infra changes.

## Phase 0 — Load context

- Read `AGENTS.md`, `AGENTS.md`, `policies/0-prime-directives.md`.
- Capture `SHORT_SHA` and UTC timestamp.
- Confirm whether `TENANT_RLS_ENFORCEMENT_ENABLED` is set to `true` in all deployed environments (check Vercel env via the MCP or from memory).
- Pull the current Linear `soc-2` + `database` + `security` tickets to avoid duplicates.
- Identify the tenant scoping columns from the schema: `org_id`, `workspace_id`, or equivalent — grep migrations for the canonical column names.

## Phase 1 — Schema tenant coverage

For every table in `packages/database/src/schema/`:

1. Classify the table:
   - **System table** (users, orgs, workspaces, plans) — tenant-root; RLS not expected.
   - **Org-scoped table** — must have `org_id` column + RLS policy filtering by `app.current_org_id`.
   - **Workspace-scoped table** — must have `workspace_id` (and usually `org_id`) + RLS policy filtering by `app.current_workspace_id`.
   - **Audit/log table** — append-only; tenant filter is read-time, not write-time; check that read queries are filtered.

2. For each org-scoped and workspace-scoped table:
   - Confirm `org_id` / `workspace_id` column exists → FAIL if missing.
   - Confirm a corresponding RLS policy exists in the migration files (`grep -rn "CREATE POLICY.*<table_name>"`) → FAIL if missing.
   - Confirm the policy is `USING (org_id = current_setting('app.current_org_id')::uuid)` or equivalent — policy that uses a superuser bypass or is always-true → FAIL.

## Phase 2 — Application layer enforcement

3. **Raw `db()` ban** — `grep -rn "\bdb()\b"` in `apps/` (not `packages/database`). Any direct `db()` call → FAIL with file:line.
4. **withTenantDb coverage** — for every route handler and server action that reads org/workspace data, confirm it calls `withTenantDb` or `scopedSession`. A route that calls `withSystemDb` for a tenant-scoped query → FAIL.
5. **Session setting injection** — confirm `withTenantDb` sets `app.current_org_id` and `app.current_workspace_id` via `SET LOCAL` before queries. Grep the implementation for `SET LOCAL app.current_org_id` → FAIL if missing.
6. **`oxagen_app` role** — confirm routes use the `oxagen_app` non-superuser connection (not the superuser `DATABASE_URL`). The `oxagen_app` role must have `NOBYPASSRLS` — confirm in migration or seed script.

## Phase 3 — API / MCP surface audit

7. For every API route in `apps/api/src/routes/v1/`:
   - Confirm `org_slug` and/or `workspace_slug` are extracted from the path or session, not from the request body (user-supplied org scope → FAIL).
   - Confirm the resolved org/workspace ID is set in the DB session before any query.

8. For every MCP tool in `apps/mcp/src/tools/`:
   - Confirm tools accept org/workspace scope from the authenticated API key, not from tool arguments (tool argument–driven tenant scope → FAIL).

## Phase 4 — Cross-tenant leak test (static analysis)

9. **Unscoped list queries** — `grep -rn "\.findMany()\|\.select()\.from(" ` in `apps/` (not `packages/database`). Any `.findMany()` or `.select()` call with no `.where(` clause that would filter by `org_id` / `workspace_id` → WARN (may be intentional for system queries — annotate with comment).
10. **Shared cache keys** — `grep -rn "cache\|Redis\|unstable_cache"` in `apps/`. Cache keys that don't include `org_id` or `workspace_id` on tenant-scoped data → FAIL.
11. **File/blob access control** — confirm every `/api/v1/assets/[id]` and `/api/v1/files/[id]` route verifies the requesting user's org matches the asset's `org_id`. Missing ownership check → FAIL.

## Phase 5 — Plugin isolation

12. **Plugin credential scoping** — confirm `plugin_credentials` table has `workspace_id` and RLS policy. Plugin tools must not share credentials across workspaces.
13. **MCP server routing** — confirm the agent's MCP server list is filtered to the active workspace (`agent.mcp_servers` joined to `org_listings` with workspace scope). Unscoped MCP server list → FAIL.

## Fix protocol

- Schema gaps (missing `org_id` column, missing RLS policy): **file a Linear ticket** with label `database` + `soc-2`, size L/XL, P1. Do not write migrations without user confirmation.
- Application layer gaps (raw `db()` calls, missing `withTenantDb`): **fix in-place now**.
- Cache key gaps: **fix in-place now** — add org/workspace segment to cache key.

## Output

`mkdir -p docs/audits/tenancy-audits`

Write: `docs/audits/tenancy-audits/<SHORT_SHA>_<TIMESTAMP>_tenancy-audit.html`

Self-contained HTML (all CSS + JS inline). Must contain:

1. Header: repo, branch, SHA, timestamp, `TENANT_RLS_ENFORCEMENT_ENABLED` value.
2. **Isolation hero** — overall tenancy score (0–100), color-coded, with ISOLATED / GAPS verdict.
3. **Layer score grid** — stat boxes for: Schema RLS, App Layer, API Surface, MCP Surface, Plugin Isolation, Blob Access. Each 0–100.
4. **Table coverage table** — one row per DB table: table name · classification · `org_id` present · RLS policy · status badge.
5. **Gap findings table** — file/table · layer · issue · severity · action taken or Linear ticket.
6. **Linear tickets filed** list.
7. **Scoring methodology** footnote.

Same dark-panel visual style as `/release-audit`. Data-driven renderer — `DATA` object, JS renders tables.
