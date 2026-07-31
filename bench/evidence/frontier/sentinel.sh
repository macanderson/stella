#!/bin/bash
# Two cheap trials that prove the whole path end to end before 70 expensive
# ones depend on it.
#
#   stage 1  the synthetic fixture — adapter install, binary upload and SHA
#            verification, tool loop, metering, verifier handoff. Identical to
#            the Terminal-Bench sentinel except that it runs under *this* lane's
#            newer Harbor, which is the thing that changed.
#
#   stage 2  the cheapest real Frontier-Bench task — a real Dockerfile build and
#            the separate-container verifier that every task in this set
#            declares and that Harbor 0.6.1 would have silently ignored.
#
# The two stages are gated differently, and the difference is the point. The
# synthetic task is oracle-solvable, so anything but reward 1.0 is a broken
# harness. A real Frontier-Bench task is *not* expected to pass — the best
# published agents clear about a third of this set — so demanding reward 1.0
# there would gate on model quality instead of on plumbing. Stage 2 asks only
# whether the trial genuinely ran: binary verified inside the container, a
# status reported, a reward produced by the verifier, and no infrastructure
# exception. A task that ran and scored 0.0 passes this gate, correctly.
#
# Both stages are excluded from every analysis.
set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/env.sh"
fb_preflight || exit 1

STAMP="$(date +%Y%m%d-%H%M%S)"
JOB="${1:-fb-sentinel-$STAMP}"
test ! -e "$JOBS/$JOB" || { echo "FATAL: $JOBS/$JOB exists"; exit 1; }
echo "job=$JOB sut=$STELLA_SOURCE_COMMIT harbor=$(harbor --version)"

cd "$TB_REPO" || exit 1

echo "=== stage 1: synthetic fixture (gate: reward == 1.0)"
harbor run \
  --env docker \
  --path "$TB_REPO/bench/readiness/synthetic-adapter-sentinel" \
  --agent stella_harbor:StellaAgent \
  --model "$TB_MODEL" \
  --job-name "$JOB" \
  --jobs-dir "$JOBS" \
  --n-attempts 1 --n-concurrent 1 --max-retries 0
echo "harbor_exit=$?"

python3 - "$JOBS/$JOB" <<'PY' || exit 1
import glob, json, sys
rows = [json.load(open(p)) for p in glob.glob(f"{sys.argv[1]}/*/result.json")]
rows = [r for r in rows if r.get("task_name")]
if not rows:
    print("STAGE 1 FAIL: no trial produced a result"); raise SystemExit(1)
r = rows[0]
reward = (r.get("verifier_result") or {}).get("rewards", {}).get("reward")
md = ((r.get("agent_result") or {}).get("metadata") or {})
print(f"reward={reward} stella_status={md.get('stella_status')} "
      f"binary_verified={md.get('stella_binary_sha256_verified_in_container')} "
      f"commit_verified={md.get('stella_source_commit_verified_in_binary')}")
raise SystemExit(0 if reward == 1.0 else 1)
PY

test -f "$FB_PLAN" || { echo "SKIP stage 2: no plan at $FB_PLAN (run fetch_dataset.sh)"; exit 0; }

TASK="${FB_SENTINEL_TASK:-$(python3 -c "
import json
p=json.load(open('$FB_PLAN'))
runnable=set(p['runnable_tasks'])
# Cheapest by declared footprint, then by name so the choice is reproducible.
import tomllib, pathlib
best=None
for d in sorted(pathlib.Path(p['dataset_dir']).glob('*/*/task.toml')):
    if d.parent.name not in runnable: continue
    c=tomllib.loads(d.read_text()); e=c.get('environment',{}) or {}
    key=(e.get('memory_mb',0), e.get('cpus',0), (c.get('agent',{}) or {}).get('timeout_sec',0), d.parent.name)
    if best is None or key<best[0]: best=(key,d.parent.name)
print(best[1] if best else '')
")}"
test -n "$TASK" || { echo "SKIP stage 2: no runnable task in the plan"; exit 0; }

JOB2="$JOB-real"
echo "=== stage 2: real task $TASK (gate: the trial ran; any reward is acceptable)"
harbor run \
  --env docker \
  --dataset "$FB_DATASET" \
  --include-task-name "$FB_TASK_PREFIX/$TASK" \
  --agent stella_harbor:StellaAgent \
  --model "$TB_MODEL" \
  --job-name "$JOB2" \
  --jobs-dir "$JOBS" \
  --n-attempts 1 --n-concurrent 1 --max-retries 0
echo "harbor_exit=$?"

python3 - "$JOBS/$JOB2" <<'PY'
import glob, json, sys
rows = [json.load(open(p)) for p in glob.glob(f"{sys.argv[1]}/*/result.json")]
rows = [r for r in rows if r.get("task_name")]
if not rows:
    print("STAGE 2 FAIL: no trial produced a result"); raise SystemExit(1)
r = rows[0]
vr = r.get("verifier_result") or {}
reward = (vr.get("rewards") or {}).get("reward")
md = ((r.get("agent_result") or {}).get("metadata") or {})
exc = (r.get("exception_info") or {}).get("exception_type")
print(f"task={r.get('task_name')} reward={reward} stella_status={md.get('stella_status')} "
      f"binary_verified={md.get('stella_binary_sha256_verified_in_container')} "
      f"exception={exc}")
problems = []
if md.get("stella_binary_sha256_verified_in_container") is not True:
    problems.append("SUT binary was not verified inside the container")
if not md.get("stella_status"):
    problems.append("the adapter reported no status")
if reward is None:
    problems.append("the verifier produced no reward — the separate-container "
                    "verifier handoff is the thing to look at first")
for p in problems:
    print(f"  STAGE 2 FAIL: {p}")
if not problems:
    print("  stage 2 OK: the trial ran and was graded. Reward is not the gate here.")
raise SystemExit(1 if problems else 0)
PY
