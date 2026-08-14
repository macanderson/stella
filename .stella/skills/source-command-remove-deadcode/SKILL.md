---
name: "source-command-remove-deadcode"
description: "Find and remove dead code — unused exports, unreachable functions, orphaned files, and stale feature flags. Fixes in-place and commits."
---

# source-command-remove-deadcode

Use this skill when the user asks to run the migrated source command `remove-deadcode`.

## Command Template

# /remove-deadcode

Find and remove dead code across the monorepo. Work autonomously. Delete unused code; never stub or comment out — if it's dead, it's gone. Follow the prime directive: prefer deleting code to adding it.

## Phase 0 — Load context

- Read `AGENTS.md`, `AGENTS.md`, `policies/3-code-bloat-avoidance.md` (if it exists).
- Capture `SHORT_SHA`. Confirm clean working tree (`git status --porcelain`); if dirty, abort and tell the user.
- Run `pnpm typecheck 2>&1 | tail -20` to establish a baseline. Record error count.

## Phase 1 — TypeScript unused exports (static analysis)

Run TypeScript-powered dead export detection:

```bash
cd /Users/macanderson/oxagen-monorepo && npx ts-prune --error 2>&1 | head -200
```

If `ts-prune` is not installed, use `knip` (`npx knip 2>&1 | head -200`) — whichever is available. If neither, use LSP diagnostics or grep-based heuristics (see Phase 2).

Classify findings:

- **Public package API** (`packages/*/src/index.ts` re-exports) — keep; these are intentional.
- **Internal unused export** (exported from a non-index file, imported nowhere in the repo) → DELETE the `export` keyword (make it unexported) or delete the function if it has no callers.
- **Entire file with no imports** — candidate for deletion (verify in Phase 2).

## Phase 2 — Grep-based dead code scan

1. **Unused `async` functions** — `grep -rn "export async function\|export function" apps/ packages/` → for each, `grep -rn "<function_name>" --include="*.ts" --include="*.tsx"`. If the only hit is the declaration → dead.
2. **Unused React components** — `grep -rn "export default function\|export const.*= ()" apps/app/src/components/ | grep -v index` → for each component name, `grep -rn "<ComponentName\|ComponentName}" --include="*.tsx"`. Zero callers → dead.
3. **Orphaned files** — files that are never imported and not entry points (`page.tsx`, `route.ts`, `layout.tsx`, `index.ts`):
   ```bash
   find apps/app/src -name "*.ts" -o -name "*.tsx" | grep -v "page.tsx\|route.ts\|layout.tsx\|index.ts\|proxy.ts\|loading.tsx\|error.tsx\|not-found.tsx" | xargs -I{} sh -c 'base=$(basename {} .tsx); base=${base%.ts}; grep -rl "$base" apps/ packages/ --include="*.ts" --include="*.tsx" | grep -v "^{}$" | head -1 || echo "ORPHAN: {}"' 2>/dev/null | grep ORPHAN
   ```
4. **`console.log` debug statements** — `grep -rn "console\.log" apps/ packages/ --include="*.ts" --include="*.tsx"` (not in test files). Remove non-intentional debug logs; keep structured logger calls.
5. **Commented-out code blocks** — `grep -rn "//.*const \|//.*function \|//.*return \|/\*.*TODO\|/\*.*FIXME" apps/ packages/ --include="*.ts" --include="*.tsx"`. Multi-line commented code blocks → remove. Single-line non-obvious WHY comments → keep.
6. **Empty catch blocks** — `grep -rn "catch.*{[[:space:]]*}" apps/ packages/ --include="*.ts"`. Empty catch that swallows errors → replace with `catch (err) { logger.error(err) }` or remove the try/catch.
7. **Feature flags that are always-true or always-false** — `grep -rn "process.env.FEATURE_\|process.env.FF_\|featureFlag" apps/ packages/ --include="*.ts" --include="*.tsx"`. Flags hardcoded to `true`/`false` → inline the branch and remove the flag.
8. **Stub implementations** — `grep -rn "console.log.*stub\|// stub\|NOT_IMPLEMENTED\|TODO: implement"` — unless the stub is explicitly tracked in Linear (check the memory: `video.generate`, Google/MS docs stubs are intentional stubs). Remove untracked stubs; leave intentional ones untouched.

## Phase 3 — Schema dead code

9. **Orphaned migrations** — `grep -rn "CREATE TABLE\|ALTER TABLE" packages/database/src/migrations/ | cut -d: -f3 | grep "CREATE TABLE" | sed 's/.*CREATE TABLE //'` → for each table, check it exists in the Drizzle schema files. A migration that creates a table with no schema definition → WARN (may be intentional historical artifact; do not auto-delete migrations).
10. **Unused schema columns** — for non-trivial tables, grep for column names in application code. Columns never read or written in `apps/` or `packages/` (excluding `packages/database` itself) → list as candidates. Do not auto-remove schema columns — file them as suggestions in the report.

## Phase 4 — Package-level dead dependencies

```bash
npx depcheck --json 2>&1 | head -100
```

For each unused dependency in `package.json`:

- Confirm it's not used via dynamic `require()` or as a peer/type dependency.
- If truly unused: `pnpm remove <pkg> --filter <workspace>`.

## Execution rules

- **Fix in-place** for: unused exports (remove `export`), dead functions (delete), debug `console.log` (delete), empty catch blocks (fix), always-true/false feature flags (inline + remove).
- **Do not auto-delete**: whole files (confirm in output first), schema columns, migrations, intentional stubs.
- After all fixes, run `pnpm typecheck 2>&1 | tail -20` and confirm error count has not increased.
- Run `pnpm test --passWithNoTests 2>&1 | tail -20` to confirm tests still pass.
- Commit: `git add -A && git commit -m "chore: remove dead code — unused exports, debug logs, empty catches"`.
- Rebase and push: `git fetch origin && git rebase origin/main && git push`.

## Output

Print a summary table to the terminal (no HTML report needed):

| Category            | Found | Removed | Skipped (manual) |
| ------------------- | ----- | ------- | ---------------- |
| Unused exports      | N     | N       | N                |
| Dead functions      | N     | N       | N                |
| Orphaned components | N     | N       | N                |
| Debug console.log   | N     | N       | N                |
| Empty catch blocks  | N     | N       | N                |
| Always-on/off flags | N     | N       | N                |
| Unused deps         | N     | N       | N                |

Then list any skipped items with the reason (e.g. "whole-file deletion needs confirmation", "schema column — file Linear ticket instead").
