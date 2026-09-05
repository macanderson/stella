---
id: adr-readme
title: "Architecture Decision Records"
status: living
---

# Architecture Decision Records

This is Stella's numbered, ratifiable decision record. **ADRs 0001–0012 are one
series** — the Phase 0 baseline for the adaptive-context work — and the notes
below are about that series. From 0013 the directory also carries decisions
outside it; an ADR that is not part of the Phase 0 series says so in its own
header.

(A bare "ADR-033" in `stella-serve` is not from this directory at all. It means
the *Oxagen* ADR in the private `oxagen-platform` repository; `docs/spec/serve-surface.md`
is the self-contained Stella-side account.)

## Phase 0 — Adaptive Context (0001–0012)

These ADRs capture the baseline decisions for the adaptive-context work in
Stella. ADRs 0001–0009 mostly *record* a decision the 2026-07 planning bundle
already made, rather than making a new one; each grounds its claims in those
source docs and, where relevant, in the current Stella code.

**That bundle has been superseded** and removed from the tree (it remains in
git history). The current specification is
[`../spec/adaptive-context/adaptive-context.md`](../spec/adaptive-context/adaptive-context.md), with phases in
[`../spec/adaptive-context/adaptive-context-plan.md`](../spec/adaptive-context/adaptive-context-plan.md).
References to the old plan/lifecycle pair in ADRs 0001–0009 are left as written:
they are accurate records of what was decided and why, and rewriting them to
cite a document that did not exist at the time would falsify the record.

The current spec annotates two ADRs whose conclusions did not survive contact
with the shipped code — 0003 (the point-in-time cutoff reached adjacency only,
one layer below `recall`) and 0006 (the compiled frame is now reached by
extending the step manifest, not by building a parallel aggregate). Read those
notes alongside the ADRs. 0003's gap was **closed on 2026-07-26** by Phase 1
(#712); 0006's amendment **landed the same day** (#713 deliverable 6), in the
ADR's own body — the original text is left as written, per the rule above, with
the amendment recorded beside it.

ADRs 0002 and 0007 originally FLAGGED open questions for human sign-off; the
repository owner **ratified both on 2026-07-23**, so all Phase 0 ADRs are now
Accepted. The ratified resolutions: `SharingScope` is the 4-value set
(`user, repository, workspace, organization`); `DirectiveEnforcement` is the
2-value set from the 4→2 mapping. The related `Origin`-arity item (ADR 0001) was
spec-verified as the full 5-value set for all families.

ADR 0009 (Phase 1) resolves the seven decisions flagged in issue #483 that
blocked the Phase-1 enum/validator freeze — four resolved by existing
spec/ADRs, three ratified by the owner on 2026-07-24 (including the
`informational → advisory` edge amended into ADR 0007).

ADR 0010 amends ADR 0005: the destination is unchanged, but the route from
big-bang cutover becomes incremental transfer. The owner **ratified it on
2026-07-26** (issue #711), settling its second open question in the same act —
`node`, `edge`, and `embedding` are a derived index and never transfer
authority, so `lineage_id` lands on `memory` and `episode` only. Its first open
question (whether the backfill ever becomes mandatory) is left
open; nothing before Phase 3 forces it.

| # | Title | Status |
|---|---|---|
| [0001](0001-semantic-taxonomy.md) | Semantic Taxonomy | Accepted (Phase 0) |
| [0002](0002-scope-vs-sharing.md) | Scope vs. Sharing | Accepted — ratified 2026-07-23 (4-value SharingScope) |
| [0003](0003-bitemporal-semantics.md) | Bitemporal Semantics | Accepted (Phase 0) — recall-layer gap closed 2026-07-26 (#712) |
| [0004](0004-record-revision-identity.md) | Record Revision Identity | Accepted (Phase 0) |
| [0005](0005-storage-authority.md) | Storage Authority | Accepted (Phase 0) |
| [0006](0006-contextframe-vs-compiledcontextframe.md) | ContextFrame vs. CompiledContextFrame | Accepted (Phase 0) — amended 2026-07-26 (extends the step manifest) |
| [0007](0007-immutable-promotion-history.md) | Immutable Promotion History | Accepted — ratified 2026-07-23 (enforcement 4→2); amended 2026-07-24 (`informational`→advisory) |
| [0008](0008-markdown-canonical-rules.md) | Markdown Repository Rules Remain Canonical | Accepted (Phase 0) — surface superseded by [0011](0011-context-records-are-toml.md) |
| [0009](0009-enum-freeze-resolutions.md) | Enum-Freeze Resolutions (issue #483) | Accepted — ratified 2026-07-24 |
| [0010](0010-incremental-authority-transfer.md) | Incremental Authority Transfer (amends 0005) | Accepted — ratified 2026-07-26 (index tables do not transfer) |
| [0011](0011-context-records-are-toml.md) | Context Records Are TOML (supersedes 0008's surface) | Accepted — ratified 2026-07-30 (hash-neutral; legacy `.md` rules keep loading) |
| [0012](0012-context-record-field-schema.md) | The Context-Record Field Schema, and Records-Live-in-Files | Accepted — ratified 2026-07-30 (memories in the database, context records in files; `personal` → `user`) |

## Outside the Phase 0 series

| # | Title | Status |
|---|---|---|
| [0013](0013-session-artifact-boundary.md) | The Session Artifact Boundary | **Proposed** — awaiting ratification |
| [0014](0014-memories-join-the-record-control-plane.md) | Memories Join the Context-Record Control Plane | **Proposed** — awaiting ratification |
| [0015](0015-the-task-tag-rides-the-event.md) | The Task Tag Rides the Event | Implemented — landed with #5039 |
| [0016](0016-guard-exceptions-are-a-field-not-a-pattern.md) | A Guard Exception Is a Field, Not a Pattern | Accepted |
| [0017](0017-plan-graph-persistence.md) | The Plan Graph Is Persisted in the Store, Not the Context Plane | **Proposed** — awaiting ratification |
| [0018](0018-mcp-capability-grants.md) | MCP Servers Are Withheld Until Their Handshake Is Granted | **Proposed** — awaiting ratification |
| [0019](0019-graph-session-touch-tags.md) | A Graph Node's Touch Tag Comes from the Session's File Ledger | Implemented — landed with #5211 |
| [0020](0020-voice-dictation-push-to-talk.md) | Voice Dictation Is a Held Spacebar, and Transcription Is BYOK | Proposed |
| [0021](0021-a-memory-event-names-one-memory.md) | A Memory Event Names One Memory | Implemented — landed with #5032 |
| [0022](0022-adopt-standing-decisions-scr-corpus.md) | Adopt Org Standing Decisions as a Steering Context Record Corpus | Accepted |
| [0023](0023-autonomous-tool-foundry.md) | Reconnect the Gap Detector; the Foundry Runs Autonomously Behind Standing Controls | Accepted |
| [0024](0024-release-builds-unwind.md) | Release Builds Unwind on Panic | Accepted |
| [0025](0025-nested-frontmatter-refusal-scope.md) | Refuse a Nested Key Where It Widens a Grant | Accepted |
| [0026](0026-context-editing-ships-off-until-measured.md) | Context Editing Ships Off Until Its Trigger Is Measured | Accepted |
| [0027](0027-a-fleet-worker-gets-its-own-worktree.md) | A Fleet Worker Gets Its Own Worktree | Accepted |
| [0028](0028-panel-cells-are-glyphs-in-the-contract.md) | A Panel Cell Is a Glyph in the Contract and a Column in the Host | Accepted |

ADR 0013 draws the line between what Stella owes a caller that moves a session
between machines (an artifact, a fingerprint, a version contract, a visible
fork) and what a control plane owns (identity, storage, transport, auth,
retention). It decides a boundary, not a feature: nothing in it is implemented,
and the parity rows `turn.checkpoint` and `turn.checkpoint_resume` defer to it
by number.

ADR 0015 records the four choices behind SPEC 7.1's evidence ledger and
per-task cost: the tag is a field on the event rather than an envelope around
it, it is applied at send through a slot every clone of an `EventSender` shares,
the store projects it into a column, and `est remain` is absent where nothing
measured supports it. It describes shipped code (#5039) rather than proposing a
boundary.

ADR 0014 brings workspace memories under the context-record control plane —
one enumeration, one lifecycle, one suppression trail — while keeping their
Markdown document representation per ADR 0011's field/document line. It
governs an existing surface rather than adding one; the epic is #2283.

ADR 0017 answers the one question #5037 left open: which store owns the plan
graph SPEC §7.4 specifies. It routes the record to `store.db` beside the
execution it describes rather than to the retrieval plane in `context.db`, and
argues the shape (edge rows, not a JSON column) from the auditability the
issue asks for. Unlike 0013 it is implemented in the change that files it.

ADR 0018 makes `McpServerEntry::granted` a three-state field, so an entry says
which door its server came through: a grant, a registry install, or a human
editing `mcp.toml`. Only the middle one withholds, and `CapabilityGrants` then
keeps an ungranted server out of `schemas()` and refuses its calls before the
transport — on both hosts, so the property does not depend on which surface is
driving.

ADR 0024 sets `panic = "unwind"` for the release profile. Every panic boundary
in the workspace was inert in shipped binaries, including the two that are
promises to a person: a panel that panics paints an error card instead of
killing the process, and a panic in one server connection ends that connection
rather than the server. An example binary run with `--release` in CI is what
holds the profile there.

ADR 0027 makes a `git worktree` per task the fleet default. Two workers in one
checkout can revert each other's uncommitted files: one runs `git checkout`,
git restores every tracked file in the tree, and the other's edits are gone
with no error printed. The shared root and its cooperative file claims stay,
as something a plan names — a claim guards one path, and a branch switch
rewrites all of them.

ADR 0028 settles what a panel frame's cell count measures. The wire contract
counts glyphs; the host counts terminal columns. A row of wide glyphs the
contract admits is cut at the lease's edge, and the two tests named in the
record hold both halves of that.
