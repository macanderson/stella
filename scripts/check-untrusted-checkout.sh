#!/usr/bin/env bash
#
# Guard: a workflow_run job must not check out a ref its trigger picked.
#
# A workflow_run job gets this repository's token. It gets that token no
# matter which run set it off. `auto-tag.yml` is the one such job here. It can
# write to the default branch, because it pushes the release tag.
#
# So a checkout of `github.event.workflow_run.head_sha` hands that token a
# tree the trigger picked. Build scripts in the tree then run with it. CodeQL
# calls this a pwn request. Alert 129 named this workflow.
#
# The fix is to clone the default branch. Branch protection already vouches
# for it. The job then moves to the built commit, but only after
# `git merge-base --is-ancestor` shows that commit is already on the branch.
#
# One deleted line is the whole fix, so it is easy to undo by accident. The
# workflow says why the line is gone, and a comment is not a gate. CodeQL does
# see it, but only after the merge, and only on the security tab.
#
# What this checks: in a workflow with a `workflow_run:` trigger, no step sets
# `ref:` to a value naming `github.event.workflow_run`. A literal ref is fine.
# So is one the base repository controls. The rule is about where a ref comes
# from, not about having one.
#
# What it does not check: whether the job's `if:` gate is tight, or whether a
# later `run:` step reaches the same sha. Both are review questions over prose
# and shell. This one is a fact about a YAML key.
#
# POSIX tools only, so it runs on a bare CI runner.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

workflows="${1:-.github/workflows}"

fail=0
report=""
note() { report="${report}check-untrusted-checkout: $1"$'\n'; }

# The verdict is settled before anything is written. Emission is best-effort,
# so a reader that closes the pipe changes neither the report nor the exit
# status. scripts/check-gate-parity.sh is the shape copied here.
emit() {
  trap '' PIPE
  printf '%s' "$report" >&2 || true
}

if [ ! -d "$workflows" ]; then
  note "FAIL — $workflows is not a directory."
  emit
  exit 1
fi

scanned=0
for wf in "$workflows"/*.yml "$workflows"/*.yaml; do
  [ -f "$wf" ] || continue

  # A `workflow_run:` key at the trigger's own indent. Anchored, so a mention
  # in a comment or a `run:` block does not enrol an ordinary workflow.
  grep -Eq '^[[:space:]]{1,4}workflow_run:' "$wf" || continue
  scanned=$((scanned + 1))

  hits="$(grep -nE '^[[:space:]]*ref:.*github\.event\.workflow_run' "$wf" || true)"
  [ -n "$hits" ] || continue

  fail=1
  note "FAIL — $wf checks out a ref its workflow_run trigger picked:"
  while IFS= read -r line; do
    note "         $line"
  done <<EOF
$hits
EOF
  note "       Drop the \`ref:\`, so the clone is the default branch. Then move"
  note "       the tree to the built commit, once \`git merge-base"
  note "       --is-ancestor\` shows it is already on that branch."
  note "       auto-tag.yml's checkout step is the worked example."
done

if [ "$scanned" -eq 0 ]; then
  note "FAIL — no workflow declares a workflow_run trigger."
  note "       This guard is then watching nothing. Either the trigger was"
  note "       renamed away, or this script's pattern has rotted. Both want a"
  note "       human, and a silent pass is how a guard becomes decoration."
  emit
  exit 1
fi

if [ "$fail" -ne 0 ]; then
  emit
  exit 1
fi

echo "check-untrusted-checkout: OK — $scanned workflow_run workflow(s), none checks out a ref its trigger picked."
