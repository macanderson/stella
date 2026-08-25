#!/usr/bin/env python3
"""Census: what one compaction pass-0 (retention) firing costs and buys (#4452).

`RETENTION_TRIGGER_BUDGET_DIVISOR` in `crates/stella-core/src/compaction.rs`
gates the retention pass on how far into its budget a conversation is. #4452
asked whether half is the right fraction, and required the answer to be a
number over recorded runs rather than an argument. This is that census.

It reads `~/.stella/sessions/*/journal.jsonl` -- the durable session journals,
not `store.db`, because the maintainer's project store is corrupt
(`pragma quick_check` reports btreeInitPage error 11 on tree 45).

For every `compaction` event that aged something and evicted, deduped,
superseded or summarized nothing -- a pure pass-0 firing -- it records
`before_tokens`, `after_tokens`, the fraction of `effective_budget_tokens` the
conversation occupied, the observed cached fraction on the worker call either
side of the firing, and how many further worker calls the turn ran.

The trade, in uncached-equivalent input tokens, where `r` is what a cache read
costs as a fraction of an uncached token (0.1 at Anthropic, higher elsewhere):

    fire:     A billed uncached on the next call, then r*(A + growth) after
    not fire: r*B on the next call, then r*(B + growth) after

so the net of firing is `A - r*B - r*R*(N-1)` for `R = B - A` reclaimed and
`N` further worker calls. Negative is a saving. A firing pays for itself only
once the turn runs far enough past it to amortize the prefix invalidation.

The census is first-order and bounds the effect rather than settling it.
Suppressing a firing changes the transcript the next one sees, and a
conversation left un-aged can reach the budget passes, which are lossier. A
replay through `age_stale_tool_results` would settle it.

Usage: python3 scripts/retention-census.py
"""

import json
import os
import statistics
import sys

SESSIONS = os.path.expanduser("~/.stella/sessions")
TURN_END = {"complete", "turn_complete", "run_complete"}


def journal_events(path):
    """Yield each recorded `AgentEvent`, with a marker where a turn starts."""
    with open(path, "r", errors="replace") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                continue
            if rec.get("type") == "prompt_started":
                yield {"type": "__turn_boundary__"}
            elif rec.get("type") == "event":
                ev = rec.get("event")
                if isinstance(ev, dict):
                    yield ev


def collect():
    firings = []
    sessions_seen = 0
    sessions_with_firing = 0
    for name in sorted(os.listdir(SESSIONS)):
        path = os.path.join(SESSIONS, name, "journal.jsonl")
        if not os.path.isfile(path):
            continue
        sessions_seen += 1
        events = list(journal_events(path))
        got = False
        for i, ev in enumerate(events):
            if ev.get("type") != "compaction" or not ev.get("aged"):
                continue
            if any(ev.get(k) for k in ("evicted", "deduped", "superseded", "summarized")):
                continue
            before = ev.get("before_tokens") or 0
            after = ev.get("after_tokens") or 0
            budget = ev.get("effective_budget_tokens") or 0
            if before <= 0 or budget <= 0:
                continue

            prev_hit = None
            for j in range(i - 1, -1, -1):
                e = events[j]
                if e.get("type") == "__turn_boundary__":
                    break
                if e.get("type") == "step_usage" and e.get("role") == "worker":
                    tot = (e.get("input_tokens") or 0) + (e.get("cached_input_tokens") or 0)
                    if tot > 0:
                        prev_hit = (e.get("cached_input_tokens") or 0) / tot
                    break

            next_hit = None
            remaining = 0
            for j in range(i + 1, len(events)):
                e = events[j]
                t = e.get("type")
                if t == "__turn_boundary__" or t in TURN_END:
                    break
                if t == "step_usage" and e.get("role") == "worker":
                    remaining += 1
                    if next_hit is None:
                        tot = (e.get("input_tokens") or 0) + (e.get("cached_input_tokens") or 0)
                        next_hit = ((e.get("cached_input_tokens") or 0) / tot) if tot > 0 else None

            firings.append(
                {
                    "session": name,
                    "before": before,
                    "after": after,
                    "reclaimed": before - after,
                    "fraction": before / budget,
                    "prev_hit": prev_hit,
                    "next_hit": next_hit,
                    "remaining": remaining,
                }
            )
            got = True
        if got:
            sessions_with_firing += 1
    return firings, sessions_seen, sessions_with_firing


def pct(xs, p):
    xs = sorted(xs)
    if not xs:
        return float("nan")
    k = (len(xs) - 1) * p
    lo, hi = int(k), min(int(k) + 1, len(xs) - 1)
    return xs[lo] + (xs[hi] - xs[lo]) * (k - lo)


def net(f, r):
    """Uncached-equivalent input tokens of firing minus not firing."""
    n = f["remaining"]
    if n == 0:
        return f["after"] - r * f["before"]
    return f["after"] - r * f["before"] - r * f["reclaimed"] * (n - 1)


def main():
    firings, sessions_seen, sessions_with = collect()
    print("sessions scanned: %d  with a pass-0 firing: %d" % (sessions_seen, sessions_with))
    print("pure pass-0 firings (aged > 0, nothing else): %d" % len(firings))
    if not firings:
        return 0

    frac = [f["fraction"] for f in firings]
    print(
        "budget fraction at firing: median %.3f  p25 %.3f  p75 %.3f  p90 %.3f"
        % (pct(frac, 0.5), pct(frac, 0.25), pct(frac, 0.75), pct(frac, 0.9))
    )
    for d in (2, 3, 4, 6):
        kept = [f for f in firings if f["fraction"] >= 1.0 / d]
        print(
            "divisor %d: keeps %d/%d firings (%.1f%% suppressed), retains %d of %d reclaimed tokens"
            % (
                d,
                len(kept),
                len(firings),
                100 * (1 - len(kept) / len(firings)),
                sum(f["reclaimed"] for f in kept),
                sum(f["reclaimed"] for f in firings),
            )
        )

    rem = [f["remaining"] for f in firings]
    print(
        "worker calls remaining in the turn after a firing: median %.0f  p75 %.0f  p90 %.0f  max %d  mean %.1f"
        % (pct(rem, 0.5), pct(rem, 0.75), pct(rem, 0.9), max(rem), statistics.mean(rem))
    )
    ph = [f["prev_hit"] for f in firings if f["prev_hit"] is not None]
    nh = [f["next_hit"] for f in firings if f["next_hit"] is not None]
    if ph:
        print("cached fraction on the call BEFORE a firing: median %.3f  (n=%d)" % (pct(ph, 0.5), len(ph)))
    if nh:
        print("cached fraction on the call AFTER  a firing: median %.3f  (n=%d)" % (pct(nh, 0.5), len(nh)))

    print()
    print("net billed input (uncached-equivalent tokens), firing minus not firing;")
    print("negative is a saving. Summed over the firings each gate admits:")
    for r in (0.1, 0.25, 0.5):
        row = ["r=%.2f" % r, "nogate %+d" % round(sum(net(f, r) for f in firings))]
        for d in (2, 3, 4, 6):
            kept = [f for f in firings if f["fraction"] >= 1.0 / d]
            row.append("d=%d %+d" % (d, round(sum(net(f, r) for f in kept))))
        print("  " + "  ".join(row))

    print()
    print("does the budget fraction predict which firings profit? (r=0.10)")
    prof = [f for f in firings if net(f, 0.10) < 0]
    unprof = [f for f in firings if net(f, 0.10) >= 0]
    print("  profitable firings: %d of %d (%.1f%%)" % (len(prof), len(firings), 100 * len(prof) / len(firings)))
    if prof and unprof:
        print(
            "  median budget fraction, profitable: %.3f   unprofitable: %.3f"
            % (pct([f["fraction"] for f in prof], 0.5), pct([f["fraction"] for f in unprof], 0.5))
        )
        for d in (2, 3, 4):
            kept = [f for f in firings if f["fraction"] >= 1.0 / d]
            tp = sum(1 for f in kept if net(f, 0.10) < 0)
            print(
                "  divisor %d admits %d firings, %d profitable (precision %.2f, recall %.2f)"
                % (d, len(kept), tp, tp / len(kept) if kept else 0.0, tp / len(prof))
            )
        print(
            "  median remaining worker calls, profitable: %.0f   unprofitable: %.0f"
            % (pct([f["remaining"] for f in prof], 0.5), pct([f["remaining"] for f in unprof], 0.5))
        )

    print()
    print("a gate keyed on the reclaim ratio (reclaimed/after) instead, r=0.10.")
    print("The ratio is one of the two terms in the break-even expression, so it is")
    print("a directly relevant key rather than a correlation found by search; the")
    print("other term, how many calls the turn has left, is unknowable at firing.")
    ratios = [f["reclaimed"] / f["after"] for f in firings if f["after"] > 0]
    print(
        "  reclaim ratio: median %.3f  p25 %.3f  p75 %.3f  p90 %.3f"
        % (pct(ratios, 0.5), pct(ratios, 0.25), pct(ratios, 0.75), pct(ratios, 0.9))
    )
    for t in (0.05, 0.10, 0.15, 0.25):
        kept = [f for f in firings if f["after"] > 0 and f["reclaimed"] / f["after"] >= t]
        if not kept or not prof:
            continue
        tp = sum(1 for f in kept if net(f, 0.10) < 0)
        print(
            "  ratio >= %.2f admits %d firings, %d profitable (precision %.2f, recall %.2f), net %+d"
            % (t, len(kept), tp, tp / len(kept), tp / len(prof), round(sum(net(f, 0.10) for f in kept)))
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
