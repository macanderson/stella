# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""What KIND of thing a tool call was, and the categorical class that says so.

A port of ``crates/stella-tui/src/tool_class.rs``: six visual classes (read /
write / run / verify / repo / delegate), the same ``(read_only, group)`` table
``stella-tools::catalog`` carries, and the same group -> class switch. Two
readers of the same wire event must agree on what class it belongs to — a
transcript rendered by the arena and the same trial replayed in the Command
Deck are describing the same run, and a tool that shows as a "write" in one
and a "run" in the other is a rendering bug, not a difference of opinion.

``_CATALOG`` is copied, not derived: Python has no route to the Rust crate
this data actually lives in. It was generated from the `catalog!` table in
``crates/stella-tools/src/catalog.rs`` with::

    rg -o '"\\w+"\\s*=>\\s*\\((true|false), (true|false), \\w+, "\\w+"\\)' \\
        crates/stella-tools/src/catalog.rs

``tests/test_toolclass.py`` pins the deck's own classification examples
(``crates/stella-tui/src/tool_class.rs``'s ``each_family_lands_in_its_own_class``
and ``a_read_and_a_write_of_the_same_family_are_different_classes``) against
this port, so a name the Rust catalog reclassifies and this table does not
follow shows up as a red test rather than a silent drift.
"""

from __future__ import annotations

__all__ = ["CLASSES", "classify", "class_label"]

#: `(read_only, group)` per dispatch name — copied verbatim from the
#: `catalog!` table in `crates/stella-tools/src/catalog.rs`. `speculation_safe`
#: and `availability` are that table's other two columns; neither one bears on
#: which of the six visual classes a call renders as, so only the two that do
#: are carried over.
_CATALOG: dict[str, tuple[bool, str]] = {
    "read_file": (True, "file"),
    "read_symbol": (True, "file"),
    "write_file": (False, "file"),
    "edit_file": (False, "file"),
    "apply_edits": (False, "file"),
    "delete_file": (False, "file"),
    "search": (True, "search"),
    "grep": (True, "search"),
    "glob": (True, "search"),
    "graph_query": (True, "search"),
    "semantic_code_search": (True, "search"),
    "project_overview": (True, "context"),
    "gather_context": (True, "context"),
    "explorations": (True, "context"),
    "save_exploration": (False, "context"),
    "save_memory": (False, "context"),
    "cite_memory": (False, "context"),
    "verify_done": (False, "build"),
    "build_project": (False, "build"),
    "run_tests": (False, "build"),
    "diagnostics": (True, "build"),
    "run_lint": (False, "build"),
    "format_code": (False, "build"),
    "probe_capability": (True, "environment"),
    "list_scripts": (True, "scripts"),
    "run_script": (False, "scripts"),
    "start_process": (False, "process"),
    "read_output": (False, "process"),
    "clear_output": (False, "process"),
    "send_stdin": (False, "process"),
    "restart_process": (False, "process"),
    "stop_process": (False, "process"),
    "repo_status": (True, "repo"),
    "repo_diff": (True, "repo"),
    "repo_history": (True, "repo"),
    "repo_recover": (True, "repo"),
    "repo_commit": (False, "repo"),
    "repo_push": (False, "repo"),
    "repo_pull": (False, "repo"),
    "repo_rollback": (False, "repo"),
    "ci_status": (True, "ci"),
    "screenshot": (False, "ci"),
    "generate_svg": (False, "media"),
    "task_create": (False, "task"),
    "task_list": (True, "task"),
    "task_start": (False, "task"),
    "task_complete": (False, "task"),
    "task_cancel": (False, "task"),
    "task_assign": (False, "task"),
    "task": (False, "task"),
    "bash": (False, "bash"),
    "save_state": (False, "scratch"),
    "get_state": (True, "scratch"),
    "list_state": (True, "scratch"),
    "delete_state": (False, "scratch"),
    "get_environment": (True, "environment"),
    "web_fetch": (True, "web"),
    "web_extract_assets": (True, "web"),
    "web_download": (False, "web"),
    "web_search": (True, "web"),
    "generate_image": (False, "media"),
    "generate_video": (False, "media"),
    "poll_video": (False, "media"),
    "create_issue": (False, "issue"),
    "update_issue": (False, "issue"),
    "close_issue": (False, "issue"),
    "search_issues": (True, "issue"),
    "get_issue": (True, "issue"),
    "list_labels": (True, "issue"),
    "list_members": (True, "issue"),
    "start_work_on_issue": (False, "issue"),
    "ask_user": (False, "session"),
    "search_skills": (True, "session"),
    "install_skill": (False, "session"),
    "tool_search": (True, "session"),
    "skill_search": (True, "session"),
    "invoke_skill": (False, "session"),
    "mcp_search": (True, "session"),
    "recall_context": (True, "context"),
}

#: Groups that hold both a read and a write, split on the catalog's own
#: `read_only` bit — `ToolClass::classify`'s first match arm.
_SPLIT_GROUPS = frozenset({"file", "context", "scratch"})

#: Every other group's class, fixed regardless of `read_only`.
_GROUP_CLASS: dict[str, str] = {
    "search": "inspect",
    "environment": "inspect",
    "web": "inspect",
    "media": "mutate",
    "bash": "execute",
    "process": "execute",
    "scripts": "execute",
    "build": "verify",
    "ci": "verify",
    "repo": "repo",
    "issue": "repo",
    "task": "delegate",
    "session": "delegate",
}

#: The six visual classes, in the deck's legend order.
CLASSES: tuple[str, ...] = ("inspect", "mutate", "execute", "verify", "repo", "delegate")

#: One-word label per class — matches `ToolClass::label()`.
_LABELS: dict[str, str] = {
    "inspect": "read",
    "mutate": "write",
    "execute": "run",
    "verify": "verify",
    "repo": "repo",
    "delegate": "delegate",
}


def classify(name: str) -> str:
    """Classify one dispatch name into `inspect | mutate | execute | verify |
    repo | delegate`.

    An unrecognized name — an MCP server's tool (`mcp__<server>__<tool>`), or
    a customer's own manifest tool — falls back to the catalog's `_ if
    read_only` arm: unknown names are never in `_CATALOG`, so `read_only`
    there is always `False` and the fallback is always `execute`, the class
    that promises the least about what a call nobody here recognizes actually
    did. That matches the Rust default arm exactly, which is reached the same
    way for the same names — `an_unknown_tool_promises_the_least` in
    `tool_class.rs` is the same assertion against the same two example names.
    """
    entry = _CATALOG.get(name)
    if entry is not None:
        read_only, group = entry
    else:
        read_only, group = False, ("mcp" if name.startswith("mcp__") else "custom")

    if group in _SPLIT_GROUPS:
        return "inspect" if read_only else "mutate"
    cls = _GROUP_CLASS.get(group)
    if cls is not None:
        return cls
    return "inspect" if read_only else "execute"


def class_label(cls: str) -> str:
    """The one-word legend label for a class key `classify` returned."""
    return _LABELS.get(cls, cls)
