#!/usr/bin/env bash
#
# test-arena-scripts.sh — hermetic checks on scripts/arena-kill.sh and
# scripts/arena-run.sh. Needs no server, no Docker and no network; it never
# launches or signals anything.
#
# The invariant worth a test is the one nobody would suspect: both scripts find
# their targets by matching `ps` output against an argv pattern, and `ps -A`
# lists the awk running the filter and the shell that invoked the script —
# both of which quote the pattern in their own command lines. With a plain
# literal and no ancestry guard, `arena-kill.sh` on an idle box matched three
# processes (two shells and awk) and would have escalated SIGTERM to SIGKILL on
# every one, killing the operator's own shell.
#
# Two mechanisms prevent that, and each is easy to "tidy away" later:
#   - `/[a]renabench serve/` instead of the literal, so awk's own argv does not
#     contain the string it is searching for;
#   - an ancestry walk from $$ to init, because the invoking shell's copy of
#     the pattern is already expanded and no bracket can hide it.
#
# Not part of `make gate`: it shells out to `ps` and spawns wrapper shells,
# which is fine on a developer box but is not the gate's business.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
KILL_SH="$ROOT/scripts/arena-kill.sh"
RUN_SH="$ROOT/scripts/arena-run.sh"

PASS=0
FAIL=0

ok() { PASS=$(( PASS + 1 )); printf '  ok    %s\n' "$1"; }
no() { FAIL=$(( FAIL + 1 )); printf '  FAIL  %s\n' "$1"; }

check() {
  if [ "$2" = "$3" ]; then ok "$1"; else
    no "$1"
    printf '        expected: %s\n        actual:   %s\n' "$3" "$2"
  fi
}

printf 'arena script checks\n'

# --- the matcher never reports its own ancestry -----------------------------
#
# The wrapper's argv carries the hunted pattern in a trailing comment, which is
# exactly the shape a real invocation takes (`zsh -c '… arenabench serve …'`).
# `; true` defeats bash's exec optimisation, which would otherwise replace the
# wrapper with the script and destroy the premise of the test.
OUT="$(
  bash -c "echo \"WRAPPER=\$\$\"; '$KILL_SH' --dry-run; true  # arenabench serve" 2>&1
)"
WRAPPER="$(printf '%s\n' "$OUT" | awk -F= '/^WRAPPER=/{print $2; exit}')"
LISTED="$(printf '%s\n' "$OUT" | awk -v w="$WRAPPER" '$1 == "pid" && $2 == w')"
check "arena-kill does not list the shell that invoked it" "${LISTED:-none}" "none"

# awk's own line, if it leaked, would carry no --port and land in the default
# 8900 column with a cwd of this repo — indistinguishable from a real server by
# eye. Assert on the count instead: every listed row must be a real server, and
# a real server's cwd is resolvable while a transient awk's is not.
GHOSTS="$(printf '%s\n' "$OUT" | awk '$1 == "pid" && /<cwd unknown>/')"
check "arena-kill lists no cwd-less ghost rows" "${GHOSTS:-none}" "none"

# --- argument validation ----------------------------------------------------
"$KILL_SH" --timeout abc >/dev/null 2>&1
check "arena-kill rejects a non-numeric --timeout" "$?" "1"

"$RUN_SH" --port 80x0 >/dev/null 2>&1
check "arena-run rejects a non-numeric --port" "$?" "1"

"$KILL_SH" --nonsense >/dev/null 2>&1
check "arena-kill rejects an unknown flag" "$?" "1"

# --- arena-run refuses an interpreter that cannot serve a Stella seat --------
#
# The failure this guards is silent by construction: an interpreter carrying
# `arenabench` but no `harbor` serves a perfectly healthy-looking UI whose
# every Stella seat exits 0 having measured nothing.
#
# A stub stands in for that interpreter rather than a real one off PATH. The
# ambient `python3` is not a reliable negative — on a rig that has run a
# benchmark, Harbor is often installed into it globally, and the preflight then
# passes exactly as it should. The stub fails the import on every machine.
STUB_DIR="$(mktemp -d)"
trap 'rm -rf "$STUB_DIR"' EXIT
STUB="$STUB_DIR/python"
{
  echo '#!/bin/sh'
  echo 'echo "ModuleNotFoundError: No module named '"'"'harbor'"'"'" >&2'
  echo 'exit 1'
} > "$STUB"
chmod +x "$STUB"

RUN_OUT="$(ARENA_PYTHON="$STUB" "$RUN_SH" --dry-run 2>&1)"
RUN_RC=$?
check "arena-run rejects an interpreter without stella_harbor" "$RUN_RC" "1"
case "$RUN_OUT" in
  *harbor_adapter*) ok "…and names the adapter venv as the fix" ;;
  *) no "…and names the adapter venv as the fix"
     printf '        actual: %s\n' "$RUN_OUT" ;;
esac

MISSING_OUT="$(ARENA_PYTHON="$STUB_DIR/absent" "$RUN_SH" --dry-run 2>&1)"
MISSING_RC=$?
check "arena-run rejects a missing interpreter" "$MISSING_RC" "1"
case "$MISSING_OUT" in
  *"uv sync"*) ok "…and prints the command that creates the venv" ;;
  *) no "…and prints the command that creates the venv"
     printf '        actual: %s\n' "$MISSING_OUT" ;;
esac

# --- reap-seats: liveness comes from the process tree ------------------------
#
# The predicate under test is the one that matters. An earlier spec judged a
# seat abandoned when no arena reported a running match for it, and that would
# have killed a live 48-minute benchmark: a match started with `arenabench run`
# binds no socket and is invisible to every arena endpoint by construction.
#
# Two synthetic seats stand in for the real thing — a real executable named
# `harbor`, so the argv in `ps` is genuine rather than simulated. One is
# reparented to init, one keeps a live parent. Only the first may be reaped.
REAP_SH="$ROOT/scripts/reap-seats.sh"
FAKE_DIR="$(mktemp -d)"
FAKE_PIDS=""
cleanup_fakes() {
  for p in $FAKE_PIDS; do kill -KILL "$p" 2>/dev/null || true; done
  rm -rf "$FAKE_DIR"
}
trap 'cleanup_fakes; rm -rf "$STUB_DIR"' EXIT

printf '#!/bin/sh\nsleep 300\n' > "$FAKE_DIR/harbor"
chmod +x "$FAKE_DIR/harbor"

# Reparented to init: the launcher exits immediately, so ppid becomes 1.
#
# The seat's own stdout goes to DEVNULL deliberately. Inherited, it would be
# this command substitution's pipe, which stays open as long as the seat lives
# — so `$(...)` would block for the seat's full lifetime instead of returning
# the pid it just printed.
ORPHAN_PID="$(
  python3 -c "
import subprocess
p = subprocess.Popen(
    ['$FAKE_DIR/harbor', 'run', '--env', 'docker',
     '--jobs-dir', '$FAKE_DIR/jobs', '--job-name', 'orphan-job'],
    start_new_session=True,
    stdout=subprocess.DEVNULL,
    stderr=subprocess.DEVNULL,
)
print(p.pid)
"
)"
FAKE_PIDS="$FAKE_PIDS $ORPHAN_PID"

# Owned: the launcher stays alive, so the seat keeps a live parent.
python3 -c "
import subprocess, time
subprocess.Popen(
    ['$FAKE_DIR/harbor', 'run', '--env', 'docker',
     '--jobs-dir', '$FAKE_DIR/jobs', '--job-name', 'owned-job'],
)
time.sleep(90)
" >/dev/null 2>&1 &
OWNER_PID=$!
FAKE_PIDS="$FAKE_PIDS $OWNER_PID"
# Off the job table, or bash announces "Killed: 9" when the trap reaps it and
# a clean run looks like a crashed one.
disown "$OWNER_PID" 2>/dev/null || true
sleep 1
OWNED_PID="$(
  ps -Awwo pid=,command= 2>/dev/null |
    awk '$0 ~ /[h]arbor run / && /owned-job/ {print $1; exit}'
)"
FAKE_PIDS="$FAKE_PIDS $OWNED_PID"

# Let the orphan settle into ppid 1 before asking.
sleep 1
REAP_OUT="$("$REAP_SH" --dry-run --verbose --min-idle-secs 0 --sample-secs 1 2>&1)"

case "$REAP_OUT" in
  *"$ORPHAN_PID"*) ok "reap-seats lists a seat reparented to init" ;;
  *) no "reap-seats lists a seat reparented to init"
     printf '        orphan pid %s not in:\n%s\n' "$ORPHAN_PID" "$REAP_OUT" ;;
esac

if [ -n "$OWNED_PID" ]; then
  case "$REAP_OUT" in
    *"pid $OWNED_PID "*)
      no "reap-seats spares a seat whose parent is alive"
      printf '        owned pid %s WAS listed:\n%s\n' "$OWNED_PID" "$REAP_OUT" ;;
    *) ok "reap-seats spares a seat whose parent is alive" ;;
  esac
else
  no "reap-seats spares a seat whose parent is alive (setup failed: no owned pid)"
fi

case "$REAP_OUT" in
  *"left alone"*) ok "…and says how many live seats it left alone" ;;
  *) no "…and says how many live seats it left alone"
     printf '        actual:\n%s\n' "$REAP_OUT" ;;
esac

"$REAP_SH" --min-idle-secs x >/dev/null 2>&1
check "reap-seats rejects a non-numeric --min-idle-secs" "$?" "1"

# --- arena-run classifies a dead child from its log, never from the box ------
#
# A spawned serve that exits at once either crashed or found an arena already
# serving this workspace and handed off on purpose (#2322). The workspace
# defaults to ~/.arenabench for every checkout on the machine, so probing "is
# some arena serving this workspace?" answers yes on a busy box even when THIS
# launch died of a real crash — and would bury the traceback under a green
# checkmark. The script must read which one happened out of the child's own
# slice of the log instead. These checks stand a stub interpreter in for the
# adapter venv: it passes the import preflight, swallows the launch heredoc,
# lays down a prepared log, and reports a pid that is already dead (or, with
# STUB_LINGER, one that dies mid-wait) — so classification has nothing to
# consult but the log, which is the invariant under test.
LAUNCH_STUB="$STUB_DIR/launcher"
{
  echo '#!/bin/sh'
  echo 'case "$1" in'
  echo '  -c) exit 0 ;;'
  echo '  -)'
  echo '    cat >/dev/null'
  echo '    printf "%s\n" "$STUB_LOG_BODY" >> "$STUB_LOG"'
  # The linger child's stdout is redirected for the same reason the reap
  # test's orphan is: inherited, it would hold arena-run's $(...) pipe open
  # for its whole lifetime.
  echo '    if [ -n "${STUB_LINGER:-}" ]; then'
  echo '      sleep "$STUB_LINGER" >/dev/null 2>&1 &'
  echo '      pid=$!'
  echo '    else'
  echo '      sh -c ":" & pid=$!'
  echo '      wait "$pid" 2>/dev/null'
  echo '    fi'
  echo '    echo "$pid 8964 ${STUB_LOG_OFFSET:-0} $STUB_LOG"'
  echo '    ;;'
  echo '  *) exit 0 ;;'
  echo 'esac'
} > "$LAUNCH_STUB"
chmod +x "$LAUNCH_STUB"

RUN_HOME="$(mktemp -d)"
trap 'cleanup_fakes; rm -rf "$STUB_DIR" "$RUN_HOME"' EXIT

# The marker names port 8931 while the stub reports the launch on 8964: the
# printed URL must come from the child's log line, not from the launch port.
HLOG="$RUN_HOME/server-handoff.log"
: > "$HLOG"
RUN_OUT="$(
  ARENA_PYTHON="$LAUNCH_STUB" ARENABENCH_HOME="$RUN_HOME" \
  STUB_LOG="$HLOG" STUB_LOG_OFFSET=0 \
  STUB_LOG_BODY='  arenabench  ->  http://127.0.0.1:8931/  (already serving this workspace)' \
  "$RUN_SH" 2>&1
)"
check "arena-run reports a logged handoff as a handoff" "$?" "0"
case "$RUN_OUT" in
  *"http://127.0.0.1:8931/"*) ok "…and prints the arena url from the child's log" ;;
  *) no "…and prints the arena url from the child's log"
     printf '        actual:\n%s\n' "$RUN_OUT" ;;
esac

CLOG="$RUN_HOME/server-crash.log"
: > "$CLOG"
RUN_OUT="$(
  ARENA_PYTHON="$LAUNCH_STUB" ARENABENCH_HOME="$RUN_HOME" \
  STUB_LOG="$CLOG" STUB_LOG_OFFSET=0 \
  STUB_LOG_BODY='RuntimeError: match store corrupted' \
  "$RUN_SH" 2>&1
)"
check "arena-run reports a dead child with no handoff marker as a crash" "$?" "1"
case "$RUN_OUT" in
  *"RuntimeError: match store corrupted"*) ok "…and shows the child's own log slice" ;;
  *) no "…and shows the child's own log slice"
     printf '        actual:\n%s\n' "$RUN_OUT" ;;
esac

# A previous launch on this port handed off and left its marker in the shared
# log. This launch crashed. Only the slice past the offset may be consulted.
OLOG="$RUN_HOME/server-stale.log"
printf '%s\n' \
  '  arenabench  ->  http://127.0.0.1:8900/  (already serving this workspace)' \
  > "$OLOG"
STALE_LEN="$(wc -c < "$OLOG" | tr -d ' ')"
RUN_OUT="$(
  ARENA_PYTHON="$LAUNCH_STUB" ARENABENCH_HOME="$RUN_HOME" \
  STUB_LOG="$OLOG" STUB_LOG_OFFSET="$STALE_LEN" \
  STUB_LOG_BODY='OSError: [Errno 48] Address already in use' \
  "$RUN_SH" 2>&1
)"
check "arena-run ignores a previous launch's handoff marker before the offset" "$?" "1"

# A child that outlives the 2s check and dies during the ready wait was
# previously announced as serving, banner and all.
LLOG="$RUN_HOME/server-linger.log"
: > "$LLOG"
RUN_OUT="$(
  ARENA_PYTHON="$LAUNCH_STUB" ARENABENCH_HOME="$RUN_HOME" \
  STUB_LOG="$LLOG" STUB_LOG_OFFSET=0 STUB_LINGER=3 \
  STUB_LOG_BODY='ValueError: port 8964 already serves a different arena' \
  "$RUN_SH" 2>&1
)"
check "arena-run reports a death during the ready wait as a crash" "$?" "1"
case "$RUN_OUT" in
  *"✔ arenabench serving"*)
    no "…and does not print the serving banner for a dead server"
    printf '        actual:\n%s\n' "$RUN_OUT" ;;
  *) ok "…and does not print the serving banner for a dead server" ;;
esac

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
