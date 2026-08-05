#!/usr/bin/env bash
# Hermetic tests for the fullauto control logic.
#
# Everything here runs against a throwaway $FULLAUTO_STATE_DIR: no network, no
# `gh`, no writes outside the temp tree. Deliberately NOT part of `make gate` —
# same posture as `impacted-test` and `dev-env-test`.
#
#   make fullauto-test        # or: scripts/test-fullauto.sh
#
# The cases here are the ones whose failure modes are SILENT and PERMANENT: a
# digest that over-normalizes merges two defects and the second is never filed;
# a controller that mislearns pins the batch ceiling forever; a dropped run
# record leaves a finished run reading `crashed` to every reader for good.
# None of them announce themselves, which is why they need a test rather than
# an eyeball.
#
# Partial coverage of #1547. The observatory half of the same contract is
# tested in crates/stella-observatory/src/fullauto.rs.
set -uo pipefail

FA="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/scripts/fullauto.sh"
readonly FA
ROOT="$(mktemp -d)"
readonly ROOT
trap 'rm -rf "$ROOT"' EXIT

pass_n=0
fail_n=0

ok()   { printf '  \033[32m✓\033[0m %s\n' "$*"; pass_n=$((pass_n + 1)); }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$*"; fail_n=$((fail_n + 1)); }
head_() { printf '\n\033[1m%s\033[0m\n' "$*"; }

# Each case gets its own state dir; a shared one would let an earlier case's
# seen-set or ledger decide a later one's result.
fa() {
  local case_dir="$1"; shift
  FULLAUTO_STATE_DIR="$ROOT/$case_dir" "$FA" "$@"
}

check_eq() {
  local want="$1" got="$2" what="$3"
  if [ "$want" = "$got" ]; then ok "$what"; else bad "$what (want '$want', got '$got')"; fi
}

# ---------------------------------------------------------------------------
head_ "digest — the dedup key the termination oracle rests on"

d1="$(fa digest seen --digest 'crates/stella-core/src/driver.rs:812 retry counter never reset')"
d2="$(fa digest seen --digest 'crates/stella-core/src/driver.rs:940    Retry Counter Never Reset')"
d3="$(fa digest seen --digest 'crates/stella-cli/src/agent.rs a different defect entirely')"
check_eq "$d1" "$d2" "line number, case and whitespace normalize to one finding"
if [ "$d1" != "$d3" ]; then ok "distinct findings keep distinct digests"
else bad "distinct findings collided"; fi

new_before="$(fa digest seen --new "$d1" | wc -l | tr -d ' ')"
fa digest seen --add "$d1" >/dev/null
new_after="$(fa digest seen --new "$d1" | wc -l | tr -d ' ')"
check_eq "1" "$new_before" "an unseen digest reports as new"
check_eq "0" "$new_after" "a recorded digest never reports as new again"

# ---------------------------------------------------------------------------
head_ "controller — AIMD, the only thing that moves the ceilings"

fa aimd calibrate --ok >/dev/null
fa aimd calibrate --ok >/dev/null
fa aimd calibrate --ok >/dev/null
par="$(fa aimd calibrate --show | python3 -c 'import json,sys;print(json.load(sys.stdin)["parallel_ceiling"])')"
check_eq "3" "$par" "three clean runs raise the parallel ceiling exactly once"

fa aimd calibrate --resource-fail >/dev/null
after="$(fa aimd calibrate --show | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d["batch_ceiling"],d["parallel_ceiling"],d["clean_run"])')"
check_eq "10 1 0" "$after" "a resource failure halves batch and resets parallel to serial"

# The regression that shipped: `calibrate` rebuilt the document from the four
# keys it owns and silently discarded every other key — `last_clean_head` among
# them, leaving watch mode unable to ever sleep.
python3 - "$ROOT/aimd/calibration.json" <<'PY'
import json, sys
p = sys.argv[1]
c = json.load(open(p)); c["last_clean_head"] = "deadbeef"
json.dump(c, open(p, "w"))
PY
fa aimd calibrate --ok >/dev/null
kept="$(fa aimd calibrate --show | python3 -c 'import json,sys;print(json.load(sys.stdin).get("last_clean_head","LOST"))')"
check_eq "deadbeef" "$kept" "calibrate preserves keys it does not own"

# ---------------------------------------------------------------------------
head_ "aperture — why the loop never terminates"

check_eq "rubric" "$(fa ap aperture --current)" "a fresh loop opens at the rubric lens"
fa ap cycle-end --cycle 1 --new 1 --outcome ok >/dev/null 2>&1
check_eq "rubric" "$(fa ap aperture --current)" "a cycle that discovers something holds the lens open"
fa ap cycle-end --cycle 2 --new 0 --outcome ok >/dev/null 2>&1
check_eq "rubric" "$(fa ap aperture --current)" "one dry cycle is noise, not convergence"
fa ap cycle-end --cycle 3 --new 0 --outcome ok >/dev/null 2>&1
check_eq "properties" "$(fa ap aperture --current)" "two dry cycles advance to the next lens"

# The dry streak is per LENS, not a global tail count over the ledger. When it
# was global, the two dry cycles that advanced rubric were still in the tail,
# so `properties` — and every lens after it — advanced on a single dry audit,
# collapsing the whole ladder at one cycle per lens.
fa ap cycle-end --cycle 4 --new 0 --outcome ok >/dev/null 2>&1
check_eq "properties" "$(fa ap aperture --current)" "a fresh lens does not inherit the dry tail that opened it"
fa ap cycle-end --cycle 5 --new 0 --outcome ok >/dev/null 2>&1
check_eq "invariants" "$(fa ap aperture --current)" "two dry cycles under the new lens advance again"

# ---------------------------------------------------------------------------
head_ "run lifecycle — running / completed / cancelled"

fa runs run start >/dev/null 2>&1
fa runs cycle-begin >/dev/null 2>&1
rid="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["run_id"])' "$ROOT/runs/run.json" 2>/dev/null || echo MISSING)"
if [ "$rid" != "MISSING" ] && [ -n "$rid" ]; then
  ok "cycle-begin leaves the live run intact (it does not bank a healthy run as crashed)"
else
  bad "cycle-begin destroyed the live run record"
fi

fa runs cycle-end --cycle 1 --fixed 2 --new 1 --outcome ok >/dev/null 2>&1
still="$(fa runs runs | awk '/^r-/ {print $2}')"
check_eq "running" "$still" "a completed CYCLE does not end the RUN"

# The bug a live fullauto cycle found in this file: `str.isdigit()` is true for
# characters `int()` refuses (superscripts), the ValueError took the whole
# record with it, and the run then read `crashed` to every reader forever.
fa sup run start >/dev/null 2>&1
fa sup run end --status completed --reason "²" >/dev/null 2>&1
sup="$(python3 - "$ROOT/sup/runs.jsonl" <<'PY'
import json, sys
last = ""
for line in open(sys.argv[1]):
    last = json.loads(line).get("status", "")
print(last)
PY
)"
check_eq "completed" "$sup" "a Unicode-digit field value does not drop the run record"

# ---------------------------------------------------------------------------
head_ "daemon — a stop that does not stop costs real money"

# Both cases below were live defects. `daemon stop` reported success while the
# model call kept running and kept spending, and the hand-stopped run went into
# the history as `crashed` because the KILL raced the shutdown handler.
#
# The child is identified by the pid the daemon RECORDS, never by matching a
# command line. `sh -c "sleep 120"` is exec-optimized into a bare `sleep`, so
# any pattern specific enough to be safe does not appear in the process table
# at all — and a loose pattern like `sleep 120` matches unrelated processes,
# including an orphan a previous failing run of this test left behind. That
# turns a working daemon into a red test and sends the reader hunting a bug
# that is not there. (Both happened while writing this.)
FULLAUTO_STATE_DIR="$ROOT/dmn" FULLAUTO_CYCLE_CMD="sleep 120" \
  "$FA" daemon start 60 >/dev/null 2>&1
sleep 3
cycle_pid="$(python3 -c 'import json,sys
try: print(json.load(open(sys.argv[1])).get("cycle_pid",""))
except Exception: print("")' "$ROOT/dmn/run.json" 2>/dev/null || echo "")"
if [ -n "$cycle_pid" ] && kill -0 "$cycle_pid" 2>/dev/null; then
  ok "the daemon starts a detached cycle child and records its pid"
else
  bad "the daemon never started (or never recorded) its cycle child"
fi

stop_out="$(FULLAUTO_STATE_DIR="$ROOT/dmn" "$FA" daemon stop 2>&1)"
sleep 1
if [ -n "$cycle_pid" ] && kill -0 "$cycle_pid" 2>/dev/null; then
  bad "daemon stop ORPHANED the in-flight cycle (pid $cycle_pid still running, still spending)"
  kill -KILL "$cycle_pid" 2>/dev/null || true
else
  ok "daemon stop takes the in-flight cycle down with it"
fi

if printf '%s' "$stop_out" | grep -q 'did not stop'; then
  bad "daemon stop had to escalate to KILL (the handler was raced)"
else
  ok "daemon stop lets the shutdown handler finish instead of racing it"
fi

dmn_status="$(FULLAUTO_STATE_DIR="$ROOT/dmn" "$FA" runs | awk '/^r-/ {print $2}')"
check_eq "cancelled" "$dmn_status" "a hand-stopped run records cancelled, not crashed"

# ---------------------------------------------------------------------------
head_ "governor — the plan is a function of the machine"

plan="$(FULLAUTO_DISK_FLOOR_GB=999999 fa gov plan)"
build="$(printf '%s\n' "$plan" | awk -F= '/^FULLAUTO_LOCAL_BUILD=/ {print $2}')"
check_eq "0" "$build" "a breached disk floor forbids the local build"

# ---------------------------------------------------------------------------
printf '\n'
if [ "$fail_n" -eq 0 ]; then
  printf '\033[32mfullauto-test: OK\033[0m — %s checks passed\n' "$pass_n"
  exit 0
fi
printf '\033[31mfullauto-test: FAILED\033[0m — %s passed, %s failed\n' "$pass_n" "$fail_n"
exit 1
