# Task: run a 20-task Stella vs Claude Code head-to-head on the fast rig

Run this **only after both fixes have merged to `main`**:

1. the premature-completion fix (`judge_verdict: passed=true` on zero-work
   UNVERIFIABLE turns) — see `PROMPT-glm-premature-completion.md`
2. the binary-portability guard — see `PROMPT-2-fix-install-glibc.md`

If either is unmerged, stop and say so. Running before both land reproduces a
result we already have.

## The experiment

20 Terminal-Bench 2.1 tasks, both agents, same host, same clock.
`glm-5.2`, effort `max`, thinking on, **no budget cap on either arm**,
1 attempt, 0 retries.

**The endpoint is the only permitted difference between the arms:**

| | arm A | arm B |
|---|---|---|
| agent | Stella | Claude Code |
| model | `glm-5.2` | `glm-5.2` |
| endpoint | OpenRouter | z.ai |
| effort / thinking | max / on | max / on |
| budget cap | none | none |

Any other divergence invalidates the comparison. Do not add a turn limit, a
budget, a timeout multiplier, or a retry to one arm only.

## Task set — already drawn, do not redraw

`devset.tasks` (in this folder) — 20 tasks drawn **blind** before any result was
consulted:

```python
random.Random(20260731).sample(sorted(all_89_task_names), 20)
```

Provenance in `devset_provenance.json`. `heldout.tasks` has the other 69, which
stay untouched so they remain clean for a future full run. Redrawing the set
after seeing results would be selection bias — use these exact 20.

## Infrastructure

EC2 `i-07d46341dcc9a31b3` (m6id.8xlarge, 32 vCPU, us-east-1), currently
**stopped**. Key: `tb909-key.pem` in this folder (`chmod 600`), user `ubuntu`.

```bash
aws ec2 start-instances --region us-east-1 --instance-ids i-07d46341dcc9a31b3
aws ec2 describe-instances --region us-east-1 --instance-ids i-07d46341dcc9a31b3 \
  --query 'Reservations[].Instances[].PublicIpAddress' --output text   # new IP each start
```

What survives a stop/start (EBS root): the repo, harbor 0.6.1 venv, `race.sh`,
`live.py`, `~/tb21/` task lists, and `/etc/docker/daemon.json`.

What does **not** survive: `/var/lib/docker` is instance-store NVMe, so all 89
task images are gone. Re-pull first (~10 min, 12-way parallel, idempotent):

```bash
bash ~/prepull.sh && docker images -q | wc -l    # expect 89
```

### Verify before running

```bash
# address-pool fix must still be present, or ~31 concurrent trials will fail
sudo cat /etc/docker/daemon.json | grep -A2 default-address-pools
# expect base 10.192.0.0/10, size 24
```

Docker's default pool allows only ~31 bridge networks and every Compose project
needs one; at 40 concurrent trials it exhausts and Compose failures land in
`result.json` **indistinguishable from agent failures**. This already ruined one
run (`DISCARDED-dev1` in the traces zip).

### Rebuild the SUT at the new main — do not hand-roll it

```bash
cd ~/stella && git fetch origin && git checkout origin/main
bash bench/evidence/run/build_sut.sh
```

`build_sut.sh` uses `cargo zigbuild --target x86_64-unknown-linux-gnu.2.17`.
That glibc-2.17 floor is what makes the binary run in old task containers.
Needs `cargo-zigbuild` and `zig` on the box; install them if absent.

**Do not** substitute `cargo build --release` and copy the output into
`target/x86_64-unknown-linux-gnu/release/stella`. That is exactly the mistake
that caused the glibc failures being fixed — it produces a host-glibc binary
wearing the portable binary's filename, and every integrity check still passes.
Record the resulting `binary_sha256.txt` in the preregistration.

## Settle the posture before launching

The previous run used **uncommitted** harness patches (`live_harness_patches.diff`
in this folder): per-trial budget removed, an `ALL` phase added, and all four
agent roles set to `effort: max, reasoning: on`.

That last one overrode a committed default of `triage: {effort: low, reasoning: off}`
which carries a comment saying it is deliberate. Triage then timed out at ~10s
on glm-5.2 in 9 of 11 inspected trials. The premature-completion fix should have
determined whether that timeout is a real defect or an artifact of the patch.

Before launching: check whether these patches have landed on `main`. If they
have, use `main` as-is. If not, apply the diff and **state in the preregistration
exactly which posture is in effect and why**, including the triage decision. The
posture is SHA-256 hashed into the run manifest, so it cannot be settled later.

## Launch

```bash
./race.sh dev 20 <tag>                                    # both arms, concurrently
python3 ~/live.py <tag>-armA-stella <tag>-armB-claudecode 20   # task-level dashboard
```

Expect ~20–25 min wall-clock. Preregister before launching: SUT commit, binary
sha256, posture digest, the 20 task names, both arm configurations, and the
fixed denominator (20). No reruns, no outcome-selected stops, publish failures.

## Validate before reporting anything

A number from this run means nothing until all four hold. Check each explicitly
and report the counts:

1. **Zero GLIBC failures.** `grep -ri GLIBC` across trial results. Any hit means
   the wrong binary shipped — discard the run.
2. **Zero address-pool failures.** `grep -ri "fully subnetted"`. Any hit means
   infrastructure contaminated it — discard.
3. **Zero silent premature completions.** For every Stella trial, cross-check
   `total_steps` and tool-call count from `agent/trajectory.json` against
   `judge_verdict.passed` in `agent/stella-events.jsonl`. A trial with 0 tool
   calls, `diff_lines: 0`, and `passed: true` means the fix did not take.
4. **Equal denominators.** Both arms must have scored the same 20 tasks. If one
   arm has fewer, the pairing is broken — do not compute McNemar on it.
5. **Telemetry complete, and the log agrees.** Every Stella trial should have an
   `agent/stella-events.jsonl` whose last parseable line is a `complete` event
   (except trials that genuinely timed out or were killed). Separately, grep the
   run log for `could not write` — the previous run emitted 11 of these while
   losing nothing, which is the false-alarm defect fixed in PROMPT-2. After that
   fix, any occurrence means telemetry was actually lost; investigate before
   reporting.

Then report: per-arm pass counts, the discordant pairs (McNemar input), and a
per-task table. Publish failures alongside passes.

## Cost and cleanup

Rig burns **$2.38/h**; API spend for a 20-task both-arm run is roughly $10–15.
`aws ec2 stop-instances` as soon as the run finishes — do not leave it idle.
There is a second instance `i-081bdbc8bdabd1392` holding the earlier partial
run's evidence; leave it stopped unless you need it.

## Constraints

- Do not tune Stella against these 20 tasks and then report a score on them.
  They are an iteration set; the honest published number comes from the full 89.
- Report what happened, including if Stella loses. A failed comparison that is
  trustworthy is worth more than a good one that is not.
