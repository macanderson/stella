# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""`arenabench.toolclass` against the Command Deck's own classification rules.

Every example here is copied from `crates/stella-tui/src/tool_class.rs`'s own
test module — `each_family_lands_in_its_own_class`,
`a_read_and_a_write_of_the_same_family_are_different_classes`, and
`an_unknown_tool_promises_the_least` — so a transcript rendered by the arena
and the same trial replayed in the deck are pinned to agree, name for name.
A future edit to either side's table that drops one of these names into a
different class is exactly the defect this file exists to catch.

Most of the names below appear only in archived traces; they pin the LEGACY
half of the table, which exists so archived matches keep rendering with the
classes they were recorded under.
"""

from __future__ import annotations

from arenabench.toolclass import classify, class_label


class TestEachFamilyLandsInItsOwnClass:
    """Mirrors `tool_class.rs::each_family_lands_in_its_own_class`."""

    def test_examples(self) -> None:
        for name, expected in [
            ("grep", "inspect"),
            ("glob", "inspect"),
            ("project_overview", "inspect"),
            ("graph_query", "inspect"),
            ("web_fetch", "inspect"),
            ("write_file", "mutate"),
            ("save_memory", "mutate"),
            ("bash", "execute"),
            ("run_script", "execute"),
            ("start_process", "execute"),
            ("verify_done", "verify"),
            ("run_tests", "verify"),
            ("diagnostics", "verify"),
            ("ci_status", "verify"),
            ("repo_push", "repo"),
            ("repo_status", "repo"),
            ("create_issue", "repo"),
            ("delegate", "delegate"),
            ("task", "delegate"),
            ("task_complete", "delegate"),
            ("invoke_skill", "delegate"),
        ]:
            assert classify(name) == expected, name


class TestReadWriteSplitWithinAFamily:
    """Mirrors `a_read_and_a_write_of_the_same_family_are_different_classes` —
    the one distinction the catalog's `group` column cannot draw on its own."""

    def test_examples(self) -> None:
        for read, write in [
            ("read_file", "write_file"),
            ("read_file", "edit_file"),
            ("read_file", "apply_edits"),
            ("read_file", "delete_file"),
            ("explorations", "save_exploration"),
            ("get_state", "save_state"),
            ("list_state", "delete_state"),
        ]:
            assert classify(read) == "inspect", read
            assert classify(write) == "mutate", write


class TestUnknownToolsPromiseTheLeast:
    """Mirrors `an_unknown_tool_promises_the_least`: a name nobody here has
    heard of never defaults to the read colour, since that would paint an MCP
    server's mutating call as if it were safe to ignore."""

    def test_examples(self) -> None:
        assert classify("mcp__github__create_pull_request") == "execute"
        assert classify("some_customer_manifest_tool") == "execute"


class TestClassLabels:
    def test_every_class_has_a_one_word_label(self) -> None:
        assert class_label("inspect") == "read"
        assert class_label("mutate") == "write"
        assert class_label("execute") == "run"
        assert class_label("verify") == "verify"
        assert class_label("repo") == "repo"
        assert class_label("delegate") == "delegate"
