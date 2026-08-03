#!/usr/bin/env python3
"""Task-level head-to-head: Stella vs Claude Code, Terminal-Bench 2.1.

Every per-task column is read from a field BOTH agents emit with the same
name and meaning:

  result        result.json -> verifier_result.rewards.reward   (Harbor scores it, not the agent)
  steps/tokens/ agent/trajectory.json -> final_metrics          (ATIF, identical keys both arms)
  cost
  tools         agent/trajectory.json -> steps[].tool_calls[]   (same schema both arms)

Stella-only metadata is deliberately unused: a column that populates on one
side only is not a comparison. Tool *names* differ by toolset (stella `bash`
vs claude `Bash`/`Read`/`Edit`), so counts compare and the mix does not --
the mix is printed separately and labelled.
"""
import json, glob, os, sys, time, collections

A_JOB = sys.argv[1] if len(sys.argv) > 1 else "hh8-armA-stella"
B_JOB = sys.argv[2] if len(sys.argv) > 2 else "hh8-armB-claudecode"
# denominator: explicit 3rd arg, else the union of tasks either arm has touched.
# Hardcoding 89 silently mislabels every subset run.
DENOM = int(sys.argv[3]) if len(sys.argv) > 3 else None
A_LBL, B_LBL = "STELLA", "CLAUDE CODE"
ROOT = "/home/ubuntu/tb21/jobs"


def load(job):
    out, mix = {}, collections.Counter()
    # trial dirs exist as soon as a task starts; result.json appears only
    # when it is scored. Seed the in-flight ones so a live view shows what
    # is running now, not just what has finished.
    for d in glob.glob(f"{ROOT}/{job}/*__*/"):
        t = os.path.basename(d.rstrip("/")).rsplit("__", 1)[0]
        out.setdefault(t, dict(rew="RUN", steps=None, tools=0, tin=0, tout=0, usd=0.0))
    for rf in glob.glob(f"{ROOT}/{job}/*/result.json"):
        try:
            r = json.load(open(rf))
        except Exception:
            continue
        task = (r.get("task_name") or "").replace("terminal-bench/", "")
        if not task:
            continue
        fm, ntools = {}, 0
        try:
            t = json.load(open(os.path.join(os.path.dirname(rf), "agent", "trajectory.json")))
            fm = t.get("final_metrics") or {}
            for s in t.get("steps") or []:
                for tc in (s.get("tool_calls") or []):
                    ntools += 1
                    mix[tc.get("function_name") or "?"] += 1
        except Exception:
            pass
        ar = r.get("agent_result") or {}
        out[task] = dict(
            rew=(r.get("verifier_result") or {}).get("rewards", {}).get("reward"),
            steps=fm.get("total_steps"),
            tools=ntools,
            tin=fm.get("total_prompt_tokens", ar.get("n_input_tokens")) or 0,
            tout=fm.get("total_completion_tokens", ar.get("n_output_tokens")) or 0,
            usd=fm.get("total_cost_usd", ar.get("cost_usd")) or 0.0,
        )
    return out, mix


def res(r):
    if r == "RUN":
        return " .. "
    return "PASS" if r == 1.0 else "fail" if r == 0.0 else "----" if r is None else "part"


def cell(d):
    if not d:
        return f"{'·':^4} {'':>5} {'':>5} {'':>7} {'':>6} {'':>6}"
    return (f"{res(d['rew']):^4} {str(d['steps'] or '-'):>5} {d['tools']:>5} "
            f"{d['tin']:>7,} {d['tout']:>6,} {d['usd']:>6.2f}")


HDR = f"{'res':^4} {'steps':>5} {'tools':>5} {'tok in':>7} {'tokout':>6} {'usd':>6}"
W = 34 + 2 * 37 + 4

while True:
    A, amix = load(A_JOB)
    B, bmix = load(B_JOB)
    os.system("clear")
    DEN = DENOM or max(len(set(A) | set(B)), 1)
    ap = sum(1 for d in A.values() if d["rew"] == 1.0)
    bp = sum(1 for d in B.values() if d["rew"] == 1.0)

    print(f"{A_LBL}  vs  {B_LBL}   ·   Terminal-Bench 2.1 ({DEN} tasks, 1 attempt)   ·   glm-5.2 · effort max · thinking on · NO budget cap")
    print("prereg: github.com/macanderson/stella/issues/1013   ·   same host, same tasks, same settings — the endpoint is the only difference")
    print("=" * W)
    nA = sum(1 for d in A.values() if d["rew"] != "RUN")
    nB = sum(1 for d in B.values() if d["rew"] != "RUN")
    rA = len(A) - nA
    rB = len(B) - nB
    print(f"  {A_LBL:<12} {nA:>2}/{DEN} scored  {rA} running  pass {ap:>2}   ${sum(d['usd'] for d in A.values()):>6.2f}"
          f"      |   {B_LBL:<12} {nB:>2}/{DEN} scored  {rB} running  pass {bp:>2}   ${sum(d['usd'] for d in B.values()):>6.2f}")
    shared = sorted(t for t in set(A) & set(B)
                if A[t]["rew"] != "RUN" and B[t]["rew"] != "RUN")
    if shared:
        aw = [t for t in shared if A[t]["rew"] == 1.0 and B[t]["rew"] != 1.0]
        bw = [t for t in shared if B[t]["rew"] == 1.0 and A[t]["rew"] != 1.0]
        bo = sum(1 for t in shared if A[t]["rew"] == 1.0 and B[t]["rew"] == 1.0)
        nn = sum(1 for t in shared if A[t]["rew"] != 1.0 and B[t]["rew"] != 1.0)
        print(f"  PAIRED n={len(shared)}   both pass {bo}   both fail {nn}   "
              f"stella-only {len(aw)}   claude-only {len(bw)}   <- discordant pairs are the McNemar input")
    print("=" * W)
    print(f"{'task':<34}|{A_LBL:^37}|{B_LBL:^37}|")
    print(f"{'':<34}|{HDR}|{HDR}|")
    print("-" * W)
    for t in sorted(set(A) | set(B)):
        a, b = A.get(t), B.get(t)
        flag = ""
        if a and b and "RUN" not in (a["rew"], b["rew"]) and a["rew"] != b["rew"]:
            flag = " <<" if a["rew"] == 1.0 else " >>"
        print(f"{t[:34]:<34}|{cell(a)}|{cell(b)}|{flag}")
    print("-" * W)
    print("  << stella passed, claude failed    >> claude passed, stella failed    .. = in flight    ---- = no verifier verdict")

    def top(m):
        return "  ".join(f"{k}:{v}" for k, v in m.most_common(6)) or "(none yet)"
    print(f"\n  tool mix (NOT comparable across arms — different toolsets; counts above are)")
    print(f"    {A_LBL:<12} {top(amix)}")
    print(f"    {B_LBL:<12} {top(bmix)}")
    sys.stdout.flush()
    time.sleep(20)
