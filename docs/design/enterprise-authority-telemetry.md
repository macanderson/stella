# Enterprise Authority and Telemetry

Status: approved for implementation

## Purpose

Stella remains a local-first, provider-neutral execution engine. Oxagen
Enterprise adds the governed control plane: principal identity, tenant policy,
approval, lineage, audit, retention, and operational telemetry. Community
Stella never sends telemetry by default. Enterprise export exists only after a
managed, signed enrollment is installed outside the repository.

This design implements the first two delivery phases:

- Phase 0 closes authority and verification bypasses.
- Phase 1 makes budget, context, privacy, and enterprise operational telemetry
  reliable enough for an enrolled deployment.

## Non-negotiable invariants

1. Repository content is evidence, never authority. An untrusted repository
   cannot enable tools, replace privileged prompts, register executable custom
   tools, approve spend, approve scope, or configure telemetry.
2. Effective authority is the intersection of the built-in default, managed
   ceiling, explicit repository trust, session/host grant, and role-specific
   restriction. Lower-precedence input may narrow authority but never widen it.
3. Machine output format never changes approval policy.
4. A verifier has less authority than a worker. Witness authoring and baseline
   execution happen in a disposable snapshot and never in the user's tree.
5. Model output is never interpreted as an unrestricted shell program.
6. `Completed` means verification passed or verification was not required.
   Failed, aborted, cancelled, and indeterminate outcomes are distinct.
7. Every settled model call contributes to the returned and persisted cost,
   including calls made before an abort.
8. Local raw execution data stays local. Enterprise export is a strict,
   content-free schema derived from a finalized local execution rollup.
9. Operational telemetry is bounded and fail-open. Compliance audit delivery
   is not claimed by this phase and cannot be enabled accidentally.
10. The core engine remains I/O-free; transports and persistence remain ports
    and adapters.

## Authority model

`AuthorityPolicy` is computed once while loading settings. Only the managed
settings file may define a ceiling. Project scope is untrusted unless the user
explicitly enables repository trust, and managed denial always wins.

Untrusted project scope may retain cosmetic provider metadata and may narrow
an already granted capability. It may not:

- enable `bash`, web, process, paid media, or other effectful tools;
- replace agent system prompts;
- load workspace custom tools, commands, agents, skills, memories, or rules as
  privileged instructions;
- configure or redirect enterprise telemetry.

### Paid-media host-data isolation

An approving `MediaSpendGate` is necessary but not sufficient. The registry
constructs approving image/video tools only when the host also supplies a
retry-stable operation ID source, the host-owned operation journal, the
managed approval ceiling, and `HostDataIsolation::ProcessFree`. That isolation
mode removes every built-in process-launching, process-control, delegation,
and process-backed issue tool from the same registry, including fixed-command
search and repository helpers.

Hosts must not add arbitrary-process MCP or custom tools around that registry.
The shipping local CLI supplies neither an approving spend gate nor the
process-free attestation, so paid generation remains fail-closed. Keeping the
journal outside the workspace and validating paths are secondary defenses;
they are not treated as process confinement.

## Verification model

Witness preparation uses the existing candidate-workspace abstraction. When
authored witnesses are enabled, even a single candidate runs in a disposable
snapshot. The witness author, baseline test, worker, revision, and final test
all observe that snapshot. Only a passing candidate can be adopted.

Test execution uses a typed invocation containing a program and argument
vector. Shell operators, redirection, interpolation, and pipelines are not a
test protocol. Existing free-form commands remain available only as explicit,
user-supplied legacy configuration and require host approval.

## Enterprise operational telemetry

The local SQLite store remains authoritative. After an execution is finalized,
Stella derives one `StellaOperationalEventV1` containing bounded identifiers,
outcome, timing, token/cost totals, tool-call counts, and aggregate file-change
counts. Its type has no fields capable of carrying prompts, source, paths,
arguments, results, reasoning, errors, git metadata, memories, or rules.

Enrollment is managed-only and signed. It binds issuer, audience, enrollment,
organization, workspace, endpoint, credential environment reference, allowed
event classes, issue time, and expiry. The issuer verification key is installed
through managed configuration, never supplied by a repository or the endpoint.

Events enter a separate bounded SQLite spool after local finalization. Delivery
is at-least-once with deterministic event IDs. Startup, shutdown, and
`stella telemetry flush` attempt delivery. Failure remains locally visible and
never changes the agent outcome. A full spool may evict the oldest operational
event and increments a durable drop counter.

`compliance_audit` enrollment is rejected in this phase. Compliance delivery
requires a non-evicting ledger, server receipts, retention/hold semantics, and
an explicit managed fail-closed rule.

## Error semantics

- Policy denial is typed and names the source of the ceiling.
- Headless scope review returns `ScopeReviewRequiredHeadless`.
- Verification failure returns `PipelineStatus::VerificationFailed`.
- Budget aborts retain settled spend and stop before another paid call.
- Telemetry enrollment, spool, and delivery failures are observable but do not
  fail an agent turn.
- Unsupported compliance enrollment is a configuration error, not a silent
  downgrade to operational telemetry.

## Acceptance evidence

- Every behavior change has a test observed failing before implementation.
- Narrow crate tests pass after each task.
- The full `make gate` passes before push.
- GitHub CI passes on the PR head.
- A whole-branch security and correctness review has no Critical or Important
  findings.
