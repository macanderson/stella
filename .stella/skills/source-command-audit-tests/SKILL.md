---
name: "source-command-audit-tests"
description: "Audit unit and integration test coverage, quality, and CI performance across all packages and apps. Outputs a standalone HTML report with per-package scores and a fix plan."
---

# source-command-audit-tests

Use this skill when the user asks to run the migrated source command `audit-tests`.

## Command Template

# /audit-tests

Audit the unit and integration test suite across the full monorepo. Work autonomously top to bottom.

## Phase 0 — Load context

- Read `AGENTS.md`, `AGENTS.md`, `policies/6-testing-policy.md` (if it exists).
- Capture `SHORT_SHA` (`git rev-parse --short HEAD`) and UTC timestamp for the report filename.
- Enumerate all `packages/*` and `apps/*` directories.
- Locate all vitest configs (`vitest.config.*`), jest configs (`jest.config.*`), and pytest configs (`pytest.ini`, `pyproject.toml` with `[tool.pytest]`).

## Phase 1 — Coverage measurement

Run coverage for all packages that have a test script (use `--continue` so a failing package doesn't halt others):

```bash
pnpm turbo run test:coverage --continue 2>&1 | tail -200
```

For each package, extract:
- Line coverage %
- Branch coverage %
- Whether a coverage threshold is configured in the test config
- Whether the threshold was met

Classify per package:
- **PASS** — coverage ≥85% line / ≥80% branch AND thresholds are enforced in config
- **WARN** — coverage in range but no enforced threshold (relies on CI discretion)
- **FAIL** — coverage <85% line or <80% branch, OR entire module has zero tests

## Phase 2 — Test quality scan

For each test file (`*.test.ts`, `*.spec.ts`, `*.test.py`):

- **Stub/skip detector** — `grep -rn "it\.skip\|test\.skip\|xit\|xdescribe\|\.todo\|pytest.mark.skip\|NotImplemented"` → FAIL per hit on a non-draft test.
- **Assertion density** — tests with zero `expect(` / `assert` calls → FAIL (test runs code but proves nothing).
- **Mock leakage** — `vi.mock(.*database\|vi.mock(.*db\|jest.mock(.*pg` → WARN (mocking the DB layer loses integration signal; per engineering policy, real DB preferred).
- **Broad `any` in test types** — `as any` in test assertions → WARN (defeats type-system value of the test).
- **Copy-paste tests** — exact-duplicate test body strings across files → WARN.

## Phase 3 — Turborepo / CI correctness

- Check `turbo.json` `test` task:
  - `inputs` declared? Missing → WARN (over-invalidates cache).
  - `outputs` declared? Missing → WARN.
  - `env` / `passThroughEnv` lists all vars the test suite reads? Undeclared env var → FAIL (cache poisoning risk).
- Check CI YAML for test job:
  - Runs the affected graph on PRs (`--filter='...[origin/main]'`)? Full suite on every PR → WARN.
  - Coverage artifact uploaded? Missing → WARN (no history, no trend).
  - Coverage check separate from test run? Merged into same step is fine; just confirm it fails the job.

## Phase 4 — Missing module scan

For each source file under `packages/*/src` and `apps/*/src`:
- Find files with **zero** corresponding test file (same name with `.test.` or `.spec.` suffix, or in a `__tests__/` sibling).
- Classify by risk: files that export public API surface (imported by ≥3 other files) → FAIL if untested. Internal helpers → WARN.

## Phase 5 — Fix plan

For each FAIL:
- State the exact threshold to add to vitest config.
- List the first 3 functions in the file most worth testing (highest cyclomatic complexity heuristic: most `if`/`for`/`switch` branches).
- Estimate effort: XS (<1h) / S (half-day) / M (1 day).

Do **not** write test files unless the user confirms. Output specs in the report only.

## Output

`mkdir -p docs/audits/test-audits`

Write: `docs/audits/test-audits/<SHORT_SHA>_<TIMESTAMP>_test-audit.html`

Self-contained HTML (all CSS + JS inline). Must contain:

1. Header: repo, branch, SHA, timestamp.
2. **Coverage hero** — overall weighted average line coverage across all packages, color-coded.
3. **Per-package coverage table** — package · line % · branch % · threshold enforced · status badge.
4. **Quality findings table** — file · issue type · severity badge · one-line fix.
5. **Missing module table** — file · import count · risk · effort.
6. **CI correctness table** — check · PASS/WARN/FAIL · finding.
7. **Fix plan** — grouped by package, collapsible `<details>`.

Same dark-panel visual style as `/release-audit`. Data-driven renderer — fill a `DATA` object, let JS render tables.
