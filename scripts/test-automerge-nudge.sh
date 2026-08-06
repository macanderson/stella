#!/usr/bin/env bash
#
# Tests for automerge-nudge.sh's selection rule (#1527).
#
#   ./scripts/test-automerge-nudge.sh
#
# Run it after touching that script. Not part of `make gate`: it is a dozen jq
# invocations over fixtures, the same posture as `make impacted-test`.
#
# ── What is under test ───────────────────────────────────────────────────────
#
# The script's whole job is choosing WHICH pull request to update, and the two
# ways to get that wrong are opposites — so a suite that only checks one is
# blind to the other:
#
#   too eager   nudging a PR nobody armed, a draft, one whose checks are
#               actually failing, or one targeting a different base. Each burns
#               a ~12-minute CI run on a branch that was not deadlocked.
#   too many    nudging every behind PR in one pass. Each update moves main,
#               which puts the rest behind again — O(N²) CI runs for N stacked
#               PRs, which is the cost the hand-run loop in #1527 paid.
#
# Every case below straddles one of those. The `--select` mode exists precisely
# so they can: it is the decision with the I/O taken out, so a fixture is a
# complete input and no live queue is involved.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/automerge-nudge.sh"

pass=0
fail=0

# want <name> <expected-number-or-empty> <json>
want() {
  local name="$1" expect="$2" json="$3" got
  got="$(printf '%s' "$json" | "$SCRIPT" --select 2>&1)"
  if [ "$got" = "$expect" ]; then
    pass=$((pass + 1)); echo "ok   $name"
  else
    fail=$((fail + 1)); echo "FAIL $name — wanted '${expect}', got '${got}'"
  fi
}

pr() { # pr <number> <state> <armed:yes|no> <draft:yes|no> <created> [base]
  local armed='null'
  [ "$3" = "yes" ] && armed='{"enabledAt":"2026-08-05T00:00:00Z"}'
  local draft='false'
  [ "$4" = "yes" ] && draft='true'
  printf '{"number":%s,"title":"pr %s","mergeStateStatus":"%s","autoMergeRequest":%s,"isDraft":%s,"createdAt":"%s","baseRefName":"%s"}' \
    "$1" "$1" "$2" "$armed" "$draft" "$5" "${6:-main}"
}

# ── The deadlock this exists for ─────────────────────────────────────────────
want "B1 an armed PR behind main is selected" \
  "101" "[$(pr 101 BEHIND yes no 2026-08-05T10:00:00Z)]"

want "B2 an empty queue selects nothing" "" "[]"

# ── Too many: one per invocation, oldest first ───────────────────────────────
# Six behind PRs is the #1527 pile-up exactly. Selecting more than one is the
# O(N²) failure; selecting the wrong one starves the longest waiter.
six="[$(pr 106 BEHIND yes no 2026-08-05T15:00:00Z),\
$(pr 102 BEHIND yes no 2026-08-05T11:00:00Z),\
$(pr 104 BEHIND yes no 2026-08-05T13:00:00Z),\
$(pr 101 BEHIND yes no 2026-08-05T10:00:00Z),\
$(pr 105 BEHIND yes no 2026-08-05T14:00:00Z),\
$(pr 103 BEHIND yes no 2026-08-05T12:00:00Z)]"
want "N1 six behind PRs yield exactly one, the oldest" "101" "$six"

# Same instant: the lower number wins, so two concurrent runs cannot disagree
# about who is next and double-nudge.
tie="[$(pr 205 BEHIND yes no 2026-08-05T10:00:00Z),$(pr 203 BEHIND yes no 2026-08-05T10:00:00Z)]"
want "N2 a createdAt tie is broken deterministically by number" "203" "$tie"

# ── Too eager: what must NOT be nudged ───────────────────────────────────────
want "E1 an unarmed PR behind main is left alone" \
  "" "[$(pr 301 BEHIND no no 2026-08-05T10:00:00Z)]"

want "E2 an armed draft is left alone" \
  "" "[$(pr 302 BEHIND yes yes 2026-08-05T10:00:00Z)]"

# BLOCKED is a failing check or a missing review — a human problem. Re-running
# CI does not fix it and the run is wasted.
want "E3 a BLOCKED PR is left alone" \
  "" "[$(pr 303 BLOCKED yes no 2026-08-05T10:00:00Z)]"

want "E4 a CLEAN PR is left alone" \
  "" "[$(pr 304 CLEAN yes no 2026-08-05T10:00:00Z)]"

want "E5 a DIRTY (conflicted) PR is left alone" \
  "" "[$(pr 305 DIRTY yes no 2026-08-05T10:00:00Z)]"

# A stacked PR targeting another branch is not subject to main's strict rule.
want "E6 a PR based on another branch is left alone" \
  "" "[$(pr 306 BEHIND yes no 2026-08-05T10:00:00Z release/0.6)]"

# ── Mixed queue: the realistic case ──────────────────────────────────────────
# The oldest PR overall is unarmed, the next is a draft, the next is BLOCKED —
# a rule applied in the wrong order picks one of them.
mixed="[$(pr 401 BEHIND no no 2026-08-05T09:00:00Z),\
$(pr 402 BEHIND yes yes 2026-08-05T09:30:00Z),\
$(pr 403 BLOCKED yes no 2026-08-05T09:45:00Z),\
$(pr 404 BEHIND yes no 2026-08-05T12:00:00Z),\
$(pr 405 BEHIND yes no 2026-08-05T11:00:00Z)]"
want "M1 a mixed queue skips ineligible PRs and still picks the oldest eligible" \
  "405" "$mixed"

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
