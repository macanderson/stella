#!/usr/bin/env bash
#
# arena-run.sh — launch a detached, Stella-wired `arenabench serve`.
#
#   Usage:  scripts/arena-run.sh [--port N] [--dry-run]
#
#     --port N     bind this exact port and fail if it is taken, instead of
#                  taking the first free port at or above the default
#     --dry-run    resolve and print the wiring, launch nothing
#
#   Environment:
#     ARENA_PORT      same as --port
#     ARENA_PYTHON    interpreter override — must import BOTH `arenabench` and
#                     `stella_harbor`; see "Which interpreter" below
#     STELLA_BINARY   the SUT the seats run (default ~/.arenabench/sut/stella)
#
# A bare `arenabench serve` starts a server that looks completely healthy and
# loses every Stella seat. Three things have to be true at launch, none of them
# observable from the UI afterwards, and each with its own way of failing
# quietly. This script makes all three true, then proves the server it started
# is the one answering.
#
# WHICH INTERPRETER
#
# The `harbor run` subprocess a seat spawns has to import `stella_harbor`, and
# `stella_harbor` imports `harbor`. Only the adapter's own venv
# (bench/harbor_adapter/.venv) has both that and `arenabench`'s dependencies —
# `arenabench/.venv` imports `arenabench` perfectly and then dies on
# `ModuleNotFoundError: No module named 'harbor'` inside the seat. Worse, when
# the import fails, harbor_agent.py's fallback binds `_Base = object` and
# Harbor raises `TypeError: ArenaStellaAgent() takes no arguments` — and still
# EXITS 0, so the seat reports "done" having measured nothing.
#
# So the preflight below actually imports both modules before anything is
# launched. It costs about a second and converts that whole failure mode into
# one line of text at the only moment it is cheap to fix.
#
# WHICH ADAPTER, WHICH BINARY
#
# `ARENABENCH_STELLA_ADAPTER` names the checkout the seats resolve Stella from;
# `sut.stella_repo()` derives the repository root two levels above it. Setting
# it explicitly is what keeps a server launched from a worktree measuring THAT
# worktree rather than whichever checkout happened to be discovered.
#
# `STELLA_BINARY` names the binary under test. Left unset, the adapter falls
# back to `stella` on PATH or `target/release/stella` — on a Mac that is a
# Mach-O arm64 build, which cannot execute inside a linux/amd64 task container
# at all. The staged linux SUT lives at ~/.arenabench/sut/stella, so this
# script defaults to it and checks its ELF magic rather than trusting the path.
#
# WHICH PORT
#
# `serve`'s default is 8900 for every checkout on the machine, so parallel
# sessions collide constantly (seven servers, six ports, four checkouts, one
# afternoon). A collision is not loud: the loser dies with `Address already in
# use` while `curl :8900` keeps answering from the winner, and every call after
# that silently drives a stranger's server. So the default here is "the first
# free port at or above 8900", an explicit --port is honoured strictly and
# fails rather than drifting, and the launch is verified by the pid this script
# started — never by the port merely answering.
#
# WHY DETACHED
#
# The server is spawned with `start_new_session=True`, which puts it in its own
# session and process group. Under an agent harness or a job-control shell,
# a plain background job receives the teardown SIGINT — and `serve()`'s
# KeyboardInterrupt handler cancels every running match on the way out. A
# 5-task match died exactly that way. `nohup` alone does not fix this: it masks
# SIGHUP, not SIGINT.
#
# Written for macOS's stock /bin/bash (3.2): no `mapfile`/`readarray`.

set -uo pipefail

DEFAULT_PORT=8900
PORT="${ARENA_PORT:-}"
PORT_PINNED=0
DRY_RUN=0

usage() {
  sed -n '2,17p' "$0"
}

if [ -n "$PORT" ]; then PORT_PINNED=1; fi

while [ $# -gt 0 ]; do
  case "$1" in
    --port) PORT="${2:-}"; PORT_PINNED=1; shift ;;
    --dry-run|-n) DRY_RUN=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 1 ;;
  esac
  shift
done

if [ -z "$PORT" ]; then PORT="$DEFAULT_PORT"; fi
case "$PORT" in
  ''|*[!0-9]*) echo "--port takes a whole number" >&2; exit 1 ;;
esac

bold=""; dim=""; red=""; green=""; yellow=""; reset=""
if [ -t 1 ]; then
  bold=$'\033[1m'; dim=$'\033[2m'; red=$'\033[31m'; green=$'\033[32m'
  yellow=$'\033[33m'; reset=$'\033[0m'
fi

die() { printf '%serror:%s %s\n' "$red" "$reset" "$*" >&2; exit 1; }

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ADAPTER="$ROOT/bench/harbor_adapter"
ARENA_PKG="$ROOT/arenabench"
ARENA_HOME="${ARENABENCH_HOME:-$HOME/.arenabench}"

[ -d "$ARENA_PKG/arenabench" ] || die "no arenabench package at $ARENA_PKG"
[ -d "$ADAPTER/stella_harbor" ] || die "no harbor adapter at $ADAPTER"

PY="${ARENA_PYTHON:-$ADAPTER/.venv/bin/python}"
if [ ! -x "$PY" ]; then
  die "no interpreter at $PY
  Create the adapter venv (it is the only one that can import both
  \`arenabench\` and \`stella_harbor\`):
      uv sync --project bench/harbor_adapter --locked --extra dev
  Or point ARENA_PYTHON at an interpreter that already can."
fi

# The SUT the seats will run. Checked for ELF magic, not merely for existence:
# the failure this catches is a host-native Mach-O binary that the linux task
# container cannot execute, which otherwise surfaces as an unexplained
# zero-token trial deep inside a paid run.
SUT="${STELLA_BINARY:-$ARENA_HOME/sut/stella}"
SUT_NOTE=""
if [ ! -f "$SUT" ]; then
  SUT_NOTE="${yellow}missing — seats will fall back to PATH/target/release${reset}"
else
  magic="$(head -c 4 "$SUT" 2>/dev/null | od -An -tx1 2>/dev/null | tr -d ' \n')"
  if [ "$magic" != "7f454c46" ]; then
    SUT_NOTE="${yellow}not an ELF binary — cannot execute in a linux task container${reset}"
  fi
fi

export ARENABENCH_STELLA_ADAPTER="$ADAPTER"
export PYTHONPATH="$ARENA_PKG:$ADAPTER${PYTHONPATH:+:$PYTHONPATH}"
if [ -f "$SUT" ]; then export STELLA_BINARY="$SUT"; fi

# The preflight that turns a silent, exit-0 seat failure into one line here.
if ! IMPORT_ERR="$("$PY" -c 'import arenabench, stella_harbor' 2>&1)"; then
  die "$PY cannot import both modules:
$IMPORT_ERR
  The adapter venv (bench/harbor_adapter/.venv) is the interpreter that can.
  \`arenabench/.venv\` imports arenabench but has no \`harbor\`, which makes
  every Stella seat exit 0 having measured nothing."
fi

printf '%sarenabench serve%s\n' "$bold" "$reset"
printf '  interpreter  %s\n' "$PY"
printf '  adapter      %s\n' "$ADAPTER"
printf '  package      %s\n' "$ARENA_PKG"
printf '  workspace    %s\n' "$ARENA_HOME"
printf '  SUT binary   %s%s\n' "$SUT" "${SUT_NOTE:+  $SUT_NOTE}"

# Who else is already on this box. Not fatal — this is a multi-tenant machine
# by design — but a count nobody printed is how six stale servers accumulate.
#
# `[a]renabench` for the same reason scripts/arena-kill.sh uses it: `ps -A`
# lists the awk running this filter, whose argv would otherwise contain the
# pattern and inflate the count by one. Ancestor pids are dropped too — the
# shell that invoked this script quotes the pattern in its own command line.
ANCESTRY=""
apid=$$
guard=0
while [ -n "$apid" ] && [ "$apid" -gt 1 ] && [ "$guard" -lt 32 ]; do
  ANCESTRY="$ANCESTRY $apid"
  apid="$(ps -o ppid= -p "$apid" 2>/dev/null | tr -d ' ')"
  guard=$(( guard + 1 ))
done
ANCESTRY="$ANCESTRY "

OTHERS=0
while IFS= read -r row; do
  [ -n "$row" ] || continue
  read -r opid _ <<<"$row"
  case "$ANCESTRY" in *" $opid "*) continue ;; esac
  OTHERS=$(( OTHERS + 1 ))
done < <(
  ps -Awwo pid=,command= 2>/dev/null | awk '$0 ~ /[a]renabench serve/'
)
if [ "$OTHERS" -gt 0 ]; then
  printf '  %salso running  %d other arenabench server process(es) — make kill-arena to clear%s\n' \
    "$dim" "$OTHERS" "$reset"
fi

if [ "$DRY_RUN" -eq 1 ]; then
  printf '%s--dry-run: nothing launched%s\n' "$dim" "$reset"
  exit 0
fi

mkdir -p "$ARENA_HOME" || die "cannot create $ARENA_HOME"

# Port probe, log naming and spawn all happen inside the interpreter that is
# about to serve, because they are one decision and splitting them breaks two
# ways. The probe asks exactly the question the server's own bind will ask
# (ThreadingHTTPServer sets allow_reuse_address), and the pid, the port and the
# log come back from the same call, so they cannot describe different servers —
# a log named for the *requested* port would collect a second server's output
# whenever the scan moved past a busy one.
#
# The offset is read before the handle opens: it is where this launch's output
# starts, so a traceback left by a PREVIOUS server on this port is never
# reported as this one's.
LAUNCH="$(
  "$PY" - "$PORT" "$PORT_PINNED" "$ARENA_HOME" <<'PY'
import os
import socket
import subprocess
import sys

start, pinned, home = int(sys.argv[1]), sys.argv[2] == "1", sys.argv[3]


def free(port: int) -> bool:
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        sock.bind(("127.0.0.1", port))
        return True
    except OSError:
        return False
    finally:
        sock.close()


candidates = [start] if pinned else range(start, start + 40)
port = next((p for p in candidates if free(p)), None)
if port is None:
    sys.exit(3)

logpath = os.path.join(home, f"server-{port}.log")
offset = os.path.getsize(logpath) if os.path.exists(logpath) else 0
log = open(logpath, "ab", buffering=0)
proc = subprocess.Popen(
    [
        sys.executable, "-m", "arenabench", "serve",
        "--host", "127.0.0.1", "--port", str(port), "--no-browser",
    ],
    stdout=log,
    stderr=subprocess.STDOUT,
    stdin=subprocess.DEVNULL,
    start_new_session=True,
)
print(f"{proc.pid} {port} {offset} {logpath}")
PY
)"
LAUNCH_RC=$?

if [ "$LAUNCH_RC" -eq 3 ]; then
  printf '%sport %s is already bound%s\n' "$red" "$PORT" "$reset" >&2
  if command -v lsof >/dev/null 2>&1; then
    # `>&2` BEFORE `2>/dev/null`: the other order points stdout at whatever
    # fd2 has already become — /dev/null — and silently discards the one
    # diagnostic that says who is holding the port.
    lsof -iTCP:"$PORT" -sTCP:LISTEN -Pn >&2 2>/dev/null || true
  fi
  printf 'Pick another with --port, or clear the box with make kill-arena.\n' >&2
  exit 1
fi
[ "$LAUNCH_RC" -eq 0 ] || die "could not launch the server (exit $LAUNCH_RC)"

# `logpath` last, and read with the rest of the line, so a home directory
# containing a space still resolves to one path.
read -r PID PORT LOG_OFFSET LOG <<<"$LAUNCH"

# The authority on "did it start" is this pid, not the port. A port answering
# proves only that SOME server is listening — which is exactly the trap when
# another session already holds the one you asked for.
sleep 2
if ! kill -0 "$PID" 2>/dev/null; then
  # An immediate exit is a crash today. It will not always be: `serve` is
  # growing the ability to hand off to an arena already serving this workspace
  # and exit on purpose (#2322). So ask what is actually true instead of
  # assuming — probe /api/health, which reports the workspace each arena is
  # serving, and report a handoff as a handoff. The holder's pid and cwd are
  # printed with it, because "an arena is serving this workspace" and "YOUR
  # arena is serving it" are different claims and only the second is good news.
  HANDOFF="$(
    "$PY" - "$ARENA_HOME" "$PORT" "$DEFAULT_PORT" <<'PY' 2>/dev/null
import json
import os
import sys
import urllib.request

home = os.path.realpath(sys.argv[1])
ports = [int(sys.argv[2])] + list(range(int(sys.argv[3]), int(sys.argv[3]) + 40))
for port in dict.fromkeys(ports):
    try:
        with urllib.request.urlopen(
            f"http://127.0.0.1:{port}/api/health", timeout=0.4
        ) as response:
            payload = json.load(response)
    except Exception:
        continue
    if os.path.realpath(payload.get("workspace", "")) == home:
        print(port)
        break
PY
  )"
  if [ -n "$HANDOFF" ]; then
    printf '%s✔ an arena is already serving this workspace%s  http://127.0.0.1:%s/\n' \
      "$green" "$reset" "$HANDOFF"
    if command -v lsof >/dev/null 2>&1; then
      lsof -iTCP:"$HANDOFF" -sTCP:LISTEN -Pn >&2 2>/dev/null || true
    fi
    printf '  %sthis launch handed off to it rather than binding a second port;\n' "$dim"
    printf '  make kill-arena then make run-arena for one wired to THIS checkout%s\n' "$reset"
    exit 0
  fi
  printf '%sserver pid %s died within 2s. Its output:%s\n' "$red" "$PID" "$reset" >&2
  tail -c "+$(( LOG_OFFSET + 1 ))" "$LOG" 2>/dev/null | tail -n 30 >&2
  exit 1
fi

if tail -c "+$(( LOG_OFFSET + 1 ))" "$LOG" 2>/dev/null | grep -q "Traceback"; then
  printf '%swarning: pid %s is alive but logged a traceback:%s\n' "$yellow" "$PID" "$reset" >&2
  tail -c "+$(( LOG_OFFSET + 1 ))" "$LOG" | tail -n 20 >&2
fi

URL="http://127.0.0.1:$PORT/"
READY=""
if command -v curl >/dev/null 2>&1; then
  tries=10
  while [ "$tries" -gt 0 ]; do
    if curl -fsS --max-time 2 "${URL}api/datasets" >/dev/null 2>&1; then
      READY="yes"
      break
    fi
    kill -0 "$PID" 2>/dev/null || break
    sleep 0.5
    tries=$(( tries - 1 ))
  done
fi

printf '\n%s✔ arenabench serving%s  %s\n' "$green" "$reset" "$URL"
printf '  pid %s   log %s\n' "$PID" "$LOG"
if [ -z "$READY" ]; then
  printf '  %sthe pid is alive but /api/datasets has not answered yet — check the log%s\n' \
    "$yellow" "$reset"
fi
printf '  %sstop it with make kill-arena; a match also needs Docker up (colima start)%s\n' \
  "$dim" "$reset"
