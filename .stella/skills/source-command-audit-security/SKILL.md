---
name: "source-command-audit-security"
description: "Audit application security across OWASP Top 10, auth, input validation, secrets, dependency CVEs, and API surface. Fixes safe issues in-place and files Linear tickets for the rest."
---

# source-command-audit-security

Use this skill when the user asks to run the migrated source command `audit-security`.

## Command Template

# /audit-security

Run a focused application security audit against the monorepo. Fix safe issues immediately. File Linear tickets for findings that require schema changes, infra access, or manual verification. Never defer silently — every finding gets a resolution.

## Phase 0 — Load context

- Read `AGENTS.md`, `AGENTS.md`.
- Capture `SHORT_SHA` and UTC timestamp.
- Pull current Linear `security` label tickets to avoid duplicates.
- Enumerate all `apps/` and `packages/` directories.

## Parallel audit checks (run all simultaneously after Phase 0)

### A1 — Injection

1. **SQL injection** — find raw string interpolation in SQL (`db.execute(sql\`...${`)` with user input). Parameterized queries with drizzle ORM are safe; raw template literals with user-derived values → FAIL.
2. **Command injection** — `grep -rn "exec(\|spawn(\|execSync(\|spawnSync("` in `apps/`. Any call that interpolates user input into a shell command → FAIL.
3. **Prompt injection** — find LLM calls where user-supplied content is concatenated into the system prompt without a separator/sanitization pass. `grep -rn "systemPrompt.*\+.*user\|system:.*\`.*\${"` → WARN (requires manual review).

### A2 — Broken Authentication

4. **Session fixation** — confirm Better Auth rotates the session token on login and privilege elevation. `grep -rn "session.id\s*=" ` in auth routes → check it's not reused.
5. **Token leakage in logs** — `grep -rn "console.log.*token\|logger.*accessToken\|log.*refresh_token"` → FAIL per hit.
6. **JWT algorithm confusion** — if JWTs are used, confirm `algorithm` is hardcoded to `HS256`/`RS256`, not `none` or algorithm-agnostic → FAIL if `alg: "none"` or no algorithm pinned.
7. **CSRF protection** — confirm mutating API routes check `Origin` / `Referer` header or use SameSite=Strict cookies + CSRF token. API routes that accept cross-origin POST without a CSRF check → WARN (depends on whether the API is cookie-authenticated).

### A3 — Sensitive Data Exposure

8. **PII in logs** — `grep -rn "console.log.*email\|logger.*password\|log.*ssn\|log.*credit_card"` → FAIL per hit.
9. **Secrets in responses** — grep route handlers for responses that include `password`, `hash`, `secret`, or `token` fields in the JSON output. Any leak → FAIL.
10. **Encryption at rest** — confirm Vercel Blob and Postgres are configured with encryption-at-rest (check Vercel dashboard notes or migration comments). If not confirmed → WARN (flag for manual verification).

### A4 — XML External Entities (XXE) / Deserialization

11. **Unsafe deserialization** — `grep -rn "JSON.parse.*req.body\|eval(\|new Function("` in route handlers. `JSON.parse` on trusted server data is fine; `eval` on any external input → FAIL.
12. **File upload type validation** — for any file upload route, confirm MIME type and file extension are validated server-side (not just client-side). Missing → FAIL.

### A5 — Broken Access Control

13. **IDOR** — for any route that fetches a resource by ID from the path (e.g. `/assets/:id`, `/files/:id`), confirm the handler verifies the requesting user owns or has access to that ID. Missing ownership check → FAIL.
14. **Privilege escalation** — confirm users cannot assign themselves a higher role than their current one. `org.member.role.change` and similar — confirm the caller's role is checked, not just the target's.

### A6 — Security Misconfiguration

15. **CORS wildcards** — `grep -rn "Access-Control-Allow-Origin.*\*\|cors.*origin.*true"` in API route setup. Wildcard CORS on authenticated endpoints → FAIL.
16. **Debug endpoints in production** — `grep -rn "/__debug\|/debug\|/admin/raw\|/introspect"` in route files. Any debug route without auth gating → FAIL.
17. **Stack traces in responses** — confirm error handlers return a generic message, not `error.stack` or internal file paths in production. `grep -rn "error.stack\|err.stack"` in response serialization → WARN if not behind a `NODE_ENV === 'development'` guard.

### A7 — Cross-Site Scripting (XSS)

18. **`dangerouslySetInnerHTML`** — `grep -rn "dangerouslySetInnerHTML"` in `apps/app/src`. Any usage → review: if the content is user-supplied and not sanitized → FAIL.
19. **SVG sanitization** — confirm `svg.generate` capability strips `<script>` tags and event handlers from generated SVG before returning. Already identified in the contract — verify the implementation matches.
20. **Content-Security-Policy header** — check Vercel config / `next.config.*` for `Content-Security-Policy` header. Missing → WARN; overly broad `unsafe-inline` script → WARN.

### A8 — Insecure Deserialization / Supply Chain

21. **Dependency audit** — `pnpm audit --prod 2>&1 | tail -80`. Any critical CVE → FAIL. High CVE → WARN with package + CVE ID + recommended version.
22. **Pinned versions** — `grep -rn '"\^[0-9]\|"~[0-9]\|"\*"'` in `package.json` files. Floating ranges → FAIL (reproducibility + supply chain risk).

### A9 — Using Components with Known Vulnerabilities

23. **Outdated auth library** — check Better Auth version against latest stable. If > 2 minor versions behind → WARN.
24. **Node.js version** — confirm `engines.node` in root `package.json` matches the runtime version in Vercel config. Mismatch → WARN.

### A10 — Insufficient Logging and Monitoring

25. **Audit trail completeness** — confirm these events are logged to ClickHouse with `org_id`, `actor_id`, and timestamp: login, logout, password change, role change, API key create/revoke, billing event, member add/remove. Missing event type → WARN.
26. **Error monitoring** — confirm an error monitoring integration (Sentry or equivalent) is wired in all deployed apps. Missing → WARN.

## Fix protocol

- Safe in-code fixes (sanitization missing, CORS config, `dangerouslySetInnerHTML` with a safe alternative): **fix immediately**.
- Infra or secret-rotation issues: **file Linear ticket** with label `security`, priority P1, assignee Mac Anderson, clear acceptance checklist.
- Do not duplicate existing open tickets.

## Output

`mkdir -p docs/audits/security-audits`

Write: `docs/audits/security-audits/<SHORT_SHA>_<TIMESTAMP>_security-audit.html`

Self-contained HTML (all CSS + JS inline). Must contain:

1. Header: repo, branch, SHA, timestamp.
2. **Security hero** — overall security score (0–100), color-coded, SECURE / VULNERABLE verdict.
3. **OWASP category score grid** — one stat box per OWASP Top 10 category (A1–A10), each 0–100.
4. **Findings table** — check # · OWASP category · PASS/WARN/FAIL · finding · action.
5. **Critical blockers** (all FAILs, ranked by blast radius).
6. **Dependency CVEs** table — package · CVE · severity · fixed version.
7. **Linear tickets filed** list.
8. **Fixes applied** list.
9. **Scoring methodology** footnote.

Same dark-panel visual style as `/release-audit`. Data-driven renderer — `DATA` object, JS renders tables.
