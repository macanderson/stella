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
# Covers #1547: the seen-digest dedup oracle, the AIMD controller, the
# per-lens dry streak, the governor's whole tier ladder (the probes are pinned
# with FULLAUTO_PROBE_* so the same cases pass on a loaded laptop and a bare
# CI runner), and queue ranking (a stub `gh` first on PATH serves fixtures, so
# "no network" holds even where the real `gh` is installed). The observatory
# half of the same contract is tested in
# crates/stella-observatory/src/fullauto.rs.
#
# The `bench h2h --rig` money path is rehearsed the same way (#1550): stub
# aws/ssh/scp record every call they receive, so the dry run, the stop trap,
# and the no-phantom-results contract are witnessed without an instance ever
# starting.
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

# A stub `gh` first on PATH keeps the demand half of `plan` and the whole of
# `queue` off the network — the real `gh` is installed on the dev box, and
# without the stub those cases would make live API calls and float with the
# actual issue tracker. It answers `plan`'s two count queries from
# $GH_FIX_P0 / $GH_FIX_BUGS and serves `queue` its raw payload from
# $GH_FIX_QUEUE. The `:?` on the queue fixture is a tripwire: a case that
# reaches it without declaring a fixture is a case that thought it was not
# touching gh at all.
STUB_BIN="$ROOT/bin"
mkdir -p "$STUB_BIN"
cat > "$STUB_BIN/gh" <<'SH'
#!/bin/sh
case "$*" in
  *"--label bug --label P0"*) printf '%s\n' "${GH_FIX_P0:-0}" ;;
  *"--label bug"*)            printf '%s\n' "${GH_FIX_BUGS:-0}" ;;
  *"issue list"*)             cat "${GH_FIX_QUEUE:?stub gh: no queue fixture declared}" ;;
  *) echo "stub gh: unexpected args: $*" >&2; exit 1 ;;
esac
SH
chmod +x "$STUB_BIN/gh"

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

# The other half of the streak contract: the dry cycles must be CONSECUTIVE.
# A discovering cycle between two dry ones restarts the count — the streak
# walk stops at the first non-dry record — so dry-noise-dry never advances.
# If someone "simplifies" the walk into counting dry records in the tail,
# this is the case that goes red.
fa apr cycle-end --cycle 1 --new 0 --outcome ok >/dev/null 2>&1
fa apr cycle-end --cycle 2 --new 1 --outcome ok >/dev/null 2>&1
fa apr cycle-end --cycle 3 --new 0 --outcome ok >/dev/null 2>&1
check_eq "rubric" "$(fa apr aperture --current)" "a discovering cycle between two dry ones resets the streak"
fa apr cycle-end --cycle 4 --new 0 --outcome ok >/dev/null 2>&1
check_eq "properties" "$(fa apr aperture --current)" "two consecutive dry cycles after the reset advance"

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

# One assertion shape for the whole ladder: pin every probe to a healthy
# 8-core / 16 GiB / 50 GiB baseline, apply one case's overrides on top (later
# `env` assignments win), and compare the WHOLE decision tuple exactly. Exact
# rather than per-field, because "light with the build still on" and "light
# with the build forbidden" are different decisions and a single-field check
# lets one impersonate the other — the same philosophy as
# test-impacted-crates.sh asserting the exact scope line.
want_plan() { # want_plan <name> <case_dir> "<tier build par scope bench batch audit>" [VAR=v ...]
  local name="$1" dir="$2" want="$3"
  shift 3
  local got
  got="$(env PATH="$STUB_BIN:$PATH" FULLAUTO_STATE_DIR="$ROOT/$dir" \
      FULLAUTO_PROBE_CPU=8 FULLAUTO_PROBE_LOAD1=1 \
      FULLAUTO_PROBE_MEM_TOTAL_GB=16 FULLAUTO_PROBE_MEM_FREE_GB=8 \
      FULLAUTO_PROBE_DISK_FREE_GB=50 FULLAUTO_PROBE_ON_BATTERY=0 \
      FULLAUTO_PROBE_CONTENTION=0 "$@" "$FA" plan \
    | awk -F= '
        $1 == "FULLAUTO_TIER"        { t = $2 }
        $1 == "FULLAUTO_LOCAL_BUILD" { b = $2 }
        $1 == "FULLAUTO_PARALLEL"    { p = $2 }
        $1 == "FULLAUTO_SCOPE"       { s = $2 }
        $1 == "FULLAUTO_BENCH"       { n = $2 }
        $1 == "FULLAUTO_BATCH"       { a = $2 }
        $1 == "FULLAUTO_AUDIT"       { d = $2 }
        END { print t, b, p, s, n, a, d }')"
  check_eq "$want" "$got" "$name"
}

# Supply rungs, straddled in pairs where the rule has an edge.
want_plan "a healthy mid box runs a normal cycle at the calibrated ceilings" \
  g-n1 "normal 1 2 impacted loop 20 deep"
want_plan "a big idle box earns the heavy tier and the measured match" \
  g-h1 "heavy 1 2 workspace h2h 20 deep" \
  FULLAUTO_PROBE_MEM_TOTAL_GB=64 FULLAUTO_PROBE_MEM_FREE_GB=32 FULLAUTO_PROBE_DISK_FREE_GB=200
want_plan "...but only when it is actually idle (load at cpu/2 is not)" \
  g-h2 "normal 1 2 impacted loop 20 deep" \
  FULLAUTO_PROBE_MEM_TOTAL_GB=64 FULLAUTO_PROBE_MEM_FREE_GB=32 FULLAUTO_PROBE_DISK_FREE_GB=200 \
  FULLAUTO_PROBE_LOAD1=4
want_plan "a breached disk floor forbids the local build and sheds the bench" \
  g-d1 "light 0 2 ci off 5 deep" FULLAUTO_PROBE_DISK_FREE_GB=5
want_plan "the floor outranks heavy-class hardware — the first rung wins" \
  g-d2 "light 0 2 ci off 5 deep" \
  FULLAUTO_PROBE_MEM_TOTAL_GB=64 FULLAUTO_PROBE_MEM_FREE_GB=32 FULLAUTO_PROBE_DISK_FREE_GB=5
want_plan "the floor itself is a knob — raised above real free space it trips" \
  g-d3 "light 0 2 ci off 5 deep" FULLAUTO_DISK_FLOOR_GB=999999
want_plan "a busy box gives up the build AND drops to serial" \
  g-b1 "light 0 1 ci off 5 deep" FULLAUTO_PROBE_CONTENTION=1
want_plan "a breached memory floor reads like a busy box" \
  g-m1 "light 0 1 ci off 5 deep" FULLAUTO_PROBE_MEM_FREE_GB=2
want_plan "battery sheds compute but keeps the local build" \
  g-p1 "light 1 1 impacted off 5 deep" FULLAUTO_PROBE_ON_BATTERY=1
want_plan "a saturated box narrows concurrency and nothing else" \
  g-l1 "light 1 1 impacted loop 5 deep" FULLAUTO_PROBE_LOAD1=8

# Demand rungs: the queue can shrink the batch, and a P0 can rescue a light
# cycle — but demand never upgrades the tier and never widens the batch.
want_plan "the queue clamps the batch — 3 open defects never earn a batch of 20" \
  g-q1 "normal 1 2 impacted loop 3 deep" GH_FIX_BUGS=3
want_plan "a P0 on a light tier buys a minimal fast cycle, not a skipped one" \
  g-q2 "light 1 1 impacted off 2 fast" \
  FULLAUTO_PROBE_ON_BATTERY=1 GH_FIX_BUGS=3 GH_FIX_P0=2
want_plan "a P0 on a healthy box changes nothing — the rescue is light-only" \
  g-q3 "normal 1 2 impacted loop 3 deep" GH_FIX_BUGS=3 GH_FIX_P0=2
want_plan "the light tier caps the batch at 5 whatever the queue holds" \
  g-q4 "light 1 1 impacted off 5 deep" FULLAUTO_PROBE_ON_BATTERY=1 GH_FIX_BUGS=9

# ---------------------------------------------------------------------------
head_ "queue — ranked P0 > P1 > P2, oldest first inside a rank"

QFIX="$ROOT/queue-fixture.json"
cat > "$QFIX" <<'JSON'
[
  {"number": 101, "title": "fresh P1",  "createdAt": "2026-08-01T00:00:00Z", "url": "u",
   "labels": [{"name": "bug"}, {"name": "P1"}]},
  {"number": 102, "title": "aged P1",   "createdAt": "2026-01-01T00:00:00Z", "url": "u",
   "labels": [{"name": "bug"}, {"name": "P1"}]},
  {"number": 103, "title": "the P0",    "createdAt": "2026-08-01T00:00:00Z", "url": "u",
   "labels": [{"name": "bug"}, {"name": "P0"}]},
  {"number": 104, "title": "untriaged", "createdAt": "2025-01-01T00:00:00Z", "url": "u",
   "labels": [{"name": "triage"}]},
  {"number": 105, "title": "a feature", "createdAt": "2025-01-01T00:00:00Z", "url": "u",
   "labels": [{"name": "enhancement"}]}
]
JSON

queue_numbers() { # queue_numbers <limit>
  env PATH="$STUB_BIN:$PATH" GH_FIX_QUEUE="$QFIX" FULLAUTO_STATE_DIR="$ROOT/qr" \
    "$FA" queue --json --limit "$1" \
    | python3 -c 'import json,sys;print(" ".join(str(i["number"]) for i in json.load(sys.stdin)))'
}

# 104 is oldest of all yet ranks last, and 105 never appears: rank beats age
# across ranks, an untriaged issue counts as a defect, a feature does not.
check_eq "103 102 101 104" "$(queue_numbers 10)" \
  "P0 first, age breaks ties inside a rank, triage counts, features do not"
check_eq "103" "$(queue_numbers 1)" "--limit truncates after ranking, not before"

# ---------------------------------------------------------------------------
head_ "bench h2h --rig — the money path, rehearsed with stub tools (#1550)"

# Stub aws/ssh/scp record every call they receive, so a case can assert not
# only that the stop trap fired but that nothing ever started the instance.
# This is the free rehearsal of a path whose real execution bills $1.90/hr —
# the reason it shipped unexecuted in the first place.
RIG_FIX_KEY="$ROOT/tb909-key.pem"
: > "$RIG_FIX_KEY"
RIG_FIX_MATCH="$ROOT/match.toml"
printf '[match]\n' > "$RIG_FIX_MATCH"

cat > "$STUB_BIN/aws" <<'SH'
#!/bin/sh
printf 'aws %s\n' "$*" >> "${STUB_LOG:?stub aws: no log declared}"
case "$*" in
  *"sts get-caller-identity"*)
    [ "${AWS_FIX_STS:-0}" -eq 0 ] || exit "${AWS_FIX_STS}"
    printf 'ok\n' ;;
  *"State.Name"*)      printf '%s\n' "${AWS_FIX_STATE:-stopped}" ;;
  *"PublicIpAddress"*) printf '198.51.100.7\n' ;;
esac
exit 0
SH
chmod +x "$STUB_BIN/aws"

cat > "$STUB_BIN/ssh" <<'SH'
#!/bin/sh
printf 'ssh %s\n' "$*" >> "${STUB_LOG:?stub ssh: no log declared}"
case "$*" in
  *"test -d"*)        exit "${SSH_FIX_PREFLIGHT:-0}" ;;
  *"arenabench run"*) exit "${SSH_FIX_MATCH:-0}" ;;
  *)                  exit 0 ;;
esac
SH
chmod +x "$STUB_BIN/ssh"

cat > "$STUB_BIN/scp" <<'SH'
#!/bin/sh
printf 'scp %s\n' "$*" >> "${STUB_LOG:?stub scp: no log declared}"
[ "${SCP_FIX_EXIT:-0}" -eq 0 ] || exit "${SCP_FIX_EXIT}"
# The destination is the last argument; a real scp writes the file, so the
# stub does too — the caller's JSON-parse check needs bytes to chew on.
for last in "$@"; do :; done
printf '{"verifier_result": {"rewards": {"reward": 1.0}}}\n' > "$last"
SH
chmod +x "$STUB_BIN/scp"

rig_h2h() { # rig_h2h <case_dir> [VAR=v ...] -- [extra h2h flags ...]
  local dir="$1"; shift
  local envs=()
  while [ $# -gt 0 ] && [ "$1" != "--" ]; do envs+=("$1"); shift; done
  [ "${1:-}" = "--" ] && shift
  mkdir -p "$ROOT/$dir"
  env PATH="$STUB_BIN:$PATH" FULLAUTO_STATE_DIR="$ROOT/$dir" \
      FULLAUTO_RIG_KEY="$RIG_FIX_KEY" FULLAUTO_MATCH="$RIG_FIX_MATCH" \
      STUB_LOG="$ROOT/$dir/stub.log" \
      ${envs[@]+"${envs[@]}"} "$FA" bench h2h --rig "$@"
}

# The dry run: every check runs, the plan prints, the instance never starts.
dry_out="$(rig_h2h rg-dry -- --dry-run --out "$ROOT/rg-dry/h2h.json" 2>&1)"
dry_rc=$?
check_eq "0" "$dry_rc" "a green dry run exits 0"
if grep -q 'ec2 start-instances' "$ROOT/rg-dry/stub.log" 2>/dev/null; then
  bad "the dry run STARTED the instance"
else
  ok "the dry run never starts the instance"
fi
if grep -q 'sts get-caller-identity' "$ROOT/rg-dry/stub.log" 2>/dev/null; then
  ok "the dry run really exercises the aws credential check"
else
  bad "the dry run skipped the aws credential check"
fi
if printf '%s' "$dry_out" | grep -q 'ec2 start-instances'; then
  ok "the dry run prints the commands the paid path would run"
else
  bad "the dry run never printed its command plan"
fi

# A dead credential turns the rehearsal red — a NOT READY that exits 0 would
# read as a rehearsal passed.
rig_h2h rg-nr AWS_FIX_STS=1 -- --dry-run >/dev/null 2>&1
check_eq "1" "$?" "a failed check turns the dry run red (exit 1)"

# The remote preflight fails -> the match never runs, and the trap still
# stops the instance. This is the case that used to cost a multi-hour billed
# garbage run; now it costs cents and the stop is witnessed in the log.
rig_h2h rg-pf SSH_FIX_PREFLIGHT=70 -- --out "$ROOT/rg-pf/h2h.json" >/dev/null 2>&1
pf_rc=$?
if [ "$pf_rc" -ne 0 ]; then ok "a failed remote preflight fails the run"; else bad "a failed remote preflight read as success"; fi
if grep -q 'arenabench run' "$ROOT/rg-pf/stub.log" 2>/dev/null; then
  bad "the match ran despite a failed remote preflight"
else
  ok "the match never runs after a failed remote preflight"
fi
if grep -q 'ec2 stop-instances' "$ROOT/rg-pf/stub.log" 2>/dev/null; then
  ok "the stop trap fired on the failure path (the \$45.56/day bug)"
else
  bad "the stop trap NEVER FIRED — the instance would bill until someone noticed"
fi

# A missing results file is an error, not a phantom path the caller trusts.
scp_out="$(rig_h2h rg-scp SCP_FIX_EXIT=1 -- --out "$ROOT/rg-scp/h2h.json" 2>/dev/null)"
scp_rc=$?
if [ "$scp_rc" -ne 0 ]; then ok "a missing results file fails the run"; else bad "a missing results file read as success"; fi
if printf '%s' "$scp_out" | grep -q 'rg-scp/h2h.json'; then
  bad "the run echoed a results path that does not exist"
else
  ok "no phantom results path on the failure path"
fi
if grep -q 'ec2 stop-instances' "$ROOT/rg-scp/stub.log" 2>/dev/null; then
  ok "the stop trap fired on the scp failure too"
else
  bad "the stop trap missed the scp failure path"
fi

# The happy path: results come back, parse, and the rig is stopped anyway.
ok_out="$(rig_h2h rg-ok -- --out "$ROOT/rg-ok/h2h.json" 2>/dev/null)"
ok_rc=$?
check_eq "0" "$ok_rc" "the happy path exits 0"
check_eq "$ROOT/rg-ok/h2h.json" "$(printf '%s\n' "$ok_out" | tail -1)" \
  "the happy path's last stdout line is the results path"
if python3 -m json.tool "$ROOT/rg-ok/h2h.json" >/dev/null 2>&1; then
  ok "the results file parses as JSON"
else
  bad "the results file does not parse"
fi
if grep -q 'ec2 stop-instances' "$ROOT/rg-ok/stub.log" 2>/dev/null; then
  ok "the stop trap fires on success as well — ALWAYS stop"
else
  bad "the stop trap missed the success path"
fi

# ---------------------------------------------------------------------------
printf '\n'
if [ "$fail_n" -eq 0 ]; then
  printf '\033[32mfullauto-test: OK\033[0m — %s checks passed\n' "$pass_n"
  exit 0
fi
printf '\033[31mfullauto-test: FAILED\033[0m — %s passed, %s failed\n' "$pass_n" "$fail_n"
exit 1
