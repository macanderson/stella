#!/usr/bin/env python3
"""Render the audit result into ONE self-contained HTML file, then validate it.

Three rules this file exists to enforce:

  * The payload never passes through the conversation. Template and data are read from disk
    and the joined file is written to disk. The findings blob is 80-300KB of JSON.
  * `</` is escaped to `<\\/` before splicing. Load-bearing, not defensive: a finding whose
    text contains a closing tag — and audit findings quote source constantly — would
    otherwise terminate the <script> block and produce a blank page.
  * The output is validated, not eyeballed: the inline script is extracted and syntax-checked,
    every mount point the renderer targets is asserted present, and self-containment (no
    remote asset of any kind) is asserted.

A partial result renders. A run that died in the last phase should still produce a report —
an audit with no artifact is an audit that did not happen.

    python3 render_report.py <result.json> [--scoring scoring.json] [--baseline baseline.txt]
                             [--provenance provenance.json] [--out report.html]
"""
from __future__ import annotations

import argparse
import datetime
import json
import os
import re
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
TEMPLATE = os.path.join(HERE, "..", "assets", "report.template.html")

# Every id the inline renderer writes to. If the template loses one, the section silently
# vanishes from the report — so assert them rather than trusting the markup.
MOUNTS = ["eyebrow", "ov", "ovband", "ovdelta", "stats", "alerts", "summary", "dims",
          "dimlegend", "dimdetails", "moved", "movedlegend", "panelnote", "panel", "secondop", "risks",
          "excluded", "prior", "fixes", "gate", "roadmap", "method", "foot",
          "sec-moved", "sec-panel", "sec-secondop", "sec-excluded", "sec-prior",
          "sec-fixes", "sec-gate", "sec-roadmap"]

REMOTE = re.compile(
    r"""(?:src|href)\s*=\s*["']\s*(?:https?:)?//"""      # remote script/style/img
    r"""|@import\s+url\(\s*["']?\s*(?:https?:)?//"""      # remote css import
    r"""|\bfetch\s*\(|XMLHttpRequest|new\s+WebSocket|EventSource\s*\(""",
    re.I)


def load(path: str) -> dict:
    raw = json.load(open(path))
    return raw.get("result", raw) if isinstance(raw, dict) else raw


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("result")
    ap.add_argument("--scoring", help="score.py --json output (authoritative for every number)")
    ap.add_argument("--baseline", help="file holding the measured pre-audit gate")
    ap.add_argument("--provenance", help="progress.py --provenance --json output")
    ap.add_argument("--title", default="")
    ap.add_argument("--out", default="")
    a = ap.parse_args()

    d = load(a.result)
    run = d.get("run") or {}
    root = run.get("root") or ""
    repo = os.path.basename(root.rstrip("/")) or "workspace"

    # score.py is authoritative. The harness's own scoring block is the fallback, and the
    # difference matters: score.py is the only thing that recomputes the panel median from
    # raw votes and the prior round from its recorded scores.
    scoring = dict(d.get("scoring") or {})
    if a.scoring and os.path.exists(a.scoring):
        scoring.update(json.load(open(a.scoring)))
    elif not a.scoring:
        print("!! no --scoring given: falling back to the harness's own arithmetic. Run "
              "score.py --json and pass it, or the report may publish an unreconciled number.",
              file=sys.stderr)

    panel = d.get("panel") or {}
    votes = panel.get("votes") or []
    # Reshape votes per dimension for the dot plot.
    panel_votes: dict[str, list] = {}
    for dim in (scoring.get("weights") or {}):
        row = [{"model": v.get("model"), "score": (v.get("scores") or {}).get(dim)}
               for v in votes if isinstance((v.get("scores") or {}).get(dim), (int, float))
               and (v.get("scores") or {}).get(dim) > 0]
        if row:
            panel_votes[dim] = row

    baseline = ""
    if a.baseline and os.path.exists(a.baseline):
        baseline = open(a.baseline).read().strip()

    provenance = {}
    if a.provenance and os.path.exists(a.provenance):
        provenance = json.load(open(a.provenance))

    agents_total = (len(d.get("regressionResults") or []) + len(d.get("unitResults") or [])
                    + len(d.get("fixReviews") or []) + len(d.get("xcutResults") or [])
                    + len(d.get("sweepResults") or []) + len(votes)
                    + len(d.get("critiques") or []) + 2)

    data = {
        "run": run,
        "agents_total": agents_total,
        "scoring": scoring,
        "synthesis": d.get("synthesis") or {},
        "findings": d.get("findings") or {},
        "verify": d.get("verify") or {},
        "regressionResults": d.get("regressionResults") or [],
        "fixReviews": d.get("fixReviews") or [],
        "critiques": d.get("critiques") or [],
        "panel_raw": [{"model": v.get("model"), "scores": v.get("scores"),
                       "headline": v.get("headline"),
                       "worst_dimension": v.get("worst_dimension")} for v in votes],
        "panel_votes": panel_votes,
        "provenance": provenance,
        "baseline": baseline,
        "generated_at": datetime.datetime.now().strftime("%Y-%m-%d %H:%M"),
    }

    title = a.title or (f"{repo} — engineering audit, round "
                        f"{scoring.get('round') or run.get('round') or 1}")
    out = a.out or os.path.join(
        root or ".", ".ultra-audit",
        f"report-{data['generated_at'][:10]}-{(run.get('commit') or 'nocommit')[:9]}.html")
    os.makedirs(os.path.dirname(os.path.abspath(out)), exist_ok=True)

    tpl = open(TEMPLATE).read()
    for mount in MOUNTS:
        if f'id="{mount}"' not in tpl:
            sys.exit(f"template is missing mount point id={mount!r} — the renderer writes to "
                     f"it, so that section would silently vanish from the report.")

    # Load-bearing. A finding containing `</script>` would otherwise end the script block.
    # U+2028/9 are literal line terminators in JS source and must be escaped too.
    payload = (json.dumps(data, ensure_ascii=False, default=str)
               .replace("</", "<\\/")
               .replace(" ", "\\u2028")
               .replace(" ", "\\u2029"))

    html = tpl.replace("__DATA__", payload, 1).replace("__TITLE__", title, 1)
    for ph in ("__DATA__", "__TITLE__"):
        if ph in html:
            sys.exit(f"placeholder {ph} survived the splice")

    with open(out, "w") as fh:
        fh.write(html)

    # ── validate the artifact ────────────────────────────────────────────────
    problems: list[str] = []

    m = re.search(r"<script>(.*?)</script>", html, re.S)
    if not m:
        problems.append("no inline <script> block found in the output — the splice broke the "
                        "structure")
    else:
        with tempfile.NamedTemporaryFile("w", suffix=".mjs", delete=False) as fh:
            fh.write(m.group(1))
            tmp = fh.name
        p = subprocess.run(["node", "--check", tmp], capture_output=True, text=True)
        os.unlink(tmp)
        if p.returncode:
            problems.append(f"the inline script does not parse — the page would render "
                            f"blank:\n{p.stderr.strip()}")

    # Only `</script>` can terminate the block, so that is the invariant to assert. A literal
    # `<script>` inside the payload is harmless — it sits inside a JS string — and does occur
    # legitimately: a security finding about tag sanitising quotes the opening tag verbatim.
    n_close = html.count("</script>")
    if n_close != 1:
        problems.append(f"found {n_close} literal '</script>' sequences, expected exactly 1. A "
                        f"finding's text escaped the payload and truncated the script block — "
                        f"the page will render blank or partial.")

    hit = REMOTE.search(html)
    if hit:
        problems.append(f"not self-contained — found a remote reference or network call: "
                        f"{hit.group(0)!r}")

    if re.search(r",\s*@media", html):
        problems.append("a selector is concatenated with an at-rule (',@media'), which is "
                        "invalid CSS and silently drops the rule")

    size = os.path.getsize(out)
    if size < 20_000:
        problems.append(f"output is only {size} bytes — suspiciously small; check the data "
                        f"actually spliced")

    print(f"-> {out}  ({size / 1024:.0f} KB)")
    print(f"   round {scoring.get('round') or run.get('round') or '?'} · "
          f"overall {scoring.get('overall_rounded', '?')} · "
          f"{len((d.get('findings') or {}).get('confirmed') or [])} confirmed risks · "
          f"{agents_total} agents")
    if provenance:
        print(f"   provenance verified: {provenance.get('ok')}")
    for line in problems:
        print(f"!! {line}")
    if problems:
        sys.exit(1)
    print("   validated: script parses · exactly one script block · no remote assets · "
          "all mount points present")
    print(f"\nOpen it:  open '{out}'")


if __name__ == "__main__":
    main()
