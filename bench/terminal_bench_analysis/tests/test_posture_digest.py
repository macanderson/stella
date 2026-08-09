"""The canonical engine posture still hashes to the digest every arm was claimed under.

This is one behaviour in its own file on purpose (#1452). It used to be an
`assert` at module scope in ``test_tb21_analysis.py``:

    _POSTURE, _POSTURE_JSON, _POSTURE_SHA256 = canonical_engine_posture(_MODEL)
    assert _POSTURE_SHA256 == "8ce2e820…"

which runs at *import*, so a single stale hex string never reached collection —
it reported as ``Interrupted: 1 error during collection`` and took all 258 tests
in that file with it. ``main`` was red for roughly forty minutes with whatever
else was broken in that module invisible behind the digest.

The guarantee that assert was buying is real and is kept. It is not "delete the
assert": ``_POSTURE_SHA256`` builds fixtures throughout the sibling module, and a
wrong value there would have dozens of tests assert against a fabricated posture
and pass. What makes moving it out safe is that those fixtures use the
*computed* posture — so they stay self-consistent, and the one thing that needs
saying, "the posture we compute is still the posture we published", is said
here, once, by name.
"""

from __future__ import annotations

import tb21_analysis as analysis_module

_MODEL = analysis_module.PRIMARY_MODEL

# The digest every registered TB2.1 arm was claimed under. Re-frozen twice.
#
# First when the `judge` agent slot became `verifier`: a slot rename is a NEW
# posture generation, not a cosmetic edit, because runs under `8530a36f…`
# booked an agent under the old key and are not hash-comparable with runs
# under this one.
#
# Again when the output cap left the posture (#2411), which is the first
# re-freeze where the arm itself changed rather than a key being renamed. Runs
# under `8ce2e820…` asked for 64,000 output tokens; runs under this digest ask
# for the model's own ceiling. Those are different arms, and comparing their
# solve rates compares two configurations.
_REGISTERED_POSTURE_SHA256 = (
    "20de1e34756b0ee86ced24a37fcc5f99ad743cd5c6e91921cd28d1e0570c160e"
)


def test_the_engine_posture_digest_is_the_registered_one() -> None:
    """Fails alone, and names both halves.

    Renaming anything that appears in a posture key — an agent slot, an effort,
    a model id — re-hashes every registered arm; see `bench/READINESS.md`
    §8.4.4. That is a new generation of the posture, not a cosmetic edit, so the
    remedy is to re-freeze this constant *and* stop comparing across the change.
    """
    _, _, computed = analysis_module.canonical_engine_posture(_MODEL)

    assert computed == _REGISTERED_POSTURE_SHA256, (
        f"the canonical posture for {_MODEL} now hashes to {computed}, "
        f"but every registered arm was claimed under {_REGISTERED_POSTURE_SHA256}. "
        "If the posture legitimately changed, re-freeze this constant and treat "
        "prior runs as a different generation — they are not hash-comparable."
    )
