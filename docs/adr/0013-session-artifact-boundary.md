# ADR 0013: The session artifact boundary

- Status: **Proposed** — awaiting ratification by the repository owner.
- Date: 2026-08-02
- Deciders: repository owner (pending)
- Scope note: ADRs 0001–0012 are the Phase 0 adaptive-context series. This one
  is not part of it. It is filed here because this is where Stella's numbered,
  ratifiable decision records live — see
  [README](README.md). A bare "ADR-033" anywhere in `stella-serve` still means
  the *Oxagen* ADR in the private `oxagen-platform` repository, not this series.

## Context

Stella can already survive a crash. It cannot survive a *machine*.

The durable record exists and is good: `stella-store/src/work_journal.rs` keeps
a git repository in Stella's own directory, attaches the user's workspace as a
work tree, and commits the agent's file changes onto a per-session ref
(`refs/stella/<session>/head`, `work_journal.rs:56`). Riding in the same commit
graph are two reserved blobs — the in-flight turn's resume point
(`CHECKPOINT_BLOB`, `work_journal.rs:78`) and the session's staleness map
(`OBSERVED_BLOB`, `work_journal.rs:89`). A turn that dies resumes; a session
that dies keeps its no-clobber guarantee.

All of it is local, and all of it is keyed on an identity that is deliberately
unable to travel:

- The key is minted by `stella-store::workspace_local::resolve`
  (`workspace_local.rs:132`) and stored at `.stella/private/local-id.json`.
  `private/` is gitignored by construction (`private.rs:20`), so a `git clone`
  never carries it.
- Identity is decided by comparing the marker's recorded **absolute path**
  against the current canonical path (`workspace_local.rs:150`) — a comparison
  with no meaning across machines.
- When the marker's recorded path still exists and still claims the same id,
  `resolve` concludes *copy* and mints a **fresh** id
  (`workspace_local.rs:164`), precisely so two copies never share state.

Each of those three is correct for the problem that module solves — "is this
the same working copy I saw last time?" — and each is fatal to cross-machine
resume. The module says so itself: "local sessions are local"
(`workspace_local.rs:22`).

Meanwhile the engine already exports a portable turn snapshot.
`stella_core::step::Checkpoint` (`step.rs:509`) is a versioned serde struct
whose field order is the wire order, and `Engine::resume_turn`
(`driver.rs:1108`) reconstitutes a `TurnState` from one. The cross-surface
matrix records that this format has **no production writers**: the CLI replays
its own `session_persist` journal and never calls `to_checkpoint`/`resume_turn`,
and nothing behind `stella-serve` persists at all (the `turn.checkpoint_resume`
row in `stella-parity/src/lib.rs`, deferred on both surfaces).

So the question this ADR answers is not "how do we build sync". It is: **when a
session moves between machines, which half of that problem is Stella's?**

## Decision

> **Stella provides a checkpoint API and a replay API. The checkpoint API
> returns a self-contained artifact; the replay API accepts one. Identifying a
> workspace, storing the artifact, moving it, authenticating it, and deciding
> who may read it are the caller's concerns — not Stella's.**

Stella remains a local-first engine that happens to emit and accept portable
session artifacts. Oxagen — or any other host — is the caller that gives those
artifacts names, accounts, tenancy, retention and audit.

### Why this line and not another

**1. It is the boundary Stella already has.** `stella-serve` states it
outright: the host assembles the turn and "every governed side effect — model
calls and tool calls — is remoted back to the host"; "the engine never holds
ambient authority" (`stella-serve/src/lib.rs:7`). A served turn is structurally
incapable of reading a user's file, because reading a file is a request the
host answers. The matrix already reasons from that line when it defers
server-side checkpointing: "in the reverse-RPC model the workspace lives on the
HOST side, so serve has no filesystem location it could honestly checkpoint
against" (the `turn.checkpoint` row's API note in `stella-parity/src/lib.rs`).
Applying the same line to durability is consistency, not a new concept to
defend.

**2. It deletes the hardest problem instead of solving it.** A portable
workspace identity is the part of cross-machine resume with no good answer:
every scheme either binds two unrelated projects together or fails to link two
ends of the same one, and `workspace_local` chose the honest miss on purpose
(`workspace_local.rs:39-46`). Under this posture Stella never needs a portable
identity, because the caller says which project an artifact belongs to. The
machine-local id keeps doing its one job — naming the local store — and is
never asked to mean anything off this machine.

**3. It keeps a published principle intact.** The docs site commits to
"local-first state, BYOK credentials, ... and zero telemetry egress by default",
with exactly two explicit, opt-in egress paths named
(`website/content/docs/principles/index.mdx` § "What this buys you,
concretely", and invariant 4 under § "The seven invariants, briefly"). A sync
feature built *into* Stella would carve a third. A sync feature built *on top of* an artifact API
does not: producing an artifact is a local operation that writes bytes the
caller asked for, and Stella still uploads nothing.

### The alternative, argued honestly

The alternative is that Stella owns sync end to end: a `stella login`, a
Stella-hosted store, a `stella push` / `stella pull` keyed on a Stella account.

It has two real advantages. For a solo user it is one integration instead of
two — no control plane to stand up before your laptop and your desktop can share
a session. And there is no artifact format to version, because both ends are
the same build talking to a store that Stella also ships; the version problem
in §4 below simply does not arise.

It loses on three counts:

- It requires exactly the portable identity that has no good answer, and puts
  it in the layer least equipped to decide it. Stella sees a directory. It does
  not know that `~/src/api` on one machine and `/work/api` on another are one
  project; only something with a concept of *project* does.
- It contradicts the published posture. "Zero telemetry egress by default" and
  "your keys call your provider directly" are load-bearing claims; a first-party
  upload path makes them conditional, and the condition is the thing users are
  choosing Stella to avoid.
- It builds the easy half twice. Storage, auth, tenancy, retention and audit
  are solved problems that a control plane already has, and are not improved by
  a second implementation living inside a CLI.

The solo-user cost is real and is accepted: see "What this does not commit us
to" for the local-only capability that stays available without any control
plane.

## The decisions this makes

### 1. What is in the artifact

**The artifact is a git bundle of the session's ref subgraph from the work
journal, plus a manifest.** Nothing new is invented to carry it.

That substrate already exists and already contains the three things replay
needs, in one commit graph that updates atomically:

| In the pack today | Where |
|---|---|
| the files the agent touched, at each commit | `work_journal.rs:199-214` |
| the in-flight turn's resume point | `CHECKPOINT_BLOB`, `work_journal.rs:78` |
| the session's staleness map | `OBSERVED_BLOB`, `work_journal.rs:89` |

Git supplies the pack format, incremental transfer, dedup and compression for
free — which is the whole reason the work journal is a git repository rather
than a directory of JSON snapshots (`work_journal.rs:5-10`). **If a later
implementation finds itself defining a diff format, it has taken a wrong turn.**

Two consequences follow, and both are load-bearing.

**The artifact is a delta, not a tree.** `record` stages named paths
individually and never `git add -A`, deliberately, because sweeping the tree
"would make the history a lie about what the agent did"
(`work_journal.rs:181-183`); gitignored paths are filtered out entirely
(`work_journal.rs:204`). So the pack contains exactly the files the agent
touched and nothing else. Reconstituting a whole working tree therefore
requires a **base** the caller supplies. The manifest records the base commit
id observed at capture (`staleness::git_head_sync`, `staleness.rs:197`) so the
caller can name it, but Stella does not carry the base, and will not: shipping
a user's entire repository inside a session artifact is an egress decision
Stella has no standing to make, and it would move files the agent never read.

**A checkpoint alone would be a conversation, not a session.**
`Engine::resume_turn` takes a `Checkpoint` and nothing else
(`driver.rs:1108-1110`), and a `Checkpoint` is transcript + budget + step index
(`step.rs:509-534`). Replaying only that reconstitutes the same words against
whatever tree happens to be there. The bundle — not `Checkpoint::to_json` — is
the unit of the API.

**The CLI sidecar is deliberately excluded from v1.** `journal.jsonl`,
`history.json` and `queue.json` (`stella-store/src/journal.rs:50-54`) are not
in the artifact:

- `history.json` is a `Vec<CompletionMessage>` — the same conversation
  `Checkpoint.messages` already carries verbatim. Shipping both means two copies
  of the transcript that can disagree, and a replay path that has to pick.
- `journal.jsonl` is the deck's own append-only replay format, which is a
  *different* resumption path from the engine checkpoint. Converging the two is
  already a declared deferral (the `turn.checkpoint_resume` row's CLI posture in
  `stella-parity/src/lib.rs`), and this ADR does not get to pretend it is
  solved.
- `queue.json` is the pending prompt backlog — user intent not yet acted on.
  It is genuinely part of a session and is the one sidecar file worth carrying,
  as an optional named manifest entry rather than as part of the pack.

The honest consequence: **a CLI-originated artifact is blocked on that
convergence**, and the parity row that names it is the tracking surface. This
ADR unblocks the design, not the CLI's writer.

### 2. The provenance fingerprint — the one thing Stella must keep

"Identity is the caller's problem" is right. "Any artifact may be replayed into
any tree" is not. That silently produces a session whose transcript describes
files that were never there, and nothing detects it — a strictly worse failure
than the one the no-clobber guard exists to prevent, because the guard's
failures are loud and this one is not.

**The artifact carries a fingerprint of the tree it was taken from. Replay
verifies it and refuses by default.** Stella is not naming the workspace; it is
checking that this artifact belongs to this tree.

The mechanism exists and needs no new concept. The fingerprint is the pair:

- `OBSERVED_BLOB` — `path → sha256(bytes this session saw)`, maintained by
  `ToolRegistry::remember_observed` (`registry.rs:1640`) on every read and
  write, serialized through `observed_snapshot` as a `BTreeMap` so the bytes are
  a pure function of the contents (`registry.rs:1662`).
- the base commit id from the manifest, as provenance and as a free
  short-circuit — exactly the role `git rev-parse HEAD` plays in the existing
  staleness oracle (`staleness.rs:12-16`).

Verification is `staleness::freshness_sync` over that map against the target
tree (`staleness.rs:157`), yielding the existing verdict type
(`staleness.rs:68`):

- `Fresh` — every entry re-hashes to its saved value. Replay proceeds.
- `Drifted { changed, missing }` — **replay refuses by default**, naming the
  exact paths, in the register the no-clobber refusal already uses: the artifact
  is intact, nothing was written, and the caller is told what moved.
- `Unknown` — no manifest to check. Treated as refusal, not as consent. An
  artifact with no fingerprint cannot be shown to belong anywhere.

The override is explicit, named, and never inferred from context — a distinct
argument on the replay call, not a fallback the API takes when verification
fails. An overridden replay records the override in the resulting session's
first commit message, so a divergent history says why it diverged.

**What the fingerprint deliberately does not claim.** It covers exactly the
paths the session touched, because that is all `remember_observed` records. So
it cannot false-positive on churn elsewhere in the repository, and it cannot
detect a tree that differs *only* in files the session never read. That is the
same precision the no-clobber guard already claims for itself
(`registry.rs:1709-1713`) and the same reasoning: a check that cried wolf would
be overridden by reflex, and then it would protect nothing.

### 3. What replay assumes about the tree

Two modes. They **must be distinguishable in the signature and never inferred**
from whether a tree happens to be present.

**Apply — the default.** Replay against a tree the caller already has, gated on
the fingerprint in §2. Fast, and the honest answer for the same-laptop-next-
morning case. On `Drifted` or `Unknown` it refuses, per §2.

**Materialize.** Write the agent-touched subset out of the pack onto a base the
caller names, then replay. This is the cross-machine case, and its limit must
be stated plainly rather than discovered: because the pack is a delta (§1),
Materialize is hermetic **only to the extent the caller can supply the base**.
Given the base, Stella checks that the recorded base id matches before writing
anything, and refuses on mismatch. Without a base, Materialize produces a tree
containing only the agent's own touched files — legitimate for a scratch replay,
a lie for anything else, and so it must be requested explicitly and is never
the default.

Moving the base is a control-plane job, and this is the sharpest illustration of
why the boundary sits where it does: the control plane already has the
repository, the credentials to fetch it, and the policy about who may.

### 4. The version contract

`Checkpoint::from_json` refuses any `version != CHECKPOINT_VERSION`
(`step.rs:544-553`), with `CHECKPOINT_VERSION` currently `1` (`step.rs:498`).
As an internal file format written and read by one build, that is exactly right
— the doc comment's reasoning holds: better to refuse than to "silently resume a
turn from a shape it half-understands" (`step.rs:495`).

As an API where a caller stores an artifact for weeks and replays it against a
newer Stella, equality-refusal is a **data-loss event**: the session is gone,
and the bytes that would have restored it are sitting right there.

**This ADR is the point at which `Checkpoint` stops being a struct and becomes a
public wire contract.** In those words, because the obligations that follow are
the obligations of a wire contract and not of a struct:

- **Reading is a range, not an equality.** A build accepts
  `MIN_SUPPORTED_CHECKPOINT_VERSION..=CHECKPOINT_VERSION` and writes only the
  latest. The version field stops meaning "the build that wrote this" and starts
  meaning "the contract this conforms to".
- **Unknown fields are tolerated on decode, within the range.** This is already
  the behaviour — `Checkpoint` does not set `deny_unknown_fields` — so the change
  is to *commit* to it rather than to implement it. A field added within a
  version is additive and must be optional.
- **A version bump is reserved for a change in the meaning of an existing
  field**, which is what `step.rs:495` already says a bump is for. Adding a
  field does not bump.
- **An older build may not replay a newer artifact.** It refuses. It cannot know
  what a field it never heard of means, and dropping it silently produces a
  session that is subtly wrong — the failure mode this whole ADR is organised
  around. The refusal message must name the artifact's version, this build's
  supported range, the minimum build that can read it, and — the part that makes
  it not a data-loss event — that the artifact is unmodified and nothing was
  consumed.
- **The support window is at least the two preceding versions, and never less
  than 180 days after a bump ships in a release.** An artifact is a thing
  someone put in a bucket and forgot; the window has to outlive a quarter.
- **Byte-exact round-trip stays pinned.** `Checkpoint`'s field order is the wire
  order and `checkpoint_round_trips_byte_identically` holds it (`step.rs:503-507`).
  Under a wire contract that test's status changes from a good idea to the
  conformance check.

### 5. Disclosure

`Checkpoint.messages` is the whole conversation, stored verbatim
(`step.rs:514-517`). That includes the full content of every file the agent
read, whatever those files contained — credentials in a `.env` the agent was
asked to debug, customer data in a fixture, a private key in a test.

**Any API that hands a caller that artifact must say so, in the docs of the type
that holds it.** Not a warning banner, not a release note, not a paragraph in a
guide: one accurate sentence where someone reads it before writing the code that
moves the bytes. The obligation is on the API surface. Today's doc comment says
`messages` is stored verbatim and explains *why* (the engine's next model call
needs the bytes) but never says *what a caller is about to move*, and the export
API cannot ship until it does.

**What Stella does not do about it:** no encryption, no redaction, no retention
policy, no classification of what is sensitive. Those are control-plane
concerns, which is the point of the boundary — they require knowing whose data
it is, which jurisdiction it sits in, and who is allowed to read it, and Stella
knows none of those things. Stella's whole obligation here is to be accurate
about the contents so the caller's decision is informed.

### 6. Conflict

Two machines replay the same artifact. Under this posture, which of the
resulting sessions is authoritative is the caller's policy decision.

**Stella's guarantee is that the fork is visible.** Every replay mints a new
session id and therefore its own ref (`refs/stella/<session>/head`,
`work_journal.rs:56`); a replay never adopts the session id recorded in the
artifact. The ref namespace is already per-session precisely so concurrent
sessions "share the object store ... and contend over nothing"
(`work_journal.rs:25-32`), and that property is what makes divergence honest
here: two replays produce two refs with a common ancestor, not one ref with two
histories interleaved. The artifact's origin session id is retained in the
manifest as provenance, so the fork can be *named* — which is the thing a
control-plane merge policy needs and cannot reconstruct after the fact.

**Stella explicitly leaves open:** which fork wins, whether forks are ever
merged, when a fork should have been a refusal, and when either side may be
deleted. It has no basis to decide any of them.

## The boundary

| Stella owns | The control plane (Oxagen) owns |
|---|---|
| what is in the artifact | naming a workspace / project |
| byte-exact round-trip | storage and durability of artifacts |
| refusing a tree the artifact did not come from | transport, auth, tenancy |
| the version contract and its support window | encryption at rest and in flight |
| making a fork visible | retention, deletion, audit |
| disclosing what the artifact contains | conflict *policy* (who wins, when to fork) |
| supplying the base commit id it was captured against | moving the base tree, and access to it |

## Where Oxagen comes in

A control plane adds, on top of the artifact API and without changing it:

- **Accounts and projects** — the portable identity Stella refuses to invent.
  "This artifact belongs to project *X*" is a statement only something with a
  concept of project can make, and it is the input Stella's replay API takes
  rather than a fact it derives.
- **A store** — durable, addressable, with the retention and deletion policy
  Stella declines to have.
- **Access control and audit** — who may read an artifact that contains §5's
  contents, and the record of who did.
- **Transport and the base tree** — the repository access that makes
  Materialize (§3) hermetic.
- **The web resume experience** — pick a session, resume it somewhere else.

The seam is not hypothetical. `stella-serve` exists as the headless,
host-driven engine for exactly this host (`stella-serve/src/lib.rs:4-12`), and
the Oxagen↔Stella sidecar integration is live work (oxagen#1140 / stella#856).
This ADR describes an extension of that seam, not a new one.

**This is a layering, not a dependency.** Stella must remain fully usable, and
fully durable *locally*, with no control plane present, no account, and no
network: the work journal, crash resume, and the no-clobber guarantee are local
mechanisms today and stay local mechanisms. Nothing in this ADR may make any of
them conditional on a caller existing. If a future change cannot state that
plainly, the boundary has moved to the wrong place and the change is the thing
that is wrong.

## What this does not commit us to

A posture doc earns its keep by what it refuses. None of the following follows
from anything above, and each would need its own decision:

- **A Stella-hosted store, account system, or `stella login`.** Rejected above,
  not deferred.
- **Any first-party upload path.** Producing an artifact writes bytes locally.
  Nothing in this design sends anything anywhere, and the two named egress paths
  in `principles/index.mdx` (invariant 4) remain the only two.
- **A portable workspace identity.** `workspace_local`'s id stays machine-local
  and stays out of the artifact's identity story. It is not "not yet portable";
  it is not portable.
- **Shipping the user's repository.** The artifact carries the agent's delta and
  a base *id*. It does not carry the base, and "just include the tree" is not a
  small future extension of it.
- **A merge or conflict-resolution engine.** §6 makes forks visible. Resolving
  them is not Stella's, and a "just auto-merge the obvious case" follow-on is
  the same decision wearing a smaller hat.
- **Encryption, redaction, or classification of artifact contents.** §5 is a
  disclosure obligation. It is not the first step of a redaction feature.
- **Sidecar convergence.** Excluding the CLI sidecar (§1) does not decide how
  the deck journal and the engine checkpoint eventually converge. It only
  records that the artifact does not wait on the answer.
- **Server-side checkpointing in `stella-serve`.** The reverse-RPC model still
  means the workspace is the host's (the `turn.checkpoint` row's API note in
  `stella-parity/src/lib.rs`). This ADR
  gives an embedder a defined artifact to persist; it does not give the server a
  filesystem.
- **A stability promise on the *bundle* layout.** The `Checkpoint` version
  contract (§4) is a promise. The manifest and pack layout around it are not yet,
  and a first implementation should say so in its own docs.

## Consequences

- `Checkpoint` becomes a public wire contract with a support window (§4). The
  range reader, the honest refusal message, and the disclosure sentence in §5
  are preconditions for exporting anything, not follow-ups.
- Replay gains a required mode argument and a fingerprint verification step that
  can refuse (§2, §3). Refusal is the default path and needs the same care as the
  no-clobber refusal message it is modelled on.
- The parity rows `turn.checkpoint` and `turn.checkpoint_resume` now defer to
  this ADR by number rather than to an unnamed "portable-session design", so the
  matrix and this document cannot drift apart silently.
- A CLI-originated artifact stays blocked on deck-journal convergence (§1). That
  is a real, named limitation of this design and not an oversight of it.
- Nothing in this ADR is implemented. It exists so that the export API, the
  replay API, and the version contract can be built as three separate changes
  without re-litigating the boundary in each one.

## Open questions

These are for ratification; none of them blocks the boundary itself.

1. **Does the base commit id become mandatory in the manifest?** A workspace
   that is not a git repository has no base id — the work journal supports that
   case deliberately (`work_journal.rs:16-17`). Such an artifact can be
   `Apply`-replayed against its fingerprint but never `Materialize`d. Whether
   that is a documented limitation or a hard refusal at capture time is open.
2. **Is `queue.json` in v1?** §1 argues it is the one sidecar file worth
   carrying. It is also the only one whose absence loses user intent rather than
   a reconstructible derivative, which is an argument for making it mandatory
   rather than optional.
3. **Does an overridden fingerprint mismatch mark the session permanently?**
   §2 records the override in the first commit message. Whether a session that
   started from an overridden replay should stay flagged for its whole life — so
   a later reader knows the transcript may describe files that were never there
   — is a real question about how far the honesty obligation runs.
