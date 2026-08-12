---
id: tool-reference
title: "docs/tools/ — the generated per-tool reference"
status: living
---

<!-- GENERATED FILE, DO NOT EDIT. Regenerate: make tool-docs-update -->

# `docs/tools/`

One TOML page per dispatchable tool — 78 of them — generated from the declarations by `crates/stella-cli/src/tool_docs.rs` and re-derived by the `tool-docs` gate step. A tool added to `crates/stella-tools/src/catalog.rs` without regenerating turns the gate red; there is no path where a new tool ships undocumented.

Each page carries the tool's name, description, input schema, output schema, `read_only`, `available_for_speculation`, category, and a commented example input and output payload.

Two fields are stated absences rather than values, because inventing them would manufacture a source of truth nobody reviewed:

- **`risk_level` is `"undeclared"`.** Nothing in the repository carries a per-tool risk level. Tracked in #3060.
- **`output_schema` is the envelope only.** Every tool answers in `ToolOutput { ok | error }`, which is declared and is what the field holds; the shape of the text inside `ok.content` is a per-tool convention with nothing behind it, so the observed example is its only evidence.

**Examples are observed, not written.** They come from Terminal-Bench trial traces (stella-events.jsonl), distilled by scripts/build-tool-doc-examples.py, captured 2026-08-11 over 2829 call/result pairs across 10 tasks. 21 of 78 tools carry a real example; the rest say so. 33 tools were never called once across 408 trials and 15304 model calls — the most interesting fact on those pages, and input to #3032.

| Tool | Category | Availability | Read-only | Speculation-safe | Observed example |
|---|---|---|---|---|---|
| [`apply_edits`](apply_edits.toml) | file | always | no | no | yes |
| [`ask_user`](ask_user.toml) | session | cli-session-layer | no | no | none observed |
| [`bash`](bash.toml) | bash | always | no | no | yes |
| [`build_project`](build_project.toml) | build | always | no | no | none observed |
| [`ci_status`](ci_status.toml) | ci | always | yes | no | none observed |
| [`cite_memory`](cite_memory.toml) | context | always | no | no | none observed |
| [`clear_output`](clear_output.toml) | process | always | no | no | none observed |
| [`close_issue`](close_issue.toml) | issue | requires-issue-backend | no | no | none observed |
| [`create_issue`](create_issue.toml) | issue | requires-issue-backend | no | no | none observed |
| [`delete_file`](delete_file.toml) | file | always | no | no | yes |
| [`delete_state`](delete_state.toml) | scratch | always | no | no | none observed |
| [`diagnostics`](diagnostics.toml) | build | always | yes | no | none observed |
| [`edit_file`](edit_file.toml) | file | always | no | no | yes |
| [`explorations`](explorations.toml) | context | always | yes | yes | none observed |
| [`format_code`](format_code.toml) | build | always | no | no | none observed |
| [`gather_context`](gather_context.toml) | context | always | yes | no | none observed |
| [`generate_image`](generate_image.toml) | media | requires-media-key | no | no | none observed |
| [`generate_svg`](generate_svg.toml) | media | always | no | no | none observed |
| [`generate_video`](generate_video.toml) | media | requires-video-capable-media-key | no | no | none observed |
| [`get_environment`](get_environment.toml) | environment | always | yes | yes | yes |
| [`get_issue`](get_issue.toml) | issue | requires-issue-backend | yes | no | none observed |
| [`get_state`](get_state.toml) | scratch | always | yes | yes | none observed |
| [`glob`](glob.toml) | search | always | yes | yes | yes |
| [`graph_query`](graph_query.toml) | search | always | yes | no | none observed |
| [`grep`](grep.toml) | search | always | yes | yes | none observed |
| [`install_skill`](install_skill.toml) | session | cli-session-layer | no | no | none observed |
| [`invoke_skill`](invoke_skill.toml) | session | cli-session-layer | no | no | none observed |
| [`list_labels`](list_labels.toml) | issue | requires-issue-backend | yes | no | none observed |
| [`list_members`](list_members.toml) | issue | requires-issue-backend | yes | no | none observed |
| [`list_scripts`](list_scripts.toml) | scripts | always | yes | yes | none observed |
| [`list_state`](list_state.toml) | scratch | always | yes | yes | none observed |
| [`mcp_search`](mcp_search.toml) | session | cli-session-layer | yes | no | none observed |
| [`poll_video`](poll_video.toml) | media | requires-video-capable-media-key | no | no | none observed |
| [`probe_capability`](probe_capability.toml) | environment | always | yes | yes | yes |
| [`project_overview`](project_overview.toml) | context | always | yes | no | yes |
| [`read_file`](read_file.toml) | file | always | yes | yes | yes |
| [`read_output`](read_output.toml) | process | always | no | no | yes |
| [`read_symbol`](read_symbol.toml) | file | always | yes | no | none observed |
| [`recall_context`](recall_context.toml) | context | cli-session-layer | yes | no | none observed |
| [`repo_commit`](repo_commit.toml) | repo | always | no | no | yes |
| [`repo_diff`](repo_diff.toml) | repo | always | yes | yes | none observed |
| [`repo_history`](repo_history.toml) | repo | always | yes | yes | none observed |
| [`repo_pull`](repo_pull.toml) | repo | always | no | no | none observed |
| [`repo_push`](repo_push.toml) | repo | always | no | no | none observed |
| [`repo_recover`](repo_recover.toml) | repo | always | yes | yes | none observed |
| [`repo_rollback`](repo_rollback.toml) | repo | always | no | no | none observed |
| [`repo_status`](repo_status.toml) | repo | always | yes | yes | yes |
| [`restart_process`](restart_process.toml) | process | always | no | no | none observed |
| [`run_lint`](run_lint.toml) | build | always | no | no | none observed |
| [`run_script`](run_script.toml) | scripts | always | no | no | none observed |
| [`run_tests`](run_tests.toml) | build | always | no | no | none observed |
| [`save_exploration`](save_exploration.toml) | context | always | no | no | none observed |
| [`save_memory`](save_memory.toml) | context | always | no | no | yes |
| [`save_state`](save_state.toml) | scratch | always | no | no | none observed |
| [`screenshot`](screenshot.toml) | ci | always | no | no | yes |
| [`search_issues`](search_issues.toml) | issue | requires-issue-backend | yes | no | none observed |
| [`search_skills`](search_skills.toml) | session | cli-session-layer | yes | no | none observed |
| [`semantic_code_search`](semantic_code_search.toml) | search | always | yes | no | none observed |
| [`send_stdin`](send_stdin.toml) | process | always | no | no | none observed |
| [`skill_search`](skill_search.toml) | session | cli-session-layer | yes | yes | none observed |
| [`start_process`](start_process.toml) | process | always | no | no | yes |
| [`start_work_on_issue`](start_work_on_issue.toml) | issue | requires-issue-backend | no | no | none observed |
| [`stop_process`](stop_process.toml) | process | always | no | no | yes |
| [`task`](task.toml) | task | always | no | no | none observed |
| [`task_assign`](task_assign.toml) | task | always | no | no | none observed |
| [`task_cancel`](task_cancel.toml) | task | always | no | no | none observed |
| [`task_complete`](task_complete.toml) | task | always | no | no | yes |
| [`task_create`](task_create.toml) | task | always | no | no | yes |
| [`task_list`](task_list.toml) | task | always | yes | yes | none observed |
| [`task_start`](task_start.toml) | task | always | no | no | yes |
| [`tool_search`](tool_search.toml) | session | cli-session-layer | yes | yes | none observed |
| [`update_issue`](update_issue.toml) | issue | requires-issue-backend | no | no | none observed |
| [`verify_done`](verify_done.toml) | build | always | no | no | yes |
| [`web_download`](web_download.toml) | web | always | no | no | none observed |
| [`web_extract_assets`](web_extract_assets.toml) | web | always | yes | no | none observed |
| [`web_fetch`](web_fetch.toml) | web | always | yes | no | none observed |
| [`web_search`](web_search.toml) | web | requires-search-key | yes | no | none observed |
| [`write_file`](write_file.toml) | file | always | no | no | yes |
