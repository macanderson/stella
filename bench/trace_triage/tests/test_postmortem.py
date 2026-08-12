"""The postmortem fires on a run we know is broken and is quiet on a healthy one.

The second half is the one that matters. A detector that always fires is noise,
and noise is how a real finding gets ignored — so the healthy fixture here is
not a smoke test, it is the load-bearing assertion. It is built to the measured
healthy profile (30 tool calls a trial, no identical consecutive calls, a
prompt cache above 93%) and the report on it must be one clean paragraph with
no manufactured concern in it.

The other load-bearing case is the separation the report exists to make
impossible to miss: an arm whose credential never authenticated writes reward
files full of zeros, and those zeros must never reach a solve rate.

Offline by construction — the traces are written, never fetched.
"""

from __future__ import annotations

import json
from pathlib import Path

from bands import arm_metrics, trial_metrics
from fixtures import event, ok, tool_pair, write_run
from postmortem import build_report, cohort_of, render_markdown, write_report
from run_trace import load_run

# --------------------------------------------------------------------------
# fixture arms
# --------------------------------------------------------------------------


def _usage(step: int, *, prompt: int = 20_000, cached: int = 19_800) -> dict:
    """One completed model call, with the token fields the cache band reads."""
    return event(
        ts=1000 * (step + 1),
        type="step_usage",
        step=step,
        role="worker",
        model="openrouter/z-ai/glm-5.2",
        input_tokens=prompt,
        cached_input_tokens=cached,
        cache_write_tokens=200,
    )


def _healthy_trial(task: str, *, tools: int = 30, cached: int = 19_800) -> dict:
    """A trial at the measured healthy profile: distinct calls, warm cache."""
    events: list[dict] = []
    for index in range(tools):
        events.append(_usage(index, cached=cached))
        events.extend(
            tool_pair(
                f"c{index}",
                "read_file",
                {"path": f"src/mod_{index}.rs"},
                ok(f"contents of mod_{index}"),
            )
        )
    return {"task": task, "events": events, "reward": 1.0}


def _healthy_run(root: Path, trials: int = 6) -> Path:
    return write_run(
        root,
        [_healthy_trial(f"task-{i}__abc{i}") for i in range(trials)],
        bare_loop=True,
    )


def _cold_cache_trial(task: str) -> dict:
    """The broken-roster shape: the prompt cache stopped engaging (52%)."""
    return _healthy_trial(task, cached=10_000)


def _stuck_trial(task: str) -> dict:
    """Identical consecutive calls with byte-identical output."""
    events: list[dict] = []
    for index in range(10):
        events.append(_usage(index))
        events.extend(
            tool_pair(f"a{index}", "bash", {"cmd": "ls"}, ok("same bytes"))
        )
    return {"task": task, "events": events, "reward": 0.0}


def _void_trial(task: str) -> dict:
    """Reasoning deltas and zero completed model calls — measured nothing."""
    return {
        "task": task,
        "events": [event(ts=1, type="reasoning", delta="thinking")],
        "reward": 0.0,
    }


# --------------------------------------------------------------------------
# quiet on a healthy run
# --------------------------------------------------------------------------


def test_a_healthy_run_gets_a_short_clean_report(tmp_path: Path) -> None:
    """The assertion the whole design turns on: no manufactured concern."""
    report = build_report(load_run(_healthy_run(tmp_path / "post1"), "post1"))
    assert report.clean
    assert report.findings == []
    assert report.band_exits == []
    assert report.cohort.void == ()

    markdown = render_markdown(report)
    assert "Clean" in markdown
    # No band table, no findings section, no cohort warning — a healthy run
    # must not be handed a page of tables to reassure it.
    assert "| metric |" not in markdown
    assert "Detector findings" not in markdown
    assert "measured nothing" not in markdown
    # Short: a healthy run must not cost the reader a page of tables.
    assert len(markdown.splitlines()) < 20


def test_a_healthy_run_sits_inside_every_measured_band(tmp_path: Path) -> None:
    metrics = arm_metrics(load_run(_healthy_run(tmp_path / "post1"), "post1"))
    assert metrics.mean("tools") == 30.0
    assert metrics.mean("identical_adjacent") == 0.0
    assert metrics.cache_hit is not None and metrics.cache_hit >= 93.0
    assert metrics.void == ()


# --------------------------------------------------------------------------
# fires on a known-bad run
# --------------------------------------------------------------------------


def test_a_collapsed_cache_is_reported_as_a_separator(tmp_path: Path) -> None:
    """52% against a 93% floor — the sharper of the two clean separators."""
    root = write_run(tmp_path / "w10f", [_cold_cache_trial(f"t-{i}__x") for i in range(4)])
    report = build_report(load_run(root, "w10f"))
    assert not report.clean

    exits = {band.key: value for band, value in report.band_exits}
    assert "cache_hit" in exits
    assert exits["cache_hit"] < 93.0

    finding = next(f for f in report.findings if f.detector == "cache-collapse")
    assert finding.count == 4
    # Every cited trial must be openable — a claim with no citation is not a
    # finding.
    assert all(o.s3_key.endswith("stella-events.jsonl") for o in finding.occurrences)

    markdown = render_markdown(report)
    assert "a separator" in markdown
    assert "cache-collapse" in markdown


def test_identical_consecutive_calls_reach_the_report(tmp_path: Path) -> None:
    """The band and the detector must agree; both are asserted here."""
    root = write_run(tmp_path / "stuck", [_stuck_trial(f"t-{i}__x") for i in range(3)])
    run = load_run(root, "stuck")
    report = build_report(run)

    assert any(f.detector == "repeated-identical-tool-call" for f in report.findings)
    exits = {band.key for band, _ in report.band_exits}
    assert "identical_adjacent" in exits
    # The two count the same thing, so they cannot disagree about the arm.
    assert trial_metrics(run.trials[0]).identical_adjacent == 9


def test_a_re_read_of_one_file_is_named_with_its_path(tmp_path: Path) -> None:
    events: list[dict] = [_usage(0)]
    for index in range(7):
        events.extend(
            tool_pair(
                f"r{index}",
                "read_file",
                {"path": "src/lib.rs", "offset": index},
                ok(f"page {index}"),
            )
        )
    root = write_run(tmp_path / "reread", [{"task": "t__x", "events": events, "reward": 0.0}])
    report = build_report(load_run(root, "reread"))
    finding = next(f for f in report.findings if f.detector == "repeated-file-read")
    assert "src/lib.rs" in finding.occurrences[0].location
    assert "7 read-shaped calls" in finding.occurrences[0].location


# --------------------------------------------------------------------------
# "the run is broken" vs "the agent did badly"
# --------------------------------------------------------------------------


def test_a_trial_that_never_reached_the_model_is_not_a_loss(tmp_path: Path) -> None:
    """The single most valuable thing this report makes impossible to miss.

    An arm whose credential failed writes reward files full of zeros. Scoring
    those as losses hands the other arm a win it did not earn, so they are
    counted apart from the measured cohort and said so in the first section.
    """
    root = write_run(
        tmp_path / "dead",
        [_void_trial(f"t-{i}__x") for i in range(3)] + [_healthy_trial("t-9__x")],
    )
    report = build_report(load_run(root, "dead"))

    assert len(report.cohort.void) == 3
    assert report.cohort.measured == 1
    assert not report.clean, "three trials that measured nothing is something to say"

    markdown = render_markdown(report)
    assert "3 trial(s) measured nothing" in markdown
    assert "hands the other arm a win it did not earn" in markdown


def test_void_trials_are_kept_out_of_every_band_mean(tmp_path: Path) -> None:
    """Averaging a trial that never reached the model reports the outage as
    the agent being terse."""
    root = write_run(
        tmp_path / "mixed",
        [_void_trial("t-0__x"), _healthy_trial("t-1__x")],
    )
    metrics = arm_metrics(load_run(root, "mixed"))
    assert len(metrics.void) == 1
    assert metrics.mean("tools") == 30.0  # not 15.0


def test_a_timeout_cohort_is_read_off_the_job_level_result_json(
    tmp_path: Path,
) -> None:
    """Reading the trial-root `results.json` instead reported zero exceptions
    on a run where half the trials threw."""
    trials = [_healthy_trial(f"t-{i}__x") for i in range(4)]
    for spec in trials[:2]:
        spec["exception_class"] = "AgentTimeoutError"
    report = build_report(load_run(write_run(tmp_path / "to", trials), "to"))
    assert len(report.cohort.timed_out) == 2
    assert "AgentTimeoutError" in render_markdown(report)


# --------------------------------------------------------------------------
# small samples, and where the report lands
# --------------------------------------------------------------------------


def test_a_tiny_sample_says_so_instead_of_quoting_a_rate(tmp_path: Path) -> None:
    root = write_run(tmp_path / "tiny", [_cold_cache_trial("t-0__x")])
    report = build_report(load_run(root, "tiny"))
    assert report.noisy_sample
    markdown = render_markdown(report)
    assert "Single occurrence" in markdown
    assert "not as rates" in markdown


def test_the_report_lands_on_the_assembled_match(tmp_path: Path) -> None:
    """`arenabench assemble` folds a run into one match; the report belongs
    beside the contest, not beside the download."""
    root = _healthy_run(tmp_path / "post1")
    (root / "matches" / "post1" / "jobs").mkdir(parents=True)
    markdown, payload = write_report(root, build_report(load_run(root, "post1")))
    assert markdown == root / "matches" / "post1" / "postmortem.md"
    assert json.loads(payload.read_text(encoding="utf-8"))["clean"] is True


def test_an_unassembled_run_still_gets_its_report(tmp_path: Path) -> None:
    root = _healthy_run(tmp_path / "post1")
    markdown, payload = write_report(root, build_report(load_run(root, "post1")))
    assert markdown == root / "postmortem.md"
    assert payload.is_file()


def test_the_report_and_the_triage_path_read_one_detector_registry(
    tmp_path: Path,
) -> None:
    """They must not be able to disagree about whether a run is healthy.

    Not "they currently agree" — *the same call*. `build_report` holds no
    detector list of its own, so a detector added for issue filing is in the
    report by construction and a report that says clean is the triage path
    saying nothing to file.
    """
    import detectors

    root = write_run(tmp_path / "w10f", [_cold_cache_trial(f"t-{i}__x") for i in range(4)])
    run = load_run(root, "w10f")
    assert [f.detector for f in build_report(run).findings] == [
        f.detector for f in detectors.run_all(run)
    ]
    # And the band half is declared, not implied: every band must resolve to a
    # number on a real arm, or the table prints a column nobody can read.
    metrics = arm_metrics(run)
    for band in __import__("bands").BANDS:
        assert metrics.value(band.key) is not None, band.key


def test_a_band_that_cannot_separate_says_so(tmp_path: Path) -> None:
    """The honesty rule: a metric whose healthy and unhealthy arms overlap is
    reported and never concluded from."""
    import bands as band_module

    keys = {b.key: b.verdict_capable for b in band_module.BANDS}
    assert keys["identical_adjacent"] is True
    assert keys["cache_hit"] is True
    assert keys["tools"] is False, "tool count overlaps at 32.5 vs 32.7"
    # The survey's empty-ok column has no reproducible definition, so it is not
    # a band at all rather than a threshold nobody can check.
    assert "empty_ok_results" not in keys


def test_the_json_names_a_trial_for_every_finding(tmp_path: Path) -> None:
    """A finding with no trial to open is not actionable, and this is the
    machine-readable half that a dashboard would render."""
    root = write_run(tmp_path / "w10f", [_cold_cache_trial(f"t-{i}__x") for i in range(4)])
    payload = build_report(load_run(root, "w10f")).to_json()
    assert payload["findings"]
    for finding in payload["findings"]:
        assert finding["trials"], finding["detector"]
        assert finding["fingerprint"].startswith("fp_")
