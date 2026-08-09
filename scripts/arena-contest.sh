#!/usr/bin/env bash
# Run a Stella-vs-Claude-Code contest, locally or on AWS Batch.
#
# Driven by `make contest` with make-variable arguments:
#
#   make contest num_tasks=10 versus_model=anthropic/claude-sonnet-5 \
#                max_throughput=true
#
# `max_throughput=true` means different things in the two places a contest can
# run, and conflating them is how a "fast" run becomes a slow one:
#
#   * On AWS Batch (`target=cloud`, the default when max_throughput=true), the
#     submitter already fans out ONE JOB PER TRIAL. Throughput is bounded by
#     the queue's vCPU quota, not by anything in the match file, so the file
#     stays at concurrency 1 and Batch does the parallelism.
#   * Locally (`target=local`), one task slot is TWO contestant containers
#     racing, so concurrency is bounded by CPU and RAM, not by ambition. We
#     size it from the machine and say what we picked.
#
# Everything here is argument marshalling and refusal; the contest itself is
# built by `arenabench contest`, so there is one definition of a seat.
set -euo pipefail

die() { printf 'arena-contest: %s\n' "$*" >&2; exit 2; }

num_tasks="${num_tasks:-10}"
versus_model="${versus_model:-}"
max_throughput="${max_throughput:-false}"
target="${target:-}"
ref="${ref:-main}"
attempts="${attempts:-1}"
out="${out:-}"
extra="${extra:-}"

[ -n "$versus_model" ] || die "versus_model= is required (e.g. versus_model=anthropic/claude-sonnet-5)"
case "$num_tasks" in ''|*[!0-9]*) die "num_tasks must be a positive integer, got '$num_tasks'";; esac
[ "$num_tasks" -ge 1 ] || die "num_tasks must be at least 1"
case "$attempts" in ''|*[!0-9]*) die "attempts must be a positive integer, got '$attempts'";; esac

case "$max_throughput" in
  true|yes|1|on)   max_throughput=true ;;
  false|no|0|off)  max_throughput=false ;;
  *) die "max_throughput must be true or false, got '$max_throughput'" ;;
esac

# max_throughput implies the cloud unless the caller pinned a target: "as many
# concurrent as possible" on one laptop is a handful, and on Batch it is the
# whole panel at once. Choosing silently would be the wrong kind of helpful,
# so the choice is printed below.
if [ -z "$target" ]; then
  if [ "$max_throughput" = true ]; then target=cloud; else target=local; fi
fi
case "$target" in cloud|local) ;; *) die "target must be cloud or local, got '$target'";; esac

concurrency=1
if [ "$target" = local ] && [ "$max_throughput" = true ]; then
  cores=$( { getconf _NPROCESSORS_ONLN || sysctl -n hw.ncpu || nproc; } 2>/dev/null || echo 2 )
  # Two containers per slot, and a task container is not free on RAM. Half the
  # cores, floor 1, ceiling the number of tasks — more slots than tasks buys
  # nothing and costs memory.
  concurrency=$(( cores / 2 ))
  if [ "$concurrency" -lt 1 ]; then concurrency=1; fi
  if [ "$concurrency" -gt "$num_tasks" ]; then concurrency="$num_tasks"; fi
fi

toml="${out:-contest-${num_tasks}task-$(printf '%s' "$versus_model" | tr '/' '-').toml}"

printf 'contest   : %s vs Stella, %s task(s), %s attempt(s)\n' \
  "$versus_model" "$num_tasks" "$attempts"
printf 'target    : %s' "$target"
if [ "$target" = cloud ]; then
  printf '  (AWS Batch — one job per trial; throughput is the queue quota)\n'
else
  printf '  (this machine — concurrency %s)\n' "$concurrency"
fi

# `uv run` keeps the tool on its own locked dependency set; the cloud verbs
# need the optional boto3 extra and say so themselves if it is absent.
runner=(uv run --project arenabench arenabench)
command -v uv >/dev/null 2>&1 || runner=(python3 -m arenabench)

argv=(contest
  --versus-model "$versus_model"
  --num-tasks "$num_tasks"
  --attempts "$attempts"
  --concurrency "$concurrency"
  --output "$toml")

if [ "$target" = cloud ]; then
  argv+=(--submit --ref "$ref")
fi
if [ -n "$extra" ]; then
  # shellcheck disable=SC2206  # deliberate word-splitting: extra= is a flag list
  argv+=($extra)
fi

set -x
"${runner[@]}" "${argv[@]}"
set +x

if [ "$target" = local ]; then
  printf '\nrun it with:\n  uv run --project arenabench arenabench run %s\n' "$toml"
fi
