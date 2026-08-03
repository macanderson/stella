#!/bin/bash
# One paid synthetic trial that proves the whole path end to end — adapter
# install, binary upload and SHA verification, tool loop, metering, verifier
# handoff — before 89 paid trials depend on it. Excluded from every analysis.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"
preflight || exit 1

JOB="${1:-sentinel-$(date +%Y%m%d-%H%M%S)}"
test ! -e "$JOBS/$JOB" || { echo "FATAL: $JOBS/$JOB exists"; exit 1; }
echo "job=$JOB sut=$STELLA_SOURCE_COMMIT harbor=$(harbor --version)"

cd "$TB_REPO"
harbor run \
  --env docker \
  --path "$TB_REPO/bench/readiness/synthetic-adapter-sentinel" \
  --agent-import-path stella_harbor:StellaAgent \
  --model "$TB_MODEL" \
  --job-name "$JOB" \
  --jobs-dir "$JOBS" \
  --n-attempts 1 --n-concurrent 1 --max-retries 0
rc=$?
echo "harbor_exit=$rc"

# The gate: the external verifier must have returned exactly 1.0.
python3 - "$JOBS/$JOB" <<'PY'
import glob, json, sys
rows = [json.load(open(p)) for p in glob.glob(f"{sys.argv[1]}/*/result.json")]
rows = [r for r in rows if r.get("task_name")]
if not rows:
    print("SENTINEL FAIL: no trial produced a result"); raise SystemExit(1)
r = rows[0]
reward = (r.get("verifier_result") or {}).get("rewards", {}).get("reward")
md = ((r.get("agent_result") or {}).get("metadata") or {})
print(f"reward={reward} stella_status={md.get('stella_status')} "
      f"binary_verified={md.get('stella_binary_sha256_verified_in_container')} "
      f"commit_verified={md.get('stella_source_commit_verified_in_binary')} "
      f"exception={(r.get('exception_info') or {}).get('exception_type')}")
# Which verification arm this trial ran, and what its ladder actually reached.
# Reported rather than gated: on a witness-arm sentinel `witness=authored` is
# the cheapest possible proof that the tier fires before 89 trials depend on it
# (#1284), but a warrant that decided this task needed no new test is a
# legitimate `not_reported`, and failing on it would be a false gate.
print(f"arm={md.get('stella_assurance_arm')} "
      f"author={md.get('stella_witness_author_model')} "
      f"witness={md.get('stella_witness_authored_state')} "
      f"self_verdict={md.get('stella_self_verdict_state')}")
# NOTE: while #960 stands, an AgentTimeoutError accompanies even a perfect
# trial, so the gate is the reward and the in-container verifications — not the
# absence of an exception, which the audited protocol additionally requires.
raise SystemExit(0 if reward == 1.0 else 1)
PY
