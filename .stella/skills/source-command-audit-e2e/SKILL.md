---
name: "source-command-audit-e2e"
description: "Audit end-to-end test coverage of critical Playwright paths. Identifies missing flows, flaky tests, slow suites, and critical-path gaps. Outputs a standalone HTML report."
---

# source-command-audit-e2e

Use this skill when the user asks to run the migrated source command `audit-e2e`.

## Command Template

# /audit-e2e

Audit end-to-end (Playwright) test coverage across the monorepo. Work top to bottom without stopping unless blocked.

## Phase 0 — Load context

- Read `AGENTS.md`, `AGENTS.md`. Note the critical user journeys the product promises.
- Run `git rev-parse --short HEAD` and capture `SHORT_SHA`.
- Capture current UTC timestamp as `YYYYMMDDTHHMMSSZ` for the report filename.
- Discover all Playwright config files: `find . -name "playwright.config.*" -not -path "*/node_modules/*"`.
- Enumerate all `*.spec.ts` and `*.test.ts` files under `e2e/`, `tests/`, and any `**/e2e/**` paths.

## Phase 1 — Critical path inventory

Identify the flows that MUST have e2e coverage. For each, classify as covered, partial, or missing:

1. **Auth — sign up** (`/signup` → org creation → workspace landing)
2. **Auth — log in / log out** (email+password, session persistence across refresh)
3. **Auth — social OAuth** (Google, GitHub) if wired in this env
4. **Tenant isolation** — one user cannot see another org's data (spot-check at least one resource type)
5. **Workspace isolation** — members see only their own workspace resources
6. **Chat / agent send** — send a message, receive a streamed reply, tool call appears
7. **MCP parity surface** — at least one capability exercised via the MCP endpoint (not just UI)
8. **Billing** — upgrade flow starts a Stripe Checkout session (mock Stripe in test)
9. **API key create + revoke** — full lifecycle via the settings UI
10. **Plugin install** — browse catalog, install a plugin, verify it appears in workspace tools
11. **File / asset upload** — upload a file, verify it is accessible via the access-controlled route
12. **Conversation history** — archive, delete, rename, verify state persists on refresh

## Phase 2 — Test quality scan

For each discovered `.spec.ts` file:

- Count assertions (`expect(` calls). Files with < 3 assertions per test block → WARN (assertion-light).
- Detect `page.waitForTimeout` / `sleep` calls → WARN (brittle timing).
- Detect `.only` / `.skip` / `test.fixme` → FAIL if on a critical-path test.
- Detect hard-coded `localhost` URLs without env-var indirection → WARN.
- Detect tests that never assert a network response or DOM state — only click+navigate → WARN.

## Phase 3 — Performance + CI lane check

- Grep `turbo.json` and CI YAML (`workflows/*.yml`) for how/when Playwright runs:
  - Runs unconditionally on every PR push → WARN (should be affected-only or pre-merge lane).
  - No sharding configured for a suite with > 20 specs → WARN.
  - Not using Playwright's HTML reporter → WARN (loses artifact on CI failure).
- Report total spec count, estimated wall-clock (from last CI run if available via `gh run list`).

## Phase 4 — Gap remediation plan

For each missing critical-path flow, produce a one-paragraph spec:
- What the test sets up (fixtures, seed data)
- What it exercises (navigation, interactions)
- What it asserts (network, DOM, DB state via API)

Do **not** write the test files unless the user confirms. Output the specs in the report only.

## Output

`mkdir -p docs/audits/e2e-audits`

Write: `docs/audits/e2e-audits/<SHORT_SHA>_<TIMESTAMP>_e2e-audit.html`

The report must be a self-contained standalone HTML file (all CSS + JS inline, no external assets) with:

1. Header: repo, branch, SHA, timestamp.
2. **Coverage summary band** — % of critical paths covered (0–12 flows), color-coded (≥10 green, 7–9 yellow, <7 red).
3. **Critical path table** — one row per flow: name · status (Covered / Partial / Missing) · test file(s) or "—" · notes.
4. **Quality findings table** — one row per finding: file · issue type · line · severity badge.
5. **CI lane assessment** — PASS/WARN/FAIL with one-line finding per check.
6. **Gap specs** — collapsible `<details>` per missing flow with the remediation spec.
7. **Blockers** (FAIL items) and **Warnings** lists.

Use the same dark-panel visual style as `/release-audit` (CSS variables: `--bg:#0B1020`, `--pass:#3CFF52`, `--warn:#FFC53C`, `--fail:#FF5C7A`). Data-driven renderer — fill a `DATA` object, let JS render tables — never hand-write rows.
