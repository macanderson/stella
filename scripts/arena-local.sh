#!/usr/bin/env bash
#
# arena-local.sh — launch an ArenaBench match on this machine, credentialled
# from a dotenv file, against a SUT that is actually current.
#
#   Usage:  scripts/arena-local.sh <match.toml> [options] [-- <arenabench run args>]
#
#     --env-file PATH     dotenv to seed credentials from
#                         (default: $ARENA_ENV_FILE, else ~/.env.global.local)
#     --inject-all        give BOTH seats every credential-shaped name in the
#                         file, not only the ones each seat declared
#     --pull / --no-pull  take or skip the "you are N commits behind" offer
#                         without being asked
#     --max-behind N      refuse a SUT more than N commits behind origin/main
#                         (default 5; 0 disables)
#     --allow-stale-sut   launch anyway, and say why
#     --dry-run, -n       resolve and report everything, launch nothing
# (--help text ends here.)
#
# WHY THIS EXISTS ALONGSIDE arena-run.sh
#
# `arena-run.sh` starts the long-lived *server*. This starts one *match*, which
# is a different set of preconditions with a different set of silent failures —
# and the expensive ones are silent in the same direction. A seat with no
# credential does not error; it scores 0.0, which on a scoreboard is
# indistinguishable from an agent that tried and lost. A binary two hundred
# commits behind tip does not error either; it produces a perfectly clean
# number about code nobody is asking about. Both cost real provider money to
# learn, and both are answerable for free before anything starts.
#
# So the order below is deliberate: everything that can refuse the launch runs
# before anything that can spend. Source freshness first (cheapest, and the one
# a human may want to act on), then credentials, then the SUT, then Docker.
#
# THE SPLIT WITH arena_local.py
#
# This file answers the questions a shell answers well — which checkout, how
# far behind, which interpreter, is Docker up. Its companion answers the one a
# shell answers badly: which values reach which seat. That has to be computed
# with ArenaBench's own `load_match` / `required_env` / `screen_env`, because a
# second implementation of the credential fence would agree on the day it was
# written and quietly stop agreeing later — and the failure mode of *that* is a
# report saying a seat is credentialled while the seat disagrees.
#
# It also keeps the dotenv file off the shell entirely. `source`-ing a file
# executes it; the Python side parses it. The file holds this machine's real
# provider keys, so the difference is not academic.
#
# Written for macOS's stock /bin/bash (3.2): no `mapfile`/`readarray`, no
# associative arrays.

set -uo pipefail

TEMPLATE=""
PULL_CHOICE=""
DRY_RUN=0
# Arrays, not accumulated strings: a string has to be left unquoted at the exec
# to split back into words, which then also splits any path containing a space
# and glob-expands anything holding a `*`. `--env-file` takes a path from the
# operator's own argv, so that is a real shape, not a theoretical one. Arrays
# are bash 3.2 (only *associative* arrays are 4.0), so this costs nothing on
# stock macOS.
PASSTHRU=()
FORWARD=()

# The header comment block, bounded by the sentinel above (or the first
# non-comment line) — a hardcoded line range truncates silently when the
# header grows (#3350).
usage() { awk 'NR == 1 { next } /^# \(--help text ends here\.\)$/ { exit } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0"; }

bold=""; dim=""; red=""; green=""; yellow=""; reset=""
if [ -t 1 ]; then
  bold=$'\033[1m'; dim=$'\033[2m'; red=$'\033[31m'; green=$'\033[32m'
  yellow=$'\033[33m'; reset=$'\033[0m'
fi

die() { printf '%serror:%s %s\n' "$red" "$reset" "$*" >&2; exit 1; }

# Options this script acts on are consumed; everything else is handed to the
# Python side verbatim, so a new flag there needs no edit here.
while [ $# -gt 0 ]; do
  case "$1" in
    --pull)     PULL_CHOICE="yes" ;;
    --no-pull)  PULL_CHOICE="no" ;;
    --dry-run|-n) DRY_RUN=1; FORWARD+=("--dry-run") ;;
    -h|--help)  usage; exit 0 ;;
    --)         shift; while [ $# -gt 0 ]; do PASSTHRU+=("$1"); shift; done; break ;;
    --*=*)      FORWARD+=("$1") ;;
    --env-file|--max-behind|--repo)
                FORWARD+=("$1" "${2:-}"); shift ;;
    --*)        FORWARD+=("$1") ;;
    *)          if [ -z "$TEMPLATE" ]; then TEMPLATE="$1"; else
                  die "unexpected argument: $1"
                fi ;;
  esac
  shift
done

[ -n "$TEMPLATE" ] || { usage >&2; die "a match template is required"; }
[ -f "$TEMPLATE" ] || die "no match template at $TEMPLATE"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ADAPTER="$ROOT/bench/harbor_adapter"
ARENA_PKG="$ROOT/arenabench"

[ -d "$ARENA_PKG/arenabench" ] || die "no arenabench package at $ARENA_PKG"
[ -d "$ADAPTER/stella_harbor" ] || die "no harbor adapter at $ADAPTER"

# ---------------------------------------------------------------------------
# 1. source freshness — the cheapest refusal, and the only one a human may
#    want to act on rather than merely be told about
# ---------------------------------------------------------------------------

printf '%ssource%s\n' "$bold" "$reset"
BRANCH="$(git -C "$ROOT" rev-parse --abbrev-ref HEAD 2>/dev/null)"
printf '  checkout   %s %s(%s)%s\n' "$ROOT" "$dim" "${BRANCH:-detached}" "$reset"

# `--quiet` and a discarded stderr: a fetch failing because the laptop is on a
# plane must not stop a local match, but it must not silently report "0 behind"
# either — the count below is skipped entirely when the ref is unknown, never
# defaulted to zero.
if git -C "$ROOT" fetch origin main --quiet 2>/dev/null; then
  BEHIND="$(git -C "$ROOT" rev-list --count HEAD..origin/main 2>/dev/null)"
  AHEAD="$(git -C "$ROOT" rev-list --count origin/main..HEAD 2>/dev/null)"
else
  BEHIND=""
  AHEAD=""
  printf '  %scould not reach origin — freshness unknown, not assumed current%s\n' \
    "$yellow" "$reset"
fi

if [ -n "$BEHIND" ]; then
  if [ "$BEHIND" -eq 0 ]; then
    printf '  %s✔%s up to date with origin/main' "$green" "$reset"
  elif [ "$BEHIND" -eq 1 ]; then
    printf '  %s1 commit behind%s origin/main' "$yellow" "$reset"
  else
    printf '  %s%s commits behind%s origin/main' "$yellow" "$BEHIND" "$reset"
  fi
  if [ -n "$AHEAD" ] && [ "$AHEAD" -gt 0 ]; then
    printf ' %s(and %s ahead)%s' "$dim" "$AHEAD" "$reset"
  fi
  printf '\n'
fi

# The offer is only ever made when it is both useful and safe. A fast-forward
# on a topic branch is not the same operation as one on main — it would pull
# main's commits into a branch the operator is holding open on purpose — so the
# offer is withheld there and the count is simply reported. Same for a dirty
# tree: `--ff-only` would refuse anyway, and an offer that cannot succeed is
# worse than no offer.
if [ -n "$BEHIND" ] && [ "$BEHIND" -gt 0 ]; then
  DIRTY="$(git -C "$ROOT" status --porcelain 2>/dev/null | head -n 1)"
  if [ "$BRANCH" != "main" ]; then
    printf '  %son %s, not main — pull skipped; rebase deliberately if you want those commits%s\n' \
      "$dim" "${BRANCH:-a detached HEAD}" "$reset"
  elif [ -n "$DIRTY" ]; then
    printf '  %sworking tree is dirty — pull skipped (a fast-forward would refuse anyway)%s\n' \
      "$dim" "$reset"
  else
    DO_PULL="$PULL_CHOICE"
    if [ -z "$DO_PULL" ]; then
      if [ -t 0 ]; then
        printf '  pull %s commit(s) from origin/main now? [y/N] ' "$BEHIND"
        read -r reply
        case "$reply" in [yY]*) DO_PULL="yes" ;; *) DO_PULL="no" ;; esac
      else
        # Non-interactive and unasked: report, never act. A launcher that
        # silently moves HEAD under a scripted caller is a worse bug than a
        # stale checkout.
        printf '  %snot a terminal — pass --pull to take these commits%s\n' "$dim" "$reset"
        DO_PULL="no"
      fi
    fi
    if [ "$DO_PULL" = "yes" ]; then
      if git -C "$ROOT" pull --ff-only origin main; then
        printf '  %s✔%s pulled — now at %s\n' "$green" "$reset" \
          "$(git -C "$ROOT" rev-parse --short HEAD)"
      else
        die "pull failed — resolve it and re-run"
      fi
    fi
  fi
fi

# ---------------------------------------------------------------------------
# 2. the interpreter — the same one arena-run.sh insists on, for the same
#    reason: a seat that cannot import `stella_harbor` exits 0 having measured
#    nothing at all
# ---------------------------------------------------------------------------

PY="${ARENA_PYTHON:-$ADAPTER/.venv/bin/python}"
[ -x "$PY" ] || die "no interpreter at $PY
  Create the adapter venv (the only one that imports both \`arenabench\` and
  \`stella_harbor\`):
      uv sync --project bench/harbor_adapter --locked --extra dev
  Or point ARENA_PYTHON at an interpreter that already can."

export ARENABENCH_STELLA_ADAPTER="$ADAPTER"
export PYTHONPATH="$ARENA_PKG:$ADAPTER${PYTHONPATH:+:$PYTHONPATH}"

if ! IMPORT_ERR="$("$PY" -c 'import arenabench, stella_harbor' 2>&1)"; then
  die "$PY cannot import both modules:
$IMPORT_ERR
  \`arenabench/.venv\` imports arenabench but has no \`harbor\`, which makes
  every Stella seat exit 0 having measured nothing."
fi

# ---------------------------------------------------------------------------
# 3. Docker — checked before credentials only because it is the one failure a
#    person fixes with a single command, and after everything free because it
#    costs a round trip to the daemon
# ---------------------------------------------------------------------------

if ! docker info >/dev/null 2>&1; then
  printf '\n%sDocker is not answering%s — a match runs every trial in a container.\n' \
    "$red" "$reset" >&2
  printf '  Start it first (colima start), then re-run.\n' >&2
  [ "$DRY_RUN" -eq 1 ] || exit 1
  printf '  %s--dry-run: continuing anyway, nothing will be launched%s\n' "$dim" "$reset" >&2
fi

printf '\n'
# Argument order is load-bearing, not stylistic. `arena_local.py` collects the
# `arenabench run` passthrough with `nargs=REMAINDER`, and argparse stops
# parsing options at the first positional — so every flag must precede
# $TEMPLATE and the passthrough must follow it. Built the other way round,
# `--dry-run` ended up forwarded to `arenabench run` as an unknown argument,
# *after* a preflight that had already printed clean.
#
# `${ARR[@]+"${ARR[@]}"}` rather than a bare `"${ARR[@]}"`: under `set -u`,
# bash 3.2 treats an empty array's expansion as an unbound variable and aborts.
# The launcher is normally invoked with neither list populated, so the plain
# spelling would fail on stock macOS in the common case and nowhere else.
exec "$PY" "$ROOT/scripts/arena_local.py" \
  --repo "$ROOT" \
  ${FORWARD[@]+"${FORWARD[@]}"} \
  "$TEMPLATE" \
  ${PASSTHRU[@]+"${PASSTHRU[@]}"}
