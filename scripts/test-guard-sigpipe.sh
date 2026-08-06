#!/usr/bin/env bash
#
# Tests that the gate guards survive a reader that closes their pipe (#1815).
#
#   ./scripts/test-guard-sigpipe.sh
#
# Piping a guard's output is the normal way to read it — `| tail`, `| rg FAIL`,
# `| head` — and check-god-files.sh used to report a FALSE failure, naming a
# different crate each run, whenever a pipe of its closed early: under
# `set -euo pipefail` the failed write became the script's exit status, and
# whatever partial comparison state the scan had reached became the verdict.
# Worse, its crate-membership test was itself a `printf | grep -q` pipeline,
# which `grep -q`'s early exit could flip from "present" to "absent" mid-scan.
#
# So each case here runs a real guard against this repository with its stdout
# going to a reader that exits early, and asserts the exit code is the same 0
# the unpiped run produces on a green tree. `| true` is the harshest reader —
# it closes the pipe before the guard writes anything — and it is
# deterministic: against the pre-#1815 scripts every `| true` case below dies
# on the final OK write with exit 141 (SIGPIPE) or 1 (EPIPE under `set -e`).
# `| head -1` is the repro the issue was filed with; the membership race it
# tickled was intermittent, so that case runs repeatedly.
#
# The guards are expected green here: `make gate` holds them green on every
# push, and a genuinely red tree fails loudly below with the guard's own
# report shown — it does not read as a SIGPIPE regression.
#
# Not part of `make gate` — run it after touching any scripts/check-*.sh, the
# same posture as scripts/test-file-size.sh.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
cd "$repo_root" || exit 1

pass=0
fail=0
errlog="$(mktemp "${TMPDIR:-/tmp}/stella-guard-sigpipe.XXXXXX")"
trap 'rm -f "$errlog"' EXIT INT TERM

# want <guard> <reader ...> — run scripts/<guard> with stdout piped into the
# reader, and expect the guard's exit code to be 0. PIPESTATUS[0] is the
# guard's own status, unpolluted by the reader's.
want() {
  local guard="$1"
  shift
  local name="$guard | $*"
  "scripts/$guard" 2>"$errlog" | "$@" >/dev/null 2>&1
  local rc="${PIPESTATUS[0]}"
  if [ "$rc" -eq 0 ]; then
    pass=$((pass + 1))
    echo "ok   $name  rc=0"
  else
    fail=$((fail + 1))
    echo "FAIL $name — rc=$rc (a closed pipe must not change the verdict)"
    sed 's/^/     /' "$errlog"
  fi
}

# ── Every swept guard, against both early-exiting readers ────────────────────
for guard in \
  check-god-files.sh \
  check-invariants.sh \
  check-left-behind.sh \
  check-command-docs.sh \
  check-gate-parity.sh \
  check-role-names.sh \
  check-cargo-install-pins.sh \
  check-repro-wiring.sh; do
  want "$guard" true
  want "$guard" head -1
done

# ── The original repro (#1815) ───────────────────────────────────────────────
# The `printf | grep -qx` membership race fired only intermittently — a
# different innocent crate blamed on each piped run — so this case gets room
# to show itself: ten runs, all of which must agree with the unpiped verdict.
for _ in 1 2 3 4 5 6 7 8 9 10; do
  want check-god-files.sh head -1
done

# ── The slow one ─────────────────────────────────────────────────────────────
# check-file-size.sh forks a `wc` per tracked file, which costs about a minute
# on a laptop, so it gets the one deterministic reader rather than the pair.
want check-file-size.sh true

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
