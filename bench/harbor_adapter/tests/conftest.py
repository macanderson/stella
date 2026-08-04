"""Keep the adapter's tests independent of the developer's shell.

`StellaAgent._forwarded_env` fails closed on any ambient `STELLA_*` key it
does not recognize. That guard is deliberate and stays: a stray knob in
someone's environment must never silently change what a benchmark measures.

The cost is that the test suite inherits whatever the developer exports. A
shell with `STELLA_JUDGE_MODEL` or `STELLA_TRIAGE_API` set — the posture
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

_ENV_PREFIX = "STELLA_"


@pytest.fixture(autouse=True, scope="session")
def _strip_ambient_stella_env():
    """Remove inherited ``STELLA_*`` keys for the duration of the session."""
    patcher = pytest.MonkeyPatch()
    for key in [key for key in os.environ if key.startswith(_ENV_PREFIX)]:
        patcher.delenv(key, raising=False)
    yield
    patcher.undo()
