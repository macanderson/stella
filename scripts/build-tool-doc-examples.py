#!/usr/bin/env python3
"""Distil a bench tool-call capture into the committed example fixture.

`docs/tools/*.toml` carries, per tool, one **observed** input payload and one
**observed** output payload. Observed is the whole point: an invented example
teaches the reader what the author imagined the tool is for, and this
repository has been wrong about that before — in the 2026-08-11 capture, 33 of
the 72 tools declared *then* were never called once across 408 bench trials,
which is the single most useful fact about them and exactly the fact a
plausible hand-written payload would hide. Both numbers are properties of that
capture and not of today's catalog, which declares far fewer tools; the pages
recompute their own from the fixture rather than repeating these (#4420).

The capture itself is a bench artifact and does not live in the repository, so
this script is the reproducible step between the two: point it at a capture,
get the fixture. The fixture is committed; the generator
(`crates/stella-cli/src/tool_docs.rs`) reads only the fixture, so the doc
generation and its drift guard stay hermetic.

    python3 scripts/build-tool-doc-examples.py \
        --corpus  <dir>/tool_calls.jsonl \
        --census  <dir>/tool_census.csv \
        --captured 2026-08-11 \
        --out crates/stella-cli/tests/fixtures/tool_doc_examples.json

Selection is deterministic and stated rather than curated: for each tool, the
first row in capture order whose result is the `ok` arm and whose payloads fit
the size caps below, falling back to the first row of any kind. "First" is a
choice with no judgement in it, which is what makes the fixture reproducible
from the same capture.

Scrubbing is conservative, because these payloads come off real machines:
absolute home paths are collapsed, anything shaped like a credential is
replaced, and both halves are truncated with the truncation declared in the
fixture rather than hidden.
"""

from __future__ import annotations

import argparse
import csv
import json
import pathlib
import re
import sys

# Payload caps. A tool example is there to show the *shape* of a call; a
# 40KB output payload shows nothing the first twenty lines do not.
INPUT_CAP = 600
OUTPUT_CAP = 900

HOME_PATH = re.compile(r"/(?:Users|home)/[^/\s\"']+")
SECRETISH = re.compile(
    r"\b(?:sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9]{16,}|"
    r"AKIA[0-9A-Z]{12,}|eyJ[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{10,})"
)


def scrub(text: str) -> str:
    """Collapse host-identifying paths and anything credential-shaped."""
    text = HOME_PATH.sub("/<home>", text)
    return SECRETISH.sub("<redacted>", text)


def clip(text: str, cap: int) -> "tuple[str, bool]":
    """Return the text bounded to `cap` characters and whether it was cut."""
    if len(text) <= cap:
        return text, False
    # Cut on a line boundary when one is near, so the example stays readable.
    head = text[:cap]
    newline = head.rfind("\n")
    if newline > cap // 2:
        head = head[:newline]
    return head, True


def pretty(value):
    return json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False)


def load_corpus(path):
    rows = []
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def output_envelope(raw):
    """The captured result, as the `ToolOutput` envelope it serializes to."""
    if isinstance(raw, str):
        try:
            raw = json.loads(raw)
        except json.JSONDecodeError:
            return None
    return raw if isinstance(raw, dict) else None


def render_example(row, envelope):
    input_text, input_cut = clip(scrub(pretty(row.get("args"))), INPUT_CAP)
    output_text, output_cut = clip(scrub(pretty(envelope)), OUTPUT_CAP)
    return {
        "arm": row.get("arm"),
        "task": row.get("task"),
        "trial": row.get("trial"),
        "outcome": "ok" if "ok" in envelope else "error",
        "input": input_text,
        "input_truncated": input_cut,
        "output": output_text,
        "output_truncated": output_cut,
    }


def pick_example(rows):
    """The first `ok` row in capture order, else the first row of any kind."""
    fallback = None
    for row in rows:
        envelope = output_envelope(row.get("output"))
        if envelope is None:
            continue
        if fallback is None:
            fallback = (row, envelope)
        if "ok" in envelope:
            return render_example(row, envelope)
    if fallback is None:
        return None
    return render_example(*fallback)


def load_census(path):
    """Per-tool usage rows, plus the two run totals they imply.

    `trials_scanned` and `model_calls` are not columns; they are recoverable
    from the ratios that are (`trial_share = trials_used / trials_scanned`,
    `calls_per_turn = calls / model_calls`) off the highest-volume row, where
    the rounding error is smallest. Deriving them beats copying two numbers
    out of a chat message, and disagreeing derivations are a bug report.
    """
    usage = {}
    with path.open(encoding="utf-8") as handle:
        rows = list(csv.DictReader(handle))

    busiest = max(rows, key=lambda row: int(row["calls"]))
    trials_scanned = round(int(busiest["trials_used"]) / float(busiest["trial_share"]))
    model_calls = round(int(busiest["calls"]) / float(busiest["calls_per_turn"]))

    for row in rows:
        # `in_schema` is the census's own note that a name appeared in tool
        # traffic without appearing in the advertised schema list. Those rows
        # are real observations about the runtime, not tools this catalog
        # declares, so they carry no doc page.
        #
        # It is also the only direct evidence any page has for saying a tool
        # *was* advertised, which is why `tool_docs.rs::Usage` deserializes it
        # without a default (#4420): a fixture that cannot answer must fail to
        # parse rather than let the pages infer advertisement from the row
        # merely existing.
        usage[row["tool"]] = {
            "calls": int(row["calls"]),
            "trials_used": int(row["trials_used"]),
            "calls_per_turn": float(row["calls_per_turn"]),
            "avg_first_step": float(row["avg_first_step"]) if row["avg_first_step"] else None,
            "failures": int(row["failures"]),
            "fail_rate": float(row["fail_rate"]) if row["fail_rate"] else 0.0,
            "in_schema": row["in_schema"] == "yes",
        }
    return usage, {"trials_scanned": trials_scanned, "model_calls": model_calls}


def main():
    parser = argparse.ArgumentParser(description="Build the tool-doc example fixture.")
    parser.add_argument("--corpus", required=True, type=pathlib.Path)
    parser.add_argument("--census", required=True, type=pathlib.Path)
    parser.add_argument("--captured", required=True, help="ISO date of the capture")
    parser.add_argument("--out", required=True, type=pathlib.Path)
    args = parser.parse_args()

    corpus = load_corpus(args.corpus)
    by_tool = {}
    for row in corpus:
        by_tool.setdefault(row["tool"], []).append(row)

    examples = {}
    for tool in sorted(by_tool):
        example = pick_example(by_tool[tool])
        if example is not None:
            examples[tool] = example

    usage, totals = load_census(args.census)

    arms = sorted({row["arm"] for row in corpus if row.get("arm")})
    tasks = sorted({row["task"] for row in corpus if row.get("task")})
    fixture = {
        "provenance": {
            "captured": args.captured,
            "source": "Terminal-Bench trial traces (stella-events.jsonl), "
            "distilled by scripts/build-tool-doc-examples.py",
            "corpus_rows": len(corpus),
            "corpus_arms": arms,
            "corpus_tasks": len(tasks),
            "census_trials_scanned": totals["trials_scanned"],
            "census_model_calls": totals["model_calls"],
            "selection": "first ok-arm call per tool in capture order; first call "
            "of any kind when the tool never succeeded",
            "scrubbing": "home paths collapsed to /<home>, credential-shaped "
            "tokens redacted, input clipped at {} and output at {} characters "
            "with the clip declared per example".format(INPUT_CAP, OUTPUT_CAP),
        },
        "examples": examples,
        "usage": usage,
    }

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(fixture, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(
        "wrote {}: {} observed examples, {} census rows, {} capture rows".format(
            args.out, len(examples), len(usage), len(corpus)
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
