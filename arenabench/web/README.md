<!-- SPDX-License-Identifier: Apache-2.0 -->

# ArenaBench web (v0)

The arenabench.org control-plane app: a server-rendered Next.js dashboard
over the AWS substrate provisioned by `../infra/core.yaml`. It lists built
Stella binaries, shows recent trial jobs, and can trigger a binary build
from any git ref or a smoke trial — all through the Vercel OIDC-assumed
role `arenabench-vercel-web`, so no AWS keys exist in Vercel.

**Auth posture (v0)**: the deployment sits behind Vercel Authentication
(team members only) as an interim wall. The real login — passwordless
magic links via Resend, allowlisted to `ALLOWED_EMAILS`, built SaaS-shaped
on Auth.js — is #2100, and replaces the interim wall rather than stacking
on it. The Claude Code credential lifecycle (connect/verify/renew from the
UI) is #2101.

Environment (all optional — defaults target the deployed stack):

| Variable | Default |
| --- | --- |
| `AWS_ROLE_ARN` | `arn:aws:iam::578673726240:role/arenabench-vercel-web` |
| `ARTIFACTS_BUCKET` | `arenabench-artifacts-578673726240` |
| `ARENABENCH_TABLE` | `arenabench` |

Deploy: the Vercel project is `arenabench` (team `oxagen`); OIDC sub claims
for this project are already trusted by the role. Attach `arenabench.org`
in the project's domain settings.
