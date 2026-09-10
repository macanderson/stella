"""The two-build A/B's run scripts hold two arms, and one cannot erase the other.

The pipeline A/B runs two builds of Stella over one task list. Its plan sits in
``bench/evidence/pipeline-ab/``.

``bench/evidence/run/env.sh`` gave every run one ``STELLA_BINARY`` path and one
``$TB_ROOT``. So the second arm's build wrote over the first arm's binary and
its two provenance files. The hash a report cites then belongs to whichever arm
was built last. Nothing says so. Both builds pass. Both write. The survivor
looks just like a good single build.

The cases below hold four things, in the order they matter:

* a run with no arm reads and writes what it always did, so the single-build
  scripts keep working;
* two arms land on two paths, which is what the experiment needs;
* the adapter comes from ``TB_REPO``, whatever ``TB_BUILD_REPO`` says, so the
  arms differ by the binary and not by the tool that measures them;
* ``env.sh`` exports no ``STELLA_`` name the adapter has not registered. That
  one ends a run, and reading either file alone will not show it.

Standard library only, like its neighbours here. CI runs this folder with
``--no-project``.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[3]
RUN_DIR = REPO_ROOT / "bench" / "evidence" / "run"
ENV_SH = RUN_DIR / "env.sh"
PIPELINE_AB = RUN_DIR / "pipeline_ab.sh"

#: Every ``STELLA_`` name ``env.sh`` may export.
#:
#: Each one is listed in the adapter's ``_HOST_ONLY_STELLA_ENV``. That list is
#: in ``bench/harbor_adapter/stella_harbor/__init__.py``.
#:
#: The adapter's ambient check fails closed. Every script in
#: ``bench/evidence/run/`` sources ``env.sh`` before it calls Harbor. So a
#: ``STELLA_*`` name exported here and missing there refuses the run. It does
#: that after the images are pulled, on every trial.
REGISTERED_STELLA_EXPORTS = frozenset(
    {
        "STELLA_TARGET_TRIPLE",
        "STELLA_GLIBC_FLOOR",
        "STELLA_BINARY",
        "STELLA_SPEND_LIMIT",
        "STELLA_DISABLE_REFLECTION",
        "STELLA_WITNESS_AUTHOR_MODEL",
        "STELLA_SOURCE_COMMIT",
        "STELLA_PIPELINE",
    }
)

# What a shell that sources `env.sh` is given. It holds no `STELLA_` name. So
# any such name the checks below see came from the file under test. None of it
# came from whoever ran pytest.
_BASE_ENV = {"PATH": "/usr/bin:/bin:/usr/local/bin", "HOME": "/tmp"}


def _source_env(tmp_path: Path, **overrides: str) -> subprocess.CompletedProcess[str]:
    """Source ``env.sh`` and print the variables the assertions read."""
    repo = tmp_path / "repo"
    root = tmp_path / "root"
    repo.mkdir(exist_ok=True)
    root.mkdir(exist_ok=True)
    env = dict(_BASE_ENV)
    env["TB_REPO"] = overrides.pop("TB_REPO", str(repo))
    env["TB_ROOT"] = overrides.pop("TB_ROOT", str(root))
    env.update(overrides)
    script = (
        f'source "{ENV_SH}"\n'
        'echo "STELLA_BINARY=$STELLA_BINARY"\n'
        'echo "TB_ARM_DIR=$TB_ARM_DIR"\n'
        'echo "TB_BUILD_OUTPUT=$TB_BUILD_OUTPUT"\n'
        'echo "PYTHONPATH=$PYTHONPATH"\n'
        'echo "EXPORTED_STELLA=$(compgen -e | grep "^STELLA_" | sort | tr "\\n" ",")"\n'
    )
    return subprocess.run(
        ["bash", "-c", script],
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )


def _values(result: subprocess.CompletedProcess[str]) -> dict[str, str]:
    assert result.returncode == 0, f"env.sh refused: {result.stdout}{result.stderr}"
    out: dict[str, str] = {}
    for line in result.stdout.splitlines():
        if "=" in line:
            key, _, value = line.partition("=")
            out[key] = value
    return out


def test_a_run_with_no_arm_reads_the_paths_it_always_did(tmp_path: Path) -> None:
    """No ``TB_ARM`` leaves every path where the single-build scripts expect it."""
    values = _values(_source_env(tmp_path))
    repo = tmp_path / "repo"
    assert values["TB_ARM_DIR"] == str(tmp_path / "root")
    assert values["STELLA_BINARY"] == str(
        repo / "target" / "x86_64-unknown-linux-gnu" / "release" / "stella"
    )
    # A single build runs the binary cargo just wrote. No copy sits in between.
    # So `primary.sh` and its neighbours do not change.
    assert values["STELLA_BINARY"] == values["TB_BUILD_OUTPUT"]


def test_two_arms_get_two_binaries_and_two_provenance_directories(
    tmp_path: Path,
) -> None:
    """The failure this change exists to stop: one arm overwriting the other."""
    control = _values(_source_env(tmp_path, TB_ARM="control"))
    treatment = _values(_source_env(tmp_path, TB_ARM="treatment"))

    assert control["STELLA_BINARY"] != treatment["STELLA_BINARY"]
    assert control["TB_ARM_DIR"] != treatment["TB_ARM_DIR"]
    # `build_sut.sh` writes sut_commit.txt and binary_sha256.txt into
    # TB_ARM_DIR. Two dirs is two records of which build ran.
    arms = tmp_path / "root" / "arms"
    assert control["TB_ARM_DIR"] == str(arms / "control")
    assert treatment["TB_ARM_DIR"] == str(arms / "treatment")
    assert Path(control["TB_ARM_DIR"]).is_dir()
    assert Path(treatment["TB_ARM_DIR"]).is_dir()
    # The arm's binary is a copy. It sits outside the target dir that built it.
    # So a rebuild of the other arm cannot replace it.
    assert control["STELLA_BINARY"] == str(arms / "control" / "stella")
    assert control["STELLA_BINARY"] != control["TB_BUILD_OUTPUT"]


def test_the_adapter_comes_from_tb_repo_when_the_build_repo_is_elsewhere(
    tmp_path: Path,
) -> None:
    """Only the binary may differ between arms. Never the tool that measures.

    The control arm is built from a checkout of an old commit. Its crate is
    gone from this workspace. Aim ``TB_REPO`` at that checkout to reach its
    ``target/`` and ``PYTHONPATH`` goes with it. That swaps in the old
    adapter, which has no ``--pipeline`` selector. ``TB_BUILD_REPO`` moves the
    build alone.
    """
    old = tmp_path / "old-checkout"
    old.mkdir()
    values = _values(_source_env(tmp_path, TB_ARM="control", TB_BUILD_REPO=str(old)))

    assert values["PYTHONPATH"] == str(tmp_path / "repo" / "bench" / "harbor_adapter")
    assert values["TB_BUILD_OUTPUT"].startswith(str(old))


@pytest.mark.parametrize("arm", ["../escape", "Control", "control arm", "arm/sub"])
def test_an_arm_name_that_is_not_a_plain_slug_is_refused(
    tmp_path: Path, arm: str
) -> None:
    """``TB_ARM`` becomes a directory name, so it may not carry a path."""
    result = _source_env(tmp_path, TB_ARM=arm)
    assert result.returncode == 1
    assert "TB_ARM must be" in result.stdout


def test_env_sh_exports_no_stella_name_the_adapter_has_not_registered(
    tmp_path: Path,
) -> None:
    """An unregistered ``STELLA_*`` export refuses the run on every trial."""
    values = _values(_source_env(tmp_path, TB_ARM="control", STELLA_PIPELINE="classic"))
    exported = {name for name in values["EXPORTED_STELLA"].split(",") if name}
    assert exported <= REGISTERED_STELLA_EXPORTS, (
        "env.sh exports a STELLA_ name that is not in this test's allowlist. "
        "Register it in _HOST_ONLY_STELLA_ENV in "
        "bench/harbor_adapter/stella_harbor/__init__.py and add it here, or "
        "give it a TB_ prefix if it means nothing inside a task container."
    )


def _run_arm(tmp_path: Path, *args: str, **overrides: str) -> subprocess.CompletedProcess[str]:
    """Launch an arm against the real checkout. Scratch space is ``tmp_path``.

    ``TB_REPO`` is this repo, not a fixture. The script reads
    ``plugins/stella-witness/plugin.toml`` out of it. That check asks whether
    the plugin still declares the id the plan pinned. A fixture would answer
    for itself. Nothing is written under ``TB_REPO``. The scratch tree is
    ``TB_ROOT``.
    """
    root = tmp_path / "root"
    root.mkdir(exist_ok=True)
    env = dict(_BASE_ENV)
    env["TB_REPO"] = str(REPO_ROOT)
    env["TB_ROOT"] = str(root)
    env.update(overrides)
    return subprocess.run(
        ["bash", str(PIPELINE_AB), *args],
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )


def test_an_unknown_arm_name_is_refused_before_anything_is_sourced(
    tmp_path: Path,
) -> None:
    result = _run_arm(tmp_path, "witness-on", "job1")
    assert result.returncode == 1
    assert "arm must be 'control' or 'treatment'" in result.stdout


def test_a_spend_limit_is_refused_because_the_experiment_is_uncapped(
    tmp_path: Path,
) -> None:
    """A cap that binds on one arm and not the other measures the cap.

    The control arm makes several model calls per step. The treatment arm makes
    one. So the default would cut the two arms by different amounts. The plan
    fixes the run as uncapped.
    """
    result = _run_arm(tmp_path, "control", "job1", STELLA_SPEND_LIMIT="0.60")
    assert result.returncode == 1
    assert "this experiment is uncapped" in result.stdout


def test_the_treatment_arm_refuses_while_no_plugin_reaches_the_container(
    tmp_path: Path,
) -> None:
    """The refusal costs one message. Finding it per trial costs the arm.

    ``stella_harbor`` uploads the ``stella`` binary and nothing else. So the
    plugin roster in a task container is empty, and ``PipelineChoice::resolve``
    refuses ``--pipeline witness-v1``. That fails closed. ``agent::goal`` turns
    it into a ``CliFailure``. The arm makes no number, rather than a wrong one.
    Delete this case in the change that stages a plugin.
    """
    result = _run_arm(tmp_path, "treatment", "job1")
    assert result.returncode == 1
    assert "nothing installs the plugin in the task container" in result.stdout
    assert "The control arm is unaffected" in result.stdout


def test_the_script_launches_the_pipeline_ids_the_preregistration_named() -> None:
    """The plan's ``invocation`` strings and the launcher's flags are one thing.

    ``compare_arms.py --treatment-fired`` reads ``loop_mode=PLUGIN:witness-v1``.
    The adapter builds that value from the ``--pipeline`` flag it was given. So
    an id changed in the script and left alone in the plan yields a full arm.
    The analysis then refuses it, after the spend.
    """
    import json

    prereg = json.loads(
        (
            REPO_ROOT / "bench" / "evidence" / "pipeline-ab" / "preregistration.json"
        ).read_text()
    )
    script = PIPELINE_AB.read_text()

    for arm, flag in (("control", "classic"), ("treatment", "witness-v1")):
        assert prereg["arms"][arm]["invocation"].endswith(f"--pipeline {flag}")
    assert 'WITNESS_PLUGIN_ID="witness-v1"' in script
    assert "control)   PIPELINE_ID=\"classic\"" in script


def test_the_witness_plugin_still_declares_the_pinned_id() -> None:
    """A rename would aim the treatment arm at a path nothing measures."""
    manifest = (REPO_ROOT / "plugins" / "stella-witness" / "plugin.toml").read_text()
    assert 'id = "witness-v1"' in manifest
