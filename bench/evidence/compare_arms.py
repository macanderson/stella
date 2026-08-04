#!/usr/bin/env python3
"""Compare a witness-off run against a witness-on run over the same tasks.

`score_dev_baseline.py` answers "what did this run score". This answers the
question #1284 exists for: **does an independent checker earn its cost?**

Stella decides whether finished work is correct on a ladder. The strongest rung
is the authored witness — a *second* model writes a test the worker must pass,
so the model that did the work does not write its own exam. The weakest is the
model judge, which reads the work and gives an opinion. Every Terminal-Bench
number published before #1007 ran with the witness rung structurally off,
because the posture pinned one model for every role and Stella refuses to let a
worker author its own witness. Those numbers are a lower bound on the ladder,
not a measurement of it.

Turning the rung on is one environment variable (`STELLA_WITNESS_AUTHOR_MODEL`,
#1007) over a workspace that now has a git baseline to compare against (#1225).
What was missing is the arithmetic that turns two runs into an answer, and the
evidence to run it over: the per-trial verdict and witness observations lived
only in the multi-gigabyte Harbor job tree, which is never committed, so the
comparison was computable only while the run was still on the disk that made
it. `score_dev_baseline.py extract` now carries those fields into
`trials.jsonl`, and this script reads two of them.

Three properties, in the order they can invalidate a conclusion:

1. **Identity.** Two runs of different SUTs, or two runs of the same arm, are
   not an experiment. Mismatches refuse rather than produce a number.
2. **The tier actually fired.** A posture that *declares* witness-on is not a
   run that *authored* witnesses — #1147 is exactly that failure, where an
   unresolvable author left the control arm executing under a treatment-arm
   digest. A treatment arm that authored nothing measures nothing, and is
   refused with that stated.
3. **Both outcomes, paired by task.** How many more tasks pass, and how many
   wrong "this passed" calls disappear. The second is the point of the rung and
   is invisible to the score: a trial that fails the task and knows it costs the
   same reward as one that fails and reports success, but only the second one
   lies to a caller who then ships it.

Usage::

    python compare_arms.py <control-trials.jsonl> <treatment-trials.jsonl> \\
        --tasks 89 [--markdown out.md]

Exit status is 0 only for a comparison that is allowed to mean something. The
report is printed either way — a refusal with the numbers withheld is a refusal
nobody can check.
"""

from __future__ import annotations

import argparse
import json
import math
import random
import sys
from datetime import datetime
from pathlib import Path
from typing import Any

# Fixed by the preregistration in `witness-ab/README.md` before any paid call.
# Changing any of them changes the answer and invalidates the comparison.
BOOTSTRAP_SEED = 20260803
BOOTSTRAP_DRAWS = 50_000
SIGNIFICANCE_ALPHA = 0.05
# The witness arm buys a second model's opinion on every candidate, so it is
# expected to cost more; the question is how much more is worth paying. Half
# again is the preregistered ceiling.
COST_RATIO_CEILING = 1.5

CONTROL_ARM = "witness-off"
TREATMENT_ARM = "witness-on"


def _dict(value: Any) -> dict[str, Any]:
    return value if isinstance(value, dict) else {}


def _number(value: Any) -> float | None:
    return (
        float(value)
        if isinstance(value, (int, float)) and not isinstance(value, bool)
        else None
    )


def _bool(value: Any) -> bool | None:
    return value if isinstance(value, bool) else None


def _passed(row: dict[str, Any]) -> bool:
    """The external grader's answer, under the same rule as the scorer.

    The verifier's reward is authoritative and the agent never sees it; a trial
    that produced no reward at all — errored, timed out with nothing on disk,
    never ran — is a failure and stays in the denominator.
    """
    accuracy = _number(row.get("accuracy"))
    return accuracy is not None and accuracy >= 1.0


def _seconds(row: dict[str, Any]) -> float | None:
    """Agent wall clock for one trial, or ``None`` when it cannot be known."""
    started, finished = row.get("agent_started_at"), row.get("agent_finished_at")
    if not isinstance(started, str) or not isinstance(finished, str):
        return None
    try:
        begin = datetime.fromisoformat(started.replace("Z", "+00:00"))
        end = datetime.fromisoformat(finished.replace("Z", "+00:00"))
    except ValueError:
        return None
    return (end - begin).total_seconds()


def load_arm(path: Path) -> dict[str, dict[str, Any]]:
    """Read one arm's ``trials.jsonl`` into a task-keyed map.

    A task appearing twice is a resume or a rerun, both forbidden, and both
    would silently double-weight that task in the pairing below.
    """
    rows: dict[str, dict[str, Any]] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        row = json.loads(line)
        name = row.get("task_name")
        if not isinstance(name, str) or not name:
            raise ValueError(f"{path}: a trial row has no task_name")
        if name in rows:
            raise ValueError(f"{path}: task {name!r} appears in more than one row")
        rows[name] = row
    return rows


def _uniform(rows: list[dict[str, Any]], field: str) -> Any:
    """The one value every row agrees on, or a stated disagreement."""
    seen = sorted({json.dumps(row.get(field), sort_keys=True) for row in rows})
    if not seen:
        return None
    if len(seen) == 1:
        return json.loads(seen[0])
    return {"DISAGREEMENT": [json.loads(value) for value in seen]}


def check_identity(
    control: dict[str, dict[str, Any]],
    treatment: dict[str, dict[str, Any]],
) -> tuple[dict[str, Any], list[str]]:
    """Confirm the two files are the two arms of one experiment.

    Every check here fails closed. The alternative is a plausible number
    computed over a pair of runs that never shared a SUT, a dataset or an arm —
    and a plausible number is the failure mode this whole directory exists to
    prevent.
    """
    refusals: list[str] = []
    control_rows, treatment_rows = list(control.values()), list(treatment.values())
    identity: dict[str, Any] = {}

    for label, rows, expected in (
        ("control", control_rows, CONTROL_ARM),
        ("treatment", treatment_rows, TREATMENT_ARM),
    ):
        if not rows:
            refusals.append(f"{label} arm has no trials")
            continue
        arms = {row.get("assurance_arm") for row in rows}
        identity[f"{label}_arm"] = _uniform(rows, "assurance_arm")
        identity[f"{label}_witness_author_model"] = _uniform(
            rows, "witness_author_model"
        )
        identity[f"{label}_engine_posture_sha256"] = _uniform(
            rows, "engine_posture_sha256"
        )
        if None in arms:
            # v1 evidence predates the assurance block entirely (#1007). It
            # cannot state which arm it ran, so it cannot be one.
            refusals.append(
                f"{label} arm: {sum(1 for r in rows if r.get('assurance_arm') is None)}"
                " trial(s) do not record an assurance arm — pre-#1007 evidence"
                " cannot be an arm of this experiment"
            )
        if arms - {None} != {expected}:
            refusals.append(
                f"{label} arm should be {expected!r}, trials say "
                f"{sorted(str(a) for a in arms)}"
            )

    # One SUT, or it is two experiments. Recorded per trial by the adapter, so
    # this reads what ran rather than what the current checkout would produce.
    for field in ("source_commit", "binary_sha256"):
        both = _uniform(control_rows + treatment_rows, field)
        identity[field] = both
        if isinstance(both, dict) and "DISAGREEMENT" in both:
            refusals.append(
                f"the two arms did not run the same {field}: {both['DISAGREEMENT']}"
            )

    if control and treatment:
        only_control = sorted(set(control) - set(treatment))
        only_treatment = sorted(set(treatment) - set(control))
        identity["tasks_only_in_control"] = only_control
        identity["tasks_only_in_treatment"] = only_treatment
        if only_control or only_treatment:
            # Not a refusal: a trial that never produced a row scores zero under
            # the fixed denominator, which is the same rule the scorer applies.
            # It is disclosed because an unpaired task is the one input a reader
            # cannot recover from the totals.
            identity["unpaired_tasks_note"] = (
                "a task absent from an arm scores 0 in that arm, as it does in "
                "`score_dev_baseline.py`; the pairing below counts it as a "
                "failure rather than dropping it"
            )
    return identity, refusals


def check_tier_fired(treatment: dict[str, dict[str, Any]]) -> tuple[dict[str, Any], list[str]]:
    """Did the treatment arm actually author witnesses, or only declare them?

    #1147: a witness-arm posture whose author failed model validation dropped
    the judge pin and executed the control arm — under a witness-arm digest.
    The posture cannot answer this question about itself; only the run's own
    proof stream can, and `witness_authored_state` is that stream folded into a
    field (#1007).
    """
    states: dict[str, int] = {}
    authored_total = 0
    for row in treatment.values():
        state = row.get("witness_authored_state") or "not_reported"
        states[str(state)] = states.get(str(state), 0) + 1
        authored_total += int(row.get("witness_authored_count") or 0)

    fired = states.get("authored", 0)
    summary = {
        "trials_by_witness_state": states,
        "trials_with_an_authored_witness": fired,
        "witnesses_authored_total": authored_total,
    }
    refusals: list[str] = []
    if not treatment:
        return summary, refusals
    if fired == 0:
        refusals.append(
            "the treatment arm authored no witness on any task: its trials report "
            f"{states} — a run that declares the rung without exercising it "
            "measures the control arm under a treatment-arm digest (#1147)"
        )
    return summary, refusals


def mcnemar_exact_p(gained: int, lost: int) -> float | None:
    """Two-sided exact McNemar over the discordant pairs.

    The concordant pairs carry no information about a *change*, so the test is a
    fair-coin binomial on the tasks that moved. Exact rather than the chi-square
    approximation because the discordant count in a run this size is routinely
    under ten, which is exactly where the approximation stops being one.
    """
    n = gained + lost
    if n == 0:
        return None
    k = min(gained, lost)
    tail = sum(math.comb(n, i) for i in range(k + 1)) / (2**n)
    return min(1.0, 2 * tail)


def bootstrap_delta(pairs: list[tuple[float, float]]) -> tuple[float, float]:
    """Percentile bootstrap over per-task paired differences.

    Resampling *tasks*, not trials, and keeping each task's pair together: the
    two arms ran the same task set, and a bootstrap that broke the pairing would
    report the interval of an unpaired comparison the run did not perform.
    """
    rng = random.Random(BOOTSTRAP_SEED)
    n = len(pairs)
    means = []
    for _ in range(BOOTSTRAP_DRAWS):
        total = 0.0
        for _ in range(n):
            treatment, control = pairs[rng.randrange(n)]
            total += treatment - control
        means.append(total / n)
    means.sort()
    return means[int(0.025 * BOOTSTRAP_DRAWS)], means[int(0.975 * BOOTSTRAP_DRAWS) - 1]


def self_verdict_stats(rows: list[dict[str, Any]]) -> dict[str, Any]:
    """How often Stella's own verdict matched the external grader.

    Two denominators, because they answer different questions. `reported_by` is
    every trial that closed its ladder with a verdict at all — a trial killed
    mid-turn made no claim and must not be scored as a wrong one. Inside that,
    `model_judge_only` restricts to verdicts a model *opined*
    (`deterministic: false`); a flip-oracle verdict is a different instrument
    and averaging the two hides the one #1284 is about.
    """

    def summarize(subset: list[dict[str, Any]]) -> dict[str, Any]:
        agree = sum(1 for row in subset if _bool(row["self_verdict_passed"]) == _passed(row))
        false_pass = [
            row["task_name"]
            for row in subset
            if row["self_verdict_passed"] is True and not _passed(row)
        ]
        false_fail = [
            row["task_name"]
            for row in subset
            if row["self_verdict_passed"] is False and _passed(row)
        ]
        return {
            "reported_by": len(subset),
            "agreements": agree,
            "agreement_rate": (agree / len(subset)) if subset else None,
            "false_pass": len(false_pass),
            "false_pass_tasks": sorted(false_pass),
            "false_fail": len(false_fail),
            "false_fail_tasks": sorted(false_fail),
        }

    reported = [row for row in rows if _bool(row.get("self_verdict_passed")) is not None]
    judged = [row for row in reported if row.get("self_verdict_deterministic") is False]
    stats = summarize(reported)
    stats["silent_trials"] = len(rows) - len(reported)
    stats["model_judge_only"] = summarize(judged)
    return stats


def _arm_totals(rows: list[dict[str, Any]]) -> dict[str, Any]:
    """Spend and time, each beside the number of trials that reported it.

    Absent and zero are different answers — the discipline `score.json` already
    applies to `usd_total`. A ratio built over two partially-reported totals is
    the specific mistake this shape prevents.
    """
    usd = [value for row in rows if (value := _number(row.get("usd"))) is not None]
    wall = [value for row in rows if (value := _seconds(row)) is not None]
    return {
        "trials": len(rows),
        "usd_total": round(sum(usd), 4),
        "usd_reported_by": len(usd),
        "prompt_tokens": sum(row.get("n_input_tokens") or 0 for row in rows),
        "completion_tokens": sum(row.get("n_output_tokens") or 0 for row in rows),
        "agent_wall_s": round(sum(wall), 1),
        "agent_wall_reported_by": len(wall),
        "trials_with_an_exception": sum(1 for row in rows if row.get("exception_type")),
        "trials_with_an_agent_timeout": sum(
            1 for row in rows if row.get("exception_type") == "AgentTimeoutError"
        ),
    }


def _cost(control: list[dict[str, Any]], treatment: list[dict[str, Any]], gained_net: int) -> dict[str, Any]:
    left, right = _arm_totals(control), _arm_totals(treatment)
    complete = (
        left["usd_reported_by"] == left["trials"] > 0
        and right["usd_reported_by"] == right["trials"] > 0
    )
    ratio = (
        round(right["usd_total"] / left["usd_total"], 4)
        if complete and left["usd_total"] > 0
        else None
    )
    per_pass = (
        round((right["usd_total"] - left["usd_total"]) / gained_net, 4)
        if ratio is not None and gained_net > 0
        else None
    )
    return {
        "control": left,
        "treatment": right,
        "usd_ratio": ratio,
        "usd_ratio_note": (
            None
            if complete
            else "withheld: at least one arm reported spend on only part of its "
            "trials, and a ratio of two partial sums is not a cost comparison"
        ),
        "usd_per_additional_task_passed": per_pass,
        # Until #960 lands, Stella's headless process does not exit after its
        # turn, so Harbor kills it at the agent timeout: those trials burn the
        # full timeout regardless of how long the work took. Wall clock is
        # reported because it is what the operator waits through, but it is not
        # a measurement of the witness rung's time cost while this is true.
        "wall_clock_note": (
            "wall clock is timeout-bound on trials that raised AgentTimeoutError "
            "(#960): "
            f"{left['trials_with_an_agent_timeout']}/{left['trials']} control, "
            f"{right['trials_with_an_agent_timeout']}/{right['trials']} treatment"
        ),
    }


def decide(primary: dict[str, Any], verdicts: dict[str, Any], cost: dict[str, Any]) -> dict[str, Any]:
    """Apply the preregistered rule. No thresholds are chosen after the data.

    Stated in `bench/evidence/witness-ab/README.md` and fixed before the first
    paid call, in this order:

    * **Reject** if the arm loses more tasks than it gains at p < 0.05, or if it
      increases wrong "this passed" calls. Either one makes the rung worse than
      what already shipped.
    * **Adopt** if it gains more tasks than it loses, that gain is either
      significant or accompanied by fewer wrong "passed" calls, and it costs at
      most `COST_RATIO_CEILING`x the control arm.
    * **Inconclusive** otherwise — which changes nothing, and says so.
    """
    gained, lost = primary["gained"], primary["lost"]
    p_value = primary["mcnemar_exact_p"]
    false_pass_delta = verdicts["false_pass_delta"]
    ratio = cost["usd_ratio"]
    reasons: list[str] = []

    if lost > gained and p_value is not None and p_value < SIGNIFICANCE_ALPHA:
        reasons.append(
            f"the witness arm lost {lost} tasks and gained {gained} "
            f"(exact McNemar p={p_value:.4f})"
        )
    if false_pass_delta is not None and false_pass_delta > 0:
        reasons.append(
            f"wrong 'this passed' calls rose by {false_pass_delta}, which is the "
            "failure the rung exists to remove"
        )
    if reasons:
        return {"verdict": "reject", "reasons": reasons}

    significant = p_value is not None and p_value < SIGNIFICANCE_ALPHA
    fewer_false_passes = false_pass_delta is not None and false_pass_delta < 0
    affordable = ratio is not None and ratio <= COST_RATIO_CEILING
    if gained > lost and (significant or fewer_false_passes) and affordable:
        return {
            "verdict": "adopt",
            "reasons": [
                f"net +{gained - lost} tasks (gained {gained}, lost {lost}"
                + (f", exact McNemar p={p_value:.4f}" if p_value is not None else "")
                + ")",
                f"wrong 'this passed' calls changed by {false_pass_delta}",
                f"spend ratio {ratio}x ≤ {COST_RATIO_CEILING}x ceiling",
            ],
        }

    unmet = []
    if gained <= lost:
        unmet.append(f"no net task gain (gained {gained}, lost {lost})")
    if not significant and not fewer_false_passes:
        unmet.append(
            "the gain is neither significant at "
            f"p<{SIGNIFICANCE_ALPHA} nor accompanied by fewer wrong 'passed' calls"
        )
    if not affordable:
        unmet.append(
            f"spend ratio {ratio} is above the {COST_RATIO_CEILING}x ceiling"
            if ratio is not None
            else "spend could not be compared (see usd_ratio_note)"
        )
    return {"verdict": "inconclusive", "reasons": unmet}


def compare(
    control: dict[str, dict[str, Any]],
    treatment: dict[str, dict[str, Any]],
    tasks: int,
) -> dict[str, Any]:
    """The whole comparison, as one JSON-serializable report."""
    identity, refusals = check_identity(control, treatment)
    tier, tier_refusals = check_tier_fired(treatment)
    refusals = refusals + tier_refusals

    universe = sorted(set(control) | set(treatment))
    missing_everywhere = tasks - len(universe)
    if missing_everywhere < 0:
        refusals.append(
            f"{len(universe)} tasks present, more than the declared denominator {tasks}"
        )
        missing_everywhere = 0

    pairs: list[tuple[float, float]] = []
    per_task: list[dict[str, Any]] = []
    both_pass = gained = lost = both_fail = 0
    for name in universe:
        left, right = control.get(name), treatment.get(name)
        control_pass = left is not None and _passed(left)
        treatment_pass = right is not None and _passed(right)
        pairs.append((float(treatment_pass), float(control_pass)))
        if control_pass and treatment_pass:
            both_pass += 1
        elif treatment_pass:
            gained += 1
        elif control_pass:
            lost += 1
        else:
            both_fail += 1
        per_task.append(
            {
                "task": name,
                "control_passed": control_pass,
                "treatment_passed": treatment_pass,
                "control_self_verdict": (left or {}).get("self_verdict_passed"),
                "treatment_self_verdict": (right or {}).get("self_verdict_passed"),
                "treatment_witness": (right or {}).get("witness_authored_state"),
                "control_usd": (left or {}).get("usd"),
                "treatment_usd": (right or {}).get("usd"),
            }
        )
    # Tasks no trial covered in either arm are failures in both, by the fixed
    # denominator, and they belong in the pairing so the interval is computed
    # over the preregistered task count rather than the tasks that reported.
    both_fail += missing_everywhere
    pairs.extend([(0.0, 0.0)] * missing_everywhere)

    control_passes = both_pass + lost
    treatment_passes = both_pass + gained
    low, high = bootstrap_delta(pairs) if pairs else (0.0, 0.0)
    primary = {
        "tasks_declared": tasks,
        "tasks_with_a_trial_in_either_arm": len(universe),
        "tasks_missing_from_both_arms": missing_everywhere,
        "control_passes": control_passes,
        "treatment_passes": treatment_passes,
        "control_accuracy": control_passes / tasks if tasks else None,
        "treatment_accuracy": treatment_passes / tasks if tasks else None,
        "delta_passes": treatment_passes - control_passes,
        "both_pass": both_pass,
        "gained": gained,
        "lost": lost,
        "both_fail": both_fail,
        "mcnemar_exact_p": mcnemar_exact_p(gained, lost),
        "ci95_bootstrap_delta_accuracy": {
            "low": low,
            "high": high,
            "seed": BOOTSTRAP_SEED,
            "draws": BOOTSTRAP_DRAWS,
            "method": "percentile over per-task paired differences",
        },
    }

    control_stats = self_verdict_stats(list(control.values()))
    treatment_stats = self_verdict_stats(list(treatment.values()))
    control_false = set(control_stats["false_pass_tasks"])
    treatment_false = set(treatment_stats["false_pass_tasks"])
    verdicts = {
        "control": control_stats,
        "treatment": treatment_stats,
        "false_pass_delta": treatment_stats["false_pass"] - control_stats["false_pass"],
        # Named tasks, not just counts: "which five did it stop lying about" is
        # the reviewable form of the claim, and the counts can cancel out.
        "false_passes_fixed": sorted(control_false - treatment_false),
        "false_passes_introduced": sorted(treatment_false - control_false),
    }

    cost = _cost(list(control.values()), list(treatment.values()), gained - lost)
    decision = (
        {"verdict": "refused", "reasons": refusals}
        if refusals
        else decide(primary, verdicts, cost)
    )
    return {
        "schema": "stella-witness-ab-comparison-v1",
        "comparable": not refusals,
        "refusals": refusals,
        "identity": identity,
        "witness_tier": tier,
        "primary_outcome": primary,
        "self_verdict": verdicts,
        "cost": cost,
        "decision": decision,
        "per_task": per_task,
    }


def _mark(value: Any) -> str:
    return {True: "pass", False: "fail", None: "—"}.get(value, "—")


def markdown(report: dict[str, Any]) -> str:
    primary, verdicts = report["primary_outcome"], report["self_verdict"]
    cost, decision = report["cost"], report["decision"]
    lines = [
        "# Authored witness on vs off — paired comparison",
        "",
        f"**{decision['verdict'].upper()}** — " + "; ".join(decision["reasons"] or ["—"]),
        "",
        f"- tasks passed: control {primary['control_passes']}/{primary['tasks_declared']} "
        f"→ witness {primary['treatment_passes']}/{primary['tasks_declared']} "
        f"({primary['delta_passes']:+d})",
        f"- paired: gained {primary['gained']}, lost {primary['lost']}, "
        f"both passed {primary['both_pass']}, both failed {primary['both_fail']}"
        + (
            f", exact McNemar p={primary['mcnemar_exact_p']:.4f}"
            if primary["mcnemar_exact_p"] is not None
            else ", no discordant pairs"
        ),
        f"- wrong \"this passed\" calls: control {verdicts['control']['false_pass']} "
        f"→ witness {verdicts['treatment']['false_pass']} "
        f"({verdicts['false_pass_delta']:+d})",
        f"- spend: ${cost['control']['usd_total']:.2f} → "
        f"${cost['treatment']['usd_total']:.2f}"
        + (f" ({cost['usd_ratio']}x)" if cost["usd_ratio"] is not None else "")
        + (
            f" · ${cost['usd_per_additional_task_passed']:.2f} per additional task passed"
            if cost["usd_per_additional_task_passed"] is not None
            else ""
        ),
        f"- witness tier fired on {report['witness_tier']['trials_with_an_authored_witness']} "
        f"treatment trials ({report['witness_tier']['witnesses_authored_total']} witnesses authored)",
        "",
    ]
    if report["refusals"]:
        lines += ["> **Refused.** " + " · ".join(report["refusals"]), ""]
    lines += [
        "| task | control | witness | control says | witness says | tier |",
        "|---|---|---|---|---|---|",
    ]
    for row in report["per_task"]:
        lines.append(
            f"| `{row['task']}` | {_mark(row['control_passed'])} | "
            f"{_mark(row['treatment_passed'])} | {_mark(row['control_self_verdict'])} | "
            f"{_mark(row['treatment_self_verdict'])} | {row['treatment_witness'] or '—'} |"
        )
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("control", help="witness-off trials.jsonl")
    parser.add_argument("treatment", help="witness-on trials.jsonl")
    parser.add_argument(
        "--tasks", type=int, required=True, help="preregistered denominator"
    )
    parser.add_argument("--markdown", help="also write the per-task table here")
    args = parser.parse_args(argv)

    try:
        control = load_arm(Path(args.control))
        treatment = load_arm(Path(args.treatment))
    except (OSError, ValueError) as exc:
        print(f"FATAL: {exc}", file=sys.stderr)
        return 1

    report = compare(control, treatment, args.tasks)
    print(json.dumps(report, indent=2))
    if args.markdown:
        Path(args.markdown).write_text(markdown(report), encoding="utf-8")
        print(f"\nwrote {args.markdown}", file=sys.stderr)
    if report["refusals"]:
        for refusal in report["refusals"]:
            print(f"REFUSED: {refusal}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
