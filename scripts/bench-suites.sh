#!/usr/bin/env bash
#
# The Python bench suites, derived from the workflow that gates them. See #2847.
#
# `.github/workflows/bench.yml` is a REQUIRED status check, so its list of
# pytest suites is the one that decides whether `main` is green. Until this
# script existed the Makefile carried a second, hand-written copy of that list,
# and the two disagreed: the workflow ran seven suites and `make bench-test`
# ran three. Nothing noticed, because nothing compared them.
#
# It cost a red `main`. On 2026-08-11 #2844 merged with its own bench check
# already reporting `failure`, on a DETERMINISTIC failure in
# `arenabench/tests/test_runner_netns.py` — `flock(1)` forks, so killing the
# `Popen` left the orphaned `sleep` child holding the lock. Any local run of
# that file would have caught it. There was no local command that ran it.
#
# So there is now exactly one list, and it lives in the workflow. This script
# reads it and nothing restates it:
#
#   list    print one canonical line per suite: <mode> TAB <workdir> TAB <args>
#   run     run every suite, in the workflow's order, the workflow's way
#   filter  print the workflow's own scope-filter regex, so `.githooks/pre-push`
#           can select on the same paths without a second copy of it
#
# The parse is TOTAL by construction: a `pytest` invocation in the workflow that
# this script cannot classify is a hard error, never a skip. That is the whole
# point — a silently unrecognised suite is the defect above, wearing a different
# hat. `scripts/check-bench-suites.sh` (`make bench-suites`) runs the parse on
# every gate so a workflow edit meets it immediately rather than at the next
# push that happens to run the suites.
#
# The three modes are the three shapes the workflow actually uses. They are
# distinguished by what they do to the environment, which is why `run` cannot
# flatten them into one command:
#
#   sync        `uv sync --locked --extra dev` in the suite's own project, then
#               `uv run --no-sync pytest -q` there.
#   no-project  `uv run --with pytest --no-project pytest -q` — no project to
#               sync, because the suite's subject is standard-library only.
#               Deliberate, and load-bearing: recomputing a published score from
#               committed evidence must not need the benchmark harness
#               installed. Two spellings, distinguished by cwd: a suite named by
#               path runs from the repository root, a suite named by
#               `working-directory` runs from that directory (arenabench needs
#               it — its `[tool.pytest.ini_options]` sets `pythonpath = ["."]`
#               relative to its own rootdir).
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

workflow=".github/workflows/bench.yml"

die() {
  printf 'bench-suites: %s\n' "$@" >&2
  exit 1
}

[ -f "$workflow" ] || die "$workflow does not exist." \
  "  That workflow is the source of the suite list; there is no second copy" \
  "  to fall back to, on purpose."

# ── The parse ────────────────────────────────────────────────────────────────
#
# Emits <mode> TAB <workdir> TAB <args>, one line per suite, in file order.
# `workdir` is `.` when the step declares none. `args` is what the workflow
# passes to pytest after `-q`, and is empty for a step that passes nothing.
#
# Comment lines are dropped first: the workflow's header and its per-step
# rationale both say "pytest" in prose, and prose must never be mistaken for a
# suite. A `name:` line is dropped for the same reason (the job itself is named
# "harbor_adapter + analyzer pytest"), and a step-opening `- name:` additionally
# resets the working directory, which YAML scopes to the step.
suites() {
  awk '
    /^[[:space:]]*#/ { next }

    /^[[:space:]]*-[[:space:]]*name:/ { workdir = ""; next }
    /^[[:space:]]*name:/              { next }

    # A bare YAML key carries no command — the job that runs these suites is
    # itself keyed `pytest:`, and a key is not an invocation.
    /^[[:space:]]*[A-Za-z0-9_-]+:[[:space:]]*$/ { next }

    /^[[:space:]]*working-directory:[[:space:]]*/ {
      sub(/^[[:space:]]*working-directory:[[:space:]]*/, "")
      sub(/[[:space:]]+$/, "")
      workdir = $0
      next
    }

    /pytest/ {
      line = $0
      sub(/^[[:space:]]+/, "", line)
      sub(/^run:[[:space:]]*/, "", line)
      sub(/[[:space:]]+$/, "", line)

      # Every step that runs a suite runs it through uv, at -q. Anything else
      # wearing the word "pytest" is unclassifiable and must stop the parse:
      # skipping it is how a suite goes unrun without anyone deciding it should.
      if (line !~ /^uv run .*pytest -q/) {
        printf "bench-suites: cannot classify a pytest line in %s:%d\n", FILENAME, FNR > "/dev/stderr"
        printf "  %s\n", line > "/dev/stderr"
        printf "  Recognised shapes are documented in scripts/bench-suites.sh.\n" > "/dev/stderr"
        exit 1
      }

      if (line ~ /--no-sync/)          mode = "sync"
      else if (line ~ /--no-project/)  mode = "no-project"
      else {
        printf "bench-suites: pytest line in %s:%d is neither --no-sync nor --no-project\n", FILENAME, FNR > "/dev/stderr"
        printf "  %s\n", line > "/dev/stderr"
        exit 1
      }

      args = line
      sub(/^.*pytest -q/, "", args)
      sub(/^[[:space:]]+/, "", args)
      sub(/[[:space:]]+$/, "", args)

      printf "%s\t%s\t%s\n", mode, (workdir == "" ? "." : workdir), args
    }
  ' "$workflow"
}

# ── The scope filter ─────────────────────────────────────────────────────────
#
# The workflow decides whether a change touches the bench tooling with one
# `grep -Eq` over the PR diff. `.githooks/pre-push` asks the same question about
# the pushed diff, so it reads the answer from here instead of carrying a copy —
# the copy is what this whole file exists to abolish.
scope_filter() {
  local pattern
  pattern="$(sed -n "s/.*grep -Eq '\(.*\)'.*/\1/p" "$workflow" | head -1)"
  [ -n "$pattern" ] || die \
    "could not find the scope filter (a \`grep -Eq '…'\`) in $workflow." \
    "  .githooks/pre-push selects on that pattern; it has no copy of its own."
  printf '%s\n' "$pattern"
}

# ── Running them ─────────────────────────────────────────────────────────────

run_suites() {
  command -v uv >/dev/null 2>&1 || die \
    "uv is not on PATH, so no suite ran." \
    "  Every suite in $workflow runs through uv." \
    "  Install it (https://docs.astral.sh/uv/), or ./scripts/setup-dev-env.sh."

  local listing mode workdir args
  listing="$(suites)"
  [ -n "$listing" ] || die "found no pytest suites in $workflow."

  while IFS=$'\t' read -r mode workdir args; do
    [ -n "$mode" ] || continue
    local -a argv=()
    # Word-split on purpose: `args` is the workflow's own pytest argument
    # list, which is a list of shell words there too.
    read -r -a argv <<<"$args"

    printf '\033[36m▸ bench suite: %s (%s)%s\033[0m\n' \
      "$workdir" "$mode" "${args:+ $args}" >&2

    case "$mode" in
    sync)
      (cd "$workdir" && uv sync --locked --extra dev && uv run --no-sync pytest -q "${argv[@]}")
      ;;
    no-project)
      (cd "$workdir" && uv run --with pytest --no-project pytest -q "${argv[@]}")
      ;;
    *)
      die "unknown suite mode '$mode' — scripts/bench-suites.sh is out of date."
      ;;
    esac
  done <<<"$listing"
}

case "${1:-}" in
list) suites ;;
filter) scope_filter ;;
run) run_suites ;;
*)
  printf 'usage: %s {list|run|filter}\n' "$0" >&2
  exit 2
  ;;
esac
