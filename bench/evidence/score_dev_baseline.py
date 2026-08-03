#!/usr/bin/env python3
"""Score a development-baseline Terminal-Bench run and print its interval.

This is deliberately *not* `terminal_bench_analysis/tb21_analysis.py`. That one
scores the audited public claim: it re-runs the public GitHub GETs, validates the
intent ledger and host attestations, checks the comparator, and applies the
registered thresholds. None of that scaffolding exists for a development
baseline, and pretending otherwise would be the dishonest part.

What this does instead is the whole of the preregistered analysis plan for the
`#909` baseline, and nothing else:

* ``extract``  — read a Harbor job directory into ``trials.jsonl``, one row per
  trial. This is the only step that needs the (large, uncommitted) job tree.
* ``score``    — read ``trials.jsonl`` and compute the number. This is the step
  a reader reproduces from the committed evidence alone.

Reward semantics are copied from the claim analyzer on purpose, so the two
cannot disagree about what counts as a pass: the external verifier's reward is
authoritative; a trial that finished or raised but produced no reward scores
0.0; and the denominator is the task count fixed by the preregistration, never
the number of trials that happened to produce a row.

Usage::

    python score_dev_baseline.py extract <job-dir> -o <run-dir>/trials.jsonl
    python score_dev_baseline.py score <run-dir>/trials.jsonl --tasks 89
"""

from __future__ import annotations

import argparse
import json
import math
import random
import sys
from pathlib import Path
from typing import Any

# Fixed by the preregistration before any data was seen. Changing either of
# these changes the published number and invalidates the comparison.
BOOTSTRAP_SEED = 20260729
BOOTSTRAP_DRAWS = 50_000


def _dict(value: Any) -> dict[str, Any]:
    return value if isinstance(value, dict) else {}


def _number(value: Any) -> float | None:
    return float(value) if isinstance(value, (int, float)) and not isinstance(value, bool) else None


def _int(value: Any) -> int | None:
    return int(value) if isinstance(value, int) and not isinstance(value, bool) and value >= 0 else None


def extract_trial(result_path: Path) -> dict[str, Any]:
    """Flatten one Harbor ``trials/<id>/result.json`` into a scoring row."""
    result = _dict(json.loads(result_path.read_text()))
    config = _dict(result.get("config"))
    agent_result = _dict(result.get("agent_result"))
    verifier_result = _dict(result.get("verifier_result"))
    rewards = _dict(verifier_result.get("rewards"))
    exception_info = _dict(result.get("exception_info"))
    metadata = _dict(agent_result.get("metadata"))
    accounting = _dict(metadata.get("stella_accounting"))
    fields = _dict(accounting.get("fields"))

    reward = _number(rewards.get("reward"))
    exception_type = exception_info.get("exception_type")
    # Same rule as the claim analyzer: an agent may time out having already left
    # a correct solution behind, and the verifier's reward still stands. Only an
    # attempted trial with no reward at all is scored zero.
    accuracy = reward
    if accuracy is None and (exception_type or result.get("finished_at")):
        accuracy = 0.0

    def total(name: str) -> int | None:
        return _int(_dict(fields.get(name)).get("total"))

    execution = _dict(result.get("agent_execution"))

    return {
        "trial_id": result_path.parent.name,
        "task_name": result.get("task_name") or _dict(config.get("task")).get("name"),
        "model": _dict(config.get("agent")).get("model_name"),
        "reward": reward,
        "accuracy": accuracy,
        "exception_type": exception_type,
        "exception_message": exception_info.get("exception_message"),
        "started_at": result.get("started_at"),
        "finished_at": result.get("finished_at"),
        "agent_started_at": execution.get("started_at"),
        "agent_finished_at": execution.get("finished_at"),
        "n_input_tokens": _int(agent_result.get("n_input_tokens")),
        "n_output_tokens": _int(agent_result.get("n_output_tokens")),
        "n_cache_tokens": _int(agent_result.get("n_cache_tokens")),
        # Harbor records the spend on the agent result; the adapter's accounting
        # block carries Stella's own per-step totals. Prefer Harbor's, because it
        # is the same field the claim analyzer bills from. `is not None`, not
        # `or`: a genuine $0.0000 from Harbor must not be silently replaced.
        # The fallback keys are the ones `envelope_accounting` actually emits —
        # `usd_total`/`model_calls` never existed, so every dev-baseline row
        # published `"usd": null` when Harbor's field was absent and
        # `"model_calls": null` always.
        "usd": (
            harbor_usd
            if (harbor_usd := _number(agent_result.get("cost_usd"))) is not None
            else _number(accounting.get("envelope_total_cost_usd"))
        ),
        "model_calls": _int(accounting.get("step_usage_records")),
        "tool_calls": total("tool_calls"),
        # Stella's self-reported terminal state, which is NOT the same thing as
        # the process exiting: a completed turn whose process lingers is killed
        # by Harbor's agent timeout and lands here as
        # status=completed + exception_type=AgentTimeoutError.
        "stella_status": metadata.get("stella_status"),
        "stella_steps": _int(metadata.get("stella_steps")),
        "stella_return_code_state": metadata.get("stella_return_code_state"),
        "accounting_state": accounting.get("state"),
        "binary_sha256_verified": metadata.get("stella_binary_sha256_verified_in_container"),
        "source_commit_verified": metadata.get("stella_source_commit_verified_in_binary"),
        # What the run actually ran, as the adapter recorded it at the time.
        #
        # These do not feed the score. They are here because the manifest used
        # to recompute the posture from whatever checkout it happened to run
        # in, which is the same tree only while the run is still warm — build a
        # manifest for a finished run after the adapter has moved and it
        # confidently describes a posture no trial ever used. Carrying them per
        # trial also makes the committed `trials.jsonl` answer "did every trial
        # run the same configuration" on its own, without the job tree.
        "engine_posture_sha256": metadata.get("stella_engine_posture_sha256"),
        "assurance_arm": metadata.get("stella_assurance_arm"),
        "assurance_tiers_sha256": metadata.get("stella_assurance_tiers_sha256"),
        "binary_sha256": metadata.get("stella_binary_sha256"),
        "source_commit": metadata.get("stella_source_commit"),
        "harbor_version": metadata.get("stella_harbor_version"),
    }


def find_trial_results(job_dir: Path) -> list[Path]:
    """Locate per-trial results in a Harbor job directory.

    Harbor 0.6.1 writes one `<task>__<suffix>/result.json` per trial directly
    under the job directory, alongside the job's own summary `result.json`.
    Later releases nest them under `trials/`. Accept either, and never mistake
    the job summary for a trial (it has no `task_name`).
    """
    nested = sorted((job_dir / "trials").glob("*/result.json"))
    if nested:
        return nested
    return sorted(
        path
        for path in job_dir.glob("*/result.json")
        if path.parent.name != "trials"
    )


def cmd_extract(args: argparse.Namespace) -> int:
    job_dir = Path(args.job_dir)
    trials = find_trial_results(job_dir)
    if not trials:
        print(f"no per-trial result.json found under {job_dir}", file=sys.stderr)
        return 1
    rows = [extract_trial(path) for path in trials]
    # A row with no task name cannot be scored against a per-task denominator,
    # and silently keying one under `None` would invent a task. Drop it loudly.
    named = [row for row in rows if row.get("task_name")]
    if len(named) != len(rows):
        print(f"WARNING: dropped {len(rows) - len(named)} result(s) with no task_name", file=sys.stderr)
    rows = named
    if not rows:
        print(f"no named trials under {job_dir}", file=sys.stderr)
        return 1
    out = Path(args.output)
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("w") as handle:
        for row in rows:
            handle.write(json.dumps(row, sort_keys=True) + "\n")
    print(f"wrote {len(rows)} trial rows to {out}")
    return 0


def clopper_pearson(passes: int, total: int, alpha: float = 0.05) -> tuple[float, float]:
    """Exact binomial interval — deterministic, so it needs no seed to reproduce."""
    if total == 0:
        return (0.0, 1.0)

    def beta_quantile(p: float, a: float, b: float) -> float:
        # Bisection on the regularised incomplete beta function. Plenty precise
        # for a reported interval and avoids a scipy dependency in the evidence.
        low, high = 0.0, 1.0
        for _ in range(200):
            mid = (low + high) / 2
            if _betainc(a, b, mid) < p:
                low = mid
            else:
                high = mid
        return (low + high) / 2

    lower = 0.0 if passes == 0 else beta_quantile(alpha / 2, passes, total - passes + 1)
    upper = 1.0 if passes == total else beta_quantile(1 - alpha / 2, passes + 1, total - passes)
    return (lower, upper)


def _betainc(a: float, b: float, x: float) -> float:
    """Regularised incomplete beta I_x(a, b) by continued fraction.

    The continued fraction only converges for x below roughly the
    distribution's mode; above it, reflect through I_x(a, b) = 1 - I_(1-x)(b, a).
    Without that reflection this silently returns 0.0 in the divergent region,
    which would hand the bisection above a wrong root and publish a wrong
    interval — visibly so only when the score is near 0 or 1.
    """
    if x <= 0:
        return 0.0
    if x >= 1:
        return 1.0
    if x > (a + 1) / (a + b + 2):
        return 1.0 - _betainc(b, a, 1.0 - x)
    ln_beta = math.lgamma(a) + math.lgamma(b) - math.lgamma(a + b)
    front = math.exp(math.log(x) * a + math.log(1 - x) * b - ln_beta) / a
    # Lentz's algorithm.
    f, c, d = 1.0, 1.0, 0.0
    for i in range(0, 300):
        m = i // 2
        if i == 0:
            numerator = 1.0
        elif i % 2 == 0:
            numerator = (m * (b - m) * x) / ((a + 2 * m - 1) * (a + 2 * m))
        else:
            numerator = -((a + m) * (a + b + m) * x) / ((a + 2 * m) * (a + 2 * m + 1))
        d = 1.0 + numerator * d
        d = 1e-30 if abs(d) < 1e-30 else d
        d = 1.0 / d
        c = 1.0 + numerator / c
        c = 1e-30 if abs(c) < 1e-30 else c
        f *= c * d
        if abs(1.0 - c * d) < 1e-15:
            break
    result = front * (f - 1.0)
    return min(max(result, 0.0), 1.0)


def bootstrap(outcomes: list[float], draws: int, seed: int) -> tuple[float, float]:
    """Percentile bootstrap over the per-task binary outcomes."""
    rng = random.Random(seed)
    n = len(outcomes)
    means = []
    for _ in range(draws):
        means.append(sum(outcomes[rng.randrange(n)] for _ in range(n)) / n)
    means.sort()
    return (means[int(0.025 * draws)], means[int(0.975 * draws) - 1])


def cmd_score(args: argparse.Namespace) -> int:
    rows = [json.loads(line) for line in Path(args.trials).read_text().splitlines() if line.strip()]

    # One outcome per task. With one attempt per task these coincide; the max is
    # here so a replication with k>1 reports pass@k over the same denominator
    # rather than silently averaging attempts into it.
    by_task: dict[str, float] = {}
    for row in rows:
        name = row.get("task_name")
        if not name:
            print("FATAL: a trial row has no task_name; refusing to score", file=sys.stderr)
            return 1
        value = row.get("accuracy")
        value = 0.0 if value is None else float(value)
        by_task[name] = max(by_task.get(name, 0.0), value)

    expected = args.tasks
    missing = expected - len(by_task)
    if missing < 0:
        print(f"FATAL: {len(by_task)} tasks present, more than the declared {expected}", file=sys.stderr)
        return 1

    # The preregistered denominator is fixed. A task that produced no row at all
    # counts as a failure, so an aborted run cannot inflate its own score.
    outcomes = [1.0 if value >= 1.0 else 0.0 for value in by_task.values()] + [0.0] * missing
    passes = int(sum(outcomes))
    accuracy = passes / expected

    boot_low, boot_high = bootstrap(outcomes, BOOTSTRAP_DRAWS, BOOTSTRAP_SEED)
    exact_low, exact_high = clopper_pearson(passes, expected)

    # `.get`, not `[...]`: these are descriptive fields that the score does not
    # depend on, and this command is the one an outside reader runs against the
    # committed `trials.jsonl` to recompute a published number. A row that omits
    # an optional token count should cost that reader a blank cell, not a
    # KeyError in place of the score.
    #
    # "Did not report" and "reported zero" are different claims, and every total
    # below is published beside the score where it will be read as complete.
    # Both counts that can be partial therefore carry their own denominator.
    # This is not hypothetical: `usd` and `tool_calls` are filled from the
    # adapter's accounting block, which only Stella emits, so scoring a
    # comparator arm summed cost over the trials that happened to report it and
    # called the rest $0 — and reported every one of its trials as having made
    # zero tool calls.
    usd_reported = [row["usd"] for row in rows if _number(row.get("usd")) is not None]
    tools_reported = [row["tool_calls"] for row in rows if _int(row.get("tool_calls")) is not None]
    tokens_in = sum(row.get("n_input_tokens") or 0 for row in rows)
    tokens_out = sum(row.get("n_output_tokens") or 0 for row in rows)
    usd = sum(usd_reported)
    zero_tool = sum(1 for value in tools_reported if not value)
    errored = sum(1 for row in rows if row.get("exception_type"))

    summary = {
        # v2 adds the `_reported_by` denominators. The bump is the point, as it
        # was for the manifest: a v1 `usd_total` cannot be told apart from a
        # complete one, so it must not silently keep its old name and shape.
        "schema": "stella-tb21-dev-baseline-score-v2",
        "tasks_declared": expected,
        "tasks_with_a_trial": len(by_task),
        "tasks_missing_a_trial": missing,
        "trials": len(rows),
        "passes": passes,
        "accuracy": accuracy,
        "ci95_bootstrap": {
            "low": boot_low,
            "high": boot_high,
            "seed": BOOTSTRAP_SEED,
            "draws": BOOTSTRAP_DRAWS,
            "method": "percentile over per-task binary outcomes",
        },
        "ci95_clopper_pearson": {"low": exact_low, "high": exact_high, "method": "exact binomial"},
        "descriptive": {
            "usd_total": round(usd, 4),
            "usd_reported_by": len(usd_reported),
            "prompt_tokens": tokens_in,
            "completion_tokens": tokens_out,
            "trials_with_zero_tool_calls": zero_tool,
            "tool_calls_reported_by": len(tools_reported),
            "trials_with_an_exception": errored,
            "trials": len(rows),
        },
    }
    print(json.dumps(summary, indent=2))

    if args.markdown:
        lines = [
            f"**pass@1 = {passes}/{expected} = {accuracy:.2%}**",
            "",
            f"- 95% CI (bootstrap, seed {BOOTSTRAP_SEED}, {BOOTSTRAP_DRAWS:,} draws): "
            f"[{boot_low:.2%}, {boot_high:.2%}]",
            f"- 95% CI (Clopper–Pearson exact): [{exact_low:.2%}, {exact_high:.2%}]",
            f"- spend: ${usd:.2f} (reported by {len(usd_reported)}/{len(rows)} trials) "
            f"· tokens: {tokens_in:,} in / {tokens_out:,} out",
            f"- trials with an exception: {errored} · trials with zero tool calls: "
            f"{zero_tool} (of {len(tools_reported)}/{len(rows)} reporting)",
            "",
            "| task | reward | tool calls | USD |",
            "|---|---:|---:|---:|",
        ]
        for row in sorted(rows, key=lambda item: str(item["task_name"])):
            reward = row.get("accuracy")
            reward = "—" if reward is None else f"{float(reward):.1f}"
            lines.append(
                f"| `{row['task_name']}` | {reward} | {row.get('tool_calls') or 0} | "
                f"{(row.get('usd') or 0):.4f} |"
            )
        Path(args.markdown).write_text("\n".join(lines) + "\n")
        print(f"\nwrote {args.markdown}", file=sys.stderr)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)

    extract = sub.add_parser("extract", help="flatten a Harbor job dir into trials.jsonl")
    extract.add_argument("job_dir")
    extract.add_argument("-o", "--output", required=True)
    extract.set_defaults(func=cmd_extract)

    score = sub.add_parser("score", help="score trials.jsonl")
    score.add_argument("trials")
    score.add_argument("--tasks", type=int, required=True, help="preregistered denominator")
    score.add_argument("--markdown", help="also write a per-task markdown table here")
    score.set_defaults(func=cmd_score)

    args = parser.parse_args()
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
