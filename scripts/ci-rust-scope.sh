#!/usr/bin/env bash
#
# Does this diff reach past the prose paths? Prints `true` or `false`.
#
#   ./scripts/ci-rust-scope.sh <base> <head>
#
# `.github/workflows/ci.yml`'s `changes` job is the only caller. See #1892 for
# why that job exists at all, #4632 for the carve-out, and #4606 for why the
# push side asks the same question the pull_request side does.
#
# ── One rule, one place (#4606) ──────────────────────────────────────────────
#
# The rule used to be written twice: once as `paths-ignore` on ci.yml's `push:`
# trigger, and once as a regex inside the `changes` job for pull requests. The
# trigger carried a comment asking a human to keep the two in agreement, which
# is what a repository writes when it cannot enforce something — and they were
# not in agreement. The job had a carve-out for the website files a Rust test
# reads; the trigger could not have one, because `paths-ignore` has no negation
# and "ignore website/** except this one file" is not expressible. So a merge
# whose diff was confined to website/ started no Rust gate at all.
#
# That is how `main` went red on 2026-08-23 with nothing reporting it: #4588's
# pull_request run answered `rust=false` (before the carve-out existed) and its
# push run never started. Four consecutive commits were red, one root cause,
# none of them the change that caused it.
#
# Now the trigger has no path filter and this script answers for both events. A
# website-only merge pays one checkout and one diff instead of an hour of Rust
# — the same saving, decided once.
#
# ── Failing open ─────────────────────────────────────────────────────────────
#
# A base this clone cannot resolve — a force-push whose predecessor is gone, a
# branch's first push, a fetch too shallow to reach it — answers `true`. The
# expensive answer is the safe one: the cost of running the gate when it was
# not needed is minutes, and the cost of skipping it when it was is a red
# `main` nobody is watching.
#
# Uses portable POSIX tools so it runs on a bare CI runner.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

base="${1:-}"
head="${2:-HEAD}"

inventory="${WEBSITE_INPUTS_FILE:-scripts/website-rust-inputs.txt}"

open() {
  echo "ci-rust-scope: $1 — running the full gate." >&2
  echo "true"
  exit 0
}

# The all-zero SHA is what GitHub sends as `github.event.before` for a branch's
# first push. There is no predecessor to diff against.
case "$base" in
"") open "no base commit given" ;;
0000000000000000000000000000000000000000) open "the base is the null commit" ;;
esac

if ! git rev-parse --verify --quiet "$base^{commit}" >/dev/null; then
  open "the base commit $base is not in this clone"
fi

# Three-dot: what HEAD changed since it and the base diverged. For a push where
# the base is an ancestor that is the push's own diff; for a pull request it is
# the PR's diff, unpolluted by whatever landed on the base branch meanwhile.
# With no merge base at all it errors, and the trap above turns that into the
# expensive answer.
if ! changed="$(git diff --name-only "$base...$head" 2>/dev/null)"; then
  open "no merge base between $base and $head"
fi

# An empty diff runs nothing here. Reporting that a PR is empty is
# .github/workflows/empty-diff.yml's job, not this script's.
if [ -z "$changed" ]; then
  echo "false"
  exit 0
fi

# The file list goes to a file, and every grep below reads that file rather
# than a pipe. `grep -q` exits on its first match and closes the pipe under it,
# so the writer takes a SIGPIPE — and `pipefail` then reports 141 for a
# pipeline that MATCHED. It is a race, so it answers wrongly on some runs and
# not others, and the wrong answer here is `false`: a skipped Rust gate. Same
# shape as #1815, which scripts/check-gate-parity.sh carries a note about.
files="$(mktemp "${TMPDIR:-/tmp}/stella-ci-rust-scope.XXXXXX")"
trap 'rm -f "$files"' EXIT INT TERM
printf '%s\n' "$changed" >"$files"

# The prose paths: website/, docs/, .github/ISSUE_TEMPLATE/, and root-level
# *.md only — `[^/]+\.md$` stops at the first `/`, like the glob `*.md`, so a
# crate's own README still pays the gate. Anything outside them means the gate
# runs.
if grep -Evq '^(website/|docs/|\.github/ISSUE_TEMPLATE/)|^[^/]+\.md$' "$files"; then
  echo "true"
  exit 0
fi

# The prose paths that are not prose, because a Rust test reads the file and a
# test that never ran is green. The list is scripts/website-rust-inputs.txt and
# is not restated here: scripts/check-website-inputs.sh holds those entries to
# what the Rust sources actually name, so the filter and the tests cannot drift
# apart the way the filter and the trigger did.
if [ ! -f "$inventory" ]; then
  open "$inventory is missing, so the carve-out cannot be built"
fi

carve="$(sed -e 's/#.*//' "$inventory" |
  awk '$1 == "read" { p = $2; sub(/\/$/, "", p); gsub(/\./, "\\.", p); print "^" p "(/|$)" }' |
  paste -sd '|' -)"

if [ -z "$carve" ]; then
  open "$inventory declares no read paths, so the carve-out would be empty"
fi

if grep -Eq "$carve" "$files"; then
  echo "true"
  exit 0
fi

echo "false"
