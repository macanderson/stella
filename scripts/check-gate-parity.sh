#!/usr/bin/env bash
#
# Guard: AGENTS.md and CONTRIBUTING.md describe the gate that actually exists.
# See #1437.
#
# The gate's real composition lives in one place — `GATE_STEPS` in the Makefile.
# Two documents restate it in prose, because a contributor reading either one
# should learn what `make gate` will do without reading a Makefile. Prose that
# restates a list drifts; this one drifted twice, in the same direction both
# times, and both times it under-reported:
#
#   * AGENTS.md claimed THIRTEEN steps and omitted repro-wiring and
#     command-docs. Repaired 2026-08-04.
#   * By the time this guard was written it claimed FIFTEEN and omitted
#     no-secrets, design-refs and brand-case. CONTRIBUTING.md's command list
#     was missing the same three.
#
# The failure mode is quiet and it is the bad direction: a reader trusts the
# short list, runs those commands, and believes a green result means the gate is
# green. The next guard added is the next omission — there is nothing structural
# stopping it, which is the entire argument for this script.
#
# What it checks:
#
#   1. Every step in `GATE_STEPS` is named in AGENTS.md's gate block.
#   2. Every step is named in CONTRIBUTING.md's gate fence, via the alias table
#      below (that fence lists raw commands, on purpose — its reader wants to
#      run them without make).
#   3. Both documents' spelled-out count matches the real number of steps.
#   4. CONTRIBUTING.md's fence does not run a `check-*.sh` that is no longer a
#      gate step — which is how a removed guard leaves a ghost behind. Only
#      that fence is checked this way: it is a delimited list of commands,
#      whereas AGENTS.md's block is prose with parenthetical glosses.
#
# What it deliberately does NOT check: the prose *around* the lists. Whether
# "ci.yml's required job runs everything except invariants and doc-links" is
# still true is a claim about a workflow, not about this list, and pretending a
# grep could settle it would be worse than leaving it to review.
#
# Uses portable POSIX tools so it runs on a bare CI runner. `make` is required —
# it is how the step list is read, and a runner that cannot run make cannot run
# the gate this guard describes.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

agents="AGENTS.md"
contributing="CONTRIBUTING.md"

fail=0

# The verdict is decided before anything is written (#1815). Failure lines are
# buffered while the checks run and emitted in one final write: a guard that
# prints as it scans dies mid-report when its reader exits early, and under
# `set -euo pipefail` whatever partial state it had reached becomes the exit
# status. scripts/check-file-size.sh is the shape being copied.
report=""
note() { report="${report}check-gate-parity: $1"$'\n'; }

# Emission is best-effort: the verdict is already decided, so a reader that
# closed the pipe (`| head -1`, `| true`) must be able to change neither the
# report nor the exit code. SIGPIPE is ignored so a failed write surfaces as a
# discarded error instead of killing the script (#1815).
emit() {
  trap '' PIPE
  printf '%s' "$report" >&2 || true
}

# ── The truth ────────────────────────────────────────────────────────────────

if ! steps="$(make -s print-gate-steps 2>/dev/null)"; then
  note "FAIL — could not read GATE_STEPS (\`make -s print-gate-steps\`)."
  note "     That target is what makes this guard derived rather than a"
  note "     second copy of the list. Restore it in the Makefile."
  emit
  exit 1
fi

count="$(printf '%s\n' "$steps" | wc -w | tr -d ' ')"
if [ "$count" -eq 0 ]; then
  note "FAIL — GATE_STEPS is empty."
  emit
  exit 1
fi

# Spelled-out numbers, because both documents spell the count in prose ("the
# fifteen of them in order") and a digit would read wrong there. Covers a range
# no plausible gate will leave.
number_word() {
  case "$1" in
  10) echo ten ;; 11) echo eleven ;; 12) echo twelve ;; 13) echo thirteen ;;
  14) echo fourteen ;; 15) echo fifteen ;; 16) echo sixteen ;;
  17) echo seventeen ;; 18) echo eighteen ;; 19) echo nineteen ;;
  20) echo twenty ;; 21) echo twenty-one ;; 22) echo twenty-two ;;
  23) echo twenty-three ;; 24) echo twenty-four ;; 25) echo twenty-five ;;
  *) echo "" ;;
  esac
}

word="$(number_word "$count")"
if [ -z "$word" ]; then
  note "FAIL — $count gate steps is outside the range number_word() spells."
  note "     Extend the table in this script; the documents spell the count."
  emit
  exit 1
fi

# CONTRIBUTING.md lists raw commands rather than make targets. Every guard is
# `scripts/check-<target>.sh` except the ones named here, and the four compile
# steps are cargo invocations. An alias is a deliberate, reviewable statement
# that two spellings mean the same step.
contributing_alias() {
  case "$1" in
  shellcheck) echo 'shellcheck ' ;;
  doc-links) echo 'check-doc-links' ;;
  doc-warnings) echo 'cargo doc' ;;
  format-check) echo 'cargo fmt' ;;
  lint) echo 'cargo clippy' ;;
  test) echo 'cargo test' ;;
  *) echo "check-$1.sh" ;;
  esac
}

# ── The two documents ────────────────────────────────────────────────────────

for doc in "$agents" "$contributing"; do
  if [ ! -f "$doc" ]; then
    note "FAIL — $doc does not exist."
    fail=1
    continue
  fi

  for step in $steps; do
    if [ "$doc" = "$contributing" ]; then
      needle="$(contributing_alias "$step")"
    else
      needle="$step"
    fi
    if ! grep -qF -- "$needle" "$doc"; then
      note "FAIL — $doc never mentions the gate step '$step'"
      note "     (looked for '$needle')."
      fail=1
    fi
  done

  if ! grep -qF -- "$word" "$doc"; then
    note "FAIL — $doc does not spell the gate's step count as '$word'."
    note "     The gate runs $count steps. Find the stale count and fix it."
    fail=1
  fi
done

# ── Ghosts ───────────────────────────────────────────────────────────────────
#
# The other direction: a guard *removed* from GATE_STEPS leaves its command
# behind in CONTRIBUTING.md's fence, where it reads as a step that still runs.
# That fence is a delimited list of commands, so every `check-*.sh` inside it
# can be held to the step list exactly. (AGENTS.md's block is prose with
# parenthetical glosses and gets no equivalent check — see the header.)

fence="$(awk '
  /^### The gate/        { seen = 1 }
  seen && /^```/         { fences++; next }
  seen && fences == 1    { print }
  seen && fences == 2    { exit }
' "$contributing")"

if [ -z "$fence" ]; then
  note "FAIL — could not find the command fence under '### The gate' in $contributing."
  fail=1
else
  for named in $(printf '%s\n' "$fence" | tr ' ' '\n' | sed -n 's|.*/\(check-[a-z0-9-]*\)\.\(sh\|py\).*|\1|p' | sort -u); do
    target="${named#check-}"
    case " $steps " in
    *" $target "*) continue ;;
    esac
    note "FAIL — $contributing's gate fence runs '$named', which is not a gate step."
    note "     Either it was removed from GATE_STEPS and the fence kept it, or"
    note "     the fence is running something the gate does not."
    fail=1
  done
fi

if [ "$fail" -ne 0 ]; then
  note ""
  note "The gate's composition lives in GATE_STEPS in the Makefile, and these"
  note "two documents restate it for readers. When you add or remove a guard,"
  note "update all three in the same PR — that is what this guard is for."
  emit
  exit 1
fi

emit
printf 'check-gate-parity: OK — %s (%s) gate steps, named in %s and %s.\n' \
  "$count" "$word" "$agents" "$contributing" || true
