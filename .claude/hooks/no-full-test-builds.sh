#!/usr/bin/env bash
# Reject full-suite test compiles in the inner loop (SCR-001).
#
# PreToolUse hook for Claude Code's Bash tool: receives the tool input as
# JSON on stdin; exit 2 blocks the call and shows stderr to the agent.
# This is a teaching guard, not a security boundary — it catches the common
# full-suite invocations across the org's stacks (the same script ships in
# every macanderson repo); a deliberate CI reproduction can be phrased
# around it, and SCR-001 says to do exactly that out loud.
set -euo pipefail

cmd=$(jq -r '.tool_input.command // empty' 2>/dev/null || true)
[ -z "$cmd" ] && exit 0

block() {
  echo "Blocked (SCR-001): full test-suite builds are CI's job. Scope it: $1 — see docs/scr/SCR-001-no-full-suite-builds.md" >&2
  exit 2
}

# Rust: cargo test / nextest, workspace-wide or without a package scope.
if [[ "$cmd" =~ cargo[[:space:]]+(test|nextest[[:space:]]+run) ]]; then
  if [[ "$cmd" =~ --workspace ]] || [[ "$cmd" =~ --all([[:space:]]|$) ]]; then
    block "cargo test -p <crate> [filter]"
  fi
  if ! [[ "$cmd" =~ [[:space:]](-p|--package)[[:space:]] ]]; then
    block "cargo test -p <crate> [filter]"
  fi
fi

# JS package managers: bare test scripts with no --filter scope.
if [[ "$cmd" =~ (^|[[:space:]])(pnpm|npm|yarn|bun)([[:space:]]+run)?[[:space:]]+(test|test:unit|test:coverage)([[:space:]]|$) ]] \
   && ! [[ "$cmd" =~ --filter ]]; then
  block "pnpm --filter <package> test"
fi

# Turborepo: any test task fanned out across the whole graph.
if [[ "$cmd" =~ (^|[[:space:]])turbo[[:space:]]+(run[[:space:]]+)?test ]] \
   && ! [[ "$cmd" =~ --filter ]]; then
  block "turbo run <task> --filter <package>"
fi

# Python: pytest with flags only — no path or node selector.
if [[ "$cmd" =~ (^|[[:space:]])pytest([[:space:]]+-[a-zA-Z-]+)*[[:space:]]*$ ]]; then
  block "pytest path/to/test_x.py"
fi

# Full-suite make targets.
if [[ "$cmd" =~ (^|[[:space:]])make[[:space:]]+test([[:space:]]|$) ]]; then
  block "the scoped runner for the touched unit (see AGENTS.md)"
fi

# Go: whole-tree test sweeps.
if [[ "$cmd" =~ go[[:space:]]+test[[:space:]]+\./\.\.\. ]]; then
  block "go test ./pkg/..."
fi

exit 0
