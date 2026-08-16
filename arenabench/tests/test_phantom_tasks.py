# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Refusing a match that names a task the dataset does not contain (#3255)."""

from __future__ import annotations

from arenabench.model import MatchSpec

class TestPhantomTaskRefusal:
    """#3255: a task name the dataset does not contain must be refused at
    submit, not dispatched as a container that can only exit 1.

    Three consecutive 89-task cloud runs each lost their index-000 job
    because the submitted task list began with the literal string ``"89"`` —
    the task count where a task slug belongs. ``plan_trials`` fanned it out
    like any other name and the trial died as an operational abort, visible
    only in the job label ``<run>-000-<seat>-89``.
    """

    @staticmethod
    def _spec(tasks: list[str]) -> MatchSpec:
        return MatchSpec.from_json(
            {
                "dataset": "terminal-bench-2.1",
                "tasks": tasks,
                "contestants": [
                    {"name": "stella", "agent": "stella", "engine": {"model": "m"}}
                ],
            }
        )

    def test_the_phantom_89_is_named_as_unknown(self):
        spec = self._spec(["89", "fix-git"])
        assert spec.unknown_tasks({"fix-git", "hello-world"}) == ["89"]

    def test_a_qualified_name_is_accepted(self):
        """A spec may spell a task either way; rejecting the qualified form
        would be a worse failure than the one this guard exists to catch."""
        spec = self._spec(["terminal-bench/fix-git"])
        assert spec.unknown_tasks({"fix-git", "terminal-bench/fix-git"}) == []

    def test_an_unmaterialised_corpus_cannot_tell_and_says_nothing(self):
        """An empty corpus means "cannot enumerate", never "no tasks exist" —
        a submitting host without the dataset on disk must not be blocked."""
        spec = self._spec(["89", "fix-git"])
        assert spec.unknown_tasks(set()) == []

