---
id: adr/0011-context-records-are-toml
title: "ADR 0011: Context Records Are TOML (supersedes the surface decision in 0008)"
status: implemented
---

# ADR 0011: Context Records Are TOML (supersedes the surface decision in 0008)

- Status: **Accepted** — ratified by repository owner 2026-07-30 (was: Proposed).
  Supersedes ADR 0008's *surface* decision only; 0008's thesis is preserved
  unchanged. Ratified together with [ADR 0012](0012-context-record-field-schema.md),
  as the closing section of this ADR requires.
- Date: 2026-07-28 (ratified 2026-07-30)
- Deciders: repository owner (ratified 2026-07-30)

## Context

ADR 0008 decided that repository rules remain Markdown at `.stella/rules/*.md`,
with the database as a derived read-only mirror, and rejected the `.yaml` rule
surface proposed by `context-prs-spec.md`. It was careful to name that a
*surface* conflict — "since its thesis (Git canonical, graph derived) is the
same model."

That reasoning still holds. What has changed is what the surface has to carry.

**The frontmatter became a record.** When 0008 was written, a rule file was a
`description` line and up to three guard keys. Today `crates/stella-core/src/rules/metadata.rs`
validates eleven required fields with enum parsing, RFC-3339 timestamp
validation, list parsing, and duplicate-key detection. That is a typed record
serialised into a document header.

**The parser underneath it is not a YAML parser.** `crates/stella-core/src/rules.rs`
hand-rolls roughly thirty-five lines of line reader: `key: value` scalars, plus
block sequences flattened into a comma-joined string. It supports no nesting,
and a nested key is not rejected — indentation is stripped and the key is
silently promoted to the top level.

Two things follow, and both are load-bearing for this decision:

1. **The canonical specification's own example does not parse as written.**
   `docs/spec/adaptive-context/context-pr.md` §6.1 shows a nested `scope:` block. Against the shipped
   reader, `scope` resolves to an empty string and `repository_id` becomes a
   sibling of `record_id`. Nothing errors.
2. **Adding a field means extending a bespoke parser**, not adopting a format —
   and every extension has to re-derive nesting, typing, and error reporting
   that a real parser provides.

A surface chosen when the payload was one line is being asked to carry a
validated record. That is the decision 0008 could not have made and this one
has to.

## Decision

**Context records are TOML.** A git-tracked context record is a `.toml` file.

**Documents remain Markdown.** Where the prose *is* the artifact — skills, and
long-form guidance meant to be read — Markdown with frontmatter is retained. The
line is: if the prose is a **field**, TOML; if the prose is the **document**,
Markdown. Burying a rule statement in a `"""` block would make the Context PR
diff worse, and a reviewable diff is the premise of the whole workflow.

**0008's thesis is preserved.** Git remains the authoritative, human-governed
policy ledger; the database remains a derived, read-only mirror. Nothing here
reopens that, and the single-source-of-truth argument 0008 used against a second
authority applies to this surface exactly as it did to Markdown.

**0008's rejection of YAML stands.** This is not that decision revisited. TOML
is chosen *over* YAML, not as a lighter spelling of it:

- `toml` is already a workspace dependency (`Cargo.toml`, used across at least
  eight modules). `serde_yaml` is not a dependency of this workspace at all, so
  adopting YAML would add one and TOML adds none.
- TOML has native RFC 3339 datetimes, which retires the hand-rolled byte-index
  timestamp validator in `metadata.rs`.
- TOML has real integers. `confidence` is specified as an integer 0–100 and is
  currently parsed out of a string.
- TOML has real arrays, replacing the block-sequence-to-comma-joined-string
  round trip and its ambiguity for any value containing a comma.
- TOML has no implicit type coercion. For a governance ledger, YAML resolving an
  unquoted value to a boolean is a bad day, and the mitigation is quoting
  discipline enforced by review rather than by the format.

**The change is hash-neutral.** `record_hash` is RFC 8785 canonical JSON over
the *serialised record struct*, with `record_hash` removed from the preimage
(`crates/stella-core/src/context_record/hash.rs`). The on-disk surface never enters
that preimage. Moving from Markdown to TOML therefore changes no `record_id` and
no `record_hash`, and no revision is minted by the migration itself. This is the
property that makes the decision reversible in practice rather than only on
paper.

## Consequences

- The loader gains a TOML path. **Existing `.stella/rules/*.md` continue to
  load**: they are a shipped format with files already written against it, and
  0008's migration language — legacy rule files imported as read-only mirror
  rows, never promoted above Git — is unaffected.
- The hand-rolled frontmatter parser survives only to serve that legacy path,
  and stops being extended. New fields land in the TOML schema.
- `render_rule_metadata`, currently exported from `stella-core` with no callers,
  is superseded by a TOML writer rather than wired up.
- The silent-nesting defect above should be made loud on the legacy path
  regardless of this ADR. A parser that promotes a nested key to the top level
  without complaint is the same failure shape as a check that reports OK for
  inputs it skipped. **Done** (issue #891): nested keys are recorded on
  `Frontmatter::nested_keys` and `rule_from_file_checked` refuses the file with an
  error naming the keys. `docs/spec/adaptive-context/context-pr.md` §6.1's example was corrected to a
  shape the reader accepts, since it would now fail to load rather than load wrong.
- ADR 0008's open question — owner-routing policy, deferred to Phase 8 — is
  untouched by this decision.

## What this deliberately does not decide

- **The field schema.** Which fields a context record carries, and what their
  vocabularies are, is a separate decision constrained by ADR 0009's enum
  freeze. This ADR settles the format and nothing about the contents.
- **Whether `.claude/rules/` remains a live read path.** Today it is the second
  of three rule directories the loader reads on every load. Whether it should
  instead be an explicit import source is a behavioural question, not a format
  one, and needs its own decision.

## Open question — answered by ADR 0012

The substrate rule that the current design assumes — context records are files,
and `sharing_scope` selects *which* file location (repository tree, or
`~/.stella/rules/` for personal scope) rather than whether it is a file at all —
was not ratified anywhere. ADR 0002 covers scope versus sharing but does not
reach storage.

[ADR 0012](0012-context-record-field-schema.md) writes that rule down as its
Decision 1, along with the field schema this ADR deferred. Ratifying 0011 without
0012 leaves the format decided and the contents undecided, which is the state
that let each module answer the schema question for itself.
