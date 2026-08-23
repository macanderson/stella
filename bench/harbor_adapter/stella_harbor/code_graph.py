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

import re
from typing import Any

#: The step did not run at all. A real state, and deliberately spelled the
#: same way :func:`run_git_baseline`'s absence is, so a reader learns one
#: convention rather than two.
NOT_ATTEMPTED: dict[str, str] = {"state": "not_attempted"}

#: The substring ``stella init`` prints its index summary on.
_SUMMARY_MARKER = "code graph:"

#: The substring every line about the semantic-search embedding pass carries
#: — progress ticks and the final outcome line alike
#: (``warm_semantic_index``/``format_warm_outcome`` in
#: ``crates/stella-cli/src/agent/graph.rs``).
_SEMANTIC_MARKER = "semantic index:"

#: ``format_warm_outcome``'s success line: ``✓ semantic index: N files
#: embedded by <model>``, with or without a trailing "N left unembedded"
#: clause the parser does not need. Matched only against a line already
#: known to start with the ✓ prefix, so a partial/failed line with the same
#: "N files embedded by <model>" substring can never be mistaken for one.
_SEMANTIC_BUILT_PATTERN = re.compile(
    r"(?P<count>\d+) files? embedded by (?P<model>\S+)"
)


def from_stdout(stdout: str | None) -> dict[str, Any]:
    """Classify what ``stella init`` said about the graph it built.

    ``no_summary_line`` is the state that earns this function: an init that
    ran, exited cleanly and printed nothing about a code graph is not the
    same event as an init that reported an empty one, and the two were
    previously indistinguishable because only the second was ever recorded.
    The line count rides along because it is the cheapest thing that
    separates "produced no output at all" from "produced output we did not
    recognise" without shipping the container's stdout into trial metadata.

    The returned dict carries a ``semantic`` sub-dict answering the
    independent question of whether the semantic index was built — built,
    skipped for want of a backend, or attempted and not fully built — so a
    trial's metadata no longer reads identically across all three (#3669).
    """
    text = stdout or ""
    lines = text.splitlines()
    result: dict[str, Any] | None = None
    for line in lines:
        if _SUMMARY_MARKER in line:
            result = {"state": "reported", "summary": line.strip()}
            break
    if result is None:
        result = {
            "state": "no_summary_line",
            "detail": (
                f"`stella init` exited without a '{_SUMMARY_MARKER}' line "
                f"({len(lines)} line(s) of stdout)"
            ),
        }
    return {**result, "semantic": _semantic_from_lines(lines)}


def _semantic_from_lines(lines: list[str]) -> dict[str, str]:
    """Classify the semantic-index outcome, keyed on the *last* matching line.

    Progress ticks (``· semantic index: N files embedded…``) share the same
    marker as the final outcome line and are printed first, so the last
    match in program order is always the terminal state — the one
    ``format_warm_outcome`` writes once the pass has stopped.
    """
    semantic_lines = [line.strip() for line in lines if _SEMANTIC_MARKER in line]
    if not semantic_lines:
        return {"state": "no_summary_line"}
    last = semantic_lines[-1]
    if "skipped" in last:
        return {"state": "skipped_no_backend", "line": last}
    if last.startswith("✓"):
        match = _SEMANTIC_BUILT_PATTERN.search(last)
        if match:
            return {
                "state": "built",
                "files_embedded": match.group("count"),
                "model": match.group("model"),
                "line": last,
            }
    return {"state": "attempted_failed", "line": last}


#: Harbor's own ``InstalledAgentBase._exec`` (harbor/agents/installed/base.py)
#: formats a non-zero exit exactly this way, with ``stdout``/``stderr``
#: already truncated to 1000 characters on its side. Parsed rather than
#: re-truncated here, so a change to Harbor's truncation length cannot
#: silently disagree with a second one of ours.
_NONZERO_EXIT_PATTERN = re.compile(
    r"^Command failed \(exit (?P<exit_code>-?\d+)\):.*?\nstderr: (?P<stderr>.*)$",
    re.DOTALL,
)


def unavailable(error: BaseException) -> dict[str, str]:
    """The step raised. Best-effort by construction, so never fatal — but a
    failure that is not recorded is a failure that gets attributed to
    whatever is measured next.

    ``kind`` carries the exception's class name so a reader does not have to
    re-derive, from the message shape alone, which of Harbor's two failure
    modes this was. The two disclose differently: a non-zero exit
    (``NonZeroAgentExitCodeError``) embeds an exit code and captured stderr
    in its message, which this function lifts into their own ``exit_code``/
    ``stderr`` fields; a timeout (``RuntimeError("Command timed out after N
    seconds")``) does not, because Harbor's own Docker exec discards the
    process's stdout/stderr before raising it, and there is nothing this
    adapter can recover for that path (#3670).
    """
    message = str(error)
    result = {"state": "unavailable", "kind": type(error).__name__, "detail": message}
    match = _NONZERO_EXIT_PATTERN.match(message)
    if match:
        result["exit_code"] = match.group("exit_code")
        result["stderr"] = match.group("stderr")
    return result
