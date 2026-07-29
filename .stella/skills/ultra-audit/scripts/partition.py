#!/usr/bin/env python3
"""Partition a workspace into audit units with disjoint, enumerable file ownership.

Two things this guarantees, both mechanically:

  * DISJOINT — no file is owned by two agents. Two agents editing one file clobber
    each other, and the loser's work vanishes without an error.
  * COMPLETE — every non-generated source file is owned by exactly one agent, and the
    coverage number is computed, not claimed. "We audited the codebase" is only true
    if this prints 100%.

Splits happen on directory boundaries only, never mid-directory, so ownership is always
expressible as a short list of globs rather than 400 enumerated paths.

    python3 partition.py <gate.json> [--target 13000] [--max-agents N] [--out units.json]

Reads the inventory produced by detect.py; does not walk the tree again. Stdlib only.
"""
from __future__ import annotations

import argparse
import json
import os
import sys
from collections import defaultdict

TARGET = 13000   # LOC per agent — empirically the size that finishes inside the
                 # connection-drop window (operations.md §1)
CONCURRENCY = 8  # min(16, cores-2) on this machine; agents/CONCURRENCY = waves
TOL = 1.25       # a unit may exceed target by this much before it must be split again.
                 # Used for BOTH the "no split needed" test and the "that pack was too
                 # unbalanced" retry, so the two can never disagree and leak an oversized part.

MANIFESTS = ("Cargo.toml", "package.json", "pyproject.toml", "go.mod", "setup.py",
             "build.gradle", "build.gradle.kts", "pom.xml", "Gemfile", "Package.swift")


def find_unit_roots(root: str, paths: list[str]) -> list[str]:
    """Directories holding a package manifest — the seams the project itself declares."""
    dirs = {os.path.dirname(p) for p in paths}
    dirs.add("")
    roots = set()
    for d in dirs:
        cur = d
        while True:
            if any(os.path.exists(os.path.join(root, cur, m)) for m in MANIFESTS):
                roots.add(cur)
                break
            if not cur:
                break
            cur = os.path.dirname(cur)
    # A manifest at the repo root is a fallback, not a seam, when real sub-units exist.
    if len(roots) > 1:
        roots.discard("")
    return sorted(roots, key=len, reverse=True)


def owning_root(path: str, roots: list[str]) -> str:
    """Longest-prefix match: the innermost declared unit containing this file."""
    d = os.path.dirname(path)
    for r in roots:                      # roots is sorted longest-first
        if r == "" or d == r or d.startswith(r + "/"):
            return r
    return ""


def atoms(base: str, files: list[dict], target: int) -> list[tuple[str, int, list[str]]]:
    """Recursively descend `base` into (glob, loc, paths) atoms that each fit the target.

    An atom is the largest directory subtree that is small enough to hand to one agent, so
    bin-packing atoms can never produce an oversized part unless a single indivisible leaf
    is itself oversized. Descending — rather than grouping at a fixed depth — is what makes
    this work on a tree like apps/app, where one child holds 87k lines and a fixed-depth
    split just hands it back whole.
    """
    loc = sum(f["loc"] for f in files)
    if loc <= target * TOL or len(files) == 1:
        return [(f"{base}/**" if base else "**", loc, [f["path"] for f in files])]

    here: list[dict] = []
    kids: dict[str, list[dict]] = defaultdict(list)
    for f in files:
        rel = os.path.relpath(f["path"], base) if base else f["path"]
        parts = rel.split("/")
        (here if len(parts) == 1 else kids[parts[0]]).append(f)

    if not kids:                       # all files sit directly here and together exceed
        return [(f"{base}/**" if base else "**", loc,
                 [f["path"] for f in files])]      # indivisible; caller packs by file

    out: list[tuple[str, int, list[str]]] = []
    if here:
        out.append((f"{base}/* (files directly in this directory only)" if base
                    else "* (top-level files only)",
                    sum(f["loc"] for f in here), [f["path"] for f in here]))
    for k, fs in sorted(kids.items()):
        out.extend(atoms(os.path.join(base, k) if base else k, fs, target))
    return out


def binpack(groups: list[tuple[str, int, list[str]]], target: int, min_bins: int = 1
            ) -> list[list[tuple[str, int, list[str]]]]:
    """Greedy largest-first pack of directory groups into ~target-sized bins.

    Ceiling, not rounding: `round(1.36 * target / target)` is 1, which would silently
    hand a 1.36x-oversized unit back as a single "split" part.
    """
    total = sum(g[1] for g in groups)
    n = max(min_bins, -(-total // target)) if target else min_bins
    n = min(n, len(groups))
    bins: list[list] = [[] for _ in range(n)]
    sizes = [0] * n
    for g in sorted(groups, key=lambda g: -g[1]):
        i = sizes.index(min(sizes))
        bins[i].append(g)
        sizes[i] += g[1]
    return [b for b in bins if b]


def split_unit(name: str, base: str, files: list[dict], target: int) -> list[dict]:
    """One unit, split on directory boundaries until every part is near target."""
    loc = sum(f["loc"] for f in files)
    if loc <= target * TOL:
        return [{"unit": name, "group": name, "loc": loc, "n_files": len(files),
                 "owns_globs": [f"{base}/**" if base else "**"],
                 "owns_files": [f["path"] for f in files]}]

    parts = atoms(base, files, target)

    # An atom bigger than the target is an indivisible leaf (one directory of files, or one
    # very large file). Break it down by explicit file list — ownership must stay unambiguous
    # even when it cannot stay tidy.
    flat: list[tuple[str, int, list[str]]] = []
    for glob, loc_a, paths in parts:
        if loc_a > target * TOL and len(paths) > 1:
            per_file = [(p, next(f["loc"] for f in files if f["path"] == p), [p])
                        for p in paths]
            for i, b in enumerate(binpack(per_file, target, min_bins=2), 1):
                flat.append((f"{glob} [file group {i}]", sum(g[1] for g in b),
                             sorted(g[0] for g in b)))
        else:
            flat.append((glob, loc_a, paths))

    out = []
    for i, b in enumerate(binpack(flat, target, min_bins=2), 1):
        globs, owned = [], []
        for glob, _, paths in sorted(b):
            globs.append(glob)
            owned.extend(paths)
        out.append({"unit": f"{name}::part-{i}", "group": name,
                    "loc": sum(g[1] for g in b), "n_files": len(owned),
                    "owns_globs": globs, "owns_files": sorted(owned)})
    return out


def verify(units: list[dict], expected: list[str]) -> dict:
    """The coverage proof. Computed, never asserted by prose."""
    seen: dict[str, list[str]] = defaultdict(list)
    for u in units:
        for p in u["owns_files"]:
            seen[p].append(u["unit"])
    dupes = {p: us for p, us in seen.items() if len(us) > 1}
    missing = sorted(set(expected) - set(seen))
    extra = sorted(set(seen) - set(expected))
    return {
        "files_expected": len(expected),
        "files_owned": len(seen),
        "coverage_pct": round(100.0 * len(seen) / len(expected), 2) if expected else 0.0,
        "duplicated": dupes,
        "unowned": missing,
        "phantom": extra,
        "disjoint": not dupes,
        "complete": not missing and not extra,
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("gate", help="gate.json from detect.py")
    ap.add_argument("--target", type=int, default=TARGET)
    ap.add_argument("--max-agents", type=int, default=0,
                    help="cap unit agents; excluded units are listed explicitly, never dropped "
                         "silently")
    ap.add_argument("--out", default="units.json")
    a = ap.parse_args()

    gate = json.load(open(a.gate))
    root = gate["root"]
    inv = [f for f in gate["inventory"] if not f["generated"]]
    if not inv:
        sys.exit("inventory is empty — check detect.py output")
    expected = [f["path"] for f in inv]

    roots = find_unit_roots(root, expected)
    by_root: dict[str, list[dict]] = defaultdict(list)
    for f in inv:
        by_root[owning_root(f["path"], roots)].append(f)

    units: list[dict] = []
    for base in sorted(by_root, key=lambda b: -sum(f["loc"] for f in by_root[b])):
        name = base or "(repo root)"
        units.extend(split_unit(name, base, by_root[base], a.target))

    units.sort(key=lambda u: -u["loc"])
    proof = verify(units, expected)

    excluded: list[dict] = []
    if a.max_agents and len(units) > a.max_agents:
        excluded = units[a.max_agents:]
        units = units[: a.max_agents]
        proof["capped"] = {
            "kept": len(units), "excluded": len(excluded),
            "excluded_loc": sum(u["loc"] for u in excluded),
            "excluded_units": [u["unit"] for u in excluded],
            "coverage_after_cap_pct": round(
                100.0 * sum(u["n_files"] for u in units) / len(expected), 2),
        }

    for u in units:
        u.setdefault("note", "")

    with open(a.out, "w") as fh:
        json.dump({"root": root, "target": a.target, "units": units,
                   "excluded": excluded, "proof": proof}, fh, indent=1)

    total = sum(u["loc"] for u in units)
    waves = -(-len(units) // CONCURRENCY)
    print(f"{root}")
    print(f"{len(units)} unit agents · {total:,} lines · target {a.target:,}/agent")
    print(f"{waves} sequential wave(s) at concurrency {CONCURRENCY}\n")
    for u in units:
        flag = "  !! OVERSIZED" if u["loc"] > a.target * TOL else ""
        print(f"  {u['unit'][:46]:<46} {u['loc']:>8,}  {u['n_files']:>4}f{flag}")

    print(f"\nCOVERAGE PROOF")
    print(f"  files expected     {proof['files_expected']:,}")
    print(f"  files owned        {proof['files_owned']:,}  ({proof['coverage_pct']}%)")
    print(f"  disjoint           {proof['disjoint']}")
    print(f"  complete           {proof['complete']}")
    if proof["duplicated"]:
        print(f"  !! {len(proof['duplicated'])} FILES OWNED TWICE — agents will clobber "
              f"each other:")
        for p, us in list(proof["duplicated"].items())[:10]:
            print(f"       {p}  <- {', '.join(us)}")
    if proof["unowned"]:
        print(f"  !! {len(proof['unowned'])} FILES UNOWNED (nobody audits these):")
        for p in proof["unowned"][:10]:
            print(f"       {p}")
    if excluded:
        c = proof["capped"]
        print(f"\n  !! CAPPED AT {a.max_agents} AGENTS — coverage is now "
              f"{c['coverage_after_cap_pct']}%, NOT 100%.")
        print(f"     {c['excluded']} units / {c['excluded_loc']:,} lines are NOT audited:")
        for n in c["excluded_units"][:12]:
            print(f"       {n}")
        print("     Say this in the report. An audit that silently skipped code is a lie.")

    print(f"\n-> {a.out}")
    print("EDIT IT: add a 'note' per unit saying why the unit matters and what to "
          "scrutinise — per-unit notes measurably improved finding quality on prior runs.")
    if not (proof["disjoint"] and proof["complete"]):
        sys.exit("\nREFUSING to declare this partition sound. Fix the errors above first.")


if __name__ == "__main__":
    main()
