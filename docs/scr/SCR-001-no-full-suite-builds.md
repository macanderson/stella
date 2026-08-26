---
id: scr/001-no-full-suite-builds
title: Never compile the full test suite in the inner loop
status: living
origin: repeated manual prompt, ~daily, 2025–2026
trigger: any build/test invocation during development
autonomy: L2
enforcement: AGENTS.md standing-decisions block (imported by CLAUDE.md); .claude/hooks/no-full-test-builds.sh PreToolUse guard for Claude Code
---

## Directive

Compile and run only the tests relevant to the change — the touched
crate/package/module, plus direct dependents when an interface changed. The
full suite is CI's job.

## Rationale

Full-suite compiles dominate inner-loop latency and add no signal the scoped
run doesn't provide; CI already runs everything on push. On this org's
hardware, concurrent full-suite runs have saturated RAM and ballooned
swap into the ~100 GB range.

## How an agent complies

Scope every test command to the touched unit, using the repo's stack:

| Stack | Scoped (do this) | Full-suite (CI's job) |
|-------|------------------|------------------------|
| Rust workspace (stella, context-graph-protocol) | `cargo test -p <crate> [filter]` · `cargo nextest run -p <crate>` | bare `cargo test`, `--workspace`, `--all` |
| pnpm/turbo monorepo (oxagen) | `pnpm --filter <package> test` · `turbo run test:unit --filter <package>` | bare `pnpm test`, `turbo run test:unit` |
| Python / uv (arenabench) | `uv run pytest tests/test_x.py -q` | bare `pytest`, `make test` |
| Next.js (cgp-website) | scope any future suite to the touched module | any bare full-suite `test` script |

Reproduce full CI locally only when debugging a CI-only failure, and say so
out loud in the session.

## Exceptions

Release verification; explicit maintainer request; stated reproduction of a
CI-only failure.
