# 16. Guard exceptions are a field, not a cleverer deny pattern

- Status: accepted
- Date: 2026-08-26

## Context

Issue #5128 asks for SCR-001 — *never compile the full test suite in the inner
loop* — to be enforced by Stella's own machinery rather than only by a Claude
Code `PreToolUse` shell hook. Stella already has the right surface: workspace
rules carry Tier-2 guards, and `guard-deny-command` blocks a `Bash` command by
glob at the tool boundary.

It could not express the rule.

Every guard field is a **positive match**. SCR-001 is not a positive
statement — it is "workspace-wide test compiles are forbidden **except** the
per-crate form". The permitted case is a *subset* of the forbidden family, and
the glob language (`*` as the only wildcard, no alternation, no negation) has
no way to say "matches this and not that".

Three ways around it were considered, and each fails in the same direction:

1. **Enumerate the bad spellings.** `*cargo test --workspace*`,
   `*cargo test --all*`, and so on. This misses `cargo test some_filter`,
   which compiles the entire workspace and is the most ordinary way to trip
   the rule. Worse, the failure is silent — the rule renders as `[enforced]`,
   the guard never fires, and nobody learns the difference. A guard that
   cannot fire is worse than no guard, because it reports enforcement that
   does not exist. This codebase already treats that as a defect class
   (`blank_guard_frontmatter_values_do_not_manufacture_a_guard`).

2. **Extend the glob language** with negation or alternation. This changes the
   matcher shared by rule guards *and* hook matchers (`glob.rs` is used by
   both), turning a tiny, auditable language into a small regex
   dialect — and every existing pattern would have to be re-read under the new
   grammar to be sure none changed meaning.

3. **Special-case SCR-001 in `stella-core`.** Stella is a product; SCR-001 is
   one organisation's process rule. Hard-coding it into the engine puts a
   customer's policy in the vendor's binary, and the next org's rule has
   nowhere to go.

## Decision

`RuleGuard` gains an `allow_command_glob` field, authored as
`guard-allow-command:` in markdown frontmatter and `guard_allow_command` in
the TOML record schema. A command matching it is exempt from the guard's
`deny_command_glob`.

The exception is **command-scoped and narrowing only**:

- It applies to the command branch and nothing else. If it could suppress the
  whole guard, one permissive command glob would silently disarm a
  `deny-path` condition it was never written to reason about — and a guard
  that stops firing is indistinguishable from a guard that was never
  violated.
- It cannot soften a bare whole-tool guard (`guard-tool: Bash` with no deny
  glob). Allowing that would turn the guard into a whole-tool *allowlist*,
  which is a different feature wearing this one's name.
- An exception alone is not a guard. `guard-allow-command:` with nothing to
  except from parses as Tier 1, for the same reason a blank `guard-tool:`
  does: a rule must not advertise enforcement it structurally cannot deliver.

The field is carried on **every** surface a guard travels through — markdown
frontmatter, the TOML `Enforcement` schema, the record bridge, and the
extraction DTO — rather than only where SCR-001 needs it. Dropping it at any
hop would *widen* the guard in transit: the rule would begin blocking the
scoped form it was written to permit, and over-blocking reads as the rule
working rather than as data loss.

SCR-001 itself stays data. Stella gains the general capability; the org writes
the rule.

## Why durable

The safe direction for a guard is *deny broadly, permit narrowly*, and this
field is what makes that direction expressible. The asymmetry is the whole
argument: **a missing exception announces itself immediately as a false
block** — someone runs the scoped command, is refused, and fixes the rule
within a minute. A missing deny pattern announces nothing at all. Enumerating
deny globs optimises for the silent failure; naming an exception optimises for
the loud one.

The change is additive and provably so. Every guard written before it has
`allow_command_glob: None` and behaves exactly as it did — asserted directly
rather than assumed. The TOML field is `skip_serializing_if = "Option::is_none"`,
so records predating it serialise byte-identically, mint no revision, and
change no `record_hash` (ADR 0011). The glob language is untouched, so no
existing pattern changes meaning.

It also keeps the vendor/customer line clean. Ten years on, `stella-core`
still contains no organisation's process rules — only the vocabulary those
rules are written in.

## Consequences

- The guard vocabulary now has one field whose effect is subtractive.
  `concrete()` and the frontmatter parser both had to learn that it does not,
  by itself, constitute a guard; both are covered by tests.
- Authoring the SCR-001 record itself is follow-up work, not part of this
  decision. A repository-published record with a hard guard does
  not arm on clone — it reaches the tool boundary only through the local
  decision ledger (`stella context promote`), which is a human act by design
  and correctly so: a repository must not be able to arm blocking rules on
  everyone who checks it out.
- `guard-allow-path` is the obvious symmetric question and is
  not answered here. Path guards have not yet shown the same
  broad-family-with-a-carve-out shape, and adding an unused subtractive field
  to the file surface is how a vocabulary accretes. Filed as residue; add it
  when a real rule needs it.
