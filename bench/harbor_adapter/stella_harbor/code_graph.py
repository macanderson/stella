"""What ``stella init`` built in the container, as a state a trial can record.

``_build_code_graph`` runs ``stella init`` before the first turn and parses a
``code graph:`` line out of its stdout. That line was assigned to an instance
attribute and **read nowhere** — one write, zero reads, no class-level
declaration — so every container computed the answer and dropped it.

The cost of dropping it is #3087. A real trial reported ``files: 0,
symbols: 0, imports: 0`` from inside an ``/app`` workspace that git had a
committed, non-empty baseline for, and the recorded evidence could not say
which of four things had happened:

* the step never ran (an outer timeout, an adapter that predates it);
* it ran and raised;
* it ran, exited cleanly, and printed nothing about a code graph;
* it ran and honestly reported an empty graph.

All four left the same trace, which was none — so a ``files: 0`` reading was
consistent with every one of them. Evidence that cannot distinguish a claim
from its opposite is not evidence, and no amount of reasoning about the
container recovers what was never recorded.

This module is the classification, kept out of ``__init__.py`` because that
file is grandfathered at its file-size ceiling and closed to growth. It is
the shape :mod:`upstream_pin` and :mod:`tool_set` already use: a small
pure module beside the adapter, so the adapter keeps one line per concern.
"""

from __future__ import annotations

#: The step did not run at all. A real state, and deliberately spelled the
#: same way :func:`run_git_baseline`'s absence is, so a reader learns one
#: convention rather than two.
NOT_ATTEMPTED: dict[str, str] = {"state": "not_attempted"}

#: The substring ``stella init`` prints its index summary on.
_SUMMARY_MARKER = "code graph:"


def from_stdout(stdout: str | None) -> dict[str, str]:
    """Classify what ``stella init`` said about the graph it built.

    ``no_summary_line`` is the state that earns this function: an init that
    ran, exited cleanly and printed nothing about a code graph is not the
    same event as an init that reported an empty one, and the two were
    previously indistinguishable because only the second was ever recorded.
    The line count rides along because it is the cheapest thing that
    separates "produced no output at all" from "produced output we did not
    recognise" without shipping the container's stdout into trial metadata.
    """
    text = stdout or ""
    for line in text.splitlines():
        if _SUMMARY_MARKER in line:
            return {"state": "reported", "summary": line.strip()}
    return {
        "state": "no_summary_line",
        "detail": (
            f"`stella init` exited without a '{_SUMMARY_MARKER}' line "
            f"({len(text.splitlines())} line(s) of stdout)"
        ),
    }


def unavailable(error: BaseException) -> dict[str, str]:
    """The step raised. Best-effort by construction, so never fatal — but a
    failure that is not recorded is a failure that gets attributed to
    whatever is measured next."""
    return {"state": "unavailable", "detail": str(error)}
