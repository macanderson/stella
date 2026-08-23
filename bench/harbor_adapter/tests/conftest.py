"""Keep the adapter's tests independent of the developer's shell.

`StellaAgent._forwarded_env` fails closed on any ambient `STELLA_*` key it
does not recognize. That guard is deliberate and stays: a stray knob in
someone's environment must never silently change what a benchmark measures.

The cost is that the test suite inherits whatever the developer exports. A
shell with `STELLA_VERIFIER_MODEL` or `STELLA_TRIAGE_API` set — the posture
variables `stella_harbor/posture.py` reads, which anyone running head-to-head
arms has in their profile — turns 11 otherwise-unrelated tests red with
`RuntimeError: claim benchmark environment contains unregistered STELLA_*
knobs`. CI never sees it, because CI's environment is empty; it lands only on
the person running `make bench-test` locally, in tests that have nothing to
do with the variable that broke them.

Clearing the inherited keys for the session makes the suite hermetic: every
test starts from the same environment CI has, so a local run and a CI run
agree. Tests that want a `STELLA_*` value still set it themselves with
`monkeypatch`, which is the only way the value should ever get there.
"""

import os

import pytest

from stella_harbor.credential_bundle import (
    EMBEDDING_CREDENTIAL_NAMES,
    HOST_ONLY_CONTROL_CREDENTIAL_NAMES,
    PROVIDER_CREDENTIAL_NAMES,
)

_ENV_PREFIX = "STELLA_"

# Every credential name a resolver in this tree can read straight out of the
# process environment as a fallback (`provider_credentials_from_environment`,
# `optional_embedding_credentials` in `credential_bundle.py`). An operator's
# real key for any of these changes what a test observes exactly the way a
# stray ``STELLA_*`` knob does above — a two-name
# ``STELLA_CREDENTIAL_HANDOFF_TARGET`` instead of one, or a live secret
# rendered into an assertion diff (#3668). Clearing the class, not the two
# instances that happened to be caught, is what survives the next credential
# name being added to `credential_bundle.py` without a matching test fix.
_AMBIENT_CREDENTIAL_NAMES = (
    PROVIDER_CREDENTIAL_NAMES | EMBEDDING_CREDENTIAL_NAMES | HOST_ONLY_CONTROL_CREDENTIAL_NAMES
)


@pytest.fixture(autouse=True, scope="session")
def _strip_ambient_stella_env():
    """Remove inherited ``STELLA_*`` keys for the duration of the session."""
    patcher = pytest.MonkeyPatch()
    for key in [key for key in os.environ if key.startswith(_ENV_PREFIX)]:
        patcher.delenv(key, raising=False)
    yield
    patcher.undo()


@pytest.fixture(autouse=True, scope="session")
def _strip_ambient_provider_credentials():
    """Remove inherited provider/embedding/control credentials for the session.

    A test that wants one of these values sets it explicitly with
    ``monkeypatch`` — the only way it should ever get there.
    """
    patcher = pytest.MonkeyPatch()
    for key in _AMBIENT_CREDENTIAL_NAMES:
        patcher.delenv(key, raising=False)
    yield
    patcher.undo()
