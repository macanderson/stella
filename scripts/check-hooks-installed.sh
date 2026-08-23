#!/usr/bin/env bash
#
# Advisory: say so when the pre-push hook is not installed in this clone.
#
# `make hooks` is once per clone and easy to miss in a fresh one. When it has
# not been run, `git push` runs no gate at all — silently, because an absent
# hook has nothing to print.
#
# That silence is the gap #3887 recorded. With `enforce_admins: false` on
# `main`, branch protection declines to enforce against an admin, and
# `main-canary.yml` reports only after the merge. The pre-push hook is the
# layer AGENTS.md names as the one that catches a gate-failing push on the
# author's machine — and on 2026-08-19 it did not, because it was bypassed or
# was never installed, and nothing said which.
#
# **This never fails.** It is a notice, not a gate step: it is not in
# GATE_STEPS, it makes no claim about the tree, and a clone that deliberately
# manages its hooks elsewhere is not doing anything wrong. A guard that failed
# here would be asserting a fact about the developer's machine rather than
# about the change under test.
#
# Silent when the hook is installed: a green run should say one line, and this
# is not that line.
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root" || exit 0

git rev-parse --git-dir >/dev/null 2>&1 || exit 0

current="$(git config --get core.hooksPath 2>/dev/null || true)"

[ "$current" = ".githooks" ] && exit 0

# The verdict is advisory and already decided; every write below is
# best-effort, so a reader that closed the pipe cannot turn this into a
# failure (#1815).
trap '' PIPE
{
  echo ""
  echo "note: the pre-push hook is not installed in this clone."
  if [ -n "$current" ]; then
    echo "      core.hooksPath = $current (expected .githooks)"
  else
    echo "      core.hooksPath is unset"
  fi
  echo "      \`git push\` will run no gate here, and will say nothing about it."
  echo "      Install it once per clone:  make hooks"
  echo ""
} || true

exit 0
