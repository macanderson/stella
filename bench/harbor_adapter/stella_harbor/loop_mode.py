"""Which loop a Stella arm ran: the selector, its argv, and its manifest name.

Split out of the adapter package root for the reason :mod:`posture` was, and
recorded in that module's own docstring. The file-size ratchet holds that
separable new code becomes its own module. It must not become more length on
a file that is already too long.

The seam is genuine. Nothing here knows what a Harbor agent is. Every
function takes a *reader*: one callable that resolves an environment key to
its configured value. So :meth:`StellaAgent._configured_value` supplies the
real one, and a test supplies ``{...}.get``.

**There is no built-in staged pipeline left to select (#3865, #4023).** The
one this module used to toggle is deleted from the workspace.
``crates/stella-cli/src/wrapper_plugin.rs``'s ``PipelineChoice::resolve``
refuses ``--pipeline classic`` outright. :data:`STAGED_PIPELINE` stays
defined for a manifest that describes a run from before #3865. It is never
for one this adapter writes today. A manifest claiming it for an arm that ran
nothing of the kind was the defect #4023 fixed.

**A trial may still name an installed wrapper plugin.** The raw step loop is
the default on every door. ``--pipeline <id>`` is the sole opt-in over it,
and it names an installed verification plugin, such as
``plugins/stella-witness``'s ``witness-v1``. :data:`PIPELINE_ENV` names that
id for the arm. :func:`loop_argv` emits ``--pipeline <id>`` only when it is
set, and :func:`loop_mode_name` publishes ``PLUGIN:<id>`` to match. Unset,
both are byte-identical to every trial before this selector existed: the bare
step loop, published as :data:`BARE_STEP_LOOP`. This adapter passes a
pipeline id through without checking it. ``PipelineChoice::resolve`` is what
refuses ``classic``, and what refuses a plugin nothing has installed. This
module knows no more about the installed roster than Stella's own runtime.

**``--no-pipeline`` is a separate, deprecated no-op, so :func:`loop_argv`
never emits it.** The flag still parses, unlike ``--pipeline classic``. But
it changes nothing about which loop runs.
``crates/stella-cli/src/main.rs``'s ``print_no_pipeline_notice_if_owed``
answers it with a deprecation notice on every trial, for no change in
behaviour. :func:`bare_loop_selected` and :data:`NO_PIPELINE_ENV` stay in
place. A match template's declared intent to measure the bare loop is still
real and still worth recording. A stella seat is refused at parse time on any
other agent (``arenabench/arenabench/config.py``). The intent simply has no
CLI flag of its own now, and it plays no part in whether
:data:`PIPELINE_ENV` is honoured.

**Why both selectors are host-only.** Neither is forwarded into the task
container, so both are registered in the adapter's ``_HOST_ONLY_STELLA_ENV``.
That registration carries weight: the ambient check fails closed. An
unregistered selector would have refused the run rather than quietly running
the other arm.

**Why a boolean selector spelled to close must not open.** :func:`is_truthy`
matches Stella's own vocabulary exactly
(``crates/stella-cli/src/settings.rs::truthy_flag``). It does not accept any
non-empty value. ``STELLA_NO_PIPELINE=false`` has to leave
:func:`bare_loop_selected` **false**, the same way ``STELLA_TRUST_PROJECT=false``
must not open the trust boundary. It is the adapter root's historical
``_is_truthy``, moved here rather than copied. This module briefly shipped a
second frozenset spelling the same four words, and the move is what prevents
that drift. :data:`PIPELINE_ENV` carries no such vocabulary. It names a
plugin id rather than a switch, so only blank-vs-set matters there. Stripping
handles that, and the empty string reads as unset.
"""

from __future__ import annotations

from collections.abc import Callable

__all__ = [
    "BARE_STEP_LOOP",
    "NO_PIPELINE_ENV",
    "PIPELINE_ENV",
    "STAGED_PIPELINE",
    "bare_loop_selected",
    "is_truthy",
    "loop_argv",
    "loop_mode_name",
]

#: Maps one environment key to its value, or ``None``.
Reader = Callable[[str], "str | None"]

#: Selects the raw step loop instead of the staged pipeline. Host-only. Now
#: a deprecated no-op — see the module docstring. It is kept for the reason
#: :func:`bare_loop_selected` gives: the declared intent still counts.
NO_PIPELINE_ENV = "STELLA_NO_PIPELINE"

#: Names the installed wrapper plugin's ``[wrapper] id`` this arm asks Stella
#: to run the turn through (``--pipeline <id>``). Host-only. It is also
#: independent of :data:`NO_PIPELINE_ENV`. The raw loop is the default with
#: or without that flag, and this key is the only thing that moves a trial
#: off it.
PIPELINE_ENV = "STELLA_PIPELINE"

#: The values ``stella_loop_mode`` may take in a trial manifest. Naming them
#: here means a reader of a published result and a reader of this adapter see
#: the same strings. A plugin arm's value is not one of these two — see
#: :func:`loop_mode_name`.
BARE_STEP_LOOP = "bare_step_loop"
STAGED_PIPELINE = "staged_pipeline"

#: The words Stella reads as true. One tuple, because two copies is two
#: chances for one of them to drift into accepting ``"false"``.
_TRUTHY = ("1", "true", "yes", "on")


def is_truthy(value: str | None) -> bool:
    """Whether a string environment variable represents truth.

    Absent, empty, and every word outside :data:`_TRUTHY` are all false. So
    a selector that is merely *present* switches nothing on.
    """
    if not value:
        return False
    return value.strip().lower() in _TRUTHY


def bare_loop_selected(configured: Reader) -> bool:
    """Whether this arm *declared* an explicit ask for the raw step loop.

    Kept for the declaration's own sake. ``arenabench/arenabench/config.py``
    still refuses ``bare_loop = true`` on any agent but ``stella`` at parse
    time. So a template's intent is worth reading, whatever the CLI does with
    it. Since #3865 that intent no longer changes what :func:`loop_argv` or
    :func:`loop_mode_name` return for a bare arm. It plays no part in whether
    :func:`_pipeline_id` picks a plugin.
    """
    return is_truthy(configured(NO_PIPELINE_ENV))


def _pipeline_id(configured: Reader) -> str | None:
    """The wrapper plugin id this arm names, or ``None`` for the bare loop.

    Blank and unset are the same declaration. ``STELLA_PIPELINE=""`` must not
    read as "run a plugin with an empty id" any more than an absent variable
    does, so both strip to ``None``.
    """
    value = configured(PIPELINE_ENV)
    if value is None:
        return None
    value = value.strip()
    return value or None


def loop_argv(configured: Reader) -> tuple[str, ...]:
    """The tokens ``run`` takes for this loop mode.

    Empty when no plugin is named. That is byte-identical to every trial
    before :data:`PIPELINE_ENV` existed, and to a ``bare_loop_selected`` arm
    (#4023). ``--no-pipeline`` is a deprecated no-op after #3865, so this
    never emits it. Otherwise ``("--pipeline", <id>)``, the one opt-in
    ``PipelineChoice::resolve`` reads as anything but the raw loop. It, not
    this adapter, is what refuses ``classic`` or an uninstalled id.

    ``--pipeline`` used to be a flag *of* ``run``, like ``--output-format``
    and for the same reason (stella#1493). It is a promise about this one
    invocation rather than a session-wide setting.
    """
    pipeline = _pipeline_id(configured)
    if pipeline is None:
        return ()
    return ("--pipeline", pipeline)


def loop_mode_name(configured: Reader) -> str:
    """The manifest's name for this loop mode.

    :data:`BARE_STEP_LOOP` when no plugin is named. That is the only loop the
    binary can structurally run with no ``--pipeline`` flag
    (``crates/stella-cli/src/wrapper_plugin.rs::PipelineChoice::resolve``).
    Otherwise ``"PLUGIN:<id>"``, naming exactly the id :func:`loop_argv`
    passed. So the published record and the argv that ran can never name two
    different loops. A manifest reading as one arm while the command line ran
    another is the failure this module exists to make unrepresentable. Never
    :data:`STAGED_PIPELINE`: nothing this adapter invokes today can resolve
    to the deleted built-in path.
    """
    pipeline = _pipeline_id(configured)
    if pipeline is None:
        return BARE_STEP_LOOP
    return f"PLUGIN:{pipeline}"
