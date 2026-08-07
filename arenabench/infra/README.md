<!-- SPDX-License-Identifier: Apache-2.0 -->

# ArenaBench cloud infrastructure

Scale-to-zero AWS infrastructure for running ArenaBench matches as short,
serious bursts — 12 agents at 12-task concurrency if asked — while billing
nothing when idle. One CloudFormation template (`core.yaml`) is the whole
control plane; the web app lives on Vercel (arenabench.org) and reaches AWS
through an OIDC-assumed role, never long-lived keys.

**Standalone by design.** ArenaBench is being ejected from the stella
monorepo. Nothing here assumes that monorepo: the system under test (SUT) is
always fetched by **git URL + ref**, the runner image locates the package via
a `PkgPath` parameter (`arenabench` today, `.` post-ejection), and stella is
just the first entry in the agent roster — Claude Code is the required day-1
comparison arm, anything else is a seat in a match TOML.

## Architecture

```
arenabench.org (Vercel, Next.js)          AWS account 578673726240, us-east-1
┌─────────────────────────────┐
│ Auth.js magic-link login    │   OIDC    ┌──────────────────────────────────┐
│ (Resend; allowlist:         ├──────────►│ role arenabench-vercel-web       │
│  mac@macanderson.com)       │           └───────┬──────────────────────────┘
└─────────────────────────────┘                   │ SubmitJob / StartBuild /
                                                  │ DynamoDB / S3 / logs
        ┌─────────────────────────────────────────┼─────────────────────────┐
        │                                         ▼                         │
        │  CodeBuild arenabench-sut-build   AWS Batch queues                │
        │  (any git ref → zigbuild →        measure (on-demand, minv 0)     │
        │   s3://…/binaries/<ref>/<sha>)    burst (spot)   tune (GPU)       │
        │                                         │                         │
        │  CodeBuild arenabench-runner-image      ▼                         │
        │  (repo → ECR arenabench/runner)   runner container (privileged)   │
        │                                   dockerd-in-docker → harbor task │
        │                                   containers, `arenabench run`    │
        │                                         │                         │
        │      S3 arenabench-artifacts-*  ◄───────┘  DynamoDB `arenabench`  │
        │      binaries/ runs/ datasets/             run + trial metadata   │
        └───────────────────────────────────────────────────────────────────┘
```

Idle cost is storage only (S3/ECR/DynamoDB — cents). Batch compute
environments sit at `minvCpus: 0`: EC2 exists only while jobs are queued,
and there is deliberately **no NAT gateway** — instances live in default-VPC
public subnets behind an ingress-free security group, because a NAT bills
hourly around the clock.

## Runbook

Deploy / update (idempotent):

```bash
aws cloudformation deploy \
  --template-file arenabench/infra/core.yaml \
  --stack-name arenabench-core \
  --capabilities CAPABILITY_NAMED_IAM \
  --parameter-overrides VpcId=<default-vpc> \
      'SubnetIds=<subnet-1>,<subnet-2>,...'
```

Build the Stella binary — tip of `main` by default, any branch on request:

```bash
aws codebuild start-build --project-name arenabench-sut-build            # main
aws codebuild start-build --project-name arenabench-sut-build \
  --environment-variables-override name=GIT_REF,value=my-branch,type=PLAINTEXT
```

Artifacts land at `s3://<bucket>/binaries/<ref>/<sha>/stella` with a
`manifest.json` (+ per-ref `latest.json`), built with `cargo zigbuild
--locked` at glibc floor 2.17 — the same recipe as `sut_build.py`, so the
binary runs in any reasonable task container.

Rebuild the runner image after changing `runner/`:

```bash
aws codebuild start-build --project-name arenabench-runner-image
```

Prove the substrate end to end (scales 0 → 1 instance → 0):

```bash
aws batch submit-job --job-name smoke \
  --job-queue arenabench-measure --job-definition arenabench-trial
```

Run a match: upload an `arenabench.toml` to S3, then submit with overrides —

```bash
aws s3 cp match.toml s3://<bucket>/runs/<run-id>/match.toml
aws batch submit-job --job-name <run-id> \
  --job-queue arenabench-measure --job-definition arenabench-trial \
  --container-overrides '{
    "command": ["run"],
    "resourceRequirements": [{"type":"VCPU","value":"4"},{"type":"MEMORY","value":"15360"}],
    "environment": [
      {"name":"RUN_ID","value":"<run-id>"},
      {"name":"MATCH_S3_URI","value":"s3://<bucket>/runs/<run-id>/match.toml"},
      {"name":"SUT_S3_URI","value":"s3://<bucket>/binaries/main/<sha>/stella"},
      {"name":"SUT_COMMIT","value":"<sha>"}
    ]}'
```

The 12×12 shape is 144 such submissions (or one array job of size 144) —
Batch packs instances up to the queue's compute-environment ceiling and
tears everything down when the queue drains. Provider API keys are read at
trial start from SSM parameters under `/arenabench/` (SecureString;
`/arenabench/anthropic_api_key` → `ANTHROPIC_API_KEY`, etc.).

### Quotas (account limits, not template limits)

| Quota | At deploy time | Needed for 12×12 | Fix |
|---|---|---|---|
| EC2 on-demand Standard vCPU (`L-1216C47A`) | 64 | ~288 (+headroom → 384) | service-quotas increase |
| EC2 Spot Standard vCPU (`L-34B43A08`) | 32 | burst queue only | as needed |
| EC2 G/VT (GPU) vCPU (`L-DB2E81BA`) | 0 | tuning loop | must raise before `arenabench-tune` can place jobs |

Under-quota bursts do not fail — Batch queues trials and works through them
at whatever capacity the quota allows.

## Web plane: arenabench.org

The Next.js app (evolving `../ui/`) deploys to Vercel with arenabench.org
attached; server routes assume `arenabench-vercel-web` via Vercel's OIDC
federation (`AWS_ROLE_ARN` env var — no AWS keys in Vercel). The trust
policy accepts the `arena` and `arenabench` project slugs in team `oxagen`.

Auth is built like a real SaaS but gated to one user while testing:
Auth.js v5 with the Resend email provider (passwordless magic links) and a
database session adapter; sign-in is refused unless the address appears in
`ALLOWED_EMAILS` (currently `mac@macanderson.com`). Adding a teammate later
is an env change; adding OAuth providers later is additive, not a rework.
Every page and API route sits behind the session middleware.

## Self-tuning roadmap (the Opus 5 target)

The loop this infrastructure is built to host: Stella branches itself, digital
clones compete, and the traces train an open-weight model until it beats
frontier baselines — specifically Opus 5.

1. **Branch → build**: each candidate branch becomes a binary via
   `arenabench-sut-build` (`GIT_REF=<branch>`), content-addressed in S3.
2. **Clone vs. clone**: matches seat candidate binaries against the current
   champion and against Claude Code / Opus 5 arms; trials run on
   `arenabench-measure`, traces (`stella-events.jsonl`, trajectories,
   recordings, `results.json`) land under `runs/`.
3. **Dataset export** (to be built; tracked as a follow-up issue): a
   trace→corpus exporter compiles `runs/` into training data under
   `datasets/` — the witness stage's **oracle flip** (fail→pass,
   tamper-excluded) is the reward signal that separates verified solutions
   from claimed ones.
4. **Tune**: fine-tuning jobs run on the `arenabench-tune` GPU queue
   (g6e/g6/g5, zero idle cost; blocked on the GPU quota above). The tuned
   open-weight model is served OpenAI-compatibly and seated as just another
   contestant — closing the loop at step 2.

## Teardown

```bash
aws s3 rm s3://arenabench-artifacts-<account>/ --recursive   # bucket must be empty
aws cloudformation delete-stack --stack-name arenabench-core
```
