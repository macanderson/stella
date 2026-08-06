#!/usr/bin/env python3
"""Recompute every number in this directory's post-mortem from the committed
head-to-head table.

The input is the same per-task JSON the public docs page renders
(`docs/benchmarks/terminal-bench-2-1-glm-5-2.json`): one row per task,
one cell per arm, produced by the live-feed reducer in
`bench/harbor_adapter/stella_harbor/live_feed.py`. That reducer defines the
`cap_hits` metric this post-mortem is about: a `step_usage` event with
`output_tokens >= 16384` **and zero tool calls** — a step that spent a huge
output budget and produced no action. Under the frozen 64000-token posture a
step can sit anywhere in [16384, 64000] and still count, so the metric reads
"large-output zero-action step", of which a literal cap truncation is the
worst case.

Stdlib only, no arguments; run it from anywhere:

    python3 bench/evidence/glm52-h2h-postmortem/analyze.py

Every figure quoted in README.md is printed here, labelled with the section
that quotes it. If this script and the README disagree, the README is wrong —
that is the point of committing the script (#909's rule: a number whose
runner lives in someone's scratch directory is not reproducible).
"""

from __future__ import annotations

import json
import statistics
from pathlib import Path

CAP_HIT_MIN_OUTPUT = 16384  # mirrors live_feed._CAP_HIT_MIN_OUTPUT

DATA = (
    Path(__file__).resolve().parents[3]
    / "docs"
    / "benchmarks"
    / "terminal-bench-2-1-glm-5-2.json"
)


def fit_context_growth(rows: list[dict], arm: str) -> tuple[float, float]:
    """Least-squares fit of per-task mean input-per-step against step count.

    The slope is the marginal growth of the *average* context per additional
    step; the marginal growth of the standing context itself is ~2x the slope
    (for context C(k) ~ a + b*k, the mean over k steps grows at b/2 per step).
    """
    xs = [r[arm]["steps"] for r in rows if r[arm]["steps"] > 0]
    ys = [r[arm]["in_tokens"] / r[arm]["steps"] for r in rows if r[arm]["steps"] > 0]
    n = len(xs)
    mx, my = sum(xs) / n, sum(ys) / n
    slope = sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / sum(
        (x - mx) ** 2 for x in xs
    )
    return my - slope * mx, slope


def main() -> None:
    d = json.loads(DATA.read_text())
    rows = d["rows"]
    s_in = sum(r["stella"]["in_tokens"] for r in rows)
    c_in = sum(r["claude"]["in_tokens"] for r in rows)
    s_steps = sum(r["stella"]["steps"] for r in rows)
    c_steps = sum(r["claude"]["steps"] for r in rows)
    s_out = sum(r["stella"]["out_tokens"] for r in rows)

    print(f"tasks: {len(rows)}")
    print("== headline (README §1) ==")
    print(
        f"pass: stella {sum(1 for r in rows if r['stella']['pass'])}/89, "
        f"claude {sum(1 for r in rows if r['claude']['pass'])}/89"
    )
    print(
        f"input tokens: stella {s_in:,} claude {c_in:,} ratio {s_in / c_in:.2f}x"
    )
    # `cap_hits` is Stella-only telemetry, read from stella-events.jsonl, which
    # the Claude Code arm does not write. Its per-row value is null — no source,
    # not a measured zero — so there is no cross-arm comparison to print here
    # (#1227). Summing it as if null meant zero is the exact flattering-artifact
    # the data file was corrected to prevent.
    print(
        f"cap hits: stella {sum(r['stella']['cap_hits'] for r in rows)} "
        f"({d['totals']['stella']['cap_hits_source']}), "
        f"claude: no source — the arm does not emit this telemetry"
    )

    print("\n== cap-hit outcome split (README §2.1) ==")
    with_cap = [r for r in rows if r["stella"]["cap_hits"] > 0]
    without = [r for r in rows if r["stella"]["cap_hits"] == 0]
    print(
        f"tasks with >=1 cap hit: {len(with_cap)}; "
        f"pass rate with: {sum(1 for r in with_cap if r['stella']['pass'])}/{len(with_cap)}"
        f" = {sum(1 for r in with_cap if r['stella']['pass']) / len(with_cap):.1%}; "
        f"without: {sum(1 for r in without if r['stella']['pass'])}/{len(without)}"
        f" = {sum(1 for r in without if r['stella']['pass']) / len(without):.1%}"
    )
    print(
        f"cap hits in passing trials: "
        f"{sum(r['stella']['cap_hits'] for r in rows if r['stella']['pass'])}; "
        f"in failing trials: "
        f"{sum(r['stella']['cap_hits'] for r in rows if not r['stella']['pass'])}"
    )

    waste = sum(r["stella"]["cap_hits"] for r in rows) * CAP_HIT_MIN_OUTPUT
    print("\n== wasted output floor (README §2.2) ==")
    print(
        f"cap-hit steps burned >= {waste:,} output tokens "
        f"of {s_out:,} total = {waste / s_out:.1%} of all Stella output"
    )

    print("\n== fatal classes among the 12 cap-hit failures (README §2.3) ==")
    for r in rows:
        s = r["stella"]
        if s["pass"] or s["cap_hits"] == 0:
            continue
        share = s["cap_hits"] * CAP_HIT_MIN_OUTPUT / max(1, s["out_tokens"])
        cls = (
            "FATAL-EARLY"
            if s["steps"] < 20
            else ("CAP-HEAVY" if share > 0.5 else "late/mixed")
        )
        print(
            f"  {r['task']:35s} caps={s['cap_hits']} steps={s['steps']:3d} "
            f"cap-share>={share:5.1%}  {cls}"
        )

    print("\n== token-efficiency decomposition (README §3.1) ==")
    print(
        f"6.40x = {s_steps / c_steps:.2f}x steps "
        f"x {(s_in / s_steps) / (c_in / c_steps):.2f}x input-per-step "
        f"(stella {s_in / s_steps:,.0f}/step, claude {c_in / c_steps:,.0f}/step)"
    )

    print("\n== context growth (README §3.2) ==")
    for arm in ("stella", "claude"):
        a, b = fit_context_growth(rows, arm)
        print(f"  {arm:6s}: in/step ~= {a:,.0f} + {b:.1f} * steps")
    hi = [r for r in rows if r["stella"]["steps"] >= 100]
    lo = [r for r in rows if r["stella"]["steps"] < 50]
    print(
        f"  stella in/step: short trials {sum(r['stella']['in_tokens'] for r in lo) / sum(r['stella']['steps'] for r in lo):,.0f}, "
        f"long trials {sum(r['stella']['in_tokens'] for r in hi) / sum(r['stella']['steps'] for r in hi):,.0f}"
    )
    chi = [r for r in rows if r["claude"]["steps"] >= 100]
    clo = [r for r in rows if r["claude"]["steps"] < 50]
    print(
        f"  claude in/step: short trials {sum(r['claude']['in_tokens'] for r in clo) / sum(r['claude']['steps'] for r in clo):,.0f}, "
        f"long trials {sum(r['claude']['in_tokens'] for r in chi) / sum(r['claude']['steps'] for r in chi):,.0f}"
    )

    _, b_c = fit_context_growth(rows, "claude")
    a_c, b_c = fit_context_growth(rows, "claude")
    whatif = sum(
        (a_c + b_c * r["stella"]["steps"]) * r["stella"]["steps"] for r in rows
    )
    print("\n== counterfactual (README §3.3) ==")
    print(
        f"stella input at claude's context curve, stella's own step counts: "
        f"{whatif / 1e6:.0f}M vs actual {s_in / 1e6:.0f}M "
        f"-> {s_in / whatif:.1f}x reduction from context engineering alone"
    )
    top10 = sorted(rows, key=lambda r: -r["stella"]["in_tokens"])[:10]
    print(
        f"top-10 input tasks carry "
        f"{sum(r['stella']['in_tokens'] for r in top10) / s_in:.1%} of stella input"
    )

    print("\n== cost (README §3.4) ==")
    s_cost = sum(r["stella"]["cost_usd"] for r in rows)
    c_cost = sum(r["claude"]["cost_usd"] for r in rows)
    print(
        f"stella ${s_cost:.2f} ({1e6 * s_cost / s_in:.3f} $/M-in blended), "
        f"claude ${c_cost:.2f} ({1e6 * c_cost / c_in:.3f} $/M-in blended)"
    )

    print("\n== the 8 asymmetric failures (README §4) ==")
    for r in rows:
        s = r["stella"]
        if not s["pass"] and r["claude"]["pass"]:
            cls = (
                "ceiling/timeout death"
                if s["failure"] or s["cap_hits"]
                else "clean-fail (verifier said no)"
            )
            print(
                f"  {r['task']:28s} caps={s['cap_hits']} steps={s['steps']:3d} "
                f"wall={s['wall_s']:5d}s fail={s['failure'] or '-':26s} {cls}"
            )

    print("\n== medians, for the curious ==")
    print(
        f"median in/step: stella "
        f"{statistics.median(r['stella']['in_tokens'] / max(1, r['stella']['steps']) for r in rows):,.0f}, "
        f"claude "
        f"{statistics.median(r['claude']['in_tokens'] / max(1, r['claude']['steps']) for r in rows):,.0f}"
    )


if __name__ == "__main__":
    main()
