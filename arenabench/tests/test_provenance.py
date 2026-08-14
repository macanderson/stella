# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""What a result set was measured *with*, including the opponent.

Provenance always pinned our side and the grading stack. These cover the third
axis — each contestant's own product version — and the failure it exists to
catch: an arm whose numbers blend two releases of the agent under comparison.

The bias is the module's own: a fabricated version is worse than an absent one,
so every test that could accept a guess checks that it does not.

No Docker, no network: every fixture is a synthetic match tree.
"""

from __future__ import annotations

import json
from pathlib import Path

from arenabench import provenance
from arenabench.provenance import (
    AGENT_SOURCE_OBSERVED,
    AGENT_SOURCE_PINNED,
    Provenance,
)

# --------------------------------------------------------------------------
# fixtures
# --------------------------------------------------------------------------


def make_trial(
    job_dir: Path, trial: str, *, agent: str, version: str | None
) -> None:
    """A trial directory carrying an ATIF trajectory, as Harbor leaves it."""
    path = job_dir / trial / "agent" / "trajectory.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    agent_block: dict = {"name": agent, "model_name": "claude-opus-5"}
    if version is not None:
        agent_block["version"] = version
    path.write_text(
        json.dumps({"schema_version": "ATIF-v1.2", "agent": agent_block, "steps": []}),
        encoding="utf-8",
    )


def make_job(
    match_dir: Path,
    job: str,
    *,
    agent: str,
    contestant: str,
    versions: list[str | None],
    seat_manifest: bool = True,
) -> Path:
    job_dir = match_dir / "jobs" / job
    job_dir.mkdir(parents=True, exist_ok=True)
    if seat_manifest:
        (job_dir.parent / f"{job}.seat.json").write_text(
            json.dumps(
                {"schema": 1, "contestant": contestant, "agent": agent, "name": agent}
            ),
            encoding="utf-8",
        )
    for index, version in enumerate(versions):
        make_trial(job_dir, f"task-{index}__aaa{index}", agent=agent, version=version)
    return job_dir


def base_record(**kwargs) -> Provenance:
    defaults = dict(
        dataset_key="terminal-bench-2.1",
        dataset_digest="sha256:abcd1234ef",
        harbor_version="0.6.1",
        sut_commit="deadbeefcafe1234",
    )
    defaults.update(kwargs)
    return Provenance(**defaults)


# --------------------------------------------------------------------------
# observing what actually ran
# --------------------------------------------------------------------------


def test_the_opponents_version_is_read_off_its_trials(tmp_path: Path) -> None:
    """ATIF has carried `agent.version` all along; nothing read it."""
    make_job(
        tmp_path,
        "m1-claude-code",
        agent="claude-code",
        contestant="cc",
        versions=["2.1.226", "2.1.226"],
    )
    seats = provenance.observe_agent_versions(tmp_path)
    assert seats["cc"]["agent"] == "claude-code"
    assert seats["cc"]["versions"] == ["2.1.226"]
    assert seats["cc"]["trials"] == 2
    assert seats["cc"]["source"] == AGENT_SOURCE_OBSERVED


def test_an_arm_that_blended_two_releases_is_named_as_one(tmp_path: Path) -> None:
    """**The witness.** A match whose arm ran two products says so.

    Not hypothetical: match ``f23fbcd61adc`` in the local archive ran Claude
    Code 2.1.223 and 2.1.224 across the trials of a single job, because Harbor
    installs the CLI per container from the vendor's script with no pin and a
    release landed mid-match. Its solve rate is a blend of two agents. Before
    this, nothing in the record could say so, and its key was identical to a
    match that ran either one alone.
    """
    make_job(
        tmp_path,
        "m1-claude-code",
        agent="claude-code",
        contestant="cc",
        versions=["2.1.223", "2.1.224", "2.1.223"],
    )
    record = base_record(agent_seats=provenance.observe_agent_versions(tmp_path))

    assert record.mixed_version_seats == ("cc",)
    assert record.agent_products == ("claude-code2.1.223+2.1.224",)
    assert "2.1.223+2.1.224" in record.comparability_key


def test_two_releases_of_the_opponent_do_not_share_a_key(tmp_path: Path) -> None:
    """The whole point: they are different apparatus, so different keys."""
    older = base_record(
        agent_seats={
            "cc": {"agent": "claude-code", "versions": ["2.1.220"], "trials": 5}
        }
    )
    newer = base_record(
        agent_seats={
            "cc": {"agent": "claude-code", "versions": ["2.1.226"], "trials": 5}
        }
    )
    assert older.comparability_key != newer.comparability_key
    assert older.comparability_key_for("cc") != newer.comparability_key_for("cc")


def test_an_arms_key_names_its_own_product_not_ours(tmp_path: Path) -> None:
    """A Claude Code arm used to key as `stella?`, which names the wrong agent.

    Two Claude Code arms four releases apart therefore grouped into one series,
    and the difference between them read as noise around a single number.
    """
    record = base_record(
        sut_seats={"st": {"name": "Stella", "commit": "deadbeefcafe1234"}},
        agent_seats={
            "cc": {"agent": "claude-code", "versions": ["2.1.226"], "trials": 5}
        },
    )
    assert record.comparability_key_for("st").endswith("/stelladeadbeef")
    assert record.comparability_key_for("cc").endswith("/claude-code2.1.226")


def test_an_unknown_version_is_marked_never_guessed(tmp_path: Path) -> None:
    """A trial whose trajectory names no version leaves the label partial."""
    make_job(
        tmp_path,
        "m1-claude-code",
        agent="claude-code",
        contestant="cc",
        versions=["2.1.226", None, "unknown"],
    )
    seats = provenance.observe_agent_versions(tmp_path)
    assert seats["cc"]["unversioned"] == 2
    record = base_record(agent_seats=seats)
    assert record.agent_products == ("claude-code2.1.226+?",)


def test_a_stella_seat_is_left_to_its_commit(tmp_path: Path) -> None:
    """Stella's identity is the SUT pin; a second copy would only disagree."""
    make_job(
        tmp_path, "m1-stella", agent="stella", contestant="st", versions=["0.7.4-dev.ab"]
    )
    assert provenance.observe_agent_versions(tmp_path) == {}


def test_a_stella_seat_is_skipped_without_a_seat_manifest(tmp_path: Path) -> None:
    """Every archive predating the seat manifest still has to skip Stella.

    The manifest is what normally answers "which agent is this job"; without it
    the skip has to fall back to what the trajectory says about itself, or a
    Stella seat is recorded as a product version spelled
    ``stella 0.7.4-dev.… [binary-sha256:…]`` — competing with the pin that
    already identifies it.
    """
    make_job(
        tmp_path,
        "m1-stella",
        agent="stella",
        contestant="st",
        versions=["stella 0.6.76 [binary-sha256:3e4256b6]"],
        seat_manifest=False,
    )
    assert provenance.observe_agent_versions(tmp_path) == {}


def test_a_job_with_no_trajectories_is_absent_not_empty(tmp_path: Path) -> None:
    """A seat that produced nothing has no product to record."""
    (tmp_path / "jobs" / "m1-claude-code").mkdir(parents=True)
    assert provenance.observe_agent_versions(tmp_path) == {}


# --------------------------------------------------------------------------
# the key stays compatible with everything already archived
# --------------------------------------------------------------------------


def test_a_record_with_no_agent_versions_keys_exactly_as_before() -> None:
    """Every key written before this existed must keep its spelling.

    The archive has to stay groupable against itself: a key that changed shape
    would split every existing series at the commit that shipped this.
    """
    record = base_record()
    assert record.comparability_key == (
        "terminal-bench-2.1@abcd1234/harbor0.6.1/stelladeadbeef"
    )


def test_recording_the_opponent_is_itself_a_different_apparatus() -> None:
    """A match that knows what it raced is not the same as one that does not."""
    blind = base_record()
    knowing = base_record(
        agent_seats={"cc": {"agent": "claude-code", "versions": ["2.1.226"]}}
    )
    assert blind.comparability_key != knowing.comparability_key


# --------------------------------------------------------------------------
# pinning, and checking the pin
# --------------------------------------------------------------------------


def test_a_pin_survives_a_stamp(tmp_path: Path) -> None:
    """An observation does not overwrite what the installer was told to fetch."""
    make_job(
        tmp_path,
        "m1-claude-code",
        agent="claude-code",
        contestant="cc",
        versions=["2.1.226"],
    )
    provenance.write_match(
        tmp_path,
        base_record(
            agent_seats={
                "cc": {
                    "agent": "claude-code",
                    "versions": ["2.1.226"],
                    "source": AGENT_SOURCE_PINNED,
                }
            }
        ),
    )
    stamped = provenance.stamp_agent_versions(tmp_path)
    assert stamped is not None
    assert stamped.agent_seats["cc"]["source"] == AGENT_SOURCE_PINNED
    assert stamped.violated_pins == {}


def test_a_pin_the_installer_ignored_is_reported_not_believed(tmp_path: Path) -> None:
    """A pin is an instruction, not evidence — so it gets checked.

    A pin believed without checking is worth less than no pin, because the
    belief is what makes the comparison quotable. The key names what ran.
    """
    make_job(
        tmp_path,
        "m1-claude-code",
        agent="claude-code",
        contestant="cc",
        versions=["2.1.229", "2.1.229"],
    )
    provenance.write_match(
        tmp_path,
        base_record(
            agent_seats={
                "cc": {
                    "agent": "claude-code",
                    "versions": ["2.1.226"],
                    "source": AGENT_SOURCE_PINNED,
                }
            }
        ),
    )
    stamped = provenance.stamp_agent_versions(tmp_path)
    assert stamped is not None
    assert stamped.violated_pins == {"cc": ["2.1.229"]}
    # Labelled by what ran, never by what was asked for.
    assert stamped.agent_products == ("claude-code2.1.229",)
    assert "2.1.229" in stamped.comparability_key


def test_stamping_twice_changes_nothing(tmp_path: Path) -> None:
    """Idempotent, so a partial match can be stamped again when it completes."""
    make_job(
        tmp_path,
        "m1-claude-code",
        agent="claude-code",
        contestant="cc",
        versions=["2.1.226"],
    )
    provenance.write_match(tmp_path, base_record())
    first = provenance.stamp_agent_versions(tmp_path)
    second = provenance.stamp_agent_versions(tmp_path)
    assert first is not None and second is not None
    assert first.to_json()["agent_seats"] == second.to_json()["agent_seats"]
    assert first.comparability_key == second.comparability_key


def test_a_stamp_with_no_record_does_not_invent_one(tmp_path: Path) -> None:
    """A match that recorded nothing is backfilled deliberately, not by a stamp."""
    make_job(
        tmp_path,
        "m1-claude-code",
        agent="claude-code",
        contestant="cc",
        versions=["2.1.226"],
    )
    assert provenance.stamp_agent_versions(tmp_path) is None


# --------------------------------------------------------------------------
# history
# --------------------------------------------------------------------------


def test_a_backfill_recovers_the_version_the_archive_always_had(
    tmp_path: Path,
) -> None:
    """Unlike the Harbor version, this one *is* on disk for every old match.

    So a backfill labels the whole existing archive by the product that
    produced it, rather than by our commit alone.
    """
    job_dir = make_job(
        tmp_path,
        "m1-claude-code",
        agent="claude-code",
        contestant="cc",
        versions=["2.1.222"],
    )
    (job_dir / "config.json").write_text(
        json.dumps({"datasets": [{"name": "terminal-bench/terminal-bench-2-1"}]}),
        encoding="utf-8",
    )
    record = provenance.backfill_match(tmp_path)
    assert record is not None
    assert record.source == provenance.SOURCE_BACKFILLED
    # The grading stack stays honestly unknown; the product does not have to.
    assert record.harbor_version is None
    assert record.agent_seats["cc"]["versions"] == ["2.1.222"]


def test_a_record_round_trips_through_json(tmp_path: Path) -> None:
    """Whatever is written has to read back as the same record."""
    record = base_record(
        agent_seats={
            "cc": {
                "agent": "claude-code",
                "name": "Claude Code",
                "versions": ["2.1.226"],
                "source": AGENT_SOURCE_OBSERVED,
                "trials": 8,
                "unversioned": 0,
            }
        }
    )
    provenance.write_match(tmp_path, record)
    back = provenance.read_match(tmp_path)
    assert back is not None
    assert back.agent_seats == record.agent_seats
    assert back.comparability_key == record.comparability_key


class TestRunnerImageIdentity:
    """#3214: the one apparatus field no provenance generation had a slot for."""

    def test_the_cloud_writer_records_its_own_container_image(
        self, tmp_path: Path, monkeypatch
    ) -> None:
        monkeypatch.setenv(
            provenance.RUNNER_IMAGE_ENV,
            "123.dkr.ecr.us-east-1.amazonaws.com/arenabench/runner:latest@sha256:abc",
        )
        provenance.write_match(tmp_path, base_record())
        back = provenance.read_match(tmp_path)
        assert back is not None
        assert back.runner_image.endswith("@sha256:abc")

    def test_a_local_launch_honestly_records_nothing(
        self, tmp_path: Path, monkeypatch
    ) -> None:
        monkeypatch.delenv(provenance.RUNNER_IMAGE_ENV, raising=False)
        provenance.write_match(tmp_path, base_record())
        back = provenance.read_match(tmp_path)
        assert back is not None
        assert back.runner_image == ""

    def test_a_host_side_restamp_keeps_the_containers_answer(
        self, tmp_path: Path, monkeypatch
    ) -> None:
        """A fetched cloud record re-written on a laptop must not blank the
        identity only the container could observe."""
        monkeypatch.delenv(provenance.RUNNER_IMAGE_ENV, raising=False)
        record = base_record(runner_image="ecr/arenabench/runner@sha256:abc")
        provenance.write_match(tmp_path, record)
        back = provenance.read_match(tmp_path)
        assert back is not None
        assert back.runner_image == "ecr/arenabench/runner@sha256:abc"

    def test_a_legacy_record_reads_back_empty_not_fabricated(self) -> None:
        legacy = provenance.Provenance.from_json({"dataset_key": "tb"})
        assert legacy.runner_image == ""
