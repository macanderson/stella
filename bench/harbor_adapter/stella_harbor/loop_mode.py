"""Which loop a Stella arm ran: the selector, its argv, and its manifest name.

Split out of the adapter package root for the reason :mod:`posture` was, and
recorded in that module's own docstring: the repository's file-size ratchet
holds that separable new code becomes its own module rather than more length
on an already-oversized file.

The seam is genuine. Nothing here knows what a Harbor agent is: every function
takes a *reader* — one callable resolving an environment key to its configured
value — so :meth:`StellaAgent._configured_value` supplies the real one and a
test supplies ``{...}.get``.

**There is no staged pipeline left to select (#3865, #4023).** The built-in
staged pipeline this module used to toggle is deleted from the workspace:
``crates/stella-cli/src/wrapper_plugin.rs``'s ``PipelineChoice::resolve``
refuses ``--pipeline classic`` outright, and every remaining resolution —
with or without ``--no-pipeline`` — lands on the agent's raw step loop. So
:func:`loop_mode_name` always publishes :data:`BARE_STEP_LOOP`: that is what
the binary structurally runs on every arm, and a manifest claiming
:data:`STAGED_PIPELINE` for an arm that said nothing would be publishing a
loop this binary cannot run — the defect #4023 fixed. :data:`STAGED_PIPELINE`
stays defined for a manifest describing a pre-#3865 run, never for one this
adapter writes today.

**``--no-pipeline`` is a deprecated no-op, so :func:`loop_argv` never emits
it.** The flag still parses (unlike ``--pipeline classic``), but changes
nothing about which loop runs — ``crates/stella-cli/src/main.rs``'s
``print_no_pipeline_notice_if_owed`` answers it with a deprecation notice on
every trial for no behavioural difference. :func:`bare_loop_selected` and
:data:`NO_PIPELINE_ENV` stay in place: a match template's declared intent to
measure the bare loop is still real and still worth recording (a stella seat
is refused at parse time on any other agent, ``arenabench/arenabench/config.py``),
it just no longer has a CLI flag or a manifest value to disagree with.

**Why it is host-only.** :data:`NO_PIPELINE_ENV` is read on the host; nothing
about it is forwarded into the task container, so it is registered in the
adapter's ``_HOST_ONLY_STELLA_ENV``. That registration is not bookkeeping: the
ambient check fails closed, so an unregistered selector would have refused the
run rather than quietly running the other arm.

**Why a selector spelled to close must not open.** :func:`is_truthy` matches
Stella's own vocabulary (``crates/stella-cli/src/settings.rs::truthy_flag``)
exactly, and deliberately not "any non-empty value". ``STELLA_NO_PIPELINE=false``
has to leave :func:`bare_loop_selected` **false**, the same way
``STELLA_TRUST_PROJECT=false`` must not open the trust boundary. It is the
adapter root's historical ``_is_truthy``, moved here rather than copied: this
module briefly shipped a second frozenset spelling the same four words, which
is the drift the move prevents.
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
    """Whether this arm *declared* an explicit ask for the raw step loop.

    Kept for the declaration's own sake — ``arenabench/arenabench/config.py``
    still refuses ``bare_loop = true`` on any agent but ``stella`` at parse
    time, so a template's intent is worth reading regardless of what the CLI
    does with it. Since #3865 that intent no longer changes anything
    downstream: see :func:`loop_argv` and :func:`loop_mode_name`.
    """
    return is_truthy(configured(NO_PIPELINE_ENV))


def loop_argv(configured: Reader) -> tuple[str, ...]:
    """The tokens ``run`` takes for this loop mode — always empty (#4023).

    ``--no-pipeline`` used to be a flag *of* ``run``, like ``--output-format``
    and for the same reason (stella#1493): a promise about this one
    invocation, not a session-wide setting. Post-#3865 it is a deprecated
    no-op — every invocation already runs the raw step loop, flag or not — so
    emitting it only earns the trial a deprecation notice
    (``print_no_pipeline_notice_if_owed``) for a difference that does not
    exist. ``configured`` is accepted and unused so the signature stays the
    one seam :meth:`StellaAgent._build_command` calls through.
    """
    del configured
    return ()


def loop_mode_name(configured: Reader) -> str:
    """The manifest's name for this loop mode — always the raw step loop.

    Post-#3865 that is the only loop the binary can structurally run
    (``crates/stella-cli/src/wrapper_plugin.rs::PipelineChoice::resolve``
    refuses ``--pipeline classic`` and lands every other resolution here
    too), so publishing :data:`STAGED_PIPELINE` for an arm that said nothing
    would claim a loop this binary cannot run — the defect #4023 fixed.
    ``configured`` is accepted and unused for the same reason as
    :func:`loop_argv`'s.
    """
    del configured
    return BARE_STEP_LOOP
