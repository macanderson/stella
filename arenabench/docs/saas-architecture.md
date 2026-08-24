<!-- SPDX-License-Identifier: Apache-2.0 -->

# ArenaBench as a standalone, multi-tenant SaaS

**Status:** proposed. Nothing in this document is built yet.
**Supersedes:** the Vercel web plane described in `infra/README.md` § "Web
plane: arenabench.org" and issue #2100 (magic-link login on Vercel).
**Scope:** the ejection of ArenaBench from the Stella monorepo, a durable
multi-tenant datastore, an authenticated free SaaS on AWS, and the SDK that
lets someone who has never heard of Stella register an agent, declare the
dimensions they care about, and race it against somebody else's.

---

## 0. The one-paragraph version

ArenaBench today is a local tool with an in-memory index, a Stella-shaped
model catalog, and a Stella-shaped notion of proof. It should be a product:
a free, multi-tenant web app on AWS where anyone signs up, points a runner at
their own machine or their own cloud, and gets a durable, comparable,
publishable record of how their agent performs. The measurement machinery does
not change — it is already artifact-derived and already honest about what it
cannot measure. What changes is where the index lives (DynamoDB, not a Python
dict), who is allowed to read it (a tenant, not everyone with the port open),
and where the Stella-specific parts live (a plugin package, not the core).
The economics force one structural decision, stated up front in §3: **free
and multi-tenant means the control plane is hosted and the execution plane is
brought by the user.**

---

## 1. Why now — the three problems this solves

### 1.1 Benchmark progress for Stella is not actually tracked

`series.py` computes cross-match outcome series with a real comparability key,
and then that series is read from an in-memory registry populated only by
matches *this process* launched (#1707). Restarting `arenabench serve` loses
every match; `arenabench run` never registers one at all. The result is that
the question the benchmark exists to answer — *is Stella getting better,
commit over commit* — is answered today by hand-written scripts pointed at a
job tree, or not answered.

The evidence that this costs real time is in the repository's own hard rules:
matches `4bed110003d8` (5/5) and `61e01ca06fc7` (3/5) differed by ~12 commits
and were read as a one-change regression. A durable store keyed by SUT commit,
with `series.py`'s comparability grouping enforced at query time rather than
at render time, is what turns that from an investigation into a lookup.

### 1.2 ArenaBench cannot be used by anyone who is not us

Seven modules import Stella-shaped assumptions (inventoried in §8). The most
required:

- `catalog.py` parses `crates/stella-model/src/catalog.rs` to populate the
  model select. Without a Stella checkout the select is an error state.
- `sut.py` / `sut_build.py` are 1,268 lines of "clone Stella, resolve a ref,
  `cargo zigbuild` it". There is no other kind of system under test.
- `proof.py` reads Stella's proof rail; two of the eleven scoreboard
  dimensions (`proven`, `claimed_without_proof`) exist only for agents that
  publish one.
- `adapter.py` stages the `stella_harbor` package, and `AdapterUnavailableError`
  is a first-class infrastructure-failure class.

None of this is wrong — it is what made the tool useful — but it means the
`pip install arenabench` in the README lands a user in a tool whose flagship
contestant they cannot seat and whose model list they cannot populate.

### 1.3 There is nowhere to put a result that outlives a laptop

Runs live in `~/.arenabench/matches/<id>/`. Sharing one means sharing a
directory. Comparing yours to mine means trusting a screenshot. A benchmark
whose whole argument is *"a number is only comparable to another number from
the same digest"* has no mechanism for two people to establish that they ran
the same digest.

---

## 2. Goals and non-goals

### Goals

| # | Goal |
|---|---|
| G1 | ArenaBench is its own repository, its own PyPI package, and builds and tests with no Stella checkout present. |
| G2 | Runs and results are durable, queryable, and survive process restarts, machine loss, and the six-month gap between a run and someone asking about it. |
| G3 | arenabench.org is a multi-tenant web app, free to use, deployed 100% to AWS, with signup / email verification / login / password reset / session management / API tokens. |
| G4 | An SDK lets a third party register an agent, declare dimensions, submit runs, and publish results without writing ArenaBench code. |
| G5 | Two tenants can compete: agent A from org X against agent B from org Y, on a shared task list, with an explicit and visible trust tier on the result. |
| G6 | Stella's benchmark progress is a first-class view: solve rate, proven rate and cost by SUT commit, within enforced comparability groups. |
| G7 | Phase 1 changes **no measurement behavior**: the same numbers, from the same artifacts, through the same folds. |

### Non-goals (explicitly, for now)

- **Running other people's benchmark trials on our money.** See §3.
- **Executing tenant-supplied code in our account.** An agent registration is
  a manifest and a container reference; it runs on the tenant's own runner.
- **Replacing `arenabench serve`.** The loopback tool stays, unauthenticated,
  exactly as it is. It gains an optional "push to the cloud" flag, and nothing
  else.
- **A paid tier, billing, or quotas beyond abuse limits.** Free is a
  requirement, not a starting point — which is why §3 is a constraint section
  and not a pricing section.
- **Real-time cross-tenant "live" racing.** Runs are submitted and streamed
  per tenant; a *shared* live race between two orgs' runners is a Phase 4+
  question and probably a bad one.

---

## 3. The constraint that shapes everything

**Free + multi-tenant + hosted execution is not simultaneously satisfiable,
and pretending otherwise would be the design's first lie.**

The arithmetic, from this repository's own infrastructure:

- The Batch job definition sizes one trial at 4 vCPU / 15 GB — one whole
  `m6i.xlarge` per trial, because `assert_sole_dockerd` establishes that two
  dockerds cannot share a Batch host network namespace.
- `m6i.xlarge` on-demand in `us-east-1` is ~$0.192/hour. A Terminal-Bench 2.1
  trial that runs its full allowance is 15–30 minutes wall clock.
- So one trial costs roughly **$0.05–$0.10** in EC2 alone, before S3, data
  transfer, and the verifier's second container.
- The README's own headline shape — 12 agents × 12 tasks — is 144 trials:
  **~$10–$15 of compute for one match.** One enthusiastic user running a
  nightly costs $300–$450/month.

There is no free tier on earth that absorbs that for strangers. The choices
are: charge, cap so hard the product is useless, or move execution.

**The decision: move execution.** ArenaBench splits into two planes.

```
   CONTROL PLANE  (hosted, free, multi-tenant, ours)
   identity · run index · results · leaderboards · artifacts · SDK API
   cost: DynamoDB on-demand + S3 + Lambda + CloudFront — cents per tenant-month
                            ▲
                            │ signed result envelopes, artifact uploads
                            │ (HTTPS, presigned S3, no inbound access)
                            │
   EXECUTION PLANE  (the tenant's, always)
   `arenabench run` on a laptop │ a self-hosted runner │ the tenant's own
   AWS account via the same CloudFormation template in infra/core.yaml
   cost: the tenant's, on the tenant's hardware, with the tenant's API keys
```

This is not a compromise made reluctantly — it is a better security posture
than the alternative. Provider credentials never leave the machine that owns
them. The control plane stores credential *names*, never values, which is
already the invariant `MatchSpec.to_json()` enforces and `test_security.py`
guards. Hosting execution would have required us to hold other people's API
keys, which is the single worst asset a free service can custody.

**Where our Batch substrate fits.** `infra/core.yaml` stays, and stays ours:
it is how *we* run Stella's nightlies, and it is published as a
one-command "bring your own execution plane" template any tenant can deploy
into their own account. A tenant who deploys it gets the same runner, pointed
at the same control plane, billed to themselves.

---

## 4. Target architecture

### 4.1 Topology

Everything below is AWS, `us-east-1`, account 578673726240 today. The Vercel
plane is retired (§9.4).

```
                         Route 53  arenabench.org
                              │
                        ACM + CloudFront
              ┌───────────────┴────────────────┐
              │                                │
      S3 (static site)              API Gateway HTTP API  /v1/*
      the Next.js export            └── Lambda (python3.13, arenabench.api)
      from ui/ — unchanged                    │
      build, new deploy target                │  Cognito JWT authorizer
                                              │
              ┌───────────────────────────────┼─────────────────────┐
              │                               │                     │
   Cognito User Pool              DynamoDB `arenabench`        S3 artifacts
   signup / verify / login        single table, PK/SK,         runs/<tenant>/
   reset / MFA-ready              GSI1 + GSI2                  presigned only
   SES for mail                   tenant-prefixed keys         lifecycle rules
              │
   Lambda Function URL (RESPONSE_STREAM)  ── SSE for live runs
              │
   EventBridge  ── scheduled Stella nightly → the tenant-owned Batch plane
```

**Why these services, briefly, because each one is a decision:**

- **Cognito** rather than Auth.js/Resend (#2100's plan). Signup, email
  verification, password reset, password policy, token rotation, account
  recovery, and rate limiting are the "all the normal stuff" the request asks
  for, and Cognito is the AWS-native version of all of it — 50,000 MAUs free,
  which is the entire product for years. It also gives us a JWT authorizer on
  API Gateway with no custom code, so the authorizer cannot have a bug we
  wrote. Federated providers (GitHub, Google) are additive later.
- **API Gateway HTTP API + Lambda** rather than ECS/Fargate. The control plane
  is request/response over DynamoDB and S3; it has no long-running work
  (execution is elsewhere, §3). Lambda scales to zero, which is the same
  property `minvCpus: 0` buys on the Batch side. The handlers are the existing
  pure functions — `telemetry.py`, `series.py`, `proof.py`, `pricing.py` are
  already I/O-free folds over parsed artifacts.
- **Lambda Function URL with response streaming** for the SSE endpoint. HTTP
  APIs cannot stream; Function URLs can, and CloudFront can front them. This
  preserves `GET /api/matches/<id>/stream` semantics rather than degrading the
  live view to polling. If streaming proves unreliable at the CloudFront edge,
  the documented fallback is a WebSocket API — but polling is not a fallback,
  because the live race *is* the product.
- **S3 + CloudFront for the client.** `ui/` is already a static export with no
  SSR (`arenabench serve` serves it as plain files). It deploys unchanged.
  This is the single largest piece of luck in this migration: the web app does
  not need rewriting, only rehosting and re-pointing at `/v1`.
- **DynamoDB single table.** Already provisioned (`arenabench`, PK/SK, GSI1,
  PAY_PER_REQUEST) and already written to by the Batch runner entrypoint. The
  access patterns are all "one tenant's things, newest first", which is what a
  single table with a tenant-prefixed partition key is for.

### 4.2 The measurement machinery does not move

This must be said precisely, because it is the required claim of Phase 1.

Every number ArenaBench reports is derived from files a run already wrote —
`result.json`, `stella-events.jsonl`, trajectory files, `flip.json`. The
control plane does not receive numbers. It receives **artifacts**, and it runs
the *same folds* (`telemetry.MetricsReader`, `proof.distill`,
`pricing.price`, `series.match_row`) over them, in Lambda, that
`arenabench serve` runs locally. A run's scoreboard is therefore reproducible
from its artifacts by anyone who has them, including the tenant, forever.

The corollary is the integrity property that makes cross-tenant leaderboards
possible at all: **the platform derives, the runner supplies evidence.** A
runner that posts `solve_rate: 1.0` is ignored; a runner that uploads a
`result.json` whose verifier rewards sum to 1.0 is believed exactly as much as
its trust tier (§7.3) says it should be.

---

## 5. Data model

One DynamoDB table, `arenabench`, extended from the shape `infra/core.yaml`
already provisions. `PK`/`SK` strings, `GSI1PK`/`GSI1SK` present, plus a new
`GSI2PK`/`GSI2SK` for boards and SUT timelines.

### 5.1 Items

| Entity | PK | SK | GSI1PK / GSI1SK | Notes |
|---|---|---|---|---|
| User | `USER#<uid>` | `PROFILE` | `EMAIL#<lower(email)>` / `USER` | `uid` is the Cognito `sub`. Email GSI is for support lookups, not auth. |
| Tenant | `TENANT#<tid>` | `PROFILE` | `SLUG#<slug>` / `TENANT` | Slug is the public URL segment. |
| Membership | `TENANT#<tid>` | `MEMBER#<uid>` | `USER#<uid>` / `TENANT#<tid>` | Role: `owner` \| `admin` \| `member` \| `viewer`. GSI1 answers "my orgs". |
| API token | `TENANT#<tid>` | `TOKEN#<token_id>` | `TOKHASH#<sha256>` / `TOKEN` | Only the SHA-256 is stored. Scopes + `last_used_at`. |
| Agent | `TENANT#<tid>` | `AGENT#<slug>` | `PUBAGENT#<visibility>` / `<tid>#<slug>` | The registered agent manifest (§6.2). |
| Dimension | `TENANT#<tid>` | `DIM#<key>` | — | Declarative extractor (§7.1). |
| Board | `TENANT#<tid>` | `BOARD#<board_id>` | `PUBBOARD` / `<board_id>` | A leaderboard definition (§7.2). |
| Run | `TENANT#<tid>` | `RUN#<ts>#<run_id>` | `RUNS#<tid>` / `<ts>` | The redacted `MatchSpec` + status + provenance. `SK` sorts newest-last natively. |
| Seat aggregate | `RUN#<run_id>` | `SEAT#<seat_id>` | — | Per-contestant dimension values, derived. |
| Trial | `RUN#<run_id>` | `TRIAL#<seat>#<task>#<attempt>` | — | Per-trial metrics, verdict, proof rung, artifact keys. |
| Board entry | `BOARD#<board_id>` | `ENTRY#<seat_ref>` | GSI2: `BOARD#<id>` / `<zero-padded score>#<run_id>` | Materialized on run completion. |
| SUT timeline | `SUT#<tid>#<project>` | `POINT#<committed_at>#<run_id>` | GSI2: `SUTLINE#<tid>#<project>` / `<committed_at>` | **G6.** One point per run, carrying the comparability digest. |

Artifacts live at `s3://<bucket>/runs/<tenant>/<run_id>/...`, mirroring the
existing `runs/<run-id>/` layout the Batch entrypoint already writes, with the
tenant segment inserted. Existing keys are grandfathered under a
`runs/_legacy/` prefix rather than rewritten.

### 5.2 Tenant isolation is a chokepoint, not a discipline

Every read and write goes through one module (`arenabench/api/store.py`) whose
public functions take a `TenantId` as their first argument and construct every
key from it. No handler builds a `PK` string. This is the same
ports-not-concretions split the rest of the tree holds, and it buys a witness
test that is worth more than a policy: a test that walks every handler's AST
and fails on a string literal beginning `TENANT#`, `RUN#` or `USER#` outside
`store.py`.

Defense in depth beyond the chokepoint:

1. The Cognito JWT carries `custom:tenants` (a claim listing tenant ids); the
   authorizer rejects a request whose path tenant is not in the claim before
   any handler runs.
2. `dynamodb:LeadingKeys` condition on the Lambda execution role is **not**
   used — it cannot express "one of N tenants" for a shared-Lambda design, and
   an IAM policy that looks like isolation but is not is worse than none.
   Stated here so nobody adds it later believing it works.
3. Presigned S3 URLs are minted per object, 15-minute expiry, and only for
   keys under the caller's tenant prefix.

### 5.3 Why not one table per tenant, or one account per tenant

Both are real multi-tenancy patterns and both are wrong here. Free means the
marginal cost of a tenant who signs up and never returns must be
approximately zero; a per-tenant table is 25 GB of free-tier accounting and an
operational unit per curious visitor. The blast radius argument that favors
isolation applies to *data at rest we cannot afford to leak*, and by §3 we
deliberately hold none of that — no credentials, no source, only benchmark
artifacts the tenant chose to upload.

---

## 6. Identity, tenancy, and the SDK

### 6.1 Auth flows (Phase 1 — "all the normal stuff")

| Flow | Mechanism |
|---|---|
| Sign up | Cognito `SignUp` with email + password; password policy 12+ chars, no composition rules (NIST 800-63B). |
| Verify email | Cognito confirmation code via SES from `no-reply@arenabench.org`. Unverified accounts cannot create runs. |
| Log in | Cognito SRP (`USER_SRP_AUTH`) — the password never crosses the wire, even to Cognito. |
| Reset password | `ForgotPassword` → code → `ConfirmForgotPassword`. |
| Change password / email | Authenticated Cognito calls; email change re-verifies. |
| Session | Access token 1h, refresh token 30d, refresh rotation on. Tokens in memory + an `HttpOnly; Secure; SameSite=Lax` refresh cookie scoped to `arenabench.org`. |
| Delete account | Self-service; cascades to owned tenants with no other owner, per §11.4. |
| MFA | TOTP, off by default, available from day one because Cognito gives it free. |
| Federated | GitHub/Google via Cognito identity providers — Phase 3, additive. |

**Tenant provisioning.** First login creates a personal tenant named for the
user (`slug = <username>`); a user may create additional tenants and invite
members by email. There is no "no tenant" state, because every object in §5.1
hangs off one.

### 6.2 The SDK

Two packages, Python first (the runner is Python; the audience writes Python).

```
arenabench            the CLI + local server + measurement folds   (exists)
arenabench-sdk        the client: auth, registration, submission   (new)
```

```python
from arenabench import Client, Agent, Dimension

ab = Client()                        # ARENABENCH_TOKEN, or `arenabench login`

# 1. Register an agent. This is a manifest, not code we run.
ab.agents.register(Agent(
    slug="my-agent",
    title="My Agent",
    launch=Agent.HarborImport("my_pkg.harbor:MyAgent"),   # or Harbor built-in,
                                                          # or a container ref
    honours=["model", "effort", "base_url"],   # what it *actually* applies
    credentials=["OPENAI_API_KEY"],            # names only, forever
    visibility="public",                       # private | unlisted | public
    challengeable=True,
))

# 2. Declare a dimension you care about. Declarative: no code runs server-side.
ab.dimensions.declare(Dimension(
    key="tool_calls",
    label="Tool Calls",
    direction="lower",
    unit="calls",
    source="custom.tool_calls",     # a key your adapter writes to
                                    # <trial>/arena/metrics.json
    aggregate="sum",
    blurb="tool invocations across the trial",
))

# 3. Run locally; results land in the cloud.
#    (Or: `arenabench run match.toml --publish`)
run = ab.runs.submit_local("match.toml")
run.wait()
print(run.url)          # https://arenabench.org/<tenant>/runs/<id>
```

**The registration contract, stated as a boundary.** An agent registration
carries: how Harbor launches it, which engine knobs it honours, which
credential names it needs, and optionally a proof reader (§8). ArenaBench
never receives the agent's source, never executes it, and never holds its
keys. `honours` remains the honest-reporting seam it already is: a knob a
tenant sets that the agent does not apply is reported on the seat, because
"two arms that were secretly identical" is still the one failure a head-to-head
cannot survive.

### 6.3 API surface (`/v1`)

```
POST   /v1/auth/*                     thin proxies to Cognito where the SPA
                                      needs a server-side secret; otherwise
                                      the browser talks to Cognito directly
GET    /v1/me                         profile + tenant memberships
POST   /v1/tenants                    create; GET /v1/tenants/{t}
POST   /v1/tenants/{t}/tokens         mint an SDK token (value shown once)
GET    /v1/tenants/{t}/agents         CRUD over registered agents
GET    /v1/tenants/{t}/dimensions     CRUD over declared dimensions
POST   /v1/tenants/{t}/runs           create a run → run_id + upload grants
POST   /v1/runs/{id}/artifacts        presign a batch of object keys
POST   /v1/runs/{id}/complete         seal it: artifact manifest + SHA-256s
GET    /v1/runs/{id}                  scoreboard, derived server-side
GET    /v1/runs/{id}/stream           SSE (Function URL), live trials
GET    /v1/runs/{id}/trials/{...}     per-trial metrics, transcript, files
GET    /v1/boards/{id}                a leaderboard
GET    /v1/tenants/{t}/suts/{proj}    the SUT timeline — G6
POST   /v1/challenges                 cross-tenant match request (Phase 4)
```

The local server's `/api` routes keep their shapes; `/v1` is the same
vocabulary with a tenant segment and an `Authorization: Bearer`. The client's
`lib/api.ts` gains a base-URL + auth-header seam and nothing else.

---

## 7. Dimensions, boards, and trust

### 7.1 Custom dimensions without arbitrary code

A dimension is `(key, label, direction, unit, source, aggregate, blurb)`. The
existing eleven in `model.py` become the built-in set; a tenant may declare
more. Two rules make this safe and comparable:

**No user code runs on our compute.** `source` is a dotted path into a typed
record — either a field of `TrialMetrics` or a key under `custom.*`, which is
a flat `{string: number}` bag an adapter writes to `<trial>/arena/metrics.json`.
`aggregate` is one of `sum | mean | min | max | rate(num,den) | count_if(...)`.
That is an expression language with no loops, no I/O, and no eval.

**The unmeasured crown nobody.** This is `model.py`'s existing discipline,
promoted to a rule the API enforces: a dimension with no value for a seat
aggregates to `None`, and a `None` never wins. A custom dimension that only
one contestant emits is displayed for that contestant and crowns no one —
because the alternative is crowning whichever seat happens to spell
"unmeasured" as zero, which is exactly the trap `wasted_time`, `proven` and
`claimed_without_proof` were designed around.

A dimension declared by tenant X is visible on tenant X's boards. Promoting a
dimension to a *shared* board requires it to be declared identically by every
participating tenant — a hash of `(source, aggregate, unit, direction)` must
match, or the board refuses the entry and says which field differs.

### 7.2 Boards

A board is a saved query: `(dataset digest, task list digest, dimension,
trust tier floor, visibility)`. Entries are materialized on run completion
into GSI2 sorted by score. The dataset digest is part of the key and not
optional, because "every number ArenaBench reports is only comparable to
another number from the same digest" stops being a README sentence and starts
being a foreign key.

### 7.3 Trust tiers — the honest part

A result the platform did not execute is a claim. Rather than pretend
otherwise, every run carries a tier, always displayed, never inferred:

| Tier | Meaning |
|---|---|
| `self-reported` | Artifacts uploaded by a runner we do not control. Metrics are re-derived from those artifacts, so they are internally consistent — but the artifacts themselves could have been authored. |
| `attested` | Produced by a runner that authenticated with a tenant token *and* whose artifact manifest hashes match what was uploaded, on a `sut_ref` that resolves to a public commit. Tampering now requires forging a coherent event stream. |
| `platform-executed` | Ran on ArenaBench's own Batch substrate (ours, or a tenant's deployment of `infra/core.yaml` registered with an execution-role handshake). |
| `replayed` | An independent execution of the same digest + task list + seat signature reproduced the headline dimension within tolerance. |

Default board filter is `attested` and above. A public board may require
`replayed`. This is the ArenaBench analogue of Stella's witness contract:
**a result is proven or it is labelled unproven — it is never quietly
promoted.** Anything less and the first time a leaderboard matters, it stops
being worth anything.

### 7.4 Cross-tenant competition (Phase 4)

A *challenge* is a match spec that seats another tenant's public,
`challengeable` agent. Mechanics:

1. The challenger supplies the execution plane and their own credentials for
   every seat, including the opponent's (the opponent's manifest declares the
   credential names; nobody's keys are shared).
2. The opponent's agent is launched from its published manifest — a Harbor
   built-in, an import path from a public package, or a container digest.
3. The result is written to both tenants and labelled with its tier. The
   opponent may dispute it, which flags the entry and requests a `replayed`
   run; a disputed entry is visibly disputed until replay resolves it.
4. An agent's owner can revoke `challengeable` at any time; existing results
   remain, because retracting history is how a leaderboard becomes a lie.

---

## 8. De-Stella-ing: the plugin boundary

The core keeps everything that is a fact about *benchmarking*. Everything that
is a fact about *Stella* moves into an **agent pack** — a pip-installable
package the core discovers via entry points (`arenabench.agents`).

| Coupling | Today | After |
|---|---|---|
| `agents.py` Stella entry | Hard-coded `AgentSpec` | Contributed by the `arenabench-stella` pack |
| `catalog.py` (148 ln) | Parses `crates/stella-model/src/catalog.rs` and `bench/harbor_adapter/.../posture.py` | `ModelCatalogProvider` port. Core ships a static provider-keyed catalog; a pack may contribute its own. The Rust parser moves into the pack, where a crate move breaks the pack rather than the product. |
| `sut.py` + `sut_build.py` (1,268 ln) | Clone Stella, resolve ref, `cargo zigbuild` | `SutBuilder` port keyed by a `[sut]` block: `kind = "none" \| "git+command" \| "container"`. The Stella recipe (`cargo zigbuild --locked`, glibc 2.17 floor) becomes the pack's builder. The pin/drift discipline — `MAX_BEHIND_UNPINNED`, refuse-on-drift, never fall back to a `PATH` lookup (#2098) — is **core**, because it is a fact about measurement, not about Rust. |
| `adapter.py` | Stages `stella_harbor` | Generic pack staging: content-addressed copy of any pack's adapter directory. `AdapterUnavailableError` stays core. |
| `proof.py` (492 ln) | Reads Stella's proof rail | A pack may register a `ProofReader`. The `proven` / `claimed_without_proof` dimensions stay core and stay `None` for agents with no reader — which is already exactly what they do. |
| `presets.py` | Stella-shaped quick starts | Core presets are agent-agnostic (oracle vs nop, and the free smoke test); the pack contributes Stella presets. |
| `pricing.py` | Shared price table | Stays core. Provider pricing is not Stella-specific, and one table for all seats is the whole point. |
| `harbor_agent.py` | ArenaBench's Stella adapter subclass | Moves wholesale into the pack. |

**Licensing falls out cleanly.** ArenaBench is Apache-2.0; Stella's Harbor
adapter is AGPL-3.0-only and already distributed separately. The pack is the
natural home for the AGPL half, so the core never vendors it — which is the
posture the README already claims and this makes structural.

**The witness for G1** is a CI job that installs `arenabench` into a container
with no Stella checkout, no `ARENABENCH_STELLA_*` variables, and no Rust
toolchain, and runs a full oracle-vs-nop match to a scoreboard. It fails on
`main` today (the model select 500s and presets reference an absent pack).

---

## 9. The ejection

### 9.1 The split

New repository: **`macanderson/arenabench`**, public, Apache-2.0. It does not
exist yet (checked). PyPI name `arenabench` must be claimed before the first
release; if taken, the fallback is `arenabench-cli` with the CLI still spelled
`arenabench`.

History is preserved for the subtree:

```bash
git clone https://github.com/macanderson/stella arenabench-split
cd arenabench-split
git filter-repo --path arenabench/ --path-rename arenabench/:
# `arenabench/arenabench/` becomes `arenabench/` — the package at the root,
# which is exactly what pyproject.toml already expects.
```

`infra/core.yaml`'s `PkgPath` parameter was written for this: `arenabench`
today, `.` post-ejection. The nested `.github/workflows/ci.yml` was written to
be inert inside the monorepo and to become the whole gate at the split with no
edits. Both were deliberate; both should be used as intended.

### 9.2 What the monorepo must clean up

Nothing here is optional — each one is a dangling reference the moment the
folder leaves.

| Site | Problem | Resolution |
|---|---|---|
| `.github/workflows/bench.yml` | "arenabench — pytest (until ejection)" step, and the path filter `^(bench/\|arenabench/\|...)` | Drop the step; drop `arenabench/` from the filter. |
| `Makefile` `run-arena`, `kill-arena`, `run-match` | Wrap `scripts/arena-run.sh` / `arena-kill.sh` / `arena-local.sh` | Keep the targets; the scripts learn to use an installed `arenabench` (uv tool / pip) instead of `$ROOT/arenabench`. |
| `scripts/arena-run.sh` | Asserts `$ROOT/arenabench` exists; builds a PYTHONPATH containing both `arenabench` and `stella_harbor` | Resolve `arenabench` from the environment; keep the `stella_harbor` half, which is genuinely Stella's. The diagnostic that distinguishes "arenabench importable but harbor missing" is the valuable part — keep it. |
| `scripts/arena-local.sh`, `scripts/arena_local.py` | Same `$ROOT/arenabench` assertion and PYTHONPATH as `arena-run.sh`; the Python half additionally *imports* `arenabench.config`, `.credentials`, `.claude_oauth`, `.model` and `.sut`, and replays `cli.py`'s credential fill order | Resolve `arenabench` from the environment like `arena-run.sh`. The replayed fill order is a **behavioural coupling, not an import one**: it exists to report which credential reaches which seat, and it is wrong the moment ArenaBench changes those layers. Post-ejection it must either call one published helper or be deleted — see #2654, which is the same coupling already disagreeing with itself inside this repo. |
| `scripts/check-role-names.sh` | Reads `arenabench/arenabench/model.py` and `harbor_agent.py` to enforce role-name parity | The guard splits: Stella's half stays; the arenabench half moves to the new repo's gate. The parity between them becomes a **cross-repo wire contract** — the role names are part of the adapter's published interface, so the Stella-side guard checks the *adapter* (`bench/harbor_adapter`), which is the boundary that actually crosses. |
| `scripts/reap-seats.sh` | Documents arenabench's process shapes | Comments only; retarget the prose. |
| `docs/spec/agent-monitor-protocol.md` | Normative for `arenabench watch --format jsonl`, cited from the ArenaBench README | Stays in Stella (it is a Stella spec that ArenaBench implements). Post-ejection ArenaBench cites it **by URL**, per this repo's own citation rule for anything outside the tree. |
| `crates/stella-model/src/catalog.rs`, `bench/harbor_adapter/.../posture.py` | Parsed by `catalog.py` | Both readers move into the Stella agent pack (§8). |
| `scripts/file-size-baseline.txt` | Carries arenabench Python offenders | Regenerate with `make file-size-update` in the ejection PR, as its own commit. |

### 9.3 Issue migration

72 open issues in `macanderson/stella` have `arenabench` in the title. GitHub
transfers issues within an owner and leaves redirects. The ejection PR should
not land before the transfer plan is agreed, because a transferred issue keeps
its body and loses cross-references to Stella PRs that closed adjacent work.
Recommendation: transfer everything whose title starts `arenabench`, keep
`bench:`-prefixed ones (they are about Stella's use of the benchmark), and
comment on each transferred issue with a link back to its original number.

### 9.4 The Vercel plane is retired, not migrated

`web/` (Next.js + AWS SDK + `@vercel/functions`, 11 files) exists as a v0
control-plane app behind Vercel Authentication, and #2100 plans magic-link
auth on it. That whole plane is superseded: "100% deployed to AWS" is the
requirement, and Vercel is not AWS. `web/` is deleted in Phase 1d, its two
useful behaviors (list SUT builds, trigger a build from a ref) become `/v1`
routes, and #2100 is closed as superseded with a pointer here. The
`VercelOidcProvider` / `VercelWebRole` resources in `core.yaml` are removed in
the same change — including the open question in #2254 about SecureString
access, which the removal answers by deletion.

---

## 10. Phasing

Each phase is independently shippable and independently valuable. Phase 1 is
the one the request scopes as "no change in functionality today".

### Phase 0 — Eject (no behavior change)

- New repo with filtered history; nested CI becomes live; monorepo cleanup
  (§9.2) lands as one PR in Stella and one in ArenaBench, in that order.
- Claim PyPI; publish `0.1.0` from the new repo, wheel including the built
  `web/` export.
- Transfer issues (§9.3).
- **Witness:** `pip install arenabench` in a clean container runs
  `arenabench datasets` and the oracle-vs-nop smoke match to a scoreboard.

### Phase 1 — Durable store + auth (the scoped ask)

- **1a — Storage port + local backend.** Extract a `RunStore` port; implement
  it over the filesystem/SQLite so `arenabench serve` discovers past matches
  on disk. This *is* #1707, and it ships value with no cloud attached. The
  redaction rule (`MatchSpec.to_json()` never persists `Contestant.env`) is
  enforced by the existing security test.
- **1b — DynamoDB backend + `/v1` API.** The same port, over the table in
  §5.1, behind API Gateway + Lambda. Metrics are re-derived in Lambda from
  uploaded artifacts (§4.2) — no new folds, no new numbers.
- **1c — Cognito auth.** Signup, verification, login, reset, sessions, API
  tokens, personal tenant provisioning. Every `/v1` route authorized; the
  local server unchanged and unauthenticated.
- **1d — Web app on AWS.** `ui/` static export to S3 + CloudFront at
  arenabench.org; `lib/api.ts` gains base-URL + auth seams; `web/` and the
  Vercel resources are deleted.
- **1e — `--publish`.** `arenabench run match.toml --publish` uploads
  artifacts and seals the run. Local-only remains the default.
- **Witness:** a run published from a laptop is visible at
  `arenabench.org/<tenant>/runs/<id>` after the laptop is closed; a second
  tenant gets 404 for the same URL; the scoreboard rendered from DynamoDB is
  byte-identical to the one `arenabench serve` renders from the same
  artifacts.

### Phase 2 — Standalone + SDK

- The plugin boundary (§8); `arenabench-stella` published from the Stella repo.
- `arenabench-sdk` with agent registration and run submission.
- **Witness:** the no-Stella container job in §8.

### Phase 3 — Dimensions, boards, trust tiers

- Declarative dimensions (§7.1), boards (§7.2), tiers (§7.3), redaction review
  before any run becomes public (§11.2).
- **G6 lands here too:** the SUT timeline view — solve rate, proven rate and
  priced cost by commit, grouped by `series.py`'s comparability key, with
  non-comparable points visibly disconnected rather than joined by a line.

### Phase 4 — Cross-tenant challenges + BYO execution plane

- Challenges (§7.4); a one-command deploy of `infra/core.yaml` into a tenant's
  own account, registered with the control plane for `platform-executed`
  status.

---

## 11. Security and privacy

### 11.1 Credentials

Provider API keys never reach the control plane. Not encrypted, not
write-only, not "in a vault" — **not present**. The existing screening
(`is_credential_name`, `screen_env`, the `PATH`/`LD_PRELOAD`/`DOCKER_HOST`
refusals) stays exactly where it is: in the process that spawns the seat,
which is on the tenant's machine.

### 11.2 Artifacts can contain secrets, and this is the sharp edge

A transcript is a verbatim record of what an agent did, and agents `echo $VAR`,
print environments, and paste config files. Today that artifact never leaves
the machine that produced it. In a SaaS where runs can be made public, it is a
credential-disclosure vector with a URL.

Mitigations, all required before any run can be public:

1. A redaction pass on upload — high-entropy strings, known key prefixes
   (`sk-`, `sk-ant-`, `sk-or-`, `ghp_`, AWS access key IDs), and any value the
   run's own `required` env names would have held.
2. Public visibility is an explicit, per-run action with a diff-style preview
   of what will be exposed. Never a default, never inherited from the tenant.
3. GitHub-style secret scanning on the sealed artifact set, with the run
   auto-unpublished and the owner notified on a hit.

### 11.3 Abuse limits on a free service

Per-tenant: 50 runs/day, 500 MB artifacts/run, 10 GB/tenant, 25 registered
agents, 5 API tokens. Anonymous read of public runs is CloudFront-cached and
rate-limited by WAF. Every limit is a number in one config module, because a
free tier's limits change and hunting them across handlers is how a limit
becomes wrong.

### 11.4 Deletion

Account deletion removes the user, transfers or deletes owned tenants, and
S3-deletes the tenant prefix. Published board entries are anonymized rather
than removed — a leaderboard that silently loses entries is not a record.
Stated so the tension is a decision and not a surprise.

---

## 12. Cost model

Steady state, control plane only, order-of-magnitude:

| Service | Driver | Monthly at 1,000 tenants / 10,000 runs |
|---|---|---|
| DynamoDB (on-demand) | ~1M writes, ~10M reads | ~$5–15 |
| S3 | 200 GB artifacts, lifecycle to IA at 90d | ~$5 |
| Lambda | ~5M invocations, mostly <200ms | ~$5 |
| CloudFront + WAF | ~500 GB egress | ~$50 |
| Cognito | 1,000 MAU | $0 (50k free) |
| SES | ~5,000 mails | ~$1 |
| Route 53 + ACM | 1 zone | ~$1 |

**Under $100/month at a thousand tenants** — because the expensive thing is
not hosted. That is the whole argument of §3, restated as a number. The
existing Batch substrate keeps costing what it costs today, for our runs only,
and remains at `minvCpus: 0` when idle.

---

## 13. Open questions for the maintainer

These change the work materially and are not mine to decide:

1. **PyPI name.** Claim `arenabench` now, before the ejection PR? If it is
   taken, which fallback?
2. **Issue transfer.** Move all 72, or fork the list (§9.3)? Transfer is
   irreversible and rewrites numbers referenced from Stella PR bodies.
3. **Domain and account.** arenabench.org currently sits on Vercel DNS. Move
   the zone to Route 53, or keep DNS where it is and point records at
   CloudFront? And: does the SaaS live in account 578673726240 alongside the
   Batch substrate, or in its own account with the substrate cross-account?
   (Recommendation: separate account. A free public app and our measurement
   fleet should not share a blast radius or a bill.)
4. **`self-reported` on public boards.** Allowed at all, or is `attested` the
   floor for anything public? (Recommendation: `attested` floor.)
5. **The `arenabench serve` posture after auth exists.** Stay unauthenticated
   loopback forever (recommended), or gain an optional local login so one
   machine can serve a small team?
6. **Stella's nightly.** Does Stella's own progress tracking (G6) run through
   the public control plane as a normal tenant — eating our own dog food and
   proving the SDK — or through a private path? (Recommendation: normal
   tenant, public runs.)

---

## 14. Risks

| Risk | Why it bites | Mitigation |
|---|---|---|
| **Ejection strands the monorepo's arena tooling** | `make run-arena` is how matches actually get launched here; a broken script means nobody runs benchmarks for a week | §9.2 is a checklist in the ejection PR, and the PR is not merged until `make run-arena` works from a clean clone against an installed `arenabench` |
| **The plugin boundary is drawn wrong** | If `proof.py` or the SUT pin discipline lands in the pack, the core loses the honesty machinery that makes it worth using | §8 assigns each: measurement discipline is core, Stella recipes are the pack. Reviewed as a boundary, not as a refactor |
| **SSE through CloudFront + Lambda streaming is flaky** | The live race is the product; a degraded live view is a degraded product | Prototype 1b's stream endpoint first, before the rest of the API. WebSocket API is the documented fallback; polling is not |
| **A leaked key in a public transcript** | A privacy incident with our name on it | §11.2 — redaction, explicit publication, scanning, auto-unpublish |
| **Free tier abused as generic compute** | We hold no compute, so mostly N/A — but artifact storage is real | §11.3 quotas; artifacts are the only writable surface |
| **Cross-tenant leaderboards without replay become theatre** | Unverifiable claims sorted into a ranking is worse than no ranking | Trust tiers are displayed always and default-filtered (§7.3) |
| **Phase 1 quietly changes numbers** | The whole "no functionality change" claim | The scoreboard-equality witness in Phase 1's DoD: same artifacts → byte-identical scoreboard, local vs cloud |

---

## 15. What this document does not answer

- The exact `TrialMetrics` schema the `custom.*` bag hangs off. It should be
  frozen and versioned before the SDK ships, or every custom dimension breaks
  on the first internal change.
- Whether Harbor itself should be an optional dependency (a tenant with a
  container-only agent and a non-Harbor dataset has no use for it). Probably
  yes, eventually; out of scope here.
- The TypeScript SDK. Same wire contract, later.
- Migration of the ~dozen historical local matches on maintainer machines.
  Probably: a `arenabench publish <match-dir>` verb, which falls out of 1e
  anyway.
