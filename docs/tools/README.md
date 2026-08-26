---
id: tool-reference
title: "docs/tools/ — the generated per-tool reference"
status: living
---

<!-- GENERATED FILE, DO NOT EDIT. Regenerate: make tool-docs-update -->

# `docs/tools/`

One TOML page per dispatchable tool — 19 of them — generated from the declarations by `crates/stella-cli/src/tool_docs.rs` and re-derived by the `tool-docs` gate step. A tool added to `crates/stella-tools/src/catalog.rs` without regenerating turns the gate red; there is no path where a new tool ships undocumented.

Each page carries the tool's name, description, input schema, output schema, `read_only`, `available_for_speculation`, `risk_level`, category, and a commented example input and output payload.

`risk_level` is a reviewed judgement declared beside the flags it sits with in `crates/stella-tools/src/catalog.rs`, graded against the rubric on `ToolEntry::risk` (#2716, #3060). It answers a different question from `read_only` — what one honest call costs the world, rather than whether the workspace changes — and is deliberately not derived from the booleans above it, which would be a relabelling rather than information. A policy grant is expressed as a ceiling over this grade; every tool that is not a built-in (MCP, custom manifest) is graded `high` for being unreviewed.

One field remains a stated absence rather than a value, because inventing it would manufacture a source of truth nobody reviewed:

- **`output_schema` is the envelope only.** Every tool answers in `ToolOutput { ok | error }`, which is declared and is what the field holds — including the two optional members a result may carry, `ok.data` and `error.class`. What the envelope does not declare is what rides *inside* those: the text in `ok.content` and the structure in `ok.data` are per-tool conventions with nothing behind them, so the observed example is their only evidence.

**Examples are observed, not written.** They come from Terminal-Bench trial traces (stella-events.jsonl), distilled by scripts/build-tool-doc-examples.py, captured 2026-08-11 over 2829 call/result pairs across 10 tasks. 4 of 19 tools carry a real example; the rest say so. 5 tools were advertised and never called once across 408 trials and 15304 model calls — the most interesting fact on those pages, and input to #3032. Advertisement is the census's own `in_schema` column, not the presence of a row (#4420).

| Tool | Category | Availability | Read-only | Speculation-safe | Observed example |
|---|---|---|---|---|---|
| [`ask_question`](ask_question.toml) | question | always | yes | no | none observed |
| [`bash`](bash.toml) | shell | always | no | no | none observed |
| [`delegate`](delegate.toml) | task | always | no | no | none observed |
| [`delete_file`](delete_file.toml) | file | always | no | no | none observed |
| [`delete_state`](delete_state.toml) | scratch | always | no | no | none observed |
| [`edit_file`](edit_file.toml) | file | always | no | no | none observed |
| [`get_environment`](get_environment.toml) | environment | always | yes | yes | yes |
| [`get_state`](get_state.toml) | scratch | always | yes | yes | none observed |
| [`list_state`](list_state.toml) | scratch | always | yes | yes | none observed |
| [`read_file`](read_file.toml) | file | always | yes | yes | none observed |
| [`save_state`](save_state.toml) | scratch | always | no | no | none observed |
| [`search`](search.toml) | search | always | yes | no | none observed |
| [`task_assign`](task_assign.toml) | task | always | no | no | none observed |
| [`task_cancel`](task_cancel.toml) | task | always | no | no | none observed |
| [`task_complete`](task_complete.toml) | task | always | no | no | yes |
| [`task_create`](task_create.toml) | task | always | no | no | yes |
| [`task_list`](task_list.toml) | task | always | yes | yes | none observed |
| [`task_start`](task_start.toml) | task | always | no | no | yes |
| [`write_file`](write_file.toml) | file | always | no | no | none observed |
