#!/usr/bin/env bash
# The deterministic half of `fullauto` — the autonomous delivery loop.
#
# `fullauto` is a cycle: fix a batch of defects, audit what is left, file what
# it cannot fix, benchmark against the comparator, ship, and repeat until two
# consecutive audits surface nothing new. The judgement half of that loop lives
# in the `/fullauto` slash commands (scripts/fullauto/commands/); everything a
# machine can decide without a model lives here, so the model never has to
# re-derive it and cannot get it subtly wrong.
#
#   scripts/fullauto.sh preflight          # is this machine able to run a cycle?
#   scripts/fullauto.sh plan --explain     # size this cycle to THIS machine, now
#   scripts/fullauto.sh queue --limit 20   # the defect queue, ranked
#   scripts/fullauto.sh cycle-begin        # allocate cycle N, emit shell vars
#   scripts/fullauto.sh seen --new <sha>…  # which finding digests are unseen
#   scripts/fullauto.sh cycle-end …        # append the ledger record, recalibrate
#   scripts/fullauto.sh aperture --list    # which audit lens is open
#   scripts/fullauto.sh watch              # low-duty mode: wake only on a change
#   scripts/fullauto.sh metrics            # the loop's evidence about ITSELF
#   scripts/fullauto.sh bench loop|h2h     # the comparator arm
#   scripts/fullauto.sh upgrade            # ship: brew from the tap, drop the shadow
#   scripts/fullauto.sh install-commands   # put /fullauto in ~/.claude/commands
#
# The loop does not terminate. When a lens goes dry the aperture widens; when
# every lens is dry it drops to `watch` and wakes on a change. "No defects" is
# a statement about the lens, never about the code.
#
# It also sizes itself: `plan` reads the box's real disk/memory/compute and the
# controller's learned ceilings, and `calibrate` moves those ceilings from
# evidence (AIMD) so the same command is correct on a 16 GiB laptop and a
# 123 GiB rig without anyone editing a threshold.
#
# State lives OUTSIDE the repo ($FULLAUTO_STATE_DIR, default ~/.fullauto/stella)
# because `make no-scratch` (#448) fails the gate if a tracked file matches a
# .gitignore rule, and session scratch must never reach the remote.
set -uo pipefail

# ---------------------------------------------------------------------------
# Constants that are facts about this machine and this repository
# ---------------------------------------------------------------------------

# Homebrew-core owns the name `stella` (an Atari 2600 emulator, 245 installs a
# year). `brew install stella` therefore installs the WRONG software and exits
# 0. The tap-qualified name is the only safe spelling; never shorten it.
readonly BREW_FORMULA="macanderson/stella/stella"
readonly BREW_TAP="macanderson/stella"
readonly BREW_TAP_REMOTE="https://github.com/macanderson/homebrew-tap.git"

# The PATH entry that shadows a released install with the local dev build. The
# `upgrade` subcommand removes exactly this string and refuses to touch the rc
# file if it does not find it verbatim — mangling a login shell is not a
# recoverable mistake.
#
# shellcheck disable=SC2016  # deliberate: this is the literal TEXT of a line in
# ~/.zshrc, matched byte-for-byte. Expanding $HOME here would stop it matching.
readonly SHADOW_PATH_LINE='export PATH="$HOME/Projects/stella/target/release:$PATH"'

# The Terminal-Bench rig. Native x86_64 Linux Docker host; see
# bench/RUNBOOK.md Part 0. It bills $1.90/hr RUNNING, so every path that starts
# it also stops it, including on failure.
readonly RIG_INSTANCE="${FULLAUTO_RIG_INSTANCE:-i-07d46341dcc9a31b3}"
readonly RIG_REGION="${FULLAUTO_RIG_REGION:-us-east-1}"
readonly RIG_USER="ubuntu"
readonly RIG_KEY="docs/design/stella-bench-handoff/tb909-key.pem"

# The head-to-head match: Claude Code vs Stella on Terminal-Bench 2.1, same
# model on both arms so the agent architecture is the variable under test.
readonly H2H_MATCH="${FULLAUTO_MATCH:-arenabench/matches/fable5-claude-code-vs-stella.toml}"

# Two consecutive audits with zero unseen findings ends an APERTURE, not the
# loop. One dry cycle is noise; two means this lens has stopped producing, so
# the next lens opens. The loop itself never terminates — see `aperture`.
readonly DRY_STREAK_TARGET="${FULLAUTO_DRY_STREAK:-2}"

# The audit apertures, widest-yield first. Each is a distinct LENS, not a
# deeper pass of the same one: a lens that has never run cannot have found
# nothing, which is why "we found no defects" is only ever a statement about
# the lens, never about the code. Exhausting the list drops the loop into
# watch mode rather than ending it.
readonly APERTURES="rubric properties invariants concurrency performance supply-chain security docs soak"

# AIMD ceilings. Additive increase on a clean cycle, multiplicative decrease on
# a resource failure — the controller shape that provably converges instead of
# oscillating, and the reason this loop gets FASTER on a big machine and
# SMALLER on a constrained one without anyone editing a threshold.
readonly BATCH_HARD_MAX="${FULLAUTO_BATCH_MAX:-20}"
readonly BATCH_HARD_MIN=2
readonly PARALLEL_HARD_MAX="${FULLAUTO_PARALLEL_MAX:-4}"

# Below these the machine cannot host the work at all, whatever the ceilings
# say. A workspace target dir for this repo is ~10-20 GiB; starting a build
# with less free than this ends as a killed compiler, not a small build.
readonly DISK_FLOOR_GB="${FULLAUTO_DISK_FLOOR_GB:-15}"
readonly MEM_FLOOR_GB="${FULLAUTO_MEM_FLOOR_GB:-4}"

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
readonly REPO_ROOT

# The state must follow the REPOSITORY, not the directory it is checked out in.
# Keying off `basename "$REPO_ROOT"` gives every git worktree its own ledger,
# seen-set and calibration — so a cycle run from a worktree would re-file every
# finding the main checkout already triaged, and the controller would relearn
# the machine from scratch. The remote URL is the stable identity; the shared
# git common dir is the fallback when there is no remote.
repo_slug() {
  local url common
  url="$(git -C "$REPO_ROOT" config --get remote.origin.url 2>/dev/null)"
  if [ -n "$url" ]; then
    url="${url%.git}"
    basename "$url"
    return 0
  fi
  common="$(git -C "$REPO_ROOT" rev-parse --git-common-dir 2>/dev/null || echo "")"
  case "$common" in
    "")  basename "$REPO_ROOT" ;;
    /*)  basename "$(dirname "$common")" ;;
    *)   basename "$(dirname "$REPO_ROOT/$common")" ;;
  esac
}

# State lives under the stella home, not a top-level ~/.fullauto, so it sits
# beside every other durable per-user artifact and `STELLA_HOME` moves it with
# the rest. The observatory resolves the same path through `stella-home` and
# reads it read-only.
stella_home() {
  if [ -n "${STELLA_HOME:-}" ]; then printf '%s' "$STELLA_HOME"; else printf '%s' "$HOME/.stella"; fi
}

STATE_DIR="${FULLAUTO_STATE_DIR:-$(stella_home)/fullauto/$(repo_slug)}"
readonly STATE_DIR

# `gh` colorizes --json output even when it is redirected to a file, if
# CLICOLOR_FORCE is set in the environment — and it is, in this developer's
# shell. ANSI escapes inside a JSON payload are invisible in a terminal and
# fatal to every parser, which makes this the exact class of bug that survives
# a manual eyeball check. Every gh call that is PARSED goes through here.
gh_plain() { NO_COLOR=1 CLICOLOR_FORCE=0 gh "$@"; }

readonly LEDGER="$STATE_DIR/ledger.jsonl"
readonly SEEN="$STATE_DIR/seen.txt"
readonly CYCLE_FILE="$STATE_DIR/cycle"
readonly CALIBRATION="$STATE_DIR/calibration.json"
readonly APERTURE_FILE="$STATE_DIR/aperture"

# The durable run record. This is what makes a cycle observable from outside the
# terminal that started it: pid, cycle, phase and a heartbeat, rewritten in
# place as the cycle advances. A terminal that quits leaves this file behind
# with a stale heartbeat, which is how a crashed cycle is told apart from a
# running one — an absent record would look identical to a clean finish.
readonly RUN_FILE="$STATE_DIR/run.json"

# Which workspace roots this loop belongs to. The observatory cannot run git, so
# it cannot re-derive the slug the way this script does; the loop writes down
# its own roots and the dashboard matches on them.
readonly WORKSPACE_FILE="$STATE_DIR/workspace.json"

# One record per RUN — a `/loop` session or a daemon lifetime, spanning many
# cycles. Append-only and event-sourced: every transition appends a record and
# readers fold by run_id, last write winning. Rewriting one JSON document in
# place would be the only structure that a crash mid-write could corrupt, and a
# crash is precisely the case this file exists to record.
readonly RUNS="$STATE_DIR/runs.jsonl"

readonly DAEMON_PID="$STATE_DIR/daemon.pid"
readonly DAEMON_LOG="$STATE_DIR/daemon.log"

# A heartbeat older than this means nobody is driving the cycle any more.
# Generous on purpose: a single model call inside an audit phase can legitimately
# run for minutes, and calling a live cycle dead is worse than noticing late.
readonly STALE_AFTER_SECS="${FULLAUTO_STALE_AFTER_SECS:-900}"

# ---------------------------------------------------------------------------
# Output
# ---------------------------------------------------------------------------

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  C_B=$'\033[1m'; C_R=$'\033[31m'; C_G=$'\033[32m'; C_Y=$'\033[33m'; C_0=$'\033[0m'
else
  C_B=''; C_R=''; C_G=''; C_Y=''; C_0=''
fi

say()  { printf '\n%s%s%s\n' "$C_B" "$*" "$C_0"; }
pass() { printf '  %s✓%s %s\n' "$C_G" "$C_0" "$*"; }
warn() { printf '  %s!%s %s\n' "$C_Y" "$C_0" "$*"; }
fail() { printf '  %s✗%s %s\n' "$C_R" "$C_0" "$*"; HARD_FAIL=1; }
die()  { printf '%sfullauto: %s%s\n' "$C_R" "$*" "$C_0" >&2; exit 1; }

HARD_FAIL=0

have() { command -v "$1" >/dev/null 2>&1; }

ensure_state() {
  migrate_legacy_state
  mkdir -p "$STATE_DIR" || die "cannot create state dir $STATE_DIR"
  record_workspace_root
  [ -f "$LEDGER" ] || : > "$LEDGER"
  [ -f "$SEEN" ] || : > "$SEEN"
  [ -f "$CYCLE_FILE" ] || echo 0 > "$CYCLE_FILE"
  [ -f "$APERTURE_FILE" ] || echo "rubric" > "$APERTURE_FILE"
  # Seed the controller OPTIMISTICALLY (at the hard max) rather than
  # pessimistically. A ceiling that starts low and only rises after several
  # clean cycles wastes a capable machine for a week; AIMD's decrease is fast
  # enough that starting high costs at most one degraded cycle.
  [ -f "$CALIBRATION" ] || cat > "$CALIBRATION" <<EOF
{"batch_ceiling": $BATCH_HARD_MAX, "parallel_ceiling": 2, "clean_run": 0, "note": "seeded"}
EOF
}

# An earlier build kept state at ~/.fullauto/<slug>. Move it, once, rather than
# silently starting a fresh ledger — losing the seen-set would re-file every
# finding ever triaged. Never overwrite: if both exist the new home wins and the
# legacy directory is left untouched for the user to inspect.
migrate_legacy_state() {
  [ -n "${FULLAUTO_STATE_DIR:-}" ] && return 0
  local legacy
  legacy="$HOME/.fullauto/$(repo_slug)"
  [ -d "$legacy" ] || return 0
  [ -e "$STATE_DIR" ] && return 0
  mkdir -p "$(dirname "$STATE_DIR")" || return 0
  if mv "$legacy" "$STATE_DIR" 2>/dev/null; then
    warn "migrated fullauto state: $legacy -> $STATE_DIR"
  fi
}

# Record every workspace root that has driven this loop. The observatory reads
# this to answer "which of these loops is the project I am looking at?" — it has
# no git, so it cannot derive the slug itself. Worktrees legitimately add extra
# roots for one repository, so this is a set, not a single value.
record_workspace_root() {
  python3 - "$WORKSPACE_FILE" "$REPO_ROOT" "$(repo_slug)" 2>/dev/null <<'PY' || true
import json, os, sys
path, root, slug = sys.argv[1], sys.argv[2], sys.argv[3]
try:
    doc = json.load(open(path))
except Exception:
    doc = {}
roots = doc.get("roots") or []
if root not in roots:
    roots.append(root)
# Bound it: a machine that churns worktrees should not grow this without limit.
doc.update({"slug": slug, "roots": roots[-32:]})
tmp = path + ".tmp"
with open(tmp, "w") as fh:
    json.dump(doc, fh, indent=2)
os.replace(tmp, path)
PY
}

# ---------------------------------------------------------------------------
# The durable run record
# ---------------------------------------------------------------------------
#
# Written on cycle-begin, rewritten at every phase boundary, cleared on
# cycle-end. Every write is atomic (temp file + rename) because the observatory
# polls this file and a half-written JSON would render as a broken panel rather
# than as the truth.

run_write() {
  local phase="$1" cycle="${2:-}" tier="${3:-}"
  python3 - "$RUN_FILE" "$phase" "$cycle" "$tier" "$(cat "$APERTURE_FILE" 2>/dev/null || echo '')" \
           "${FULLAUTO_DRIVER:-interactive}" "$$" 2>/dev/null <<'PY' || true
import json, os, socket, sys, time
path, phase, cycle, tier, aperture, driver, pid = sys.argv[1:8]
try:
    doc = json.load(open(path))
except Exception:
    doc = {}
now = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
if cycle:
    doc["cycle"] = int(cycle)
if tier:
    doc["tier"] = tier
doc.setdefault("started_at", now)
doc.update({
    "phase": phase,
    "aperture": aperture,
    "driver": driver,
    "pid": int(pid),
    "host": socket.gethostname(),
    "heartbeat": now,
    "heartbeat_unix": int(time.time()),
})
tmp = path + ".tmp"
with open(tmp, "w") as fh:
    json.dump(doc, fh, indent=2)
os.replace(tmp, path)
PY
}

# `phase` is the hook the cycle calls at each boundary. It is what turns the run
# record from "something is running" into "something is running, and it is in
# the audit stage of cycle 12" — which is the difference between a status light
# and an observable workflow.
cmd_phase() {
  ensure_state
  local phase="${1:?phase needs a name}"
  local cycle
  cycle="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("cycle",""))' "$RUN_FILE" 2>/dev/null || echo '')"
  run_write "$phase" "$cycle" ""
  printf 'phase: %s\n' "$phase"
}

run_clear() {
  rm -f "$RUN_FILE" 2>/dev/null || true
}

# ---------------------------------------------------------------------------
# Run lifecycle — running / completed / cancelled / crashed
# ---------------------------------------------------------------------------
#
# A RUN spans many cycles: one `/loop` session, or one daemon lifetime. It is
# the unit the dashboard lists and drills into, and the only one that can be
# CANCELLED — a cycle either finishes or is abandoned, but a person stops a run.

new_run_id() {
  printf 'r-%s-%s' "$(date -u +%Y%m%dT%H%M%SZ)" "$(od -An -N2 -tx1 /dev/urandom | tr -d ' \n')"
}

current_run_id() {
  [ -f "$RUN_FILE" ] || return 1
  python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("run_id",""))' "$RUN_FILE" 2>/dev/null
}

run_append() {
  python3 - "$RUNS" "$@" <<'PY' || true
import json, socket, sys, time
path = sys.argv[1]
fields = {}
for pair in sys.argv[2:]:
    k, _, v = pair.partition("=")
    fields[k] = v
rec = {
    "at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    "at_unix": int(time.time()),
    "host": socket.gethostname(),
}
for k, v in fields.items():
    if v.isdigit():
        rec[k] = int(v)
    else:
        rec[k] = v
with open(path, "a") as fh:
    fh.write(json.dumps(rec, sort_keys=True) + "\n")
print(json.dumps(rec))
PY
}

cmd_run() {
  ensure_state
  local verb="${1:-status}"; shift || true
  case "$verb" in
    start)
      local rid
      rid="$(new_run_id)"
      run_append "run_id=$rid" "status=running" "driver=${FULLAUTO_DRIVER:-interactive}" \
                 "slug=$(repo_slug)" "workspace_root=$REPO_ROOT" "pid=$$" >/dev/null
      python3 - "$RUN_FILE" "$rid" <<'PY' || true
import json, os, sys, time
path, rid = sys.argv[1], sys.argv[2]
try:
    doc = json.load(open(path))
except Exception:
    doc = {}
doc["run_id"] = rid
doc.setdefault("started_at", time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()))
tmp = path + ".tmp"
json.dump(doc, open(tmp, "w"), indent=2)
os.replace(tmp, path)
PY
      # Stamp the first heartbeat through the SAME writer every other phase
      # uses. Two writers producing different shapes is how a fresh run ends up
      # looking infinitely old to the abandonment check — the record must be
      # complete from its first byte, not completed by whatever happens next.
      run_write "idle" "" ""
      say "run $rid started"
      echo "FULLAUTO_RUN_ID=$rid"
      ;;
    end)
      local status="completed" reason="" rid
      while [ $# -gt 0 ]; do
        case "$1" in
          --status) status="${2:?}"; shift 2 ;;
          --reason) reason="${2:?}"; shift 2 ;;
          *) die "run end: unknown flag $1" ;;
        esac
      done
      rid="$(current_run_id || echo '')"
      [ -n "$rid" ] || die "run end: no run in progress"
      run_append "run_id=$rid" "status=$status" "reason=$reason" >/dev/null
      run_clear
      say "run $rid -> $status"
      ;;
    cancel)
      cmd_run end --status cancelled --reason "${1:-stopped by hand}"
      ;;
    list)  cmd_runs_report ;;
    status|"") cmd_runs_report ;;
    *) die "run: use start | end | cancel | list" ;;
  esac
}

# Fold runs.jsonl by run_id (last write wins) and resolve the one live status
# the file cannot state: a run whose last record says `running` but whose
# heartbeat has gone stale is CRASHED, not running. Only a reader can know that,
# because the process that would have written it is gone.
cmd_runs_report() {
  ensure_state
  python3 - "$RUNS" "$LEDGER" "$RUN_FILE" "$STALE_AFTER_SECS" <<'PY'
import json, os, sys, time
runs_path, ledger_path, run_path, stale = sys.argv[1:5]
stale = int(stale)

runs = {}
if os.path.exists(runs_path):
    for line in open(runs_path):
        line = line.strip()
        if not line:
            continue
        try:
            rec = json.loads(line)
        except Exception:
            continue
        rid = rec.get("run_id")
        if not rid:
            continue
        runs.setdefault(rid, {}).update(rec)

cycles = {}
if os.path.exists(ledger_path):
    for line in open(ledger_path):
        try:
            c = json.loads(line)
        except Exception:
            continue
        cycles.setdefault(c.get("run_id", "-"), []).append(c)

live = {}
if os.path.exists(run_path):
    try:
        live = json.load(open(run_path))
    except Exception:
        live = {}
now = int(time.time())

if not runs:
    print("no fullauto runs recorded yet")
    raise SystemExit(0)

print(f"{'run':<28} {'status':<10} {'cyc':>3} {'fixed':>5} {'filed':>5} {'new':>4}  phase")
for rid, r in sorted(runs.items(), key=lambda kv: kv[1].get("at_unix", 0), reverse=True):
    status = r.get("status", "?")
    cs = cycles.get(rid, [])
    fixed = sum(c.get("fixed", 0) for c in cs)
    filed = sum(c.get("filed", 0) for c in cs)
    new = sum(c.get("new_findings", 0) for c in cs)
    phase = ""
    if status == "running":
        if live.get("run_id") == rid:
            age = now - int(live.get("heartbeat_unix", 0) or 0)
            phase = f"{live.get('phase', '?')} ({age}s ago)"
            if age > stale:
                status = "crashed"
        else:
            # Last record says running, but nothing is holding the live pointer.
            status = "crashed"
    print(f"{rid:<28} {status:<10} {len(cs):>3} {fixed:>5} {filed:>5} {new:>4}  {phase}")
PY
}

# A run record left behind by a driver that died is banked as `crashed` the next
# time anything starts, so the history records the abandonment rather than the
# cycle silently disappearing.
reconcile_abandoned_run() {
  [ -f "$RUN_FILE" ] || return 0
  local rid
  rid="$(current_run_id || echo '')"
  [ -n "$rid" ] || { run_clear; return 0; }

  # A live run legitimately owns a run record — that is the whole point of it.
  # Only an ABANDONED one may be banked; treating the mere existence of the file
  # as abandonment destroys the run in progress.
  #
  # The witness is the HEARTBEAT, not the pid. Every subcommand here is its own
  # short-lived process, so the pid in the record is dead microseconds after it
  # is written — checking it would declare every interactive run crashed on its
  # very next command. A pid is only meaningful when a supervisor owns the run,
  # so it is consulted for `driver=daemon` and ignored otherwise.
  local age driver pid
  age="$(python3 -c 'import json,sys,time;print(int(time.time())-int(json.load(open(sys.argv[1])).get("heartbeat_unix",0) or 0))' "$RUN_FILE" 2>/dev/null || echo 999999)"
  driver="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("driver",""))' "$RUN_FILE" 2>/dev/null || echo '')"
  if [ "${age:-999999}" -lt "$STALE_AFTER_SECS" ]; then
    if [ "$driver" != "daemon" ]; then
      return 0
    fi
    pid="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("pid",0))' "$RUN_FILE" 2>/dev/null || echo 0)"
    if [ "${pid:-0}" -gt 0 ] && kill -0 "$pid" 2>/dev/null; then
      return 0
    fi
  fi

  run_append "run_id=$rid" "status=crashed" "reason=driver exited without ending the run" >/dev/null
  warn "previous run $rid was abandoned — banked as crashed"
  run_clear
}

# ---------------------------------------------------------------------------
# Resource probes — cross-platform, best-effort, never fatal
# ---------------------------------------------------------------------------
#
# Every probe returns a NUMBER on stdout and degrades to a conservative value
# rather than failing. A governor that dies because `pmset` is missing is worse
# than one that assumes mains power.

probe_cpu_total() {
  if have nproc; then nproc
  elif have sysctl; then sysctl -n hw.ncpu 2>/dev/null || echo 2
  else echo 2; fi
}

probe_load1() {
  # Integer part only; the governor compares magnitudes, not decimals.
  if [ -r /proc/loadavg ]; then cut -d' ' -f1 < /proc/loadavg | cut -d. -f1
  elif have uptime; then uptime | sed -E 's/.*load averages?: *//; s/[, ].*//' | cut -d. -f1
  else echo 0; fi
}

probe_mem_total_gb() {
  if [ -r /proc/meminfo ]; then
    awk '/^MemTotal:/ {printf "%d", $2/1024/1024}' /proc/meminfo
  elif have sysctl; then
    sysctl -n hw.memsize 2>/dev/null | awk '{printf "%d", $1/1024/1024/1024}' || echo 8
  else echo 8; fi
}

probe_mem_free_gb() {
  if [ -r /proc/meminfo ]; then
    awk '/^MemAvailable:/ {printf "%d", $2/1024/1024}' /proc/meminfo
  elif have vm_stat; then
    # macOS has no MemAvailable. Free + inactive + speculative is the closest
    # honest analogue: pages the kernel can hand out without swapping.
    vm_stat 2>/dev/null | awk '
      /page size of/ { gsub(/[^0-9]/, "", $8); ps = $8 }
      /Pages free/ || /Pages inactive/ || /Pages speculative/ {
        gsub(/[^0-9]/, "", $NF); pages += $NF
      }
      END { printf "%d", (ps ? ps : 4096) * pages / 1024 / 1024 / 1024 }'
  else echo 2; fi
}

probe_disk_free_gb() {
  # -P forces POSIX single-line output; without it a long device name wraps and
  # the field offsets silently shift.
  df -Pk "${1:-$REPO_ROOT}" 2>/dev/null | awk 'NR==2 {printf "%d", $4/1024/1024}' || echo 0
}

probe_on_battery() {
  if have pmset; then
    pmset -g batt 2>/dev/null | grep -q "AC Power" && echo 0 || echo 1
  else echo 0; fi
}

# Is something already using this box? A 16 GiB machine cannot host a benchmark
# match AND a workspace build; the match owns the box, and a concurrent cargo
# build turns a measured run into garbage.
#
# Match the WORKLOAD, never the watchers. `tail -f` on a match log, and a shell
# polling for the string "cargo build", both MENTION the work without doing any
# of it — and they outlive the run. Counting them pins the loop to the light
# tier forever after a single benchmark ends, which is a worse failure than not
# detecting contention at all: it is permanent, silent, and looks like caution.
probe_contention() {
  have pgrep || { echo 0; return 0; }
  local hits
  hits="$(pgrep -fl 'arenabench|harbor|cargo|docker' 2>/dev/null | awk '
    /(^|[ \/])(tail|grep|less|watch|pgrep|ps|awk|sed|tee)([ ]|$)/ { next }
    /(^|\/)(sh|bash|zsh)[ ]+-[a-z]*c([ ]|$)/                      { next }
    /arenabench[ ]+(run|serve)|harbor[ ]+run|cargo[ ]+(build|test|clippy|zigbuild|install)|docker[ ]+(build|compose)/ { n++ }
    END { print n + 0 }')"
  # A running container is real work even when no host process names it — the
  # benchmark drives Docker, and on a Linux rig nothing on the host says so.
  #
  # But EXISTENCE is not activity. A finished match routinely leaves its
  # recorder sidecars up for hours (measured: five `arenabench/recorder`
  # containers still up 8 hours after the match ended). Counting those sheds the
  # loop's compute forever for work that finished last night — the same
  # permanent, silent, looks-like-caution failure as counting a `tail -f`
  # watcher. Ask what the containers are DOING, not whether they exist.
  if [ "${hits:-0}" -eq 0 ] && have docker; then
    local busy_containers
    busy_containers="$(docker stats --no-stream --format '{{.CPUPerc}}' 2>/dev/null \
      | tr -d '%' | awk '$1 + 0 > 20 { n++ } END { print n + 0 }')"
    if [ "${busy_containers:-0}" -gt 0 ]; then hits=1; fi
  fi
  if [ "${hits:-0}" -gt 0 ]; then echo 1; else echo 0; fi
}

# Is the headless scope-review bypass on in any settings scope this run will
# see? Project wins per field, so project is checked first — but for a boolean
# gate like this, ON anywhere is what matters to the question being asked.
scope_bypass_on() {
  local f
  for f in "$REPO_ROOT/.stella/settings.json" "$(stella_home)/settings.json"; do
    [ -f "$f" ] || continue
    if python3 -c '
import json, sys
try:
    cfg = json.load(open(sys.argv[1])).get("agent_engine_config") or {}
except Exception:
    raise SystemExit(1)
raise SystemExit(0 if str(cfg.get("headless_scope_bypass", "")).lower() == "on" else 1)
' "$f" 2>/dev/null; then
      return 0
    fi
  done
  return 1
}

json_get() {
  python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get(sys.argv[2], sys.argv[3]))' \
    "$CALIBRATION" "$1" "$2" 2>/dev/null || echo "$2"
}

# ---------------------------------------------------------------------------
# plan — size this cycle to the machine it is actually running on
# ---------------------------------------------------------------------------
#
# Supply (what the box has right now) x demand (what the queue needs) x
# calibration (what previous cycles proved this box can survive) -> a tier and
# a concrete set of knobs. Emitted as shell assignments so the caller cannot
# mis-transcribe them; `--explain` shows the reasoning instead.

cmd_plan() {
  ensure_state
  local explain=0
  [ "${1:-}" = "--explain" ] && explain=1

  local cpu load mem_total mem_free disk battery busy
  cpu="$(probe_cpu_total)";        load="$(probe_load1)"
  mem_total="$(probe_mem_total_gb)"; mem_free="$(probe_mem_free_gb)"
  disk="$(probe_disk_free_gb)";    battery="$(probe_on_battery)"
  busy="$(probe_contention)"

  local batch_ceil parallel_ceil
  batch_ceil="$(json_get batch_ceiling "$BATCH_HARD_MAX")"
  parallel_ceil="$(json_get parallel_ceiling 2)"

  # Demand: how much work is actually waiting, and how urgent.
  local q_total=0 q_p0=0
  if have gh; then
    q_total="$(gh_plain issue list --state open --label bug --json number --jq 'length' 2>/dev/null || echo 0)"
    q_p0="$(gh_plain issue list --state open --label bug --label P0 --json number --jq 'length' 2>/dev/null || echo 0)"
  fi

  # --- The tier ladder. Each rung states WHY, because a governor whose
  # --- decisions cannot be explained gets overridden and then ignored.
  local tier="normal" reason=""
  local local_build=1 bench="loop" audit="deep"
  local batch="$batch_ceil" parallel="$parallel_ceil"
  local scope="impacted"

  if [ "$disk" -lt "$DISK_FLOOR_GB" ]; then
    tier="light"; local_build=0; scope="ci"; bench="off"
    reason="disk ${disk}GB < ${DISK_FLOOR_GB}GB floor — a workspace build needs 10-20GB and would die as a killed compiler"
  elif [ "$busy" -eq 1 ]; then
    tier="light"; local_build=0; scope="ci"; bench="off"; parallel=1
    reason="another build or benchmark owns this box — a concurrent cargo run corrupts a measured match"
  elif [ "$mem_free" -lt "$MEM_FLOOR_GB" ]; then
    tier="light"; local_build=0; scope="ci"; bench="off"; parallel=1
    reason="only ${mem_free}GB free of ${mem_total}GB — a workspace build here swap-thrashes"
  elif [ "$battery" -eq 1 ]; then
    tier="light"; parallel=1; bench="off"; scope="impacted"
    reason="on battery — shed compute, keep the cycle useful"
  elif [ "$load" -ge "$cpu" ]; then
    tier="light"; parallel=1
    reason="load ${load} at or above ${cpu} cores — the box is already saturated"
  elif [ "$mem_total" -ge 32 ] && [ "$disk" -ge 100 ] && [ "$load" -lt $((cpu / 2)) ]; then
    tier="heavy"; scope="workspace"; bench="h2h"
    reason="${mem_total}GB / ${cpu} cores / ${disk}GB free and idle — this box can host the full workspace and a measured match"
  else
    reason="${mem_total}GB / ${cpu} cores / ${disk}GB free — ordinary cycle at the calibrated ceiling"
  fi

  # Demand overrides supply in exactly one direction: a P0 is worth a degraded
  # cycle, never a skipped one. It shrinks the batch to what will fit rather
  # than standing down.
  if [ "$q_p0" -gt 0 ] && [ "$tier" = "light" ]; then
    batch="$q_p0"; audit="fast"
    reason="$reason; but ${q_p0} P0 open — running a minimal cycle anyway"
  fi

  # An empty queue does not deserve a big batch, whatever the box can afford.
  [ "$q_total" -gt 0 ] && [ "$batch" -gt "$q_total" ] && batch="$q_total"
  [ "$batch" -lt 1 ] && batch=1
  case "$tier" in
    light) [ "$batch" -gt 5 ] && batch=5 ;;
  esac

  if [ "$explain" -eq 1 ]; then
    say "fullauto plan — tier $tier"
    printf '  %s\n\n' "$reason"
    printf '  supply    %s cores (load %s) · %sGB RAM (%sGB free) · %sGB disk · battery=%s · busy=%s\n' \
      "$cpu" "$load" "$mem_total" "$mem_free" "$disk" "$battery" "$busy"
    printf '  demand    %s open defects (%s P0)\n' "$q_total" "$q_p0"
    printf '  ceilings  batch<=%s parallel<=%s (AIMD, %s clean runs)\n\n' \
      "$batch_ceil" "$parallel_ceil" "$(json_get clean_run 0)"
    printf '  batch     %s defects\n' "$batch"
    printf '  parallel  %s worktree(s)\n' "$parallel"
    printf '  build     %s\n' "$([ "$local_build" -eq 1 ] && echo "local, scope=$scope" || echo "PUSH TO CI — do not compile here")"
    printf '  audit     %s (aperture: %s)\n' "$audit" "$(cat "$APERTURE_FILE")"
    printf '  bench     %s\n' "$bench"
    return 0
  fi

  cat <<EOF
FULLAUTO_TIER=$tier
FULLAUTO_BATCH=$batch
FULLAUTO_PARALLEL=$parallel
FULLAUTO_LOCAL_BUILD=$local_build
FULLAUTO_SCOPE=$scope
FULLAUTO_AUDIT=$audit
FULLAUTO_BENCH=$bench
FULLAUTO_APERTURE=$(cat "$APERTURE_FILE")
FULLAUTO_CPU=$cpu
FULLAUTO_MEM_FREE_GB=$mem_free
FULLAUTO_DISK_FREE_GB=$disk
FULLAUTO_QUEUE=$q_total
FULLAUTO_QUEUE_P0=$q_p0
FULLAUTO_PLAN_REASON="$reason"
EOF
}

# ---------------------------------------------------------------------------
# calibrate — the controller that makes the loop self-optimizing
# ---------------------------------------------------------------------------
#
# AIMD. A cycle that completed inside its resources raises the ceiling by a
# little; one that was killed, thrashed, or ran out of disk halves it. This is
# the ONLY place the ceilings move, and it moves them from evidence — which is
# what "always optimizing itself to its constraints" means in practice.

cmd_calibrate() {
  ensure_state
  local outcome=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --ok)            outcome="ok"; shift ;;
      --resource-fail) outcome="fail"; shift ;;
      --show)          cat "$CALIBRATION"; return 0 ;;
      *) die "calibrate: use --ok | --resource-fail | --show" ;;
    esac
  done
  [ -n "$outcome" ] || die "calibrate: use --ok | --resource-fail | --show"

  python3 - "$CALIBRATION" "$outcome" "$BATCH_HARD_MAX" "$BATCH_HARD_MIN" "$PARALLEL_HARD_MAX" <<'PY'
import json, sys
path, outcome, hard_max, hard_min, par_max = sys.argv[1:6]
hard_max, hard_min, par_max = int(hard_max), int(hard_min), int(par_max)
c = json.load(open(path))
batch = int(c.get("batch_ceiling", hard_max))
par = int(c.get("parallel_ceiling", 2))
clean = int(c.get("clean_run", 0))

if outcome == "ok":
    clean += 1
    # Additive increase: +2 batch per clean cycle, and one more worktree only
    # after three consecutive clean cycles. Parallelism is the knob that hurts
    # most when it is wrong, so it earns its increase more slowly.
    batch = min(hard_max, batch + 2)
    if clean % 3 == 0:
        par = min(par_max, par + 1)
    note = f"clean run {clean}"
else:
    # Multiplicative decrease: halve, and drop straight back to serial. A
    # resource failure is not a hint, it is proof the last plan did not fit.
    clean = 0
    batch = max(hard_min, batch // 2)
    par = 1
    note = "resource failure — halved"

# Mutate in place. Rebuilding the dict from the four keys this function owns
# silently discards every key it does not — `last_clean_head` among them, which
# would leave watch mode unable to tell "nothing changed" from "never looked"
# and so unable to ever sleep.
c.update({"batch_ceiling": batch, "parallel_ceiling": par,
          "clean_run": clean, "note": note})
json.dump(c, open(path, "w"), indent=2, ensure_ascii=False)
print(json.dumps(c, ensure_ascii=False))
PY
}

# ---------------------------------------------------------------------------
# aperture — why this loop never finishes
# ---------------------------------------------------------------------------
#
# "No more defects" is always a statement about the LENS, never about the code.
# When a lens goes dry the next one opens; when the list is exhausted the loop
# drops to watch mode and wakes on a trigger. It never exits.

cmd_aperture() {
  ensure_state
  local current
  current="$(cat "$APERTURE_FILE")"
  case "${1:-}" in
    --current) echo "$current" ;;
    --advance)
      local next="" found=0 a
      for a in $APERTURES; do
        if [ "$found" -eq 1 ]; then next="$a"; break; fi
        [ "$a" = "$current" ] && found=1
      done
      if [ -n "$next" ]; then
        echo "$next" > "$APERTURE_FILE"
        printf 'aperture %s -> %s\n' "$current" "$next"
      else
        echo "watch" > "$APERTURE_FILE"
        printf 'aperture %s -> watch (every lens dry; the loop sleeps, it does not stop)\n' "$current"
      fi
      ;;
    --reset) echo "rubric" > "$APERTURE_FILE"; echo "aperture reset to rubric" ;;
    --list)
      for a in $APERTURES watch; do
        if [ "$a" = "$current" ]; then printf '  %s* %s%s\n' "$C_B" "$a" "$C_0"
        else printf '    %s\n' "$a"; fi
      done
      ;;
    *) die "aperture: use --current | --advance | --reset | --list" ;;
  esac
}

# ---------------------------------------------------------------------------
# watch — the low-duty-cycle mode that makes "forever" affordable
# ---------------------------------------------------------------------------
#
# Every lens dry does not mean the work is over; it means nothing has CHANGED
# since the last look. So stop paying for full audits and start watching for
# the things that invalidate that conclusion. Any trigger reopens the aperture
# at `rubric` and the loop resumes at full duty.

cmd_watch() {
  ensure_state
  local triggered=0
  say "watch mode — checking what would invalidate the last clean sweep"

  local head_now head_seen
  head_now="$(git -C "$REPO_ROOT" rev-parse origin/main 2>/dev/null || echo none)"
  head_seen="$(json_get last_clean_head none)"
  if [ "$head_now" != "$head_seen" ]; then
    warn "main moved ($head_seen -> $head_now) — new code has never been audited"
    triggered=1
  else
    pass "main unchanged since the last clean sweep"
  fi

  if have gh; then
    local newq
    newq="$(gh_plain issue list --state open --label bug --json number --jq 'length' 2>/dev/null || echo 0)"
    if [ "$newq" -gt 0 ]; then warn "$newq open defects in the queue"; triggered=1
    else pass "defect queue empty"; fi

    local ci
    ci="$(gh_plain run list --branch main --limit 1 --json conclusion --jq '.[0].conclusion' 2>/dev/null || echo unknown)"
    if [ "$ci" = "failure" ]; then warn "main is red in CI"; triggered=1
    else pass "main CI: $ci"; fi
  fi

  echo
  if [ "$triggered" -eq 1 ]; then
    cmd_aperture --reset >/dev/null
    printf '%sWAKE%s — aperture reset to rubric; run a full cycle.\n' "$C_G" "$C_0"
    return 0
  fi
  printf '%sSLEEP%s — nothing changed. Check again next interval; spend nothing.\n' "$C_Y" "$C_0"
  return 1
}

# ---------------------------------------------------------------------------
# metrics — the loop's evidence about ITSELF
# ---------------------------------------------------------------------------
#
# The ledger is not just a log, it is the input to `/fullauto:evolve`. These
# are the numbers a meta-cycle argues from: yield per cycle, how much of the
# work was wasted, whether the controller is stuck.

cmd_metrics() {
  ensure_state
  python3 - "$LEDGER" "$CALIBRATION" <<'PY'
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
cal = json.load(open(sys.argv[2]))
if not rows:
    print("no cycles recorded yet"); raise SystemExit(0)

n = len(rows)
fixed = sum(r.get("fixed", 0) for r in rows)
filed = sum(r.get("filed", 0) for r in rows)
new = sum(r.get("new_findings", 0) for r in rows)
zero_fix = sum(1 for r in rows if r.get("fixed", 0) == 0)
red = sum(1 for r in rows if r.get("gate") == "red")
recent = rows[-10:]

print(f"cycles            {n}")
print(f"fixed / cycle     {fixed / n:.1f}   ({fixed} total)")
print(f"filed / cycle     {filed / n:.1f}   ({filed} total)")
print(f"discovery / cycle {new / n:.1f}   (unseen findings)")
print(f"zero-fix cycles   {zero_fix} ({100 * zero_fix / n:.0f}%)")
print(f"red-gate cycles   {red} ({100 * red / n:.0f}%)")
print(f"controller        batch<={cal.get('batch_ceiling')} "
      f"parallel<={cal.get('parallel_ceiling')} "
      f"({cal.get('clean_run', 0)} clean runs) — {cal.get('note', '')}")

# The signals a meta-cycle acts on. Each names a SPECIFIC pathology rather
# than a vague "performance is bad", because the fix differs completely.
flags = []
if zero_fix >= 3 and all(r.get("fixed", 0) == 0 for r in rows[-3:]):
    flags.append("STUCK: 3 consecutive zero-fix cycles — the blocker is structural, not the queue")
if cal.get("batch_ceiling", 20) <= 4 and cal.get("clean_run", 0) >= 5:
    flags.append("STARVED: ceiling stayed low through 5 clean runs — the decrease never decayed")
if n >= 5 and new / n < 0.5 and filed / n > 2:
    flags.append("NOISY: filing far more than it discovers — dedup is probably leaking")
if red >= max(2, n // 3):
    flags.append("FRAGILE: a third of cycles end on a red gate — batches are too large or too mixed")
if flags:
    print("\nsignals for /fullauto:evolve")
    for f in flags:
        print(f"  ! {f}")
PY
}

# ---------------------------------------------------------------------------
# preflight — can this machine run a cycle at all?
# ---------------------------------------------------------------------------
#
# Split into TIERS on purpose. A missing `harbor` blocks the benchmark arm and
# nothing else; a missing `gh` blocks ticket filing, which is the one thing the
# loop must never skip silently. Reporting them as one undifferentiated list
# would make the loop stop for things it could have worked around.

cmd_preflight() {
  local strict=0
  [ "${1:-}" = "--strict" ] && strict=1
  ensure_state

  say "1. Repository"
  if [ -d "$REPO_ROOT/.git" ] || git -C "$REPO_ROOT" rev-parse --git-dir >/dev/null 2>&1; then
    pass "repo $REPO_ROOT @ $(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo '?')"
  else
    fail "not a git repository"
  fi
  if [ -f "$REPO_ROOT/AGENTS.md" ] && [ -f "$REPO_ROOT/Makefile" ]; then
    pass "AGENTS.md + Makefile present (this is the stella workspace)"
  else
    fail "AGENTS.md or Makefile missing — fullauto is scoped to the stella workspace"
  fi

  say "2. Fix + gate tier (required)"
  for t in git cargo make rg; do
    if have "$t"; then pass "$t"; else fail "$t is required to run the gate"; fi
  done
  if have gh && gh auth status >/dev/null 2>&1; then
    pass "gh authenticated as $(gh_plain api user --jq .login 2>/dev/null || echo '?')"
  else
    fail "gh is not authenticated — the loop cannot file the tickets it cannot fix"
  fi

  say "3. Agent tier (which engine does the fixing)"
  if have stella; then
    pass "stella on PATH -> $(command -v stella)"
    if [ "$(command -v stella)" = "$REPO_ROOT/target/release/stella" ]; then
      warn "that is the LOCAL dev build shadowing any release (see \`upgrade\`)"
    fi
    # Measured, not theorised: the first live daemon cycle died here. A cycle
    # of any real size trips scope review, and a headless run has nobody to
    # ask — so without the bypass EVERY daemon cycle burns its startup cost and
    # then aborts. Better to refuse the cycle than to discover it in the log.
    if scope_bypass_on; then
      pass "headless_scope_bypass: on — a headless cycle can pass scope review"
    else
      fail "headless_scope_bypass is OFF — a headless cycle aborts at scope review.
     Set it where the working tree is disposable:
       .stella/settings.json -> {\"agent_engine_config\": {\"headless_scope_bypass\": \"on\"}}"
    fi
  else
    warn "stella not on PATH — the fix batch falls back to the driving agent"
  fi

  say "4. Benchmark tier (optional per cycle)"
  if have arenabench; then pass "arenabench $(arenabench --version 2>/dev/null || echo '?')"
  else warn "arenabench absent — \`bench h2h\` unavailable (pip install arenabench)"; fi
  if have harbor; then pass "harbor $(harbor --version 2>/dev/null || echo '?')"
  else warn "harbor absent — the head-to-head cannot run locally"; fi
  if docker info >/dev/null 2>&1; then pass "docker reachable"
  else warn "docker unreachable — head-to-head must go to the rig"; fi
  if [ "$(uname -m)" = "x86_64" ] && [ "$(uname -s)" = "Linux" ]; then
    pass "native x86_64 Linux host — claim-eligible for a measured run"
  else
    warn "$(uname -s)/$(uname -m): NOT claim-eligible (RUNBOOK Part 0). Use --rig."
  fi
  for k in OPENROUTER_API_KEY CLAUDE_CODE_OAUTH_TOKEN; do
    if [ -n "${!k:-}" ]; then pass "$k present"; else warn "$k unset — that arm scores zero, which reads as a real loss"; fi
  done

  say "5. Ship tier (optional per cycle)"
  if have brew; then
    pass "brew $(brew --version 2>/dev/null | head -1)"
    if brew tap 2>/dev/null | grep -qx "$BREW_TAP"; then pass "tap $BREW_TAP present"
    else warn "tap $BREW_TAP missing — \`upgrade\` will add it"; fi
  else
    warn "brew absent — \`upgrade\` unavailable"
  fi

  say "6. State"
  pass "state dir $STATE_DIR"
  pass "cycle $(cat "$CYCLE_FILE") · $(wc -l < "$SEEN" | tr -d ' ') findings seen · $(wc -l < "$LEDGER" | tr -d ' ') cycles logged"

  echo
  if [ "$HARD_FAIL" -ne 0 ]; then
    printf '%sNOT READY%s — a required tier failed above.\n' "$C_R" "$C_0"
    [ "$strict" -eq 1 ] && exit 1
    return 1
  fi
  printf '%sREADY%s — required tiers green; warnings above only narrow the cycle.\n' "$C_G" "$C_0"
  return 0
}

# ---------------------------------------------------------------------------
# queue — the ranked defect queue
# ---------------------------------------------------------------------------
#
# Ranked P0 > P1 > P2 > unlabelled, oldest first inside a rank, because an
# aged P1 is a worse thing to be carrying than a fresh one. `bug` and `triage`
# both count: an untriaged issue is a defect nobody has classified yet, not a
# non-defect. Feature work is deliberately excluded — this loop closes defects.

cmd_queue() {
  ensure_state
  local limit=20 as_json=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --limit) limit="${2:?--limit needs a number}"; shift 2 ;;
      --json)  as_json=1; shift ;;
      *) die "queue: unknown flag $1" ;;
    esac
  done
  have gh || die "queue needs gh"

  # One API call, ranked locally: `gh issue list` cannot sort by a label.
  # The payload goes via a file rather than argv or a pipe: a heredoc already
  # owns stdin, and 200 issues is too much to push through an argument list.
  local raw="$STATE_DIR/.queue.json"
  gh_plain issue list --state open --limit 200 \
    --json number,title,labels,createdAt,url > "$raw" 2>/dev/null \
    || die "gh issue list failed"

  python3 - "$raw" "$limit" "$as_json" <<'PY'
import json, sys
path, limit, as_json = sys.argv[1], int(sys.argv[2]), sys.argv[3] == "1"
issues = json.load(open(path))
RANK = {"P0": 0, "P1": 1, "P2": 2}

def names_of(i):
    return {l["name"] for l in i["labels"]}

def sort_key(i):
    prio = min((RANK[n] for n in names_of(i) if n in RANK), default=3)
    return (prio, i["createdAt"])

# `triage` counts as a defect: an unclassified issue is a defect nobody has
# labelled yet, not a non-defect. Feature work is excluded — this loop closes
# defects, and mixing the two makes the batch unreviewable.
defects = sorted((i for i in issues if names_of(i) & {"bug", "triage"}), key=sort_key)
picked = defects[:limit]

if as_json:
    json.dump(picked, sys.stdout, indent=2)
else:
    for i in picked:
        names = sorted(names_of(i))
        prio = next((n for n in names if n in RANK), "--")
        area = next((n for n in names if n.startswith("area:")), "")
        num, title = i["number"], i["title"]
        print(f"{prio:>2}  #{num:<6} {area:<18} {title}")
    print(f"\n{len(picked)} of {len(defects)} open defects "
          f"({len(issues)} open issues total)", file=sys.stderr)
PY
}

# ---------------------------------------------------------------------------
# cycle-begin / cycle-end / state — the ledger
# ---------------------------------------------------------------------------
#
# The loop driver is re-invoked with a fresh context every cycle, so anything
# cycle N+1 needs to know about cycle N has to be on disk. `cycle-begin` emits
# shell-evalable assignments precisely so the caller cannot mis-transcribe it.

cmd_cycle_begin() {
  ensure_state
  local n
  n=$(( $(cat "$CYCLE_FILE") + 1 ))
  echo "$n" > "$CYCLE_FILE"
  # A previous run record still present here means the last cycle never reached
  # cycle-end — the terminal quit, the machine slept, or the driver crashed.
  # Bank it before overwriting, so the ledger records the abandonment instead of
  # the cycle just vanishing from the history.
  reconcile_abandoned_run
  run_write "starting" "$n" ""
  local streak
  streak="$(cmd_state --dry-streak)"
  cat <<EOF
FULLAUTO_CYCLE=$n
FULLAUTO_DRY_STREAK_SO_FAR=$streak
FULLAUTO_DRY_STREAK_TARGET=$DRY_STREAK_TARGET
FULLAUTO_STATE_DIR=$STATE_DIR
FULLAUTO_SEEN_COUNT=$(wc -l < "$SEEN" | tr -d ' ')
FULLAUTO_STARTED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
FULLAUTO_BASE_SHA=$(git -C "$REPO_ROOT" rev-parse HEAD)
EOF
}

cmd_cycle_end() {
  ensure_state
  local cycle="" fixed=0 filed=0 new_findings=0 bench="skipped" gate="unknown" prs=""
  local tier="unknown" aperture="" outcome="ok" minutes=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --cycle)    cycle="${2:?}"; shift 2 ;;
      --fixed)    fixed="${2:?}"; shift 2 ;;
      --filed)    filed="${2:?}"; shift 2 ;;
      --new)      new_findings="${2:?}"; shift 2 ;;
      --bench)    bench="${2:?}"; shift 2 ;;
      --gate)     gate="${2:?}"; shift 2 ;;
      --prs)      prs="${2:?}"; shift 2 ;;
      --tier)     tier="${2:?}"; shift 2 ;;
      --minutes)  minutes="${2:?}"; shift 2 ;;
      # `ok` vs `resource-fail` is the ONLY input to the controller. A red gate
      # is not a resource failure — the batch was wrong, not too big. Killed
      # compiler, OOM, ENOSPC, thrash: those are resource failures.
      --outcome)  outcome="${2:?}"; shift 2 ;;
      *) die "cycle-end: unknown flag $1" ;;
    esac
  done
  [ -n "$cycle" ] || die "cycle-end needs --cycle N"
  aperture="$(cat "$APERTURE_FILE")"
  local run_id
  run_id="$(current_run_id || echo '-')"

  python3 - "$LEDGER" "$cycle" "$fixed" "$filed" "$new_findings" "$bench" "$gate" \
            "$prs" "$tier" "$aperture" "$outcome" "$minutes" "$run_id" <<'PY'
import json, sys, time
(ledger, cycle, fixed, filed, new, bench, gate,
 prs, tier, aperture, outcome, minutes, run_id) = sys.argv[1:14]
rec = {
    "cycle": int(cycle),
    "run_id": run_id,
    "ended_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    "ended_at_unix": int(time.time()),
    "fixed": int(fixed),
    "filed": int(filed),
    "new_findings": int(new),
    "bench": bench,
    "gate": gate,
    "prs": [p for p in prs.split(",") if p],
    "tier": tier,
    "aperture": aperture,
    "outcome": outcome,
    "minutes": int(minutes),
    # A cycle is DRY when the audit found nothing that was not already known.
    # Fixing things does not make a cycle dry; discovering nothing does.
    "dry": int(new) == 0,
}
with open(ledger, "a") as fh:
    fh.write(json.dumps(rec, sort_keys=True) + "\n")
print(json.dumps(rec, indent=2))
PY

  # Close the control loop. These two are the difference between a log and a
  # system that gets better: the controller learns what this box survives, and
  # the aperture widens the moment a lens stops producing.
  case "$outcome" in
    ok)            cmd_calibrate --ok >/dev/null ;;
    resource-fail) cmd_calibrate --resource-fail >/dev/null ;;
  esac

  # The CYCLE ended, not the run — a run spans many cycles, and only `run end`
  # or `run cancel` closes one. Clearing the live record here would leave
  # runs.jsonl saying `running` with nothing holding the pointer, which every
  # reader correctly interprets as a crash.
  run_write "idle" "" "$tier"

  local streak
  streak="$(cmd_state --dry-streak)"
  if [ "$streak" -ge "$DRY_STREAK_TARGET" ]; then
    say "aperture $aperture is dry after $streak cycles"
    cmd_aperture --advance
    # Remember the commit the sweep was clean AT, so watch mode can tell
    # "nothing changed" from "nobody has looked yet".
    python3 - "$CALIBRATION" "$(git -C "$REPO_ROOT" rev-parse origin/main 2>/dev/null || echo none)" <<'PY'
import json, sys
path, head = sys.argv[1], sys.argv[2]
c = json.load(open(path)); c["last_clean_head"] = head
json.dump(c, open(path, "w"), indent=2)
PY
  fi
}

cmd_state() {
  ensure_state
  case "${1:-}" in
    --dry-streak)
      python3 - "$LEDGER" <<'PY'
import json, sys
streak = 0
for line in reversed(open(sys.argv[1]).read().splitlines()):
    if not line.strip():
        continue
    if json.loads(line).get("dry"):
        streak += 1
    else:
        break
print(streak)
PY
      ;;
    --json) cat "$LEDGER" ;;
    *)
      say "fullauto state — $STATE_DIR"
      printf '  cycle           %s\n' "$(cat "$CYCLE_FILE")"
      printf '  dry streak      %s / %s\n' "$(cmd_state --dry-streak)" "$DRY_STREAK_TARGET"
      printf '  findings seen   %s\n' "$(wc -l < "$SEEN" | tr -d ' ')"
      # `grep -c` exits 1 on no match, so a `|| echo 0` fallback would append a
      # SECOND zero to the captured output rather than replacing it. wc -l is
      # the honest count of a newline-terminated jsonl file.
      printf '  cycles logged   %s\n' "$(wc -l < "$LEDGER" | tr -d ' ')"
      echo
      if [ -s "$LEDGER" ]; then
        say "last 5 cycles"
        python3 - "$LEDGER" <<'PY'
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
for r in rows[-5:]:
    flag = "DRY" if r.get("dry") else "   "
    cyc, fixed, filed = r.get("cycle", 0), r.get("fixed", 0), r.get("filed", 0)
    new, gate = r.get("new_findings", 0), r.get("gate", "?")
    tier, ap = r.get("tier", "?"), r.get("aperture", "?")
    print(f"  #{cyc:<3} {flag}  fixed {fixed:>2}  filed {filed:>2}  new {new:>2}  "
          f"gate {gate:<7} {tier:<7} {ap}")
PY
      fi
      ;;
  esac
}

# ---------------------------------------------------------------------------
# seen — the dedup set that makes "until no more defects" terminate
# ---------------------------------------------------------------------------
#
# Findings are deduped by DIGEST, not by issue number, so a finding that the
# audit re-derives in a later cycle does not count as new and does not reset
# the dry streak. Deduping against "issues I filed" instead would let a
# rejected or closed finding reappear every cycle and the loop would never
# converge.

cmd_seen() {
  ensure_state
  case "${1:-}" in
    --new)
      shift
      local d
      for d in "$@"; do
        grep -qxF "$d" "$SEEN" || echo "$d"
      done
      ;;
    --add)
      shift
      local d added=0
      for d in "$@"; do
        if ! grep -qxF "$d" "$SEEN"; then echo "$d" >> "$SEEN"; added=$((added + 1)); fi
      done
      echo "$added"
      ;;
    --digest)
      shift
      # Normalize before hashing: lowercase, collapse whitespace, drop line
      # numbers. The same defect described twice with a shifted line number is
      # one finding, and a digest that disagrees would break the oracle.
      printf '%s' "$*" \
        | tr '[:upper:]' '[:lower:]' \
        | sed -E 's/:[0-9]+/:L/g; s/[[:space:]]+/ /g; s/^ //; s/ $//' \
        | shasum -a 256 | cut -c1-16
      ;;
    --count) wc -l < "$SEEN" | tr -d ' ' ;;
    *) die "seen: use --new | --add | --digest | --count" ;;
  esac
}

# ---------------------------------------------------------------------------
# daemon — the loop outlives the terminal that started it
# ---------------------------------------------------------------------------
#
# `/loop /fullauto` dies with the terminal app. The state was always durable —
# it is files on disk — but the DRIVER was not, so quitting the terminal ended
# the loop with no record of why. The daemon detaches the driver from the tty:
# nohup so SIGHUP cannot reach it, a pidfile so it can be found again, and a
# run record so a kill -9 still leaves evidence.
#
# `daemon install` goes one step further and hands the job to launchd, which is
# what survives a logout or a reboot rather than merely a closed window.

daemon_alive() {
  [ -f "$DAEMON_PID" ] || return 1
  local pid
  pid="$(cat "$DAEMON_PID" 2>/dev/null)"
  [ -n "$pid" ] || return 1
  kill -0 "$pid" 2>/dev/null
}

# The command that runs ONE cycle. Defaults to handing the cycle prompt to
# Stella itself — this loop is Stella making Stella better, so the daemon
# dogfoods by default and only falls back when asked to.
cycle_command() {
  if [ -n "${FULLAUTO_CYCLE_CMD:-}" ]; then
    printf '%s' "$FULLAUTO_CYCLE_CMD"
    return 0
  fi
  # `stella run` takes its prompt positionally or on stdin, and reads stdin when
  # piped — which is the right channel for a prompt this long: an argv-borne
  # cycle prompt would be visible in `ps` and can hit ARG_MAX.
  #
  # The budget is per-cycle and enforced by the engine, aborting only at a safe
  # boundary between model calls (invariant 6) — never mid-tool.
  printf 'cat %s/scripts/fullauto/commands/fullauto.md | stella run --budget %s' \
    "$REPO_ROOT" "${FULLAUTO_BUDGET_USD:-10}"
}

cmd_daemon() {
  ensure_state
  local verb="${1:-status}"; shift || true
  case "$verb" in
    start)     daemon_start "$@" ;;
    stop)      daemon_stop "$@" ;;
    status)    daemon_status ;;
    logs)      tail -n "${1:-40}" "$DAEMON_LOG" 2>/dev/null || echo "(no log yet)" ;;
    loop)      daemon_loop "$@" ;;
    install)   daemon_install ;;
    uninstall) daemon_uninstall ;;
    *) die "daemon: use start | stop | status | logs | install | uninstall" ;;
  esac
}

daemon_start() {
  local interval="${1:-2700}"
  if daemon_alive; then
    warn "daemon already running (pid $(cat "$DAEMON_PID"))"
    return 0
  fi
  have stella || warn "stella is not on PATH — set FULLAUTO_CYCLE_CMD or the daemon will idle"

  # nohup detaches from the terminal's SIGHUP; the background `&` detaches from
  # job control. Both are needed: job-control stop alone would take the process
  # down with the shell that spawned it.
  nohup "$0" daemon loop "$interval" "${2:-}" >> "$DAEMON_LOG" 2>&1 &
  local pid=$!
  echo "$pid" > "$DAEMON_PID"
  disown "$pid" 2>/dev/null || true
  say "daemon started (pid $pid, interval ${interval}s)"
  pass "log: $DAEMON_LOG"
  pass "it now survives this terminal closing; \`daemon install\` also survives logout"
}

daemon_loop() {
  local interval="${1:-2700}" once="${2:-}"
  export FULLAUTO_DRIVER=daemon
  cmd_run start >/dev/null
  # Ending the run on the way out is what makes a `daemon stop` read as
  # `cancelled` rather than `crashed` — the distinction the dashboard shows.
  trap 'cmd_run end --status cancelled --reason "daemon stopped" >/dev/null 2>&1; exit 0' TERM INT
  local cmd
  cmd="$(cycle_command)"
  while true; do
    printf '\n=== %s :: cycle start ===\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    run_write "cycle" "" ""
    sh -c "$cmd" || warn "cycle command exited nonzero"
    if [ "$once" = "--once" ]; then
      printf '=== %s :: single cycle done ===\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
      cmd_run end --status completed --reason "single cycle (--once)" >/dev/null
      rm -f "$DAEMON_PID"
      return 0
    fi
    printf '=== %s :: sleeping %ss ===\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$interval"
    run_write "sleeping" "" ""
    sleep "$interval"
  done
}

daemon_stop() {
  if ! daemon_alive; then
    warn "no daemon running"
    rm -f "$DAEMON_PID"
    return 0
  fi
  local pid
  pid="$(cat "$DAEMON_PID")"
  kill -TERM "$pid" 2>/dev/null || true
  sleep 1
  if kill -0 "$pid" 2>/dev/null; then
    warn "daemon $pid did not stop on TERM; sending KILL"
    kill -KILL "$pid" 2>/dev/null || true
  fi
  rm -f "$DAEMON_PID"
  say "daemon stopped"
}

daemon_status() {
  if daemon_alive; then
    pass "daemon running (pid $(cat "$DAEMON_PID"))"
  else
    warn "daemon not running"
  fi
  cmd_runs_report
}

launchd_label() { printf 'sh.oxagen.fullauto.%s' "$(repo_slug)"; }
launchd_plist() { printf '%s/Library/LaunchAgents/%s.plist' "$HOME" "$(launchd_label)"; }

# launchd is the only thing on macOS that restarts the loop after a logout or a
# reboot. A nohup'd process survives a closed window and nothing more.
daemon_install() {
  [ "$(uname -s)" = "Darwin" ] || die "daemon install is macOS-only (use systemd --user elsewhere)"
  local plist label
  plist="$(launchd_plist)"; label="$(launchd_label)"
  mkdir -p "$(dirname "$plist")" || die "cannot write $(dirname "$plist")"
  cat > "$plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$label</string>
  <key>ProgramArguments</key>
  <array>
    <string>$REPO_ROOT/scripts/fullauto.sh</string>
    <string>daemon</string>
    <string>loop</string>
    <string>2700</string>
  </array>
  <key>WorkingDirectory</key><string>$REPO_ROOT</string>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>$DAEMON_LOG</string>
  <key>StandardErrorPath</key><string>$DAEMON_LOG</string>
</dict>
</plist>
EOF
  pass "wrote $plist"
  if launchctl bootstrap "gui/$(id -u)" "$plist" 2>/dev/null; then
    pass "loaded — the loop now survives logout and reboot"
  else
    warn "wrote the plist but launchctl bootstrap failed; load it with:"
    printf '        launchctl bootstrap gui/%s %s\n' "$(id -u)" "$plist"
  fi
  printf '\n  Remove with: %s daemon uninstall\n' "$0"
}

daemon_uninstall() {
  local plist
  plist="$(launchd_plist)"
  launchctl bootout "gui/$(id -u)/$(launchd_label)" 2>/dev/null || true
  if [ -e "$plist" ]; then
    rm -f "$plist"
    pass "removed $plist"
  else
    warn "nothing to remove"
  fi
}

# ---------------------------------------------------------------------------
# bench — the comparator arm
# ---------------------------------------------------------------------------

cmd_bench() {
  local mode="${1:-loop}"; shift || true
  case "$mode" in
    loop) bench_loop "$@" ;;
    h2h)  bench_h2h "$@" ;;
    *) die "bench: mode must be loop (cheap, CI) or h2h (measured, vs Claude Code)" ;;
  esac
}

# The cheap arm: the nightly loop-health gate, dispatched in CI. Under a dollar,
# no local Docker, and it catches the regression class that matters most between
# releases — a turn that aborts having done nothing, dies without saying why, or
# is caught cycling.
bench_loop() {
  have gh || die "bench loop needs gh"
  say "dispatching nightly-bench.yml (loop health, CI-hosted)"
  gh workflow run nightly-bench.yml "$@" || die "workflow dispatch failed"
  sleep 6
  local run_id
  run_id="$(gh_plain run list --workflow=nightly-bench.yml --limit 1 --json databaseId --jq '.[0].databaseId')"
  pass "run $run_id — gh run watch $run_id"
  echo "$run_id"
}

# The measured arm: Claude Code vs Stella on Terminal-Bench 2.1, same model on
# both seats. Native x86_64 Linux only; the rig path starts the instance, runs,
# pulls results back, and ALWAYS stops it again — an idle rig is $45.56/day and
# has already tripped a $1,000 budget alert once.
bench_h2h() {
  local use_rig=0 out
  out="$STATE_DIR/h2h-$(date -u +%Y%m%dT%H%M%SZ).json"
  while [ $# -gt 0 ]; do
    case "$1" in
      --rig) use_rig=1; shift ;;
      --out) out="${2:?}"; shift 2 ;;
      *) die "bench h2h: unknown flag $1" ;;
    esac
  done
  [ -f "$REPO_ROOT/$H2H_MATCH" ] || die "match template not found: $H2H_MATCH"

  if [ "$use_rig" -eq 0 ]; then
    have arenabench || die "arenabench not installed (pip install arenabench)"
    docker info >/dev/null 2>&1 || die "docker unreachable"
    if [ "$(uname -s)/$(uname -m)" != "Linux/x86_64" ] && [ -z "${FULLAUTO_ALLOW_EMULATED:-}" ]; then
      die "$(uname -s)/$(uname -m) is not a claim-eligible host (RUNBOOK Part 0).
     Use --rig, or set FULLAUTO_ALLOW_EMULATED=1 for a throwaway signal run."
    fi
    say "running $H2H_MATCH locally"
    DOCKER_DEFAULT_PLATFORM=linux/amd64 \
      arenabench run "$REPO_ROOT/$H2H_MATCH" --progress --results "$out" \
      || die "arenabench run failed"
    pass "results -> $out"
    echo "$out"
    return 0
  fi

  have aws || die "bench h2h --rig needs the aws CLI"
  [ -f "$REPO_ROOT/$RIG_KEY" ] || die "rig key not found at $RIG_KEY"
  chmod 600 "$REPO_ROOT/$RIG_KEY" 2>/dev/null || true

  # The rig bills by the hour. Stopping it is the only step in this script that
  # MUST happen even on failure, interrupt, or a dropped SSH connection.
  # shellcheck disable=SC2317  # invoked via trap, not fallthrough
  stop_rig() {
    say "stopping rig $RIG_INSTANCE (idle costs \$45.56/day)"
    if aws ec2 stop-instances --region "$RIG_REGION" --instance-ids "$RIG_INSTANCE" >/dev/null 2>&1; then
      pass "stop requested"
    else
      warn "STOP FAILED — stop it by hand, it is billing"
    fi
  }
  trap stop_rig EXIT INT TERM

  say "starting rig $RIG_INSTANCE"
  aws ec2 start-instances --region "$RIG_REGION" --instance-ids "$RIG_INSTANCE" >/dev/null \
    || die "could not start rig"
  aws ec2 wait instance-running --region "$RIG_REGION" --instance-ids "$RIG_INSTANCE" \
    || die "rig never reached running"

  local ip
  ip="$(aws ec2 describe-instances --region "$RIG_REGION" --instance-ids "$RIG_INSTANCE" \
        --query 'Reservations[0].Instances[0].PublicIpAddress' --output text)"
  [ -n "$ip" ] && [ "$ip" != "None" ] || die "rig has no public IP"
  pass "rig at $ip (IP changes across stop/start — never hardcode it)"

  # SSH can be refused for a minute after instance-running; poll rather than
  # racing it.
  local tries=0
  until ssh -o StrictHostKeyChecking=accept-new -o ConnectTimeout=8 \
        -i "$REPO_ROOT/$RIG_KEY" "$RIG_USER@$ip" true 2>/dev/null; do
    tries=$((tries + 1))
    [ "$tries" -gt 20 ] && die "rig unreachable over ssh after 20 tries"
    sleep 10
  done
  pass "ssh up"

  say "running the head-to-head on the rig"
  ssh -o StrictHostKeyChecking=accept-new -i "$REPO_ROOT/$RIG_KEY" "$RIG_USER@$ip" \
    "set -e; cd ~/stella && git fetch --quiet && git checkout --quiet origin/main -- . || true; \
     DOCKER_DEFAULT_PLATFORM=linux/amd64 arenabench run $H2H_MATCH --progress --results /tmp/h2h.json" \
    || warn "the match returned nonzero — read the results before calling it a loss"

  if scp -o StrictHostKeyChecking=accept-new -i "$REPO_ROOT/$RIG_KEY" \
       "$RIG_USER@$ip:/tmp/h2h.json" "$out" 2>/dev/null; then
    pass "results -> $out"
  else
    warn "no results file came back"
  fi
  echo "$out"
}

# ---------------------------------------------------------------------------
# upgrade — ship, then consume what shipped
# ---------------------------------------------------------------------------
#
# The delivery half of "fully autonomous delivery": install the RELEASED build
# from the tap and stop shadowing it with the dev build, so what runs on this
# machine is what a user gets. Idempotent, backed up, and it fails closed: if
# the rc file does not contain the shadow line verbatim, it edits nothing.

cmd_upgrade() {
  local apply=1 keep_dev_alias=1 skip_brew=0 rc="${FULLAUTO_RC:-$HOME/.zshrc}"
  while [ $# -gt 0 ]; do
    case "$1" in
      --dry-run) apply=0; shift ;;
      --no-dev-alias) keep_dev_alias=0; shift ;;
      # The two halves are independent on purpose: the formula builds from
      # source and takes minutes, while the PATH surgery is instant and is the
      # part that can go wrong. Being able to run either alone is what makes
      # the risky half testable without paying for the slow half.
      --skip-brew) skip_brew=1; shift ;;
      --rc) rc="${2:?}"; shift 2 ;;
      *) die "upgrade: unknown flag $1" ;;
    esac
  done

  if [ "$skip_brew" -eq 1 ]; then
    warn "skipping the Homebrew half (--skip-brew): PATH surgery only"
  else
    upgrade_brew "$apply"
  fi
  upgrade_unshadow "$apply" "$rc" "$keep_dev_alias"
}

upgrade_brew() {
  local apply="$1"
  have brew || die "upgrade needs Homebrew"

  say "1. Tap"
  if brew tap 2>/dev/null | grep -qx "$BREW_TAP"; then
    pass "$BREW_TAP present"
  elif [ "$apply" -eq 1 ]; then
    brew tap "$BREW_TAP" "$BREW_TAP_REMOTE" || die "brew tap failed"
    pass "tapped $BREW_TAP"
  else
    warn "would run: brew tap $BREW_TAP $BREW_TAP_REMOTE"
  fi

  say "2. Formula"
  # NEVER `brew install stella`: homebrew-core's `stella` is an Atari 2600
  # emulator and installing it exits 0, which is the worst possible failure.
  if brew list --formula 2>/dev/null | grep -qx "stella"; then
    if [ "$apply" -eq 1 ]; then
      brew upgrade "$BREW_FORMULA" 2>&1 | tail -5 || warn "already at the newest version"
    else
      warn "would run: brew upgrade $BREW_FORMULA"
    fi
  else
    if [ "$apply" -eq 1 ]; then
      brew install "$BREW_FORMULA" || die "brew install $BREW_FORMULA failed"
    else
      warn "would run: brew install $BREW_FORMULA"
    fi
  fi
  local brewed
  brewed="$(brew --prefix 2>/dev/null)/bin/stella"
  if [ -x "$brewed" ]; then
    pass "installed: $brewed ($("$brewed" --version 2>/dev/null || echo '?'))"
  else
    warn "$brewed is not executable yet"
  fi
}

upgrade_unshadow() {
  local apply="$1" rc="$2" keep_dev_alias="$3"

  say "3. PATH shadow"
  if [ ! -f "$rc" ]; then
    warn "$rc does not exist — nothing to unshadow"
  elif ! grep -qxF "$SHADOW_PATH_LINE" "$rc"; then
    pass "no shadow line in $rc (already removed, or never present)"
  elif [ "$apply" -eq 0 ]; then
    warn "would comment out in $rc:"
    printf '        %s\n' "$SHADOW_PATH_LINE"
  else
    local backup
    backup="$rc.fullauto-$(date -u +%Y%m%dT%H%M%SZ).bak"
    cp "$rc" "$backup" || die "could not back up $rc"
    pass "backup: $backup"
    # Comment out rather than delete: a commented line is self-documenting and
    # trivially reversible; a deleted one leaves the reader wondering.
    python3 - "$rc" "$SHADOW_PATH_LINE" "$keep_dev_alias" <<'PY'
import sys
rc, shadow, keep = sys.argv[1], sys.argv[2], sys.argv[3] == "1"
lines = open(rc).read().splitlines()
out, replaced = [], False
for ln in lines:
    if ln == shadow and not replaced:
        # Say plainly that the surrounding commentary is now historical. The
        # block above this line explains why the shadow exists; leaving it
        # unqualified next to a disabled shadow makes the file lie to whoever
        # reads it next, and a stale comment is a bug like any other.
        out.append("# --- NOTE: the commentary above is HISTORICAL as of the line below. ---")
        out.append("# `scripts/fullauto.sh upgrade` disabled this prepend. It used to shadow")
        out.append("# the Homebrew release with the local dev build; `stella` now resolves to")
        out.append("# the tap build (macanderson/stella/stella), which is what a user gets.")
        out.append("# " + shadow)
        if keep:
            out.append("# The dev build is still one word away:")
            out.append('stella_dev() { "$HOME/Projects/stella/target/release/stella" "$@"; }')
        replaced = True
    else:
        out.append(ln)
open(rc, "w").write("\n".join(out) + "\n")
print("commented out" if replaced else "no change")
PY
    pass "shadow removed — restore with: cp $backup $rc"
  fi

  say "4. Resolution after a new shell"
  printf '  this shell:  %s\n' "$(command -v stella || echo '(none)')"
  printf '  new shell:   %s\n' "$(env -i HOME="$HOME" PATH="$(brew --prefix)/bin:/usr/bin:/bin" \
                                   sh -c 'command -v stella' 2>/dev/null || echo '(none)')"
  echo
  printf '  Open a new terminal (or run: exec zsh) for the change to take.\n'
}

# ---------------------------------------------------------------------------
# install-commands — put /fullauto where Claude Code will find it
# ---------------------------------------------------------------------------
#
# The canonical copies are committed here; `.claude/` is gitignored in this
# repo (#448), so the commands cannot live at .claude/commands and survive.
# This copies them into the user-scope command directory instead.

cmd_install_commands() {
  local src="$REPO_ROOT/scripts/fullauto/commands"
  local dst="${FULLAUTO_COMMAND_DIR:-$HOME/.claude/commands}"
  [ -d "$src" ] || die "no command sources at $src"
  mkdir -p "$dst/fullauto" || die "cannot write $dst"
  cp "$src/fullauto.md" "$dst/fullauto.md" || die "copy failed"
  cp "$src"/fullauto/*.md "$dst/fullauto/" || die "copy failed"
  say "installed into $dst"
  pass "/fullauto"
  local f
  for f in "$src"/fullauto/*.md; do
    pass "/fullauto:$(basename "$f" .md)"
  done
  echo
  printf '  Loop it with:  /loop 45m /fullauto\n'
}

# ---------------------------------------------------------------------------

# The header comment IS the usage text. Read it structurally — from line 2 to
# the first line that is not a comment — so adding a line to the header can
# never silently truncate or overrun the help output.
usage() {
  awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"
}

main() {
  local sub="${1:-}"; shift || true
  case "$sub" in
    preflight|doctor)  cmd_preflight "$@" ;;
    plan)              cmd_plan "$@" ;;
    run)               cmd_run "$@" ;;
    runs)              cmd_runs_report ;;
    phase)             cmd_phase "$@" ;;
    daemon)            cmd_daemon "$@" ;;
    calibrate)         cmd_calibrate "$@" ;;
    aperture)          cmd_aperture "$@" ;;
    watch)             cmd_watch "$@" ;;
    metrics)           cmd_metrics "$@" ;;
    queue)             cmd_queue "$@" ;;
    cycle-begin)       cmd_cycle_begin "$@" ;;
    cycle-end)         cmd_cycle_end "$@" ;;
    state)             cmd_state "$@" ;;
    seen)              cmd_seen "$@" ;;
    bench)             cmd_bench "$@" ;;
    upgrade)           cmd_upgrade "$@" ;;
    install-commands)  cmd_install_commands "$@" ;;
    ""|-h|--help)      usage ;;
    *) die "unknown subcommand: $sub (try --help)" ;;
  esac
}

main "$@"
