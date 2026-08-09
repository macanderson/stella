---
id: prompt-skill-author
title: "skill_author — the effective prompt"
status: living
---

# `skill_author`

Generating a skill definition. One accounted model call that drafts a
`SKILL.md`, validates it, and installs it as v1 — reusable know-how (a
convention, procedure, or preference) the agent applies when relevant.

Skills are **never enforced**: they are selected and injected as volatile
context, which is why the `description` field carries so much weight in the
prompt — it is the primary selection signal.

| | |
|---|---|
| Call role | `ModelCallRole::SkillAuthor` (`"skill_author"`) |
| Dispatch | raw completion, `tools: []` |
| System prompt | `SKILL_AUTHOR_SYSTEM`, `crates/stella-cli/src/command_deck/skills.rs` |
| User message | `build_skill_creation_prompt`, same file — pure, unit-tested |
| Output cap | 4,096 visible + 4,096 reasoning headroom |
| Temperature | 0.2 |
| Effort | inherited — the written artifact *is* the product |
| Override | none |

The cap was 1,200 until #2444, which truncated three of the four real
single-file `SKILL.md` artifacts it was measured against (~700-2,855 tokens) —
and that was the whole budget, thinking included. It is now declared by
`standalone_bounds` (`crates/stella-cli/src/accounted_call.rs`); see
[README.md](README.md#output-caps).

## Wire shape

```
[ system(SKILL_AUTHOR_SYSTEM)
  user(build_skill_creation_prompt(request, ranked_candidates)) ]
```

## System message (verbatim)

```text
You author `SKILL.md` files for a coding agent. A skill is reusable know-how (a convention, procedure, or preference) the agent applies when relevant. Output ONLY the file content: YAML frontmatter delimited by `---` with `name:` (a short kebab-case slug), `description:` (one line — the primary selection signal), and optional `domains:` (comma-separated tags), followed by a concise markdown body. No commentary before or after.
```

## User message (template)

```text
Create ONE new skill for this request:

{request}

Existing registry skills, ranked by usefulness (relevance, then popularity). You may borrow whole or in part from any of them, and assemble bits from several into one coherent skill — but deliver a SINGLE skill:
1. {candidate}
2. {candidate}

Write the SKILL.md now. Keep the body focused and actionable; the description must make it easy to select for the right task.
```

When the registry search returns nothing, the middle block is replaced by:

```text
No existing skills were found in the registry. Author the skill from scratch.
```

Candidates are rendered as `{id}` when the install count is zero and
`{id} ({installs})` otherwise, so popularity is visible without a column that
is mostly zeroes.

## The "SINGLE skill" emphasis

Handing a model a ranked list of prior art and asking it to borrow is an
invitation to emit several skills, or one skill that is three skills stapled
together. The instruction says *assemble bits from several into one coherent
skill* twice — once in the borrowing clause and once in the closing line —
because the install path expects exactly one file.

## Extraction

`extract_skill_md` is forgiving in a fixed order, so a model that ignores "no
commentary" still lands a usable file:

1. the first fenced code block, if any;
2. otherwise from the first `---` onward (the frontmatter);
3. otherwise the trimmed whole reply.

The result is then validated before install, so anything that reaches disk is
guaranteed loadable.

## Where it lands

`.stella/skills/<slug>/SKILL.md`, installed as v1. The same directory receives
auto-promoted skills mined from recurring reflection lessons.

## Related

- [agent-author.md](agent-author.md) — the sibling authoring role, same shape,
  different artifact
- [reflection.md](reflection.md) — the other producer of `.stella/skills/`
