#!/usr/bin/env bash
#
# The live-tree tests for scripts/main-canary.sh (#3332, split out at #5356).
#
# Every case here expects the canary to report GREEN, and it can only do that
# when this repository is green: the `file-size` and `prose` rows take no
# `--manifest-dir` and read the live tree whatever the fixture says. So a
# failure in this suite means `main` is red, not that the canary lost a
# capability — which is main-canary.yml's job to report, after the merge, and
# is why the Makefile's `UNHOSTED_SELF_TESTS` keeps this file out of CI.
#
# It is still worth running by hand, and `make main-canary-test` runs it: the
# recovery branch is here, and a canary that only ever opens issues becomes a
# stale-issue generator, gets muted, and is then worth less than nothing.
#
# The rest — every case a fixture decides on its own — is
# scripts/test-main-canary.sh, which CI runs.
#
# Run: ./scripts/test-main-canary-live.sh   (or `make main-canary-test`, which
# runs both suites)
set -euo pipefail

# shellcheck source=scripts/lib/main-canary-harness.sh
. "$(dirname "$0")/lib/main-canary-harness.sh"

require_cargo test-main-canary-live
canary_scratch

# ── green ─────────────────────────────────────────────────────────────────
make_workspace "$tmp/clean"
expect "a composing tree passes" 0 "OK — main composes green" --manifest-dir "$tmp/clean"

# A green run must not file anything. This is the case that keeps the canary
# worth reading: a monitor that comments on healthy days gets muted.
refute "a green run announces nothing" "gh issue create" \
  --announce --dry-run --manifest-dir "$tmp/clean"

# The prose row RUNS, rather than merely being present in the checks array
# (#4828). It cannot be exercised the way `compile` and `lockfile-sync` are:
# those two take `--manifest-dir` and can be pointed at a broken fixture,
# while `check-prose` reads the live tree and has no such switch — the same
# reason `file-size` has no red fixture case here either. So the assertion
# available is that the row reports its verdict at all, which is exactly what
# was missing when the canary called a prose-red `main` green.
expect "the prose row runs and reports" 0 "ok   prose" --manifest-dir "$tmp/clean"

# ── one DoD box per FAILING check, and none for the rest (#5173) ──────────
# The other three boxes-in-the-issue cases are hermetic and live in the CI
# suite. This one is not: `prose` is named as the check that must NOT get a
# box, so a prose-red tree fails it for a reason that has nothing to do with
# the canary.
make_workspace "$tmp/nocompile"
echo 'pub fn demo() { let x: u32 = "not a u32"; }' >"$tmp/nocompile/crates/demo/src/lib.rs"
refute "and offers no box for a check that passed" \
  "- [ ] \`prose\` passes on a fresh" \
  --announce --dry-run --manifest-dir "$tmp/nocompile"

# ── recovery: the branch that keeps this worth reading ────────────────────
# A canary that only ever opens issues becomes a stale-issue generator and gets
# muted, at which point it is worse than nothing. Recovery only runs when an
# issue is already open, hence --fixture-open-issue.
expect "main going green closes the open issue" 0 "gh issue close 42" \
  --announce --dry-run --fixture-open-issue 42 --manifest-dir "$tmp/clean"
expect "and says so on the way out" 0 "main recovered — closed #42" \
  --announce --dry-run --fixture-open-issue 42 --manifest-dir "$tmp/clean"

canary_tally test-main-canary-live
