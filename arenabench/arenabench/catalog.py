# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The model catalog for the role-model select (#2065).

Slugs come from Stella's own catalog — ``crates/stella-model/src/catalog.rs``
in the checkout :func:`~.sut.stella_repo` names — because that seed is what
decides which models are *reachable inside a benchmark container at all*: an
unlisted model dies at engine startup, so a select offering anything else
would be worse than the free-text box it replaces.

The parser is the same shape as the harbor adapter's
``TestOutputCeilingParity`` parser (``bench/harbor_adapter/tests/
test_posture.py``): chunk on ``CatalogEntry::new(``, bounded by the test
module. Parsing Rust by path is known to be brittle — a crate move retargets
it with nothing on the Rust side able to notice — so every failure here
names the file and where the path logic lives, and degrades to an error the
UI shows beside a free-text fallback rather than an empty select
(an empty dropdown reads as "no models exist" when it means "we could not
tell").

``benchmarked`` mirrors the adapter's ``_BENCHMARKED_SLUGS`` — the slugs
whose output caps and timeouts were actually derived from measurement — read
via ``ast`` from ``posture.py`` rather than imported, so a posture module
that grows imports of its own never breaks this reader.
"""

from __future__ import annotations

import ast
import re
from pathlib import Path
from typing import Any

from .sut import STELLA_REPO_ENV, stella_repo

__all__ = ["models_payload", "parse_catalog"]

#: Repo-relative homes of the two sources. Literals, like the harbor
#: adapter's `_CATALOG_RS` — if a crate moves, this is the line to fix.
_CATALOG_REL = Path("crates") / "stella-model" / "src" / "catalog.rs"
_POSTURE_REL = Path("bench") / "harbor_adapter" / "stella_harbor" / "posture.py"

_ENTRY_HEAD = re.compile(r'\s*"([^"]+)"\s*,\s*"([^"]+)"')
_OUTPUT_CAP = re.compile(r"with_max_output_tokens\(\s*Some\(\s*([0-9_]+)\s*\)")


def parse_catalog(source: str) -> list[dict[str, Any]]:
    """Every shipping catalog entry as ``{slug, provider, output_cap}``.

    ``output_cap`` is ``None`` when the entry declares no
    ``with_max_output_tokens`` — the engine falls back to its global default
    for those, and the UI says so rather than inventing a number.
    """
    # Bound the region at the test module: the last real entry's chunk
    # otherwise runs on into `#[cfg(test)]` fixtures (the harbor parser
    # learned this the hard way).
    source = source.split("#[cfg(test)]")[0]
    entries: list[dict[str, Any]] = []
    for chunk in source.split("CatalogEntry::new(")[1:]:
        head = _ENTRY_HEAD.match(chunk)
        if head is None:
            continue
        slug, provider = head.groups()
        if slug.startswith("test-only"):
            continue  # fixtures for the catalog's own tests, never benchmarked
        cap = _OUTPUT_CAP.search(chunk)
        entries.append(
            {
                "slug": slug,
                "provider": provider,
                "output_cap": int(cap.group(1).replace("_", "")) if cap else None,
            }
        )
    return entries


def parse_benchmarked_slugs(posture_source: str) -> frozenset[str]:
    """``_BENCHMARKED_SLUGS`` out of the adapter's ``posture.py``.

    Read with ``ast`` rather than imported: the constant is a
    ``frozenset({...})`` literal, and evaluating just that literal keeps
    this immune to whatever else the module imports or executes.
    """
    tree = ast.parse(posture_source)
    for node in ast.walk(tree):
        if not isinstance(node, ast.Assign):
            continue
        targets = [t.id for t in node.targets if isinstance(t, ast.Name)]
        if "_BENCHMARKED_SLUGS" not in targets:
            continue
        value = node.value
        if (
            isinstance(value, ast.Call)
            and isinstance(value.func, ast.Name)
            and value.func.id == "frozenset"
            and value.args
        ):
            return frozenset(ast.literal_eval(value.args[0]))
    return frozenset()


def _read(repo: Path, relative: Path, what: str) -> str:
    path = repo / relative
    try:
        return path.read_text(encoding="utf-8")
    except OSError as exc:
        raise RuntimeError(
            f"cannot read {what} at {path}: {exc.strerror}. The repo-relative "
            "path is a literal in arenabench/catalog.py — if the file moved, "
            "fix it there."
        ) from exc


def models_payload() -> dict[str, Any]:
    """The ``/api/models`` body: catalog entries grouped by provider.

    Raises with an actionable reason when no Stella checkout is configured
    or a source is unreadable — the route surfaces that reason and the UI
    falls back to free text naming it, never an empty select.
    """
    repo = stella_repo()
    if repo is None:
        raise RuntimeError(
            f"no Stella checkout found — set {STELLA_REPO_ENV} to the "
            "repository root so the model catalog can be read"
        )
    entries = parse_catalog(_read(repo, _CATALOG_REL, "the model catalog"))
    if not entries:
        raise RuntimeError(
            f"the model catalog at {repo / _CATALOG_REL} parsed to zero "
            "entries — the parser in arenabench/catalog.py no longer matches "
            "its shape"
        )
    benchmarked = parse_benchmarked_slugs(
        _read(repo, _POSTURE_REL, "the benchmark posture")
    )
    providers: dict[str, list[dict[str, Any]]] = {}
    for entry in entries:
        bare = entry["slug"].rsplit("/", 1)[-1].lower()
        providers.setdefault(entry["provider"], []).append(
            {
                "slug": entry["slug"],
                "benchmarked": bare in benchmarked,
                "output_cap": entry["output_cap"],
            }
        )
    for listing in providers.values():
        listing.sort(key=lambda e: e["slug"])
    return {"providers": providers}
