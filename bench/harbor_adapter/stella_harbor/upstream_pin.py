"""The gateway upstream pin: parsing ``STELLA_UPSTREAM_PIN`` into argv.

Its own module rather than a block in ``__init__.py``, which is over its
recorded size ceiling (``scripts/check-file-size.sh``) — the repository's rule
is that separable new code becomes its own module, the same split
:mod:`turn_budget` and :mod:`loop_mode` already took.

**Why the pin is an argument and not a setting.** Trials launch with
``STELLA_NO_SETTINGS=1``, so a settings-defined pin cannot reach a measured
run at all — an argument is the only authority that survives the isolation,
which is why ``--base-url`` is one too. A pin that silently failed to apply
would be worse than no pin: the trace would still record a provider id and the
run would read as controlled.
"""

from __future__ import annotations

#: The host-only env var carrying the pin. Read on the host, expressed as
#: argv, never forwarded into a task container.
ENV = "STELLA_UPSTREAM_PIN"

#: The CLI flag it becomes.
FLAG = "--upstream-pin"


def validated_upstream_pin(value: str | None) -> list[str]:
    """Parse the env value into an upstream preference order.

    Comma-separated, order-preserving, blanks dropped. Each entry must be a
    bare provider slug: the order is spliced into ONE argv element, so an entry
    carrying a separator would apply a different pin than the operator wrote —
    which is the failure mode a pin exists to prevent, arriving through the pin
    itself.
    """
    if not value:
        return []
    order: list[str] = []
    for raw in value.split(","):
        entry = raw.strip()
        if not entry:
            continue
        if any(character.isspace() for character in entry):
            raise ValueError(f"{ENV} entry {entry!r} must be a bare provider slug")
        order.append(entry)
    return order


def pin_argv(value: str | None) -> list[str]:
    """The argv fragment for this pin — empty when unpinned.

    Empty rather than ``["--upstream-pin", ""]``: an empty value is a
    malformed argument that exits 2 before the agent starts, and an unpinned
    request must also keep its pre-field wire bytes, since the request body is
    the prompt-cache key.
    """
    order = validated_upstream_pin(value)
    return [FLAG, ",".join(order)] if order else []
