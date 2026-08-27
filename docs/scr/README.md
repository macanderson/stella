---
id: scr/readme
title: Steering Context Records
status: living
---

# Steering Context Records

An SCR is to steering what an ADR is to architecture: a numbered, versioned
record of a directive the maintainer would otherwise deliver by hand. The SCR
corpus is the steering context loaded by every agent working in this
repository — Claude Code loads it through the standing-decisions block in
`AGENTS.md` (imported by `CLAUDE.md`), Stella loads `AGENTS.md` directly — and
the ledger showing where each behavior sits on the autonomy ladder.

The corpus is replicated identically across the macanderson org repos
(oxagen, context-graph-protocol, cgp-website, arenabench, stella). Propose
changes once and roll them out everywhere; drift between copies is a bug.

## Keeping the copies identical

That last sentence used to be an honour system. It is now checked: the
`scr-corpus-check` workflow in oxagen compares all five `docs/scr/` trees
daily, on every push to oxagen's own corpus, and on demand. Comparison is by
git blob SHA, so "identical" means byte-identical — a reworded sentence in one
copy is drift exactly like a missing record is.

Divergence files (or updates) a `triage`-labelled issue in oxagen naming every
differing, missing, and extra file, and fails the run. The reference copy is
oxagen's, because ADR-038 and the rollout originated there — but a report
means *the copies disagree*, not *the others are wrong*. Resolve it by
deciding what the corpus should say and re-syncing all five from that
decision; blindly overwriting from oxagen can silently discard the very edit
that was correct.

The check itself lives only in oxagen and reads the other four over the API.
Installing five copies of the drift detector would replicate the thing whose
replication it polices — see ADR-039 for why enforcement is centralized while
the corpus is replicated.

**Editing the corpus is therefore a five-repo change.** Land it everywhere in
the same sitting; the check will notice within a day if you do not.

## The autonomy ladder

Each SCR carries an `autonomy` field naming its current rung:

| Rung | Meaning | Failure mode it removes |
|------|---------|-------------------------|
| L0 | Manual prompt | — |
| L1 | Written rule | Forgetting to say it |
| L2 | Enforced (hook, template, CI guard) | Agents forgetting to follow it |
| L3 | Delegated (an agent owns it) | The maintainer in the loop |
| L4 | Recorded (SCR is the source of truth) | Knowledge living in one person's habits |

**Promotion rules.** The second time the same steering prompt is typed, it
becomes an SCR at L1 minimum — write it down. The third time, it must acquire
an L2 or L3 mechanism. Typing it a fourth time is a process bug, not an
agent bug.

## Lifecycle

1. **Capture** — second occurrence of a manual steer → draft an SCR. Any
   agent may draft; status `living`, autonomy `L1`.
2. **Promote** — attach an enforcement mechanism; update the `autonomy` and
   `enforcement` fields. The SCR is the changelog of its own automation.
3. **Load** — agents receive the SCR corpus as context at session start. A
   steer that exists as an SCR should never need to be typed again.
4. **Audit** — a periodic meta-review flags SCRs stuck at L1 whose directive
   keeps appearing in transcripts; that is the promotion backlog.
5. **Retire / supersede** — SCRs are never deleted; they take `status:
   superseded` with a `superseded_by:` naming the replacement's id, so the
   10-year reader can trace why the process looks the way it does.
   Durability-first applies to the process itself.

## Template

```markdown
---
id: scr/0NN-<kebab-slug>  # `ns/name`, lowercase-kebab, as ADRs carry
title: <directive as an imperative sentence>
status: living            # living | superseded (+ superseded_by:) | archived
origin: <how/why this became a standing steer>
trigger: <the situation in which an agent must apply it>
autonomy: L1              # current rung on the ladder
enforcement: <mechanisms that deliver it without the maintainer>
---

## Directive
## Rationale
## How an agent complies
## Exceptions
```
