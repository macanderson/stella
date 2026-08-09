---
id: prompt-agent-author
title: "agent_author — the effective prompt"
status: living
---

# `agent_author`

Generating an agent definition. One accounted call that drafts a markdown file
with YAML frontmatter, validates it through the real loader, and installs it.

The interesting property of this role: **its output is another role's system
prompt.** The body it writes becomes the persona a future session runs under,
which is why the prompt insists the body be complete and self-contained rather
than a description of a persona.

| | |
|---|---|
| Call role | `ModelCallRole::AgentAuthor` (`"agent_author"`) |
| Dispatch | raw completion, `tools: []` |
| Built by | `creation_messages`, `crates/stella-cli/src/agents_installed.rs` |
| Sent from | `create_agent`, `crates/stella-cli/src/command_deck/authoring.rs` |
| Output cap | 1,200 |
| Temperature | 0.2 |
| Override | none |

## Wire shape

```
[ system(the format contract)
  user("Write an agent definition for: {description}") ]
```

## System message (verbatim)

```text
You write agent definition files for the stella CLI. An agent definition is a markdown file with YAML frontmatter and a body, in exactly this shape:
---
name: kebab-case-name
description: one line, under 80 characters, saying what the agent does
tools: Comma, Separated, Tool, Names
---
The agent's system prompt: persona, instructions, constraints.

Rules:
- `name` must be short, kebab-case, and specific.
- Include the `tools:` line ONLY when the agent should be restricted to a subset of tools; omit it entirely to grant all tools.
- The body must be a complete, self-contained system prompt.
- Respond with ONLY the complete markdown file — no code fences, no commentary before or after.
```

## User message

```text
Write an agent definition for: {description}
```

## The `tools:` omission rule

"Omit it entirely to grant all tools" is stated as a rule rather than left
implicit because the failure is asymmetric. An agent that lists too few tools
is quietly crippled — it will not report a missing capability, it will simply
work around one it cannot see, which is exactly the confusion
[worker.md](worker.md)'s tool-steering block exists to prevent. An agent
granted all tools is at worst over-permissioned.

Temperature 0.2 rather than 0.0: a persona benefits from a little variation,
and the format is pinned by the parser rather than by determinism.

## Validation before install

`parse_generated_agent` strips code fences if present, then validates the
result through **the real loader parser** (`agent_from_file`) rather than a
lookalike. Anything installed is therefore guaranteed loadable — an empty
definition, a missing body, or unparseable frontmatter is rejected at draft
time rather than discovered at the next session's open.

The generated slug is derived, so `name: Unsafe Auditor` installs as
`unsafe-auditor`.

## Where it lands

The agents directory for the requested scope — project (`.stella/agents/`) or
user (`~/.stella/agents/`) — pinned at v1, with the install path and cost
reported back in the status line.

## Related

- [skill-author.md](skill-author.md) — the sibling authoring role
- [worker.md](worker.md) — an installed agent's body replaces the base persona
  in that assembly
