---
id: adr/0025-nested-frontmatter-refusal-scope
title: "ADR 0025: Refuse a nested key where it widens a grant"
status: implemented
---

# ADR 0025: Refuse a nested key where it widens a grant

- Status: accepted
- Date: 2026-09-03
- Decides: `#5509`

## Context

Stella reads markdown files with a `key: value` header. The parser reads one
line at a time. It cannot hold a key with keys under it. So it notes the
child key names and moves on. See `Frontmatter::nested_keys` in
`crates/stella-protocol/src/frontmatter.rs`.

Two loaders read that header. They answer the nested case in two ways.

A rule record is refused. `rule_from_file` returns `NestedFrontmatterKeys`
for any record that has one. A rule is policy. A mangled policy file that
loads looks just like a sound one.

A command or an agent is refused only for `tools:`. See
`ExtensionProblem::NestedToolbelt` in `crates/stella-core/src/extensions.rs`.

Should the second loader copy the first and refuse every nested header?

## Decision

No. Keep the narrow refusal. State it as a rule:

> Refuse a nested value when reading it wrong would **widen** what the file
> may do. Read past it everywhere else.

`tools:` widens. The parser leaves the key empty. An empty `tools:` means
every tool. So an author who named two tools gets the whole registry, and no
message. That file is refused.

`description:` and `name:` do not widen. A bad `description:` falls back to
the first line of the body. A bad `name:` falls back to the file name. Each
loses a label. Neither grants a thing. Refusing the file would cost an author
their agent over a key that changes nothing.

A new field that grants something joins the refusal in the change that adds
it. The test `nesting_under_a_key_that_grants_nothing_still_loads` pins the
other half. So a wider refusal later has to delete a test that names this
record.

## Consequences

The two loaders stay apart. A reader who spots that and reaches for one rule
should read this first.

The check is weaker than the rule. `nested_keys` holds the child names, not
the parent's. So the check asks whether `tools:` is empty and whether
anything is nested at all. A file with a bare `tools:` and a nested
`description:` is refused, and nothing widened. That is `#5737`. Fixing it is
what would let the rule hold per field.
