---
id: tool-reference
title: "docs/tools/ — the generated per-tool reference"
status: living
---

<!-- GENERATED FILE, DO NOT EDIT. Regenerate: make tool-docs-update -->

# `docs/tools/`

One TOML page per dispatchable tool — 12 of them — generated from the declarations by `crates/stella-cli/src/tool_docs.rs` and re-derived by the `tool-docs` gate step. A tool added to `crates/stella-tools/src/catalog.rs` without regenerating turns the gate red; there is no path where a new tool ships undocumented.

Each page carries the tool's name, description, input schema, output schema, `read_only`, `available_for_speculation`, `risk_level`, category, and a commented example input and output payload.

`risk_level` is a reviewed judgement declared beside the flags it sits with in `crates/stella-tools/src/catalog.rs`, graded against the rubric on `ToolEntry::risk` (#2716, #3060). It answers a different question from `read_only` — what one honest call costs the world, rather than whether the workspace changes — and is deliberately not derived from the booleans above it, which would be a relabelling rather than information. A policy grant is expressed as a ceiling over this grade; every tool that is not a built-in (MCP, custom manifest) is graded `high` for being unreviewed.

One field remains a stated absence rather than a value, because inventing it would manufacture a source of truth nobody reviewed:

- **`output_schema` is the envelope only.** Every tool answers in `ToolOutput { ok | error }`, which is declared and is what the field holds; the shape of the text inside `ok.content` is a per-tool convention with nothing behind it, so the observed example is its only evidence.

**Examples are observed, not written.** They come from Terminal-Bench trial traces (stella-events.jsonl), distilled by scripts/build-tool-doc-examples.py, captured 2026-08-11 over 2829 call/result pairs across 10 tasks. 4 of 12 tools carry a real example; the rest say so. 5 tools were never called once across 408 trials and 15304 model calls — the most interesting fact on those pages, and input to #3032.

| Tool | Category | Availability | Read-only | Speculation-safe | Observed example |
|---|---|---|---|---|---|
| [`delete_state`](delete_state.toml) | scratch | always | no | no | none observed |
| [`get_environment`](get_environment.toml) | environment | always | yes | yes | yes |
| [`get_state`](get_state.toml) | scratch | always | yes | yes | none observed |
| [`list_state`](list_state.toml) | scratch | always | yes | yes | none observed |
| [`save_state`](save_state.toml) | scratch | always | no | no | none observed |
| [`task`](task.toml) | task | always | no | no | none observed |
| [`task_assign`](task_assign.toml) | task | always | no | no | none observed |
| [`task_cancel`](task_cancel.toml) | task | always | no | no | none observed |
| [`task_complete`](task_complete.toml) | task | always | no | no | yes |
| [`task_create`](task_create.toml) | task | always | no | no | yes |
| [`task_list`](task_list.toml) | task | always | yes | yes | none observed |
| [`task_start`](task_start.toml) | task | always | no | no | yes |
