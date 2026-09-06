"""Reading a run off disk — the layer every conclusion rests on.

Three of these pin defects that were live in this module during development,
which is why they are here rather than left to the detector tests: a loader
that silently reads nothing produces a run with no findings, and a run with no
findings looks exactly like a healthy one.
"""

from __future__ import annotations

import ast
import json
import re
from pathlib import Path

import run_trace
import triage_bench_traces
from fixtures import ok, proof, tool_pair, write_run
from run_trace import READ_FILENAMES, load_run


def test_a_half_written_tail_is_counted_not_fatal(tmp_path):
    """A run still streaming has an incomplete last line. It must not abort triage."""
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 0,
                "events": [{"ts": 1, "type": "step_usage", "role": "worker", "model": "m"}],
                "raw_tail": '{"ts": 2, "type": "tool_st',
            }
        ],
    )
    run = load_run(tmp_path, "r")
    (trial,) = run.trials
    assert trial.malformed_lines == 1
    assert len(trial.of_type("step_usage")) == 1, "the readable events survive the bad line"


def test_the_exception_class_comes_from_harbors_job_result_json(tmp_path):
    """`result.json` (harbor, job level) — NOT `results.json` (arenabench, trial root).

    Reading the wrong one is not a crash: it reports zero exceptions on a run
    where every trial threw, which reads as a clean run.
    """
    write_run(
        tmp_path,
        [{"task": "alpha__AAA", "reward": 0, "exception_class": "AgentTimeoutError", "events": []}],
    )
    (trial,) = load_run(tmp_path, "r").trials
    assert trial.exception_types == ("AgentTimeoutError",)


def test_the_declared_seat_is_read_from_the_run_not_from_a_local_config(tmp_path):
    write_run(
        tmp_path,
        [{"task": "alpha__AAA", "reward": 0, "events": []}],
        declared_worker="openrouter/z-ai/glm-5.2",
        declared_roles={"plan": "openrouter/deepseek/deepseek-chat"},
    )
    run = load_run(tmp_path, "r")
    assert run.declared_worker_model == "openrouter/z-ai/glm-5.2"
    assert run.declared_roles == {"plan": "openrouter/deepseek/deepseek-chat"}
    assert run.sut_ref.startswith("554bb4f")


def test_every_occurrence_can_be_fetched_by_the_reader(tmp_path):
    """A citation that is not a key is a claim nobody can check."""
    write_run(tmp_path, [{"task": "alpha__AAA", "reward": 1, "events": []}])
    (trial,) = load_run(tmp_path, "myrun").trials
    key = trial.s3_key()
    assert key.startswith("s3://arenabench-artifacts-578673726240/runs/myrun/")
    assert key.endswith("alpha__AAA/agent/stella-events.jsonl")


def test_tool_calls_join_start_to_result_and_keep_an_unanswered_call(tmp_path):
    """A `tool_start` with no result is the process dying mid-tool — evidence, not a gap."""
    events = tool_pair("c1", "bash", {"command": "ls"}, ok("a\nb"))
    events.append(
        {"ts": 5, "type": "tool_start", "call": {"call_id": "c2", "name": "bash", "input": {}}}
    )
    write_run(tmp_path, [{"task": "alpha__AAA", "reward": 0, "events": events}])
    (trial,) = load_run(tmp_path, "r").trials
    by_id = {c.call_id: c for c in trial.tool_calls}
    assert by_id["c1"].ok_content == "a\nb"
    assert by_id["c2"].result is None


def test_the_task_name_drops_the_per_trial_suffix(tmp_path):
    write_run(tmp_path, [{"task": "code-from-image__UTUWFRc", "reward": 0, "events": []}])
    (trial,) = load_run(tmp_path, "r").trials
    assert trial.task_id == "code-from-image__UTUWFRc"
    assert trial.task_name == "code-from-image"


def test_the_role_census_counts_completed_calls_only(tmp_path):
    """`step_manifest` is a call the engine intended; `step_usage` is one that returned."""
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 0,
                "events": [
                    {"ts": 1, "type": "step_manifest", "role": "worker", "model": "never-returned"},
                    {"ts": 2, "type": "step_usage", "role": "worker", "model": "m"},
                    {"ts": 3, "type": "step_usage", "role": "worker", "model": "m"},
                ],
            }
        ],
    )
    (trial,) = load_run(tmp_path, "r").trials
    assert trial.role_census() == {("worker", "m"): 2}


def test_proofs_are_selected_by_kind(tmp_path):
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 0,
                "events": [
                    proof("oracle", tree="baseline", passed=False),
                    proof("assurance", witness=True),
                ],
            }
        ],
    )
    (trial,) = load_run(tmp_path, "r").trials
    assert len(trial.proofs()) == 2
    assert len(trial.proofs("oracle")) == 1
    assert trial.proofs("oracle")[0]["tree"] == "baseline"


def test_a_trial_with_no_reward_file_is_not_counted_as_a_zero(tmp_path):
    """An unrecorded reward and a reward of 0 are different facts."""
    write_run(tmp_path, [{"task": "alpha__AAA", "reward": None, "events": []}])
    (trial,) = load_run(tmp_path, "r").trials
    assert trial.reward is None
    assert load_run(tmp_path, "r").graded_trials == []


def test_an_unreadable_results_json_does_not_stop_the_load(tmp_path):
    write_run(tmp_path, [{"task": "alpha__AAA", "reward": 0, "events": []}])
    trial_dir = next(p for p in tmp_path.iterdir() if p.is_dir())
    (trial_dir / "results.json").write_text("{ not json")
    run = load_run(tmp_path, "r")
    assert len(run.trials) == 1
    assert run.declared_roles == {}


def test_json_arrays_on_a_line_are_ignored_rather_than_crashing(tmp_path):
    """A line that parses but is not an object is not an event."""
    write_run(
        tmp_path,
        [{"task": "alpha__AAA", "reward": 0, "events": [], "raw_tail": json.dumps([1, 2])}],
    )
    (trial,) = load_run(tmp_path, "r").trials
    assert trial.events == []
    assert trial.malformed_lines == 1


# --------------------------------------------------------------------------
# `--fetch`'s include-list against what `load_run` actually reads
# --------------------------------------------------------------------------


def test_fetch_includes_every_file_load_run_reads():
    """A dropped pattern here is invisible until a run silently loads wrong.

    If `*reward.txt` ever fell out of `_INCLUDES`, every trial would load with
    `reward = None` — no crash, no malformed-line count, just every
    `oracle-flip-ungraded` finding disappearing and the run reading as
    healthy. This checks the real argv `--fetch` runs against
    `run_trace.READ_FILENAMES`, the loader's own declaration of what it reads.
    """
    argv = triage_bench_traces.fetch_argv("run-id", Path("/tmp/mirror/run-id"))
    missing = triage_bench_traces.missing_includes(argv, READ_FILENAMES)
    assert not missing, f"--fetch would not sync: {missing}"


def test_missing_includes_actually_catches_a_dropped_pattern():
    """The check above has teeth: prove it on an argv with a pattern removed.

    Without this, `test_fetch_includes_every_file_load_run_reads` could pass
    vacuously — every real filename happens to survive today, which proves
    nothing about whether the check would notice one going missing tomorrow.
    """
    argv = triage_bench_traces.fetch_argv("run-id", Path("/tmp/mirror/run-id"))
    dropped = argv.index("*reward.txt")
    truncated = argv[: dropped - 1] + argv[dropped + 1 :]  # drop `--include *reward.txt`
    assert triage_bench_traces.missing_includes(truncated, READ_FILENAMES) == ["reward.txt"]


# --------------------------------------------------------------------------
# `READ_FILENAMES` against what `run_trace.py` actually names in code
# --------------------------------------------------------------------------

_FILENAME_LITERAL = re.compile(r"^[\w.-]+\.(?:json|jsonl|txt)$")


def _filename_literals(source: str, filename: str = "<source>") -> set[str]:
    """Every string in `source` shaped like a filename, skipping the docstring.

    A module docstring may name a file in prose (`spec.json`) with no code
    behind it. Its string is one big `ast.Constant`; the tight filename shape
    below never matches a whole paragraph, so no special case is needed.
    """
    tree = ast.parse(source, filename=filename)
    return {
        node.value
        for node in ast.walk(tree)
        if isinstance(node, ast.Constant)
        and isinstance(node.value, str)
        and _FILENAME_LITERAL.match(node.value)
    }


def test_every_filename_literal_in_run_trace_is_declared():
    """A raw filename string in `run_trace.py` must be one `load_run` reads.

    `READ_FILENAMES` names each file once (`_RESULTS_JSON`, ...) and every
    read routes through that name, not a second spelling. This scans the
    module's own source, so a new read added as a fresh literal — bypassing
    the shared constants — fails here instead of silently outrunning
    `--fetch`.
    """
    source = Path(run_trace.__file__).read_text(encoding="utf-8")
    literals = _filename_literals(source, run_trace.__file__)
    assert literals, "the scan found no filename literal — it stopped reading its subject"
    stray = literals - set(READ_FILENAMES)
    assert not stray, (
        f"run_trace.py spells {sorted(stray)} as a raw string outside "
        "READ_FILENAMES — route the read through a named constant there"
    )


def test_the_filename_scan_actually_catches_a_stray_literal():
    """The check above has teeth: a raw literal outside the list is a failure."""
    source = 'path = task_dir / "stray_file.json"\n'
    assert _filename_literals(source) == {"stray_file.json"}
