# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Fetch → gate → cancel, composed as one callback the watch loop can hold.

:mod:`.gate` knows how to ask a checker about a trial and :mod:`.cancel` knows
how to stop a run; neither knows about the other, and ``cloud.py`` should not
have to. This module is the join — and it lives here rather than inside
``CloudExecutor.watch`` for two reasons. The dull one: ``cloud.py`` is within
a hundred lines of the repository's 1500-line file-size ceiling and is not in
the grandfather list, so the composition has to land in a sibling module
regardless. The required one: a three-way composition wired inline into a
polling loop can only be tested by driving the polling loop, and the thing
most worth testing here is what happens on the paths that *do not* abort.

**Why the hook is on the submitter's watch loop and not inside the container.**
One Batch job is one trial, so a trial that detects its own banned behavior can
only stop itself — and it has already finished by the time there is anything
to detect. The cost worth saving is the other N-1 trials, and the only process
that can cancel those is the one holding the run's ``jobs.json``. An
in-container gate would also have to ship the detector ledger into the runner
image, which is precisely the agent-specific dependency the subprocess seam
exists to keep out of an agent-agnostic package (#3024).

**Only the settled trial's artifacts are pulled.** A full-run fetch on every
settlement would download the same objects N times and, worse, would make the
gate's cost scale with the run rather than with the trial.
"""

from __future__ import annotations

import json
from collections.abc import Callable, Mapping, Sequence
from pathlib import Path
from typing import Any

from .cancel import CancelPlan, abort_record, cancel_run
from .gate import GateSpec, run_gate

__all__ = ["ABORT_FILENAME", "GateWatch", "settled_gate"]

#: Where the dispute artifact lands, beside the run's fetched results.
ABORT_FILENAME = "abort.json"


class GateWatch:
    """The ``on_settled`` callback, plus the abort record it may produce.

    Callable with ``(job_id, state)`` and returning ``True`` to abort, which is
    exactly the ``Callable[[str, str], bool]`` contract
    ``CloudExecutor.watch`` declares. It is a class rather than a closure only
    so the caller can ask afterwards *whether* it aborted — a watch that
    returned early and a watch that ran to completion are otherwise
    indistinguishable to the code that has to choose an exit status.
    """

    def __init__(
        self,
        executor: Any,
        *,
        run_id: str,
        spec: GateSpec,
        jobs: Sequence[Mapping[str, Any]],
        dest: Path,
        out: Callable[[str], None] = print,
    ) -> None:
        self.executor = executor
        self.run_id = run_id
        self.spec = spec
        self.dest = dest
        self.out = out
        self.labels: dict[str, str] = {
            str(job["id"]): str(job.get("label") or job["id"]) for job in jobs
        }
        self.aborted: dict[str, Any] | None = None

    def __call__(self, job_id: str, state: str) -> bool:
        label = self.labels.get(job_id)
        if label is None:
            # An id the submission record does not name is not ours to act on.
            # It cannot happen through ``watch`` (which sweeps the same list),
            # and guessing at one would be the first step toward the
            # queue-scoped cancel :mod:`.cancel` exists to forbid.
            return False

        trial_dir = self.dest / label
        try:
            self.executor._fetch_prefix(  # same package, one adapter
                f"runs/{self.run_id}/{job_id}/", trial_dir, out=self.out
            )
        except Exception as exc:  # botocore's errors are unimportable here
            # Same rule as a crashed gate: no artifacts means no claim about
            # the trial, and a transient S3 error must not abort a paid run.
            self.out(f"  !! gate failed to fetch {label} ({state}): {exc}")
            return False

        outcome = run_gate(self.spec, trial_dir)
        if outcome.error:
            self.out(f"  !! gate failed on {label} ({state}): {outcome.error}")
            self.out("     the run continues — a checker that crashed has said "
                     "nothing about this trial")
            return False
        if not outcome.banned:
            return False
        return self._abort(label, outcome.codes, outcome.report)

    def _abort(
        self, label: str, codes: tuple[str, ...], report: str
    ) -> bool:
        named = ", ".join(codes) if codes else "see the report"
        self.out("")
        self.out(f"BANNED BEHAVIOR in {label}: {named}")
        for line in report.splitlines():
            self.out(f"  | {line}")
        reason = f"arenabench gate: banned behavior in {label} ({named})"[:1024]
        try:
            plan = cancel_run(self.executor, self.run_id, reason=reason, out=self.out)
        except Exception as exc:  # botocore's errors are unimportable here
            # The record is written either way: an abort that cancelled
            # nothing is still an abort somebody has to be able to audit.
            self.out(f"  !! cancel failed entirely: {exc}")
            plan = CancelPlan()
        self.aborted = abort_record(
            run_id=self.run_id,
            codes=codes,
            trial_label=label,
            report=report,
            plan=plan,
        )
        self.dest.mkdir(parents=True, exist_ok=True)
        target = self.dest / ABORT_FILENAME
        target.write_text(
            json.dumps(self.aborted, indent=2) + "\n", encoding="utf-8"
        )
        self.out(f"abort     : {target}")
        return True


def settled_gate(
    executor: Any,
    *,
    run_id: str,
    spec: GateSpec,
    jobs: Sequence[Mapping[str, Any]],
    dest: Path,
    out: Callable[[str], None] = print,
) -> GateWatch:
    """Build the ``on_settled`` callback for one run's watch loop.

    Named as a factory rather than exposing :class:`GateWatch` directly so the
    call site in ``cloud.py`` reads as what it does — "gate every trial as it
    settles" — and so the returned object's identity stays an implementation
    detail of this module.
    """
    return GateWatch(
        executor, run_id=run_id, spec=spec, jobs=jobs, dest=dest, out=out
    )
