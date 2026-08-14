---
name: "source-command-audit-soc2"
description: "Audit SOC 2 Type II readiness across the codebase. Checks access controls, audit logging, secrets management, encryption, and change management. Outputs a scored HTML report."
---

# source-command-audit-soc2

Use this skill when the user asks to run the migrated source command `audit-soc2`.

## Command Template

# /audit-soc2

Run a focused SOC 2 Type II readiness audit. Work autonomously. File Linear tickets for every FAIL. Never defer — fix what can be fixed in code immediately; ticket what requires a human or infra action.

## Phase 0 — Load context

- Read `AGENTS.md`, `AGENTS.md`, `policies/0-prime-directives.md`.
- Capture `SHORT_SHA` and UTC timestamp.
- Enumerate all `apps/` and `packages/` directories.
- Pull current Linear labels via `list_issue_labels`. Pull current open SOC 2 tickets via `list_issues` filtered to `soc-2` label.

## Trust Service Criteria coverage

Run all checks in parallel after Phase 0. Each maps to a TSC control category.

### CC6 — Logical Access Controls

1. **Auth bypass scan** — `grep -rn "skipAuth\|bypassAuth\|noAuth\|isPublic.*=.*true\|auth.*false"` in route/middleware files. Any hit that disables auth on a non-public route → FAIL.
2. **Role enforcement** — for every mutating API route (`POST`, `PUT`, `PATCH`, `DELETE`), confirm it calls `assertBillingManager`, `assertOrgMember`, or equivalent IAM gate. A mutation route with no role check → FAIL.
3. **API key scope** — `grep -rn "api.key"` in route files. Confirm keys are scoped to org, not global. Unscoped keys → FAIL.
4. **Session token storage** — confirm no session tokens or JWTs are logged (`grep -rn "console.log.*token\|logger.*jwt\|log.*session"`) → FAIL per hit.
5. **Password hashing** — confirm `bcrypt` / `argon2` / `scrypt` in use; plaintext comparison (`===` on password fields) → FAIL.

### CC6 — Least Privilege

6. **Raw `db()` ban** — `grep -rn "from.*'@oxagen/database'.*import.*db\b\|import.*\bdb\b.*from.*database"` in `apps/` (not `packages/database`). Any direct `db()` call outside the approved wrappers (`withTenantDb`/`withSystemDb`/`scopedSession`) → FAIL.
7. **Superuser flag** — confirm `oxagen_app` DB role has `NOSUPERUSER` and `NOBYPASSRLS`. Check migration files and seed scripts for `ALTER ROLE oxagen_app SUPERUSER` → FAIL.
8. **IAM seed** — confirm `pnpm db:seed-iam` has been run (check for role rows in schema or seed idempotency guard in migrations).

### CC7 — System Operations / Change Management

9. **Secrets in code** — `grep -rn "sk_live_\|sk_test_\|AKIA\|ghp_\|lin_api_\|lin_access_\|-----BEGIN"` across the repo (excluding `node_modules`, `.git`). Any hit → FAIL.
10. **Secrets in env example files** — check `.env.example` files for real-looking secret values (not `YOUR_KEY_HERE` placeholders) → FAIL.
11. **Audit trail on privileged mutations** — for each mutation that changes IAM (role change, member add/remove, API key create/revoke), confirm an audit event is emitted to ClickHouse. Missing audit call → FAIL.
12. **Soft-delete only** — confirm destructive operations use `deleted_at` (soft delete), never hard `DELETE` on user, org, conversation, or billing rows. `grep -rn "\.delete()\|DELETE FROM"` in ORM calls on those tables → WARN per hard delete found.

### CC9 — Risk Mitigation

13. **Dependency vulnerability scan** — run `pnpm audit --prod 2>&1 | tail -50`. Any critical or high severity CVE → FAIL with package name and CVE ID.
14. **Pinned versions** — `grep -rn '"\^[0-9]\|"~[0-9]\|"\*"'` in all `package.json` files under `apps/` and `packages/`. Any floating range → FAIL.

### A1 — Availability

15. **Health endpoint** — confirm each deployed app (`apps/api`, `apps/mcp`) has a `/health` or `/healthz` route that returns 200 with no auth. Missing → WARN.
16. **Error boundary** — confirm `apps/app` has a React error boundary at the root layout level. Missing → WARN.

### PI1 — Processing Integrity

17. **Input validation** — for each API route that accepts a body, confirm Zod (or equivalent) schema validation is applied before any DB write. Route with no input validation → FAIL.
18. **Rate limiting** — confirm `rateLimit` / `rateLimits` (note: drizzleAdapter pluralizes to `rateLimits`) table exists and is wired to auth routes. Missing → FAIL.

## Fix protocol

- Issues fixable in code (missing Zod schema, missing audit call, floating dep version): **fix now, in-place**.
- Issues requiring infra or human action (DB role provisioning, env var rotation): **file Linear ticket** with label `soc-2`, priority P1, assignee Mac Anderson, title `[SOC2] <description>`, and a clear acceptance checklist.
- Do not duplicate existing open Linear tickets — check the pulled list first.

## Output

`mkdir -p docs/audits/soc2-audits`

Write: `docs/audits/soc2-audits/<SHORT_SHA>_<TIMESTAMP>_soc2-audit.html`

Self-contained HTML (all CSS + JS inline). Must contain:

1. Header: repo, branch, SHA, timestamp.
2. **Compliance hero** — overall SOC 2 readiness score (0–100), color-coded, with a READY / NOT READY verdict.
3. **TSC score grid** — one stat box per TSC category (CC6 Access, CC6 Privilege, CC7 Change Mgmt, CC9 Risk, A1 Availability, PI1 Integrity), each scored 0–100.
4. **Findings table** — check # · control category · PASS/WARN/FAIL · finding · action taken or Linear ticket link.
5. **Critical blockers** list (all FAILs).
6. **Linear tickets filed** list (title + ID for each new ticket).
7. **Scoring methodology** footnote.

Same dark-panel visual style as `/release-audit`. Data-driven renderer — fill a `DATA` object, let JS render.
