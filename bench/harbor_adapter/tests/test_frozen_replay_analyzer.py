"""The frozen replay analyzer's own load path (#5108).

Its own file rather than more of `test_secure_launcher.py`, which is
grandfathered at its size ceiling: `scripts/check-file-size.sh` refuses growth
there, and raising the ceiling to fit three tests is the expedient CLAUDE.md
forbids.

Every test here runs standalone by construction, which is the property under
test — the defect was a loader whose success depended on what some earlier
importer had put on `sys.path`.
"""

from __future__ import annotations

import hashlib
import sys
from pathlib import Path

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the package")

import stella_harbor.secure_launcher as launcher_module  # noqa: E402


def _repository_root() -> Path:
    return Path(launcher_module.__file__).resolve().parents[3]


def _identity_for_the_frozen_files() -> dict[str, str]:
    """The runtime identity a manifest would carry for the tree as it stands."""
    root = _repository_root()
    return {
        "analysis_sha256": hashlib.sha256(
            (root / launcher_module._FIXED_ANALYZER_PATH).read_bytes()
        ).hexdigest(),
        "public_timing_sha256": hashlib.sha256(
            (root / launcher_module._FIXED_PUBLIC_TIMING_PATH).read_bytes()
        ).hexdigest(),
    }


def test_posture_schema_pin_matches_the_frozen_file() -> None:
    """The pin the loader verifies against is the schema module actually on disk.

    The pin is a literal on purpose — a hash the loader computes from the file it
    is about to run proves nothing. That makes drift possible, so this is what
    catches it: edit `tb21_posture_schema.py` without updating
    `_FIXED_POSTURE_SCHEMA_SHA256` and the paid-stage replay would refuse at run
    time, hours into a benchmark, instead of here.
    """
    schema_path = _repository_root() / launcher_module._FIXED_POSTURE_SCHEMA_PATH

    assert schema_path.is_file()
    assert (
        hashlib.sha256(schema_path.read_bytes()).hexdigest()
        == launcher_module._FIXED_POSTURE_SCHEMA_SHA256
    )


def test_frozen_replay_analyzer_loads_without_a_prepared_sys_path(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The analyzer loads on its own, with no help from whoever imported first.

    The witness. `tb21_analysis` imports `tb21_posture_schema` at module scope,
    and the loader supplied neither the module nor a `sys.path` entry that would
    find it — so it loaded only when some earlier importer had already put the
    analyzer's directory on the path. Running the whole suite did; running
    `test_secure_launcher.py` alone did not, and its two refusal tests got
    "prior-stage replay analyzer could not be loaded" in place of the specific
    refusal they assert.

    Both the path entry and any cached copies of the sibling modules are removed
    here, so the load has nothing to inherit.
    """
    analysis_dir = str(
        (_repository_root() / launcher_module._FIXED_POSTURE_SCHEMA_PATH).parent
    )
    monkeypatch.setattr(
        "sys.path", [entry for entry in sys.path if entry != analysis_dir]
    )
    for cached in ("tb21_posture_schema", "tb21_analysis", "github_public_timing"):
        monkeypatch.delitem(sys.modules, cached, raising=False)

    analyzer = launcher_module._load_frozen_replay_analyzer(
        _identity_for_the_frozen_files()
    )

    # A name the schema module supplies, reached through the analyzer — so this
    # fails if the import resolved to nothing, not merely if the exec raised.
    assert analyzer.STUDY_MANIFEST_ENGINE_POSTURE_FIELDS
    assert callable(analyzer.ingest_job)

    # And the load leaves no trace: a launcher that permanently installed its own
    # `tb21_posture_schema` would change what every later import resolves to.
    assert "tb21_posture_schema" not in sys.modules


def test_a_drifted_posture_schema_refuses_by_name(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A schema whose bytes are not the pinned ones is refused, and says which.

    The refusal has to name the schema rather than the analyzer: reporting
    "analyzer bytes drifted" for a schema edit is the same misdirection this
    issue was filed about, one layer down.
    """
    monkeypatch.setattr(
        launcher_module, "_FIXED_POSTURE_SCHEMA_SHA256", "00" * 32, raising=True
    )

    with pytest.raises(RuntimeError, match="posture schema bytes drifted"):
        launcher_module._load_frozen_replay_analyzer(_identity_for_the_frozen_files())
