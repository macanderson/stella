#!/usr/bin/env bash
#
# reap-seats.sh — find and remove abandoned Harbor benchmark seats, and the
# Docker task containers they leave holding memory.
#
#   Usage:  scripts/reap-seats.sh [--yes] [--dry-run] [--verbose]
#                                 [--min-idle-secs N] [--sample-secs N]
#                                 [--keep-containers]
#
#     --yes / -y         skip the confirmation prompt and reap immediately
#     --dry-run          only report candidates, never send a signal
#     --verbose / -v     explain why each seat did or didn't qualify
#     --min-idle-secs    age threshold before a seat is considered (default 1200)
#     --sample-secs      CPU-activity sampling window (default 5)
#     --keep-containers  reap the process, leave its containers alone
# (--help text ends here.)
#
# A seat is one `harbor run` invocation — one contestant's whole arm of a match
# (`arenabench/arenabench/runner.py` spawns one per contestant with
# `start_new_session=True`). It outlives its owner by design: that isolation is
# what lets `scripts/arena-kill.sh` stop an arena without ending the benchmarks
# it was displaying. The cost is that a seat whose owner died for real has
# nothing left to end it, and it sits there holding a Docker task container and
# its memory reservation. A surviving arm of a head-to-head then runs under an
# allocation silently short by whatever the orphan holds — a measurement
# confound, not merely wasted RAM.
#
# `scripts/reap-agents.sh` does not cover this: a Harbor seat is neither a
# `stella` process nor a stella-spawned tool subprocess, so it is invisible to
# every other cleanup path in the tree.
#
# WHAT "ABANDONED" MUST MEAN — the safety story of this script
#
# The first draft of the issue behind this script (#2326) proposed asking each
# arena over HTTP whether it had a running match, and reaping any seat nobody
# claimed. That predicate is a footgun, and it was caught by aiming it at a
# real box: it marked a live, 48-minute, real-money Opus-5 benchmark as
# reapable.
#
# The reason is structural. A match started with `arenabench run` — the CLI
# path, `_cmd_run` in cli.py — builds an in-process `MatchRunner` and NEVER
# BINDS A SOCKET. It is invisible to `/api/matches` and `/api/health` by
# construction, and that flow is *recommended* over the HTTP server precisely
# because it cannot collide on a port. Worse, the HTTP view actively lies: an
# arena restarted after a run began lists that very match as `finished`,
# because it restores finished matches from disk and knows nothing of a
# CLI-owned run.
#
# So liveness here is established from the process tree and the work, never
# from an arena's opinion. All four must hold:
#
#   1. ppid == 1        Its owner is gone and it was reparented. A seat with a
#                       live parent is owned, full stop — the cheapest check
#                       and the one that rules out every healthy run.
#   2. age              Older than --min-idle-secs, as reap-agents.sh requires.
#   3. CPU is still     Its cumulative CPU time does not move across a live
#                       sample taken right now.
#   4. containers idle  `docker top` on each of its task containers shows
#                       nothing but the compose keepalive. This is ground
#                       truth: the agent runs INSIDE the container, so a seat
#                       can look quiet on the host while an agent burns 21% in
#                       the container it owns.
#
# Any one of them failing spares the seat. The script fails closed everywhere:
# an unreadable ppid, an unparseable age, a container it cannot inspect all
# mean "not a candidate".
#
# Written for macOS's stock /bin/bash (3.2): no `mapfile`/`readarray`.

set -uo pipefail

MIN_IDLE_SECS=1200
SAMPLE_SECS=5
ASSUME_YES=0
DRY_RUN=0
VERBOSE=0
KEEP_CONTAINERS=0

# The header comment block, bounded by the sentinel above (or the first
# non-comment line) — a hardcoded line range truncates silently when the
# header grows (#3350).
usage() {
  awk 'NR == 1 { next } /^# \(--help text ends here\.\)$/ { exit } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --yes|-y) ASSUME_YES=1 ;;
    --dry-run|-n) DRY_RUN=1 ;;
    --verbose|-v) VERBOSE=1 ;;
    --keep-containers) KEEP_CONTAINERS=1 ;;
    --min-idle-secs) MIN_IDLE_SECS="${2:-}"; shift ;;
    --sample-secs) SAMPLE_SECS="${2:-}"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 1 ;;
  esac
  shift
done

for n in "$MIN_IDLE_SECS" "$SAMPLE_SECS"; do
  case "$n" in
    ''|*[!0-9]*) echo "--min-idle-secs/--sample-secs take whole seconds" >&2; exit 1 ;;
  esac
done

bold=""; dim=""; green=""; yellow=""; reset=""
if [ -t 1 ]; then
  bold=$'\033[1m'; dim=$'\033[2m'; green=$'\033[32m'
  yellow=$'\033[33m'; reset=$'\033[0m'
fi

vlog() { [ "$VERBOSE" -eq 1 ] && echo "  · $*" >&2; return 0; }

# Ancestry, for the same reason scripts/arena-kill.sh walks it: this script
# matches processes by argv, and the shell that invoked it quotes the pattern
# in its own command line. A reaper that can signal its own parent is a bug
# with a body count.
ANCESTRY=""
apid=$$
guard=0
while [ -n "$apid" ] && [ "$apid" -gt 1 ] && [ "$guard" -lt 32 ]; do
  ANCESTRY="$ANCESTRY $apid"
  apid="$(ps -o ppid= -p "$apid" 2>/dev/null | tr -d ' ')"
  guard=$(( guard + 1 ))
done
ANCESTRY="$ANCESTRY "

# Parses ps's `[[dd-]hh:]mm:ss` elapsed-time format into seconds. Same shape as
# scripts/reap-agents.sh::etime_to_secs — duplicated rather than sourced,
# because that script executes its own reaping on load. Every component is
# forced to base 10, or bash reads a field like "09" as invalid octal.
etime_to_secs() {
  etime="$1"; days=0; rest="$1"; hh=0; mm=0; ss=0; a=""; b=""; c=""
  case "$etime" in
    *-*) days="${etime%%-*}"; rest="${etime#*-}" ;;
  esac
  IFS=: read -r a b c <<<"$rest"
  if [ -n "$c" ]; then hh="$a"; mm="$b"; ss="$c"
  elif [ -n "$b" ]; then mm="$a"; ss="$b"
  else ss="$a"
  fi
  echo $(( 10#$days*86400 + 10#$hh*3600 + 10#$mm*60 + 10#$ss ))
}

DOCKER_OK=0
if command -v docker >/dev/null 2>&1 && docker ps --format '{{.Names}}' >/dev/null 2>&1; then
  DOCKER_OK=1
  CONTAINERS="$(docker ps --format '{{.Names}}' 2>/dev/null)"
else
  CONTAINERS=""
fi

# The task containers belonging to one seat. Harbor names each trial's compose
# project after the trial directory under `<jobs-dir>/<job-name>/`, lowercased
# (`sqlite-with-gcov__Ng8Qumy` -> `sqlite-with-gcov__ng8qumy-main-1`), so the
# service suffix is matched by prefix rather than assumed to be `-main-1`.
seat_containers() {
  jobs_dir="$1"; job_name="$2"
  [ -n "$jobs_dir" ] && [ -n "$job_name" ] || return 0
  [ -d "$jobs_dir/$job_name" ] || return 0
  for trial in "$jobs_dir/$job_name"/*/; do
    [ -d "$trial" ] || continue
    base="$(basename "$trial")"
    lower="$(echo "$base" | tr '[:upper:]' '[:lower:]')"
    printf '%s\n' "$CONTAINERS" | while IFS= read -r name; do
      [ -n "$name" ] || continue
      case "$name" in "$lower"*) printf '%s\n' "$name" ;; esac
    done
  done
}

# True if a container is doing work. The compose file keeps the task alive with
# `sleep infinity`; anything else in the table is the agent itself, which is
# where the real evidence lives — a seat can be perfectly quiet on the host
# while the agent inside its container burns CPU.
container_busy() {
  name="$1"
  out="$(docker top "$name" 2>/dev/null)" || return 1
  printf '%s\n' "$out" | tail -n +2 | while IFS= read -r line; do
    [ -n "$line" ] || continue
    case "$line" in
      *"sleep infinity"*) continue ;;
      *) echo busy; break ;;
    esac
  done | grep -q busy
}

# --- collect candidates -----------------------------------------------------

CAND_PIDS=()
CAND_PGIDS=()
CAND_ETIMES=()
CAND_JOBS=()
SPARED=0

while IFS= read -r row; do
  [ -n "$row" ] || continue
  read -r pid ppid pgid etime cputime rest <<<"$row"
  case "$ANCESTRY" in *" $pid "*) continue ;; esac

  job_name=""
  jobs_dir=""
  if [[ "$rest" =~ --job-name[[:space:]]+([^[:space:]]+) ]]; then
    job_name="${BASH_REMATCH[1]}"
  fi
  if [[ "$rest" =~ --jobs-dir[[:space:]]+([^[:space:]]+) ]]; then
    jobs_dir="${BASH_REMATCH[1]}"
  fi
  label="${job_name:-<unnamed job>}"

  # (1) Owner still alive. The one check that would have saved a live match.
  if [ "$ppid" != "1" ]; then
    vlog "$pid ($label): owned — parent $ppid is alive"
    SPARED=$(( SPARED + 1 ))
    continue
  fi

  # (2) Age.
  age="$(etime_to_secs "$etime" 2>/dev/null)"
  case "$age" in
    ''|*[!0-9]*) vlog "$pid ($label): unreadable age '$etime' — sparing"
                 SPARED=$(( SPARED + 1 )); continue ;;
  esac
  if [ "$age" -lt "$MIN_IDLE_SECS" ]; then
    vlog "$pid ($label): only ${age}s old (< ${MIN_IDLE_SECS}s)"
    SPARED=$(( SPARED + 1 ))
    continue
  fi

  # (4) Containers first, because it is the check most likely to spare a seat
  # and the sampling window below costs real seconds.
  busy=""
  if [ "$DOCKER_OK" -eq 1 ]; then
    while IFS= read -r cname; do
      [ -n "$cname" ] || continue
      if container_busy "$cname"; then busy="$cname"; break; fi
    done < <(seat_containers "$jobs_dir" "$job_name")
  fi
  if [ -n "$busy" ]; then
    vlog "$pid ($label): container $busy is still running an agent"
    SPARED=$(( SPARED + 1 ))
    continue
  fi

  # (3) CPU movement across a live sample.
  sleep "$SAMPLE_SECS"
  now="$(ps -o time= -p "$pid" 2>/dev/null | tr -d ' ')"
  if [ -z "$now" ]; then
    vlog "$pid ($label): exited during sampling"
    continue
  fi
  if [ "$now" != "$cputime" ]; then
    vlog "$pid ($label): CPU moved ($cputime -> $now)"
    SPARED=$(( SPARED + 1 ))
    continue
  fi

  CAND_PIDS+=("$pid")
  CAND_PGIDS+=("$pgid")
  CAND_ETIMES+=("$etime")
  CAND_JOBS+=("$label|$jobs_dir|$job_name")
done < <(
  ps -Awwo pid=,ppid=,pgid=,etime=,time=,command= 2>/dev/null |
    awk '$0 ~ /[h]arbor run /'
)

if [ "$DOCKER_OK" -eq 0 ]; then
  printf '%swarning: Docker is unreachable — judging seats on the process tree\n' "$yellow"
  printf 'alone, and leaving any task containers in place.%s\n' "$reset"
fi

if [ "${#CAND_PIDS[@]}" -eq 0 ]; then
  printf '%sno abandoned harbor seats%s' "$green" "$reset"
  if [ "$SPARED" -gt 0 ]; then
    printf ' (%d live seat(s) left alone)' "$SPARED"
  fi
  printf '\n'
  exit 0
fi

printf '%s%d abandoned harbor seat(s):%s\n' "$bold" "${#CAND_PIDS[@]}" "$reset"
REAP_CONTAINERS=()
i=0
while [ "$i" -lt "${#CAND_PIDS[@]}" ]; do
  IFS='|' read -r label jobs_dir job_name <<<"${CAND_JOBS[$i]}"
  printf '  pid %-7s pgid %-7s up %-12s %s\n' \
    "${CAND_PIDS[$i]}" "${CAND_PGIDS[$i]}" "${CAND_ETIMES[$i]}" "$label"
  while IFS= read -r cname; do
    [ -n "$cname" ] || continue
    REAP_CONTAINERS+=("$cname")
    printf '        container %s\n' "$cname"
  done < <(seat_containers "$jobs_dir" "$job_name")
  i=$(( i + 1 ))
done
if [ "$SPARED" -gt 0 ]; then
  printf '%s  (%d live seat(s) left alone)%s\n' "$dim" "$SPARED" "$reset"
fi

if [ "$DRY_RUN" -eq 1 ]; then
  printf '%s--dry-run: nothing signalled%s\n' "$dim" "$reset"
  exit 0
fi

if [ "$ASSUME_YES" -eq 0 ]; then
  printf '\nReap these seats%s? [y/N] ' \
    "$( [ "$KEEP_CONTAINERS" -eq 0 ] && echo " and remove their containers")"
  read -r answer </dev/tty 2>/dev/null || answer=""
  case "$answer" in
    y|Y|yes|YES) ;;
    *) printf 'aborted\n'; exit 0 ;;
  esac
fi

# The whole process group, never the pid: Harbor spawns Docker and helper
# children, and terminating only the parent leaves the container running and
# the job directory growing — the same reason runner.py::cancel uses killpg.
for pgid in "${CAND_PGIDS[@]}"; do
  kill -TERM -"$pgid" 2>/dev/null || true
done

ticks=20
while [ "$ticks" -gt 0 ]; do
  alive=0
  for pid in "${CAND_PIDS[@]}"; do
    if kill -0 "$pid" 2>/dev/null; then alive=1; fi
  done
  [ "$alive" -eq 0 ] && break
  sleep 0.25
  ticks=$(( ticks - 1 ))
done
for pgid in "${CAND_PGIDS[@]}"; do
  kill -KILL -"$pgid" 2>/dev/null || true
done

printf '%s✔ reaped %d seat(s)%s\n' "$green" "${#CAND_PIDS[@]}" "$reset"

if [ "$KEEP_CONTAINERS" -eq 0 ] && [ "${#REAP_CONTAINERS[@]}" -gt 0 ]; then
  for cname in "${REAP_CONTAINERS[@]}"; do
    if docker rm -f "$cname" >/dev/null 2>&1; then
      printf '  removed container %s\n' "$cname"
    else
      printf '  %scould not remove container %s%s\n' "$yellow" "$cname" "$reset"
    fi
  done
fi
