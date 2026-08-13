"""The trial's advertised tool set: validating ``STELLA_LEAN_TOOLS``.

Its own module rather than a block in ``__init__.py``, which is over its
recorded size ceiling (``scripts/check-file-size.sh``) — the same split
:mod:`upstream_pin`, :mod:`turn_budget` and :mod:`loop_mode` already took.

**Why a container variable and not an argument.** :mod:`upstream_pin` is the
adapter's precedent for a host-read experiment control, and it is host-only
for a reason this knob does not share. The comment registering
``STELLA_TARGET_TRIPLE`` host-only states the test: a value that "means
nothing inside a task container" belongs on the host, and "host-only is the
honest bucket". A gateway upstream pin decides which vendor's silicon is
billed — it is a routing control, resolved before the container exists. The
advertised tool set is the opposite: it configures the very ``stella`` process
running *inside* the container, exactly like the two names already in
``_CLAIM_CONTAINER_ENV`` (``STELLA_SPEND_LIMIT``, ``STELLA_DISABLE_REFLECTION``).
The container is where it means something, so the container environment is the
honest bucket.

The alternative — a new ``stella`` CLI flag, expressed as argv the way
``--upstream-pin`` is — was measured and rejected on cost, not taste. Lean
frontloading resolves in ``crates/stella-cli/src/discovery.rs``'s
``lean_from_env`` at eight ``DiscoveryToolSet::new`` sites, and ``with_lean``
is ``#[cfg(test)]``; wiring a flag there adds builder lines to
``crates/stella-cli/src/agent.rs`` and ``src/command_deck.rs``, both
grandfathered god files sitting at exactly their recorded ceilings. A bench
harness must not force a structural change to the CLI's two largest files to
buy a knob the engine already reads.

**Why a preset is refused.** ``STELLA_LEAN_TOOLS=1`` is valid to Stella and
selects the engine's built-in ``DEFAULT_CORE_TOOLS``. A measured arm may not
use it: the tool set is the independent variable of the experiment this module
exists for, and a run whose record says ``1`` is a run whose tool set has to be
recovered by reading the binary it happened to launch. Every arm names its
tools, so the provenance carries the literal set and two arms can never be
confused after the fact.
"""

from __future__ import annotations

import re
from typing import Callable

#: An agent's ``_configured_value`` — env with Harbor's per-run overrides on
#: top. The same seam :mod:`loop_mode` and :mod:`turn_budget` take.
Configured = Callable[[str], "str | None"]

#: The env var Stella's lean frontloading reads (``LEAN_TOOLS_ENV`` in
#: ``crates/stella-cli/src/discovery.rs``). Forwarded into the container.
ENV = "STELLA_LEAN_TOOLS"

#: A built-in tool name, as ``crates/stella-tools/src/catalog.rs`` spells one.
#: Anchored and deliberately narrow: the value is spliced into one environment
#: string that the engine re-splits on commas, so an entry carrying a
#: separator, whitespace, or shell punctuation would advertise a different set
#: than the operator declared — the failure a declared tool set exists to
#: prevent, arriving through the declaration itself.
_TOOL_NAME = re.compile(r"[a-z][a-z0-9_]*\Z")

#: Values Stella reads as "the built-in core" rather than as a list. Refused
#: here; see the module docstring.
_PRESET_WORDS = frozenset({"1", "true", "0", "false"})

#: The ``LEAN_TOOLS_ENV`` prefix selecting a **closed** set — exactly these
#: tools, with no discovery layer on top (``ONLY_PREFIX`` in
#: ``crates/stella-cli/src/discovery.rs``).
#:
#: Always emitted, never optional. The bare comma list adds `tool_search`,
#: `skill_search` and `mcp_search` on top of the named core, which is right
#: for a session and wrong for a measured arm: an arm specified as five tools
#: that ships eight is not the arm anyone reasoned about, and `tool_search`
#: sitting beside the tools under test competes with them for the model's
#: attention. Accepted on input too, so an operator who writes the prefix by
#: hand gets the same set as one who does not.
ONLY_PREFIX = "only:"


def validated_tool_set(value: str | None) -> tuple[str, ...]:
    """Parse the declared tool set — empty when the arm declares none.

    Comma-separated, order-preserving, duplicates and blanks dropped. Absent
    and empty both mean "unset", which is the one value that must leave a
    trial byte-identical to one run before this knob existed.
    """
    if value is None:
        return ()
    body = value.strip()
    if body.lower().startswith(ONLY_PREFIX):
        body = body[len(ONLY_PREFIX) :]
    raw_entries = [entry.strip() for entry in body.split(",")]
    names: list[str] = []
    for entry in raw_entries:
        if not entry:
            continue
        if entry.lower() in _PRESET_WORDS:
            raise ValueError(
                f"{ENV} entry {entry!r} is a preset, not a tool — a measured "
                "arm names every tool it advertises so its provenance records "
                "the set rather than a word that resolves to one"
            )
        if not _TOOL_NAME.match(entry):
            raise ValueError(
                f"{ENV} entry {entry!r} must be a bare tool name "
                "(lowercase, digits and underscores)"
            )
        if entry not in names:
            names.append(entry)
    return tuple(names)


def declared(configured: Configured) -> tuple[str, ...]:
    """This trial's declared tool set, read through the agent's resolver.

    Takes the resolver rather than a string for the same reason
    :func:`loop_mode.loop_mode_name` does: Harbor's per-run ``--env``
    overrides beat the ambient environment, and a caller that read
    ``os.environ`` directly would disclose a set the trial did not run.
    """
    return validated_tool_set(configured(ENV))


def tool_set_env(configured: Configured) -> dict[str, str]:
    """The environment fragment for this tool set — empty when undeclared.

    Empty rather than ``{ENV: ""}``: Stella reads an empty value as "full
    catalog", so both spellings run the same session, but only the omission
    leaves the container environment identical to a pre-feature trial's. The
    weaker claim is not worth making when the stronger one is free.
    """
    names = declared(configured)
    return {ENV: ONLY_PREFIX + ",".join(names)} if names else {}
