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
A skill that is a runnable procedure (invoked as `stella skill run <slug>` or `/slug`, with the invocation's arguments replacing `$ARGUMENTS` in the body) may also declare invoke directives in the same frontmatter: `context:` (`inline` to expand into the session, `fork` to run in a fresh context), `allowed-tools:` (comma-separated tool names/groups the run is narrowed to — it can only narrow the operator's surface, never widen it), `model:` (a provider/slug the skill asks to run under), and `effort:` (`low`|`medium`|`high`|`xhigh`|`max`). Declare a directive only when the procedure needs it; plain know-how should carry none.
```

## The invoke directives

The second paragraph teaches the **skill-function** vocabulary — the four
optional frontmatter keys `parse_invoke_directives`
(`crates/stella-core/src/skills/invoke.rs`) recognizes:

| Key | Values | What it does at invocation |
|---|---|---|
| `context:` | `inline` (default) / `fork` | Expand into the session, or run in a fresh context (`stella skill run` is already fresh; in-session fork runs scoped in place) |
| `allowed-tools:` | comma/space-separated names or groups | The run's tool grant, enforced as the intersection with operator policy (`stella-tools`' `skill_grant` + `skill_plane`) — it can only narrow |
| `model:` | a `provider/slug` | The model the skill asks for; resolved like `--model`, and an explicit flag still wins |
| `effort:` | `low`…`max` | Reasoning-effort override for the invocation |

Behavior is the **skill's**, never a parameter's (AGENTS.md #9): how an
invocation runs is declared here, in the authored file, and the invocation
carries only the slug and its arguments. There is no
`invoke_skill` tool — no model call can invoke a skill. A skill function
runs when a person asks, via `stella skill run <slug>` or an in-session
`/slug` expansion, and when recall auto-selects it for a turn (#5465): the
selected skill expands exactly like the `/slug` form, its `allowed-tools`
grant narrowing the turn and its `effort` honored. That is safe without a
person in the loop because a grant can only narrow the operator's surface,
never widen it. Unknown values of these keys degrade to the default with a
diagnostic; they never refuse the skill.

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
