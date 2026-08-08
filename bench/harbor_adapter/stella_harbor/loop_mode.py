"""Which loop a Stella arm ran: the selector, its argv, and its manifest name.

Split out of the adapter package root for the reason :mod:`posture` was, and
recorded in that module's own docstring: the repository's file-size ratchet
holds that separable new code becomes its own module rather than more length
on an already-oversized file.

The seam is genuine. Nothing here knows what a Harbor agent is: every function
takes a *reader* — one callable resolving an environment key to its configured
value — so :meth:`StellaAgent._configured_value` supplies the real one and a
test supplies ``{...}.get``. That is also what makes the invariant below
mechanical rather than aspirational: the argv and the manifest are two
projections of :func:`bare_loop_selected`, so they cannot disagree about which
loop ran.

**What the selector selects.** ``stella run`` runs the staged pipeline by
default; ``--no-pipeline`` runs the agent's raw step loop instead. One flag
settles all three management stages at once, because triage, witness and
verify *are* the pipeline — the bare loop has no stage to turn off
individually. This is the arm that measures the agent loop itself: with the
pipeline in, a board number folds worker capability together with
triage/witness/verdict behaviour, and #2128/#2129 showed those stages can
dominate it outright.

**Why it is host-only.** :data:`NO_PIPELINE_ENV` is read on the host and
expressed purely as argv; nothing about it is forwarded into the task
container, so it is registered in the adapter's ``_HOST_ONLY_STELLA_ENV``.
That registration is not bookkeeping: the ambient check fails closed, so an
unregistered selector would have refused the run rather than quietly running
the other arm.

**Why a selector spelled to close must not open.** :func:`is_truthy` matches
Stella's own vocabulary (``crates/stella-cli/src/settings.rs::truthy_flag``)
exactly, and deliberately not "any non-empty value". ``STELLA_NO_PIPELINE=false``
has to leave the pipeline **on**, the same way ``STELLA_TRUST_PROJECT=false``
must not open the trust boundary. It is the adapter root's historical
``_is_truthy``, moved here rather than copied: this module briefly shipped a
second frozenset spelling the same four words, which is the drift the move
prevents.

**Why the mode is first-class in the manifest.** It is not inferable from the
posture: the posture still names a verifier and a triage author on a bare-loop
arm — inputs the CLI ignores once ``--no-pipeline`` is set — so a manifest
showing only the posture would read as a pipeline run that merely never
authored a witness, which is #2134's shape exactly.
"""

from __future__ import annotations

from collections.abc import Callable

__all__ = [
    "BARE_STEP_LOOP",
    "NO_PIPELINE_ENV",
    "STAGED_PIPELINE",
    "bare_loop_selected",
    "is_truthy",
    "loop_argv",
    "loop_mode_name",
]

#: Resolves one environment key to its configured value, or ``None``.
Reader = Callable[[str], "str | None"]

#: Selects the raw step loop instead of the staged pipeline. Host-only.
NO_PIPELINE_ENV = "STELLA_NO_PIPELINE"

#: The two values ``stella_loop_mode`` may take in a trial manifest. Named
#: rather than inline literals, so a reader of a published result and a reader
#: of this adapter are looking at the same two strings.
BARE_STEP_LOOP = "bare_step_loop"
STAGED_PIPELINE = "staged_pipeline"

#: Stella's truthy vocabulary. One tuple, because two copies is two chances for
#: one of them to drift into accepting ``"false"``.
_TRUTHY = ("1", "true", "yes", "on")


def is_truthy(value: str | None) -> bool:
    """Whether a string environment variable represents truth.

    Absent, empty, and every word outside :data:`_TRUTHY` are all false, so a
    selector that is merely *present* does not switch anything on.
    """
    if not value:
        return False
    return value.strip().lower() in _TRUTHY


def bare_loop_selected(configured: Reader) -> bool:
    """Whether this arm runs the raw step loop instead of the pipeline.

    The single resolution point both projections below read. Absent means the
    pipeline, which is what ``stella run`` does by default — an arm has to ask
    for the bare loop, so a match that says nothing measures the shipping
    configuration.
    """
    return is_truthy(configured(NO_PIPELINE_ENV))


def loop_argv(configured: Reader) -> tuple[str, ...]:
    """The tokens ``run`` takes for this loop mode — empty for the pipeline.

    ``--no-pipeline`` is a flag *of* ``run``, like ``--output-format`` and for
    the same reason (stella#1493): it is a promise about this one invocation,
    not a session-wide setting. Returning nothing for the default is what keeps
    the pipeline's argv byte-identical to what it was before this arm existed.
    """
    return ("--no-pipeline",) if bare_loop_selected(configured) else ()


def loop_mode_name(configured: Reader) -> str:
    """The manifest's name for this loop mode."""
    return BARE_STEP_LOOP if bare_loop_selected(configured) else STAGED_PIPELINE
