#!/usr/bin/env bash
#
# arena-kill.sh — stop every `arenabench serve` process on this machine.
#
#   Usage:  scripts/arena-kill.sh [--dry-run] [--cancel] [--timeout N]
#
#     --dry-run    list the servers and exit without signalling anything
#     --cancel     shut down gracefully, CANCELLING every running match —
#                  the destructive mode; see "Which signal" below
#     --timeout N  seconds to wait for a signalled server to exit before
#                  escalating to KILL (default 5, or 20 with --cancel)
# --help text ends here.
#
# WHY THIS EXISTS
#
# `arenabench serve` is launched detached (scripts/arena-run.sh) so a benchmark
# survives the terminal that started it. The price of that is servers pile up:
# every worktree, every parallel session, every abandoned investigation leaves
# one bound to a different port, and they stay invisible until one of them
# answers a request meant for another. Seven were alive here at once on
# 2026-08-08 — ports 8181/8900/8901/8902/8917/8933, the oldest twelve hours
# stale, spread across four different checkouts.
#
# That is not untidiness, it is a correctness hazard. `serve`'s default port is
# 8900 for everybody, so the second server to start fails to bind and dies —
# while `curl :8900` keeps succeeding against the FIRST one. Every subsequent
# API call then lands on a stranger's server, resolving Stella seats through
# ITS adapter, ITS worktree and ITS credentials. That failure presented as a
# `ModuleNotFoundError` from arenabench itself and cost real diagnostic time
# before anyone suspected the port.
#
# WHICH SIGNAL — the whole correctness story of this script
#
# `serve()` (arenabench/arenabench/server.py) wraps `serve_forever()` in
# `except KeyboardInterrupt`, and that handler calls `runner.cancel()` on every
# RUNNING match, which `killpg`s each seat's process group. So the signal is
# not a detail, it is the behaviour:
#
#   SIGINT   Python raises KeyboardInterrupt, the handler runs, and every
#            in-flight match is CANCELLED — Harbor logs "canceling trial",
#            the trials die, and their task containers go with them.
#   SIGTERM  Python's default disposition terminates the process outright, the
#            handler never runs, and the seats — spawned with
#            `start_new_session=True` (runner.py) — are merely reparented and
#            carry on writing their artifacts.
#
# Losing the server costs only the live UI: ArenaBench renders a match entirely
# from files the run has already written under `~/.arenabench/matches/<id>/`,
# which is why a finished match survives a restart untouched. So TERM is the
# default here and INT sits behind `--cancel`. "Stop the servers" must never be
# a synonym for "throw away whatever they were measuring" — a 5-task match was
# lost exactly that way on 2026-08-04.
#
# Seats orphaned by a TERM are reported, never signalled. Ending somebody's
# running benchmark is a decision, not a cleanup step.
#
# Written for macOS's stock /bin/bash (3.2): while-read over process
# substitution, never `mapfile`/`readarray`.

set -uo pipefail

# shellcheck source=scripts/lib/help-header.sh
. "$(dirname "$0")/lib/help-header.sh"

DRY_RUN=0
SIGNAL="TERM"
TIMEOUT=""

usage() {
  print_help_header "$0"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run|-n) DRY_RUN=1 ;;
    --cancel) SIGNAL="INT" ;;
    --timeout) TIMEOUT="${2:-}"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 1 ;;
  esac
  shift
done

# `--cancel` has real work to do before it exits — killpg every seat group,
# stop the recorder, stop the snapshotter — so it gets a longer leash before
# the escalation to KILL cuts that short. An explicit --timeout still wins.
if [ -z "$TIMEOUT" ]; then
  if [ "$SIGNAL" = "INT" ]; then TIMEOUT=20; else TIMEOUT=5; fi
fi
case "$TIMEOUT" in
  ''|*[!0-9]*) echo "--timeout takes a whole number of seconds" >&2; exit 1 ;;
esac

bold=""; dim=""; green=""; yellow=""; reset=""
if [ -t 1 ]; then
  bold=$'\033[1m'; dim=$'\033[2m'; green=$'\033[32m'
  yellow=$'\033[33m'; reset=$'\033[0m'
fi

# Every pid from this script up to init.
#
# Matching processes by argv means matching the SHELL that invoked you, whose
# command line quotes the very pattern you are hunting: a `zsh -c "make
# kill-arena && ..."` wrapper appears in `ps` as a line containing
# "arenabench serve" and is indistinguishable from a server by text alone.
# This script escalates to SIGKILL, so being unable to signal its own ancestry
# is a safety property, not a nicety — and the bracket trick below cannot
# provide it, because the wrapper's copy of the pattern is already expanded.
ANCESTRY=""
apid=$$
guard=0
while [ -n "$apid" ] && [ "$apid" -gt 1 ] && [ "$guard" -lt 32 ]; do
  ANCESTRY="$ANCESTRY $apid"
  apid="$(ps -o ppid= -p "$apid" 2>/dev/null | tr -d ' ')"
  guard=$(( guard + 1 ))
done
ANCESTRY="$ANCESTRY "

# Resolves a pid's working directory — which checkout a server belongs to is
# the only thing that tells it apart from the six others on this box.
proc_cwd() {
  pid="$1"
  if command -v lsof >/dev/null 2>&1; then
    lsof -a -p "$pid" -d cwd -Fn 2>/dev/null | awk '/^n/{print substr($0,2); exit}'
  elif [ -r "/proc/$pid/cwd" ]; then
    readlink -f "/proc/$pid/cwd" 2>/dev/null
  fi
}

# Every process whose argv contains `arenabench serve` — both the `uv run`
# wrapper and the interpreter it spawned. Both are collected: a wrapper left
# holding a dead child is the same litter as the child itself.
#
# `[a]renabench` is deliberate, not a typo to tidy away. `ps -A` lists the awk
# process running this very filter, whose own argv contains the pattern, so a
# plain literal makes the matcher report itself as a server every time. The
# bracket changes the pattern's text without changing the string it matches.
PIDS=()
PORTS=()
ETIMES=()
CWDS=()
while IFS= read -r row; do
  [ -n "$row" ] || continue
  read -r pid etime rest <<<"$row"
  case "$ANCESTRY" in *" $pid "*) continue ;; esac
  port="8900"
  if [[ "$rest" =~ --port[[:space:]=]+([0-9]+) ]]; then
    port="${BASH_REMATCH[1]}"
  fi
  PIDS+=("$pid")
  PORTS+=("$port")
  ETIMES+=("$etime")
  CWDS+=("$(proc_cwd "$pid")")
done < <(
  ps -Awwo pid=,etime=,command= 2>/dev/null | awk '$0 ~ /[a]renabench serve/'
)

if [ "${#PIDS[@]}" -eq 0 ]; then
  printf '%sno arenabench servers running%s\n' "$green" "$reset"
  exit 0
fi

printf '%s%d arenabench server process(es):%s\n' "$bold" "${#PIDS[@]}" "$reset"
i=0
while [ "$i" -lt "${#PIDS[@]}" ]; do
  printf '  pid %-7s port %-6s up %-12s %s%s%s\n' \
    "${PIDS[$i]}" "${PORTS[$i]}" "${ETIMES[$i]}" \
    "$dim" "${CWDS[$i]:-<cwd unknown>}" "$reset"
  i=$(( i + 1 ))
done

if [ "$DRY_RUN" -eq 1 ]; then
  printf '%s--dry-run: nothing signalled%s\n' "$dim" "$reset"
  exit 0
fi

if [ "$SIGNAL" = "INT" ]; then
  printf '%s--cancel: SIGINT — every RUNNING match will be cancelled and its\n' "$yellow"
  printf '          task containers torn down.%s\n' "$reset"
fi

for pid in "${PIDS[@]}"; do
  kill -"$SIGNAL" "$pid" 2>/dev/null || true
done

# Poll rather than sleep the whole timeout: the common case is every server
# gone inside a few hundred milliseconds, and a cleanup command that always
# costs five seconds is a cleanup command people stop running.
ticks=$(( TIMEOUT * 4 ))
while [ "$ticks" -gt 0 ]; do
  alive=0
  for pid in "${PIDS[@]}"; do
    if kill -0 "$pid" 2>/dev/null; then alive=1; fi
  done
  [ "$alive" -eq 0 ] && break
  sleep 0.25
  ticks=$(( ticks - 1 ))
done

STUBBORN=()
for pid in "${PIDS[@]}"; do
  if kill -0 "$pid" 2>/dev/null; then
    STUBBORN+=("$pid")
    kill -KILL "$pid" 2>/dev/null || true
  fi
done

if [ "${#STUBBORN[@]}" -gt 0 ]; then
  printf '%sescalated to KILL after %ss: %s%s\n' \
    "$yellow" "$TIMEOUT" "${STUBBORN[*]}" "$reset"
fi
printf '%s✔ stopped %d arenabench server process(es)%s\n' \
  "$green" "${#PIDS[@]}" "$reset"

# A TERM'd server leaves its seats running, which is the point — but the
# operator has to be told, because those seats now have no UI, no supervisor,
# and a Docker memory reservation nobody is accounting for.
SEAT_PIDS=()
SEAT_ETIMES=()
while IFS= read -r row; do
  [ -n "$row" ] || continue
  read -r pid etime _ <<<"$row"
  case "$ANCESTRY" in *" $pid "*) continue ;; esac
  SEAT_PIDS+=("$pid")
  SEAT_ETIMES+=("$etime")
done < <(
  ps -Awwo pid=,etime=,command= 2>/dev/null | awk '$0 ~ /[h]arbor run /'
)

if [ "${#SEAT_PIDS[@]}" -gt 0 ]; then
  printf '\n%s%d harbor seat(s) still running — NOT signalled:%s\n' \
    "$yellow" "${#SEAT_PIDS[@]}" "$reset"
  i=0
  while [ "$i" -lt "${#SEAT_PIDS[@]}" ]; do
    printf '  pid %-7s up %s\n' "${SEAT_PIDS[$i]}" "${SEAT_ETIMES[$i]}"
    i=$(( i + 1 ))
  done
  printf '%sThey keep writing trial artifacts and grade normally; only the live\n' "$dim"
  printf 'UI is gone. To end one seat and its containers, signal its whole group:\n'
  printf '  ps -o pgid= -p PID    then    kill -TERM -PGID\n'
  printf 'then docker rm -f whatever task container it orphans.%s\n' "$reset"
fi

exit 0
