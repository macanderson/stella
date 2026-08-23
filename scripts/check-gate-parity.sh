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
#      run them without make; `shellcheck` is the one exception, and the alias
#      table says why).
#   3. CONTRIBUTING.md's fence does not run a `check-*.sh` that is no longer a
#      gate step — which is how a removed guard leaves a ghost behind. Only
#      that fence is checked this way: it is a delimited list of commands,
#      whereas AGENTS.md's block is prose with parenthetical glosses.
#   4. Every step actually RUNS in some workflow, and every hermetic guard
#      self-test (`scripts/test-*`) either runs in one or is named in the
#      Makefile's `UNHOSTED_SELF_TESTS` with the reason (#3820, #4427). The
#      three checks above are about prose; this one is about whether anything
#      server-side fires at all.
#
# What it deliberately does NOT check: the spelled-out step total — that check
# existed and was removed; see "The count is deliberately NOT checked any
# more" below (#1883) — and the prose *around* the lists. Whether
# "ci.yml's required job runs everything except invariants and doc-links" is
# still true is a claim about which workflow runs a step, not about whether one
# does, and pretending a grep could settle it would be worse than leaving it to
# review.
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
  26) echo twenty-six ;; 27) echo twenty-seven ;; 28) echo twenty-eight ;;
  29) echo twenty-nine ;; 30) echo thirty ;; 31) echo thirty-one ;;
  32) echo thirty-two ;; 33) echo thirty-three ;; 34) echo thirty-four ;;
  35) echo thirty-five ;; 36) echo thirty-six ;; 37) echo thirty-seven ;;
  38) echo thirty-eight ;; 39) echo thirty-nine ;; 40) echo forty ;;
  41) echo forty-one ;; 42) echo forty-two ;; 43) echo forty-three ;;
  44) echo forty-four ;; 45) echo forty-five ;;
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

# The command a step is spelled as outside the Makefile. Every guard is
# `scripts/check-<target>.sh` except the ones named here, and the four compile
# steps are cargo invocations. An alias is a deliberate, reviewable statement
# that two spellings mean the same step.
#
# One table serves both readers below — CONTRIBUTING.md's fence and the
# workflows — because both spell a step as the command that runs it. A second
# table would be a second thing to keep in sync, which is the defect this
# script exists to catch.
step_command() {
  case "$1" in
  # The one step the fence spells as a make target. Its argument list — which
  # files get linted — is the Makefile recipe's and nowhere else's, after a
  # hand-copied second list in ci.yml and a third here drifted from it (#3375).
  # A raw command in this fence would be a fourth.
  shellcheck) echo 'make shellcheck' ;;
  # The Python guards, whose scripts the `.sh` default below does not fit.
  doc-links) echo 'check-doc-links' ;;
  module-reachability) echo 'check-module-reachability' ;;
  typed-errors) echo 'check-typed-errors' ;;
  tool-error-class) echo 'check-tool-error-class' ;;
  dead-code-allows) echo 'check-dead-code-allows' ;;
  tokens) echo 'check-tokens' ;;
  hue-separation) echo 'check-hue-separation' ;;
  contrast) echo 'check-contrast' ;;
  transcript-surfaces) echo 'check-transcript-surfaces' ;;
  prose) echo 'check-prose' ;;
  deck-paths) echo 'check-deck-paths' ;;
  css-vars) echo 'check-css-vars' ;;
  doc-warnings) echo 'cargo doc' ;;
  format-check) echo 'cargo fmt' ;;
  lint) echo 'cargo clippy' ;;
  test) echo 'cargo test' ;;
  # The two guards whose script is not check-*.sh: they are test harnesses, not
  # tree guards, so each is named for what it tests.
  self-driving-test) echo 'test-self-driving.sh' ;;
  deck-fit-all-test) echo 'test-deck-fit-all.sh' ;;
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
      needle="$(step_command "$step")"
    else
      needle="$step"
    fi
    if ! grep -qF -- "$needle" "$doc"; then
      note "FAIL — $doc never mentions the gate step '$step'"
      note "     (looked for '$needle')."
      fail=1
    fi
  done

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

# ── The workflows ────────────────────────────────────────────────────────────
#
# The question neither document can answer: does anything server-side actually
# RUN this step? The two checks above are about prose — whether a reader is
# told the truth — and a step named correctly in both documents and run by no
# workflow is still a step that only fires on a contributor's machine. The
# pre-push hook is advisory, per-clone and bypassable, and on the maintainer's
# laptop the gate is not run at all, so for such a step there is no server-side
# check for the hook to complement.
#
# It has happened twice. `tokens` was red on `main` from #4066 until #4086 —
# seven retired brand hexes — with every PR otherwise green and nothing saying
# so; #4089 wired five steps that were in `GATE_GUARDS_FAST` and in no
# workflow; and the sweep that found `prose`, `hue-separation` and
# `transcript-surfaces` in the same hole (#4427) also found the PR that wired
# the previous batch had itself landed a gate step running nowhere.

workflows=".github/workflows"

# Every command the workflows run, one per line: the text after an inline
# `run:`, and each line of a `run: |` block. Nothing else — a `paths:` entry
# naming a guard script is a trigger, not a run, and counting it would report a
# guard as hosted because a workflow *watches its file*, which is exactly what
# wire-schema.yml does. Shell comments inside a block go too, for the reason a
# YAML comment does: prose about a command is not the command, and both
# release.yml and tool-docs.yml discuss commands they do not run.
workflow_commands() {
  awk '
    function indent_of(s,   i) { i = match(s, /[^ ]/); return i ? i - 1 : length(s) }
    { line = $0; sub(/\r$/, "", line) }
    block && line ~ /[^ ]/ && indent_of(line) > block_indent {
      s = line
      sub(/^[ \t]*/, "", s)
      if (s !~ /^#/) print s
      next
    }
    block && line ~ /[^ ]/ { block = 0 }
    /^[ \t]*(-[ \t]+)?run:[ \t]*[|>]/ { block = 1; block_indent = indent_of(line); next }
    /^[ \t]*(-[ \t]+)?run:[ \t]*[^|> \t]/ {
      s = line
      sub(/^[ \t]*(-[ \t]+)?run:[ \t]*/, "", s)
      print s
    }
  ' "$workflows"/*.yml
}

if [ ! -d "$workflows" ]; then
  note "FAIL — $workflows does not exist, so no gate step can be shown to run."
  fail=1
else
  # Written to a file rather than held in a variable and piped into each grep.
  # `grep -q` exits on its first match and closes the pipe under it, so the
  # writer takes a SIGPIPE — and `pipefail` then reports 141 for a pipeline
  # that MATCHED. It is a race, so it fails on some runs and not others: the
  # same shape #1815 is about, arriving here through the reader instead of the
  # reader's reader.
  commands="$(mktemp "${TMPDIR:-/tmp}/stella-gate-parity.XXXXXX")"
  trap 'rm -f "$commands"' EXIT INT TERM
  workflow_commands >"$commands"

  for step in $steps; do
    needle="$(step_command "$step")"
    if ! grep -qF -- "$needle" "$commands"; then
      note "FAIL — no workflow runs the gate step '$step'"
      note "     (looked for '$needle' in a run: step under $workflows/)."
      note "     A step the gate names and CI never runs fires only on a"
      note "     contributor's machine, past a hook that is bypassable."
      fail=1
    fi
  done

  # The same question one layer down, for the suites that prove a guard can
  # still FAIL (#3820). A ratchet that has quietly become incapable of failing
  # reports green forever, which is worse than not having one — and #3750 is
  # the concrete instance: the typed-errors writer path grandfathered a
  # brand-new violation for as long as the guard existed, because the suite
  # that would have caught it ran only when a human typed the target.
  #
  # A suite may be excluded, but only out loud: `UNHOSTED_SELF_TESTS` in the
  # Makefile names each one with the reason beside it. Silence is the failure.
  if ! unhosted="$(make -s print-unhosted-self-tests 2>/dev/null)"; then
    note "FAIL — could not read UNHOSTED_SELF_TESTS (\`make -s print-unhosted-self-tests\`)."
    note "     Restore that target in the Makefile; it is what keeps the"
    note "     exclusions reviewable instead of hidden in this script."
    fail=1
  else
    for suite in scripts/test-*.sh scripts/test-*.py; do
      [ -f "$suite" ] || continue
      name="${suite#scripts/}"
      case " $unhosted " in
      *" $name "*) continue ;;
      esac
      if ! grep -qF -- "$name" "$commands"; then
        note "FAIL — no workflow runs the guard self-test '$suite'."
        note "     Add it to a workflow, or name it in UNHOSTED_SELF_TESTS in"
        note "     the Makefile with the reason it is excluded."
        fail=1
      fi
    done

    # And the other direction: an exclusion for a suite that no longer exists,
    # or one that has since been wired up, reads as a live decision and is not.
    for name in $unhosted; do
      if [ ! -f "scripts/$name" ]; then
        note "FAIL — UNHOSTED_SELF_TESTS names 'scripts/$name', which does not exist."
        fail=1
      elif grep -qF -- "$name" "$commands"; then
        note "FAIL — UNHOSTED_SELF_TESTS names '$name', but a workflow runs it."
        note "     Drop it from the list; the exclusion is stale."
        fail=1
      fi
    done
  fi
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
printf 'check-gate-parity: OK — %s (%s) gate steps, named in %s and %s, each run by a workflow.\n' \
  "$count" "$word" "$agents" "$contributing" || true
