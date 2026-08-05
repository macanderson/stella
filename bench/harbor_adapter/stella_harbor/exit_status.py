"""Classify Stella's nonzero exits into what actually happened (#1524).

Harbor records ``type(exc).__name__`` in ``result.json``, and every nonzero
agent exit used to raise the one official ``NonZeroAgentExitCodeError`` — so
a truthful self-reported verification failure, a deliberate engine stop, and
a segfault were the same row in a results table. The two subclasses here
keep Harbor's scoring behavior byte-identical (every ``except
NonZeroAgentExitCodeError`` still matches) while the *name* carries the
distinction into analysis.

Split out of ``__init__`` when the deliberate-stop class joined: the adapter
module sits at its file-size ceiling, and exit classification is a coherent
seam of its own. Unlike :mod:`stella_harbor.exit_cause` (the SIGKILL
post-mortem, deliberately importable without Harbor), this module subclasses
Harbor's official error and so requires it.
"""

from __future__ import annotations

from typing import Any

from harbor.agents.installed.base import NonZeroAgentExitCodeError

# The CLI's generic failure status, which a completed turn also exits with
# when its own verification verdict is red (`pipeline_status_result` returns
# `Err("verification failed: …")` and `main` maps that to `ExitCode::FAILURE`).
# See `is_self_reported_verification_failure`.
VERIFICATION_FAILED_EXIT_CODE = 1

# The CLI's deliberate-stop status (`failure::DELIBERATE_STOP_EXIT_CODE`):
# the engine chose to end the run — a stuck loop escalated past its steering
# warning, an enforced budget or deadline, the step cap, a confident-zero
# close. Only Stella's own exit path produces this code, so unlike exit 1 it
# needs no stream corroboration to classify. See ``DeliberateStopError``.
DELIBERATE_STOP_EXIT_CODE = 3


class SelfReportedVerificationFailureError(NonZeroAgentExitCodeError):
    """Stella ran the task to a verdict and its own verdict said "not verified".

    A ``VerificationFailed`` pipeline status is a completed turn: the agent
    worked, the ladder closed, and the run reported honestly that the work did
    not verify. The CLI surfaces that as exit 1, which is also what a crash
    exits with — so under one exception class a truthful self-report and a
    process that died are the same row in a results table, exactly the
    conflation :mod:`stella_harbor.portability` was written to end for loader
    failures.

    Deliberately a *subclass* of the official error rather than a sibling:
    Harbor records ``type(exc).__name__`` in ``result.json``, so the name is
    all analysis needs, while every ``except NonZeroAgentExitCodeError`` in
    Harbor and in this adapter keeps behaving exactly as before — the trial is
    still scored, and the benchmark verifier still runs.
    """


class DeliberateStopError(NonZeroAgentExitCodeError):
    """Stella deliberately stopped the run and said so at the process boundary.

    Exit ``3`` is the CLI's typed deliberate-stop status (#1524): the engine
    detected a stuck loop and escalated past its steering warning, enforced a
    budget or deadline, hit its step cap, or refused a trailing-off turn.
    That is the engine doing its job — before this class, a correct
    loop-abort raised the same ``NonZeroAgentExitCodeError`` as a segfault,
    and the benchmark could not tell "the agent correctly gave up" from "the
    agent fell over".

    Deliberately a *subclass* of the official error, exactly like
    :class:`SelfReportedVerificationFailureError`: Harbor records
    ``type(exc).__name__`` in ``result.json``, so the name is all analysis
    needs, while every ``except NonZeroAgentExitCodeError`` keeps behaving as
    before — the trial is still scored, and the verifier still runs.
    """


def is_self_reported_verification_failure(
    return_code: int | None, envelope: dict[str, Any] | None
) -> bool:
    """Is this nonzero exit Stella's own failed verdict rather than a crash?

    The discriminator is the trial's own proof stream, not the exit code: exit
    1 is the CLI's generic failure status, but a ``verdict`` event stating
    ``passed: false`` is a claim only a turn that reached the end of the
    ladder can make (`posture.fold_witness_observations` folds the last one,
    which is the claim the trial ends on). A stream that never reached a
    verdict stays unclassified — "not measured" must never read as "measured
    and failed".
    """
    if return_code != VERIFICATION_FAILED_EXIT_CODE or envelope is None:
        return False
    stream = envelope.get("_stella_stream")
    if not isinstance(stream, dict):
        return False
    return stream.get("self_verdict_state") == "failed"
