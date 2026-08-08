"""`bench/evidence/make_manifest.py` calls this adapter — pin the call shape.

The subject is #2182. The adapter's host-side selector entry points
(`_benchmark_engine_posture`, `_benchmark_assurance_tiers`) were renamed
`witness_author` → `verifier`; `make_manifest.py` was left calling them with
`witness_author=`. That is a `TypeError`, raised inline while the manifest dict
is being assembled, under a `try/except` that catches `ImportError` only — so
the run host wrote **no manifest at all**, and a benchmark run measured while
that held cannot be interpreted afterwards.

**Why the witness has to live here and not beside its subject.** The evidence
suite runs `uv run --with pytest --no-project`, so `stella_harbor` is not
importable there, so the `except ImportError` early-return is the only branch
its tests ever take: the defective line is unreachable in the one suite that
owns the file. The adapter is importable exactly and only where the crash
happens — the run host, and this suite. The guard that makes `make_manifest.py`
testable off-host is the same guard that hides the defect, so a check that
reads the *source* from the side that holds the live signature is the only
placement that can fail.

Two questions, deliberately separate:

- **Does the call bind?** Read the keyword names `make_manifest.py` passes out
  of its AST, compare them against `inspect.signature` of the live callee. This
  fails if either side is renamed without the other, which no test in either
  suite could see before.
- **Does it still bind to the right thing?** A crash is also "fixable" by
  dropping the selector, which would silently declare the control arm's posture
  and tier hashes for a treatment run — a mislabelled paid run, strictly worse
  than the crash. So the selector is asserted *present*, derived from the
  adapter rather than spelled as a literal here.

The shape is copied from `test_posture.py::TestOutputCeilingParity` and
`test_assurance_tiers.py::test_the_engine_still_resolves_a_role_in_the_order_this_module_mirrors`:
a check whose two halves live in different suites reads one of them as source
text, and says which literal to edit when the path moves.
"""

from __future__ import annotations

import ast
import importlib
import importlib.util
import inspect
import re
from pathlib import Path
from types import ModuleType
from typing import Any

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

from stella_harbor import (  # noqa: E402 - after importorskip by design
    _benchmark_assurance_tiers,
    _benchmark_engine_posture,
)

_REPO_ROOT = Path(__file__).resolve().parents[3]
_EVIDENCE = _REPO_ROOT / "bench" / "evidence"
_MAKE_MANIFEST = _EVIDENCE / "make_manifest.py"
#: Shell drivers that embed Python importing the adapter in a heredoc.
_EVIDENCE_RUN = _EVIDENCE / "run"

#: Callee name in `make_manifest.py` → the live function it must bind to.
_CALLEES = {
    "_benchmark_engine_posture": _benchmark_engine_posture,
    "_benchmark_assurance_tiers": _benchmark_assurance_tiers,
}

# The arms the manifest has to be able to describe. `None` is the control arm —
# the one `finalize.sh` runs by default, and the one whose number is published.
_WORKER = "openrouter/z-ai/glm-5.1"
_VERIFIER = "openrouter/deepseek/deepseek-v4-pro"


def _manifest_source() -> str:
    """The manifest builder's source, or a skip that names what is missing.

    Skipped rather than passed when the file is genuinely absent: a parity
    check that silently finds nothing to check is decoration, and this one
    already failed once by being unreachable.
    """
    if not _MAKE_MANIFEST.is_file():
        pytest.skip(
            f"{_MAKE_MANIFEST} is not present in this checkout; if it moved, "
            "correct `_MAKE_MANIFEST` in this file"
        )
    return _MAKE_MANIFEST.read_text(encoding="utf-8")


def _called_name(node: ast.Call) -> str | None:
    """The bare name of a call target, through either import style."""
    func = node.func
    if isinstance(func, ast.Name):
        return func.id
    if isinstance(func, ast.Attribute):
        return func.attr
    return None


def _call_sites() -> dict[str, list[ast.Call]]:
    """Every call to a tracked adapter function, keyed by callee name."""
    tree = ast.parse(_manifest_source(), filename=str(_MAKE_MANIFEST))
    sites: dict[str, list[ast.Call]] = {name: [] for name in _CALLEES}
    for node in ast.walk(tree):
        if isinstance(node, ast.Call):
            name = _called_name(node)
            if name in sites:
                sites[name].append(node)
    return sites


def _selector_keywords() -> set[str]:
    """The host-side selector, read off the adapter instead of spelled here.

    `_benchmark_assurance_tiers` exists only for the callers that hold the
    host-side inputs — `make_manifest.py` and the preregistration tooling — so
    its keyword-only parameters *are* that selector. Intersecting with the
    posture builder's keeps the tuning-only knobs (`worker_effort`,
    `triage_model`, …) out: those are the harness's business, not the
    manifest's.
    """
    return _keyword_only(_benchmark_assurance_tiers) & _keyword_only(
        _benchmark_engine_posture
    )


def _keyword_only(function: Any) -> set[str]:
    return {
        name
        for name, parameter in inspect.signature(function).parameters.items()
        if parameter.kind is inspect.Parameter.KEYWORD_ONLY
    }


def _load_make_manifest() -> ModuleType:
    """Import `make_manifest.py` by path — it is a script, not a package."""
    _manifest_source()  # skips with the actionable message when it is gone
    spec = importlib.util.spec_from_file_location("_tb_make_manifest", _MAKE_MANIFEST)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_the_manifest_builder_is_readable_and_calls_the_adapter() -> None:
    """Guard the guard: an unreachable parity check passes vacuously.

    #2182 is exactly a check that could not run where it mattered, so the two
    assertions below come before the ones that matter.
    """
    assert _MAKE_MANIFEST.is_file(), f"{_MAKE_MANIFEST} is missing"
    sites = _call_sites()
    for name, calls in sites.items():
        assert calls, (
            f"{_MAKE_MANIFEST.name} no longer calls `{name}` — either the "
            "manifest stopped reading its posture/tier declaration from the "
            "adapter (a regression: a hand-written value can drift from what "
            "ran) or this file's callee list is stale"
        )


def test_every_keyword_the_manifest_passes_exists_on_the_live_callee() -> None:
    """The crash itself: `witness_author=` against `verifier` (#2182).

    Uncaught by construction — the `try/except` around each call catches
    `ImportError`, and the adapter *is* importable on the run host, which is
    the only place this call is reached. So the manifest was not degraded, it
    was absent.
    """
    for name, calls in _call_sites().items():
        accepted = set(inspect.signature(_CALLEES[name]).parameters)
        for call in calls:
            passed = {keyword.arg for keyword in call.keywords if keyword.arg}
            unknown = passed - accepted
            assert not unknown, (
                f"{_MAKE_MANIFEST.name}:{call.lineno} calls `{name}` with "
                f"{sorted(unknown)}, which the adapter's signature does not "
                f"accept ({sorted(accepted)}). One side was renamed without "
                "the other; a TypeError here writes no manifest at all (#2182)"
            )
            assert not any(keyword.arg is None for keyword in call.keywords), (
                f"{_MAKE_MANIFEST.name}:{call.lineno} splats `**kwargs` into "
                f"`{name}`, which puts the binding beyond what this check can "
                "read — pass the keywords literally"
            )


def test_the_manifest_still_passes_the_arm_selector() -> None:
    """Dropping the kwarg also "fixes" the crash, and is worse than it.

    A manifest built without the selector records the control arm's posture and
    tier digests for whichever arm actually ran, with no outward sign — the
    mislabelled-arm failure `_arm_mismatch` was written to refuse.
    """
    selector = _selector_keywords()
    assert selector, (
        "the adapter's two host-side entry points share no keyword-only "
        "parameter, so this check derives nothing — the selector shape moved "
        "and this test has to follow it"
    )
    for name, calls in _call_sites().items():
        for call in calls:
            passed = {keyword.arg for keyword in call.keywords if keyword.arg}
            assert selector <= passed, (
                f"{_MAKE_MANIFEST.name}:{call.lineno} calls `{name}` without "
                f"{sorted(selector - passed)}: the manifest would declare the "
                "control arm's digests for whatever arm actually ran (#1007)"
            )


@pytest.mark.parametrize("verifier", [None, _VERIFIER], ids=["control", "treatment"])
def test_the_manifest_blocks_bind_against_the_live_adapter(
    verifier: str | None,
) -> None:
    """Call the two blocks for real, on the side where the adapter imports.

    The AST checks above are the durable ones — they fail on a rename in either
    direction. This is the direct demonstration: on the old code both calls
    raise `TypeError: got an unexpected keyword argument 'witness_author'`
    before either block returns.
    """
    module = _load_make_manifest()

    posture = module._posture(_WORKER, verifier)
    assert "error" not in posture, (
        f"the adapter must be importable in this suite: {posture['error']}"
    )
    assert posture["normalized_sha256"]
    assert posture["posture"]["default_model"] == _WORKER

    assurance = module._assurance(_WORKER, verifier)
    assert "error" not in assurance, assurance["error"]
    assert assurance["tiers"]["verifier_model"] == verifier
    assert assurance["tiers"]["arm"] == (
        "witness-off" if verifier is None else "witness-on"
    )


def test_every_adapter_name_the_run_scripts_import_still_exists() -> None:
    """The same rename, one seam further out — `witness_ab.sh` (#2182).

    `#1394` renamed `_validated_witness_author` to `_validated_verifier` and
    left the witness A/B driver importing the old name inside a heredoc. Under
    `ARM=on` that is an `ImportError` behind `|| exit 1`, so the *treatment*
    arm refuses to launch at all — the same shape as the manifest crash, in the
    one script whose whole job is to produce the witness-on number.

    Shell is not parseable as Python, so this reads the `from stella_harbor…
    import …` lines textually. That is enough: the failure being pinned is a
    name that no longer resolves, and the name is right there in the text.
    """
    if not _EVIDENCE_RUN.is_dir():
        pytest.skip(
            f"{_EVIDENCE_RUN} is not present in this checkout; if it moved, "
            "correct `_EVIDENCE_RUN` in this file"
        )
    pattern = re.compile(
        r"^\s*from\s+(stella_harbor(?:\.[\w.]+)?)\s+import\s+([^\n#]+)", re.MULTILINE
    )
    checked = 0
    for script in sorted(_EVIDENCE_RUN.rglob("*.sh")):
        text = script.read_text(encoding="utf-8")
        for module_name, imported in pattern.findall(text):
            module = importlib.import_module(module_name)
            for name in (part.strip() for part in imported.split(",")):
                bare = name.split(" as ")[0].strip()
                if not bare or bare == "*":
                    continue
                checked += 1
                assert hasattr(module, bare), (
                    f"{script.relative_to(_REPO_ROOT)} imports `{bare}` from "
                    f"`{module_name}`, which no longer defines it. The import "
                    "sits behind `|| exit 1`, so the arm this script drives "
                    "refuses to launch (#2182)"
                )
    assert checked, (
        f"no `from stella_harbor … import …` line was found under "
        f"{_EVIDENCE_RUN} — this check has stopped reading its subject"
    )


def test_the_two_blocks_disclose_the_arm_in_their_digests() -> None:
    """The selector has to reach the hash, not merely be accepted.

    A parameter can bind and be ignored — which is what a rename repaired by
    deleting the argument would look like from the AST. The digests are what a
    reader of committed evidence compares, so they are what must move.
    """
    module = _load_make_manifest()
    for block in (module._posture, module._assurance):
        control = block(_WORKER, None)
        treatment = block(_WORKER, _VERIFIER)
        assert control["normalized_sha256"] != treatment["normalized_sha256"], (
            f"{block.__name__} hashes the two arms identically: the arm would "
            "be invisible in the registered digest (#1007)"
        )
