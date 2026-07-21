# Task 7 report: enrolled enterprise operational telemetry

## Outcome

Task 7 is complete. Community Stella still creates no enterprise telemetry
state, HTTP client, socket, or egress by default. A managed deployment can opt
in only with a currently valid HMAC-SHA256-signed enrollment whose issuer,
audience, exact HTTPS endpoint, event class, organization, workspace, and
credential references satisfy managed allowlists and the closed schema.

After local execution finalization, Stella derives one content-free
`StellaOperationalEventV1` and durably inserts it into a separate owner-only,
host-data SQLite spool. Delivery is at-least-once with deterministic event IDs,
bounded claims, leases, retry backoff, hard row/byte capacity, oldest-unleased
eviction, and a durable drop counter. `stella telemetry status` reports local
health and `stella telemetry flush` performs one explicit bounded delivery.

Graceful shutdown guarantees durable local enqueue, not network delivery. A
detached bounded startup flush and the explicit command retry pending events;
shutdown never blocks on a network call. This deliberate safe deviation is now
part of the design contract.

## Privacy and authority invariants

- The wire type cannot represent prompts, source, paths, tool arguments or
  results, reasoning, errors, git metadata, memories, rules, project names,
  local project IDs, or local execution IDs.
- Events contain only bounded managed identifiers, provider/model dimensions,
  finalized outcome, duration, token/cost totals, tool-call/file-change counts,
  and whether output was produced.
- The deterministic event ID hashes length-prefixed enrollment and local
  identity inputs; those local inputs are never serialized.
- Enrollment is accepted only from the managed settings snapshot. User and
  project copies cannot opt in, redirect the endpoint, or add event classes.
- Every endpoint allowlist entry and the enrolled endpoint must be exact,
  credential-free HTTPS URLs without query strings, fragments, or redirects.
- `compliance_audit` is rejected rather than silently downgraded to an
  evictable operational event.
- The verification secret and bearer token are environment references, never
  configuration values. Both references must resolve from the host environment;
  a project `.env`/`.env.local` origin is rejected even when the enrollment is
  otherwise valid and correctly signed.
- Declared verification/token names are registered before model-controlled
  tools or hooks can spawn. Shared execution, shell, custom-tool, hook,
  background-process, and typed-test paths remove registered credentials from
  child environments. Status, warnings, and errors do not print those names or
  values.
- Host delivery may fail, retry, or lose an oldest record under the explicit
  capacity policy, but it never changes a completed agent outcome.

## TDD evidence

RED was observed before implementation for the missing store module, CLI
module/dependencies, managed-only settings accessor, telemetry command, and
redirect helper. Focused regressions then established and closed these cases:

- deterministic IDs, content-free serialization, unknown-field rejection, and
  invalid or unfinished rollups;
- row/byte eviction, durable drops, owner-only permissions, disjoint concurrent
  claims, retry backoff, lease recovery, and hard batch-request bounds;
- absent, invalid, expired, wrongly signed, wrong issuer/audience/schema,
  forbidden-scheme, non-allowlisted, and unsupported compliance enrollments;
- rejection of the entire endpoint allowlist when any entry violates the
  strict HTTPS policy;
- community/default construction producing no client and no host state;
- managed-only source precedence and workspace/symlinked spool-path rejection;
- redirect/non-success retry behavior, failed-delivery persistence, and
  successful acknowledgement;
- execution success when telemetry host state is rejected;
- an enrolled host successfully flushing through a fake transport while the
  exact `run_tests { command: "env" }` adversarial tool cannot observe either
  credential name or value; and
- the project-dotenv credential provenance witness is GREEN: enrollment is
  rejected when either the HMAC verification secret or bearer token came from
  project dotenv state, with neither reference disclosed in the error.

## Implementation notes

- `stella-store::enterprise_telemetry` owns the transport-neutral event and
  spool. The CLI adapter alone owns enrollment verification and HTTP.
- The spool defaults to 10,000 rows and 16 MiB. Claims are additionally capped
  at 1,000 events and 16 MiB; production delivery uses 50 events and 256 KiB.
- HTTP disables redirects, uses 2-second connect and 5-second total timeouts,
  and caps response bodies at 64 KiB while streaming.
- SQLite uses a 100 ms busy timeout so telemetry contention fails open quickly.
- New direct dependencies are existing workspace crates: `sha2` for event IDs,
  `hmac` for signed enrollment, `reqwest` for the bounded HTTPS adapter, and
  `futures-util` for capped streaming response reads.

## Verification

- `cargo test -p stella-store`: 87 unit and 7 enterprise telemetry integration
  tests passed.
- `cargo test -p stella-tools`: 335 unit tests passed; 1 existing sandbox test
  remained ignored; 4 media replay tests passed. The 6 tracker and 8 web
  localhost integration tests passed outside the restricted network sandbox.
- `cargo test -p stella-cli`: 362 tests passed, including the project-dotenv
  provenance and credential non-disclosure witnesses.
- `cargo clippy -p stella-store -p stella-tools -p stella-cli --all-targets --
  -D warnings`: passed.
- `cargo fmt --all -- --check`: passed.
- `make sizes`: all 300 tracked Rust files passed the ratchet.
- `git diff --check`: passed.
- The required documentation search confirmed that existing absolute
  no-phone-home claims need the precise managed-enrollment exception. Those
  user-facing edits remain owned by Task 8; this task updated the authoritative
  design prose for non-blocking shutdown semantics.

## Handoff

Task 8 must replace absolute public no-phone-home wording with the exact
contract: no community/default enterprise telemetry egress, plus an explicit,
signed, managed-only enterprise operational exception. No push was performed.
