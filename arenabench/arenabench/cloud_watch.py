# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""A live scoreboard for a cloud run — the head-to-head the arena cannot show.

The arena on :8900 reads *local match directories* and can only stream a match
its own server launched; anything else is restored from disk and rendered
"finished" with zero trials. A cloud run has no local match directory at all —
its state is Batch job states plus ``runs/<id>/<job>/results.json`` in S3 — so
the arena is structurally the wrong window for it. Teaching the arena to read
the substrate is issue #2100; this module is the purpose-built view that does
not wait for it.

The split here is deliberate and is what makes the thing testable without an
AWS account: :func:`assemble` and :func:`render_html` are **pure functions**
over already-fetched data, and only :class:`CloudScoreboard` talks to S3,
DynamoDB and Batch. Every test below drives the pure half with fixture dicts.

Two honesty rules it inherits from the rest of the harness:

* **A missing number is never invented.** A trial with no ``results.json`` and
  no terminal Batch state renders as ``running``/``queued``, never as 0.0 — a
  fabricated zero is indistinguishable from a real loss, which is the exact
  confusion that has cost this project whole benchmark runs.
* **The verifier's reward is the score, even when the agent errored.** Harbor
  records a non-zero agent exit as a loss and Stella's verdict exits non-zero
  by design, so reading the exit code instead of the reward systematically
  under-counts Stella. Only an attempted, finished-or-errored trial with no
  reward at all is a zero.
"""

from __future__ import annotations

import html
import json
from dataclasses import dataclass, field
from typing import Any, Iterable

__all__ = [
    "Cell",
    "Scoreboard",
    "assemble",
    "cell_for",
    "reward_of",
    "render_html",
]

#: Batch states that mean the trial will produce nothing further.
TERMINAL_STATES: frozenset[str] = frozenset({"SUCCEEDED", "FAILED"})
#: Batch states that mean the trial has not started computing yet.
QUEUED_STATES: frozenset[str] = frozenset(
    {"SUBMITTED", "PENDING", "RUNNABLE", "STARTING"}
)


def reward_of(results: Any) -> float | None:
    """The canonical Terminal-Bench score in a ``results.json``, or ``None``.

    ``None`` means *unknown*, and unknown is never rendered as zero. The path
    is ``verifier_result.rewards.reward`` — the verifier's number, which is
    the official leaderboard's score. A trial that finished or raised with no
    reward is a real 0.0 and is reported as such; a trial with neither is
    still in flight and reports nothing.

    Accepts either a single result object or Harbor's ``{"results": [...]}``
    envelope, because both shapes have been written to this key.
    """
    if isinstance(results, dict) and isinstance(results.get("results"), list):
        rewards = [reward_of(entry) for entry in results["results"]]
        known = [value for value in rewards if value is not None]
        return sum(known) / len(known) if known else None
    if not isinstance(results, dict):
        return None
    verifier = results.get("verifier_result")
    verifier = verifier if isinstance(verifier, dict) else {}
    rewards = verifier.get("rewards")
    rewards = rewards if isinstance(rewards, dict) else {}
    raw = rewards.get("reward")
    if isinstance(raw, bool):  # bool is an int; a flag is not a score
        return None
    if isinstance(raw, (int, float)):
        return float(raw)
    exception = results.get("exception_info")
    attempted = bool(exception) or bool(results.get("finished_at"))
    return 0.0 if attempted else None


@dataclass(frozen=True)
class Cell:
    """One (task, contestant) square of the grid."""

    state: str
    """``won`` | ``lost`` | ``scored`` | ``running`` | ``queued`` | ``unknown``."""
    reward: float | None = None
    detail: str = ""

    @property
    def text(self) -> str:
        if self.reward is not None:
            mark = {"won": "✓", "lost": "✗"}.get(self.state, "·")
            return f"{mark} {self.reward:.1f}"
        return self.state.upper()


def cell_for(batch_state: str | None, results: Any, detail: str = "") -> Cell:
    """One cell from a trial's Batch state and (possibly absent) results.

    The reward wins whenever there is one — a SUCCEEDED job with no reward is
    not a win, and a FAILED job that still produced a verifier reward keeps
    it, because the container's exit code and the task's score are different
    questions.
    """
    reward = reward_of(results)
    if reward is not None:
        state = "won" if reward >= 1.0 else "lost" if reward <= 0.0 else "scored"
        return Cell(state=state, reward=reward, detail=detail)
    if batch_state in QUEUED_STATES:
        return Cell(state="queued", detail=detail)
    if batch_state == "RUNNING":
        return Cell(state="running", detail=detail)
    if batch_state in TERMINAL_STATES:
        # Terminal with nothing to show: the trial died before writing
        # results. Reported as its own state rather than as a zero, because
        # an infrastructure death is not an agent loss and must not be
        # averaged into one.
        return Cell(state="unknown", detail=detail or f"{batch_state}, no results")
    return Cell(state="unknown", detail=detail)


@dataclass
class Scoreboard:
    """The assembled grid, ready to render."""

    run_id: str
    contestants: list[str] = field(default_factory=list)
    tasks: list[str] = field(default_factory=list)
    cells: dict[tuple[str, str], Cell] = field(default_factory=dict)

    def totals(self) -> dict[str, float]:
        """Summed reward per contestant, over scored cells only."""
        out = {contestant: 0.0 for contestant in self.contestants}
        for (task, contestant), cell in self.cells.items():
            if cell.reward is not None and contestant in out:
                out[contestant] += cell.reward
        return out

    def settled(self) -> int:
        """Grid squares carrying a real number."""
        return sum(1 for cell in self.cells.values() if cell.reward is not None)


def assemble(
    run_id: str,
    jobs: Iterable[dict[str, Any]],
    batch_states: dict[str, str],
    results_by_label: dict[str, Any],
    details: dict[str, str] | None = None,
) -> Scoreboard:
    """Build the grid from a submission record and whatever has landed.

    Pure: every input is already-fetched data, so the whole rendering path is
    testable with fixture dicts and no AWS account.
    """
    details = details or {}
    board = Scoreboard(run_id=run_id)
    for job in jobs:
        contestant = job.get("contestant_id") or "?"
        task = job.get("task") or "(whole dataset)"
        if contestant not in board.contestants:
            board.contestants.append(contestant)
        if task not in board.tasks:
            board.tasks.append(task)
        label = job.get("label", "")
        board.cells[(task, contestant)] = cell_for(
            batch_states.get(job.get("id", "")),
            results_by_label.get(label),
            details.get(label, ""),
        )
    return board


def render_html(board: Scoreboard, *, refresh_seconds: int = 5) -> str:
    """The whole page, self-contained — no CDN, no build step, no fonts."""
    totals = board.totals()
    head = "".join(
        f"<th>{html.escape(name)}</th>" for name in board.contestants
    )
    rows = []
    for task in board.tasks:
        cells = []
        for contestant in board.contestants:
            cell = board.cells.get((task, contestant), Cell(state="queued"))
            title = html.escape(cell.detail) if cell.detail else ""
            cells.append(
                f'<td class="s-{html.escape(cell.state)}" title="{title}">'
                f"{html.escape(cell.text)}</td>"
            )
        rows.append(f"<tr><td class='t'>{html.escape(task)}</td>{''.join(cells)}</tr>")
    total_cells = "".join(
        f"<td class='tot'>{totals.get(name, 0.0):.1f}</td>" for name in board.contestants
    )
    grid = len(board.tasks) * max(len(board.contestants), 1)
    return f"""<!doctype html>
<meta charset="utf-8">
<meta http-equiv="refresh" content="{refresh_seconds}">
<title>{html.escape(board.run_id)} — arenabench</title>
<style>
:root {{ color-scheme: dark; }}
body {{ background:#0B0B0C; color:#E8E8E8; font:14px/1.5 ui-monospace,monospace;
        margin:0; padding:2rem; }}
h1 {{ font-size:1rem; font-weight:600; color:#FFB000; margin:0 0 .25rem; }}
p.meta {{ color:#8A8A8A; margin:0 0 1.5rem; }}
table {{ border-collapse:collapse; width:100%; max-width:60rem; }}
th,td {{ text-align:left; padding:.4rem .75rem; border-bottom:1px solid #1E1E20; }}
th {{ color:#8A8A8A; font-weight:500; }}
td.t {{ color:#C8C8C8; }}
td.tot {{ color:#FFB000; font-weight:600; }}
.s-won {{ color:#4ADE80; }} .s-lost {{ color:#F87171; }} .s-scored {{ color:#FBBF24; }}
.s-running {{ color:#60A5FA; }} .s-queued,.s-unknown {{ color:#6B7280; }}
tr.totals td {{ border-top:2px solid #1E1E20; border-bottom:none; }}
</style>
<h1>{html.escape(board.run_id)}</h1>
<p class="meta">{board.settled()} of {grid} trials settled · refreshes every
{refresh_seconds}s · a blank cell is unknown, never a zero</p>
<table>
<tr><th>task</th>{head}</tr>
{''.join(rows)}
<tr class="totals"><td class='t'>reward</td>{total_cells}</tr>
</table>
"""


class CloudScoreboard:
    """The impure half: fetch a run's state, assemble, render.

    Separate from the functions above so that the only thing needing AWS is
    the fetching, and the only thing needing tests is everything else.
    """

    def __init__(self, executor: Any, run_id: str) -> None:
        self.executor = executor
        self.run_id = run_id

    def fetch(self) -> Scoreboard:
        record = self.executor.load_jobs(self.run_id)
        jobs = record["jobs"]
        states = self.executor.job_states([job["id"] for job in jobs])
        details = {}
        try:
            for row in self.executor.run_rows(self.run_id):
                details[row.get("job", "")] = row.get("detail", "")
        except Exception:  # noqa: BLE001 - a missing detail is cosmetic
            pass
        results: dict[str, Any] = {}
        for job in jobs:
            body = self._results(job["label"])
            if body is not None:
                results[job["label"]] = body
        return assemble(self.run_id, jobs, states, results, details)

    def _results(self, label: str) -> Any:
        """One trial's ``results.json``, or ``None`` while it does not exist.

        Absence is the normal state for most of a run and must not raise: a
        watcher that dies on the first unwritten result would only ever work
        on a finished run, which is the one case it is not needed for.
        """
        s3 = self.executor._client("s3")  # noqa: SLF001 - same package
        key = f"runs/{self.run_id}/{label}/results.json"
        try:
            body = s3.get_object(Bucket=self.executor.bucket(), Key=key)
            return json.loads(body["Body"].read())
        except Exception:  # noqa: BLE001 - NoSuchKey and friends are "not yet"
            return None
